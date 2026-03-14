use crate::sip_debug::SipDebugger;
use rtp_core::audio_device::{AudioConfig, DeviceSelector};
use rtp_core::{AudioRecorder, CodecType, ReceiveEvent, RtpSession, SessionConfig};
use sip_core::header::{generate_branch, generate_tag, HeaderName};
use sip_core::message::{RequestBuilder, ResponseBuilder, SipMessage, SipMethod, StatusCode};
use sip_core::sdp::SdpSession;
use sip_core::dialog::SipDialog;
use sip_core::transport::SipTransport;
use std::collections::VecDeque;
use std::net::SocketAddr;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, BufReader};
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum PhoneError {
    #[error("transport error: {0}")]
    Transport(#[from] sip_core::transport::TransportError),
    #[error("RTP error: {0}")]
    Rtp(#[from] rtp_core::session::SessionError),
    #[error("no active dialog")]
    NoDialog,
    #[error("call failed: {0}")]
    CallFailed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DtmfMethod {
    Rfc2833,
    SipInfo,
}

#[derive(Debug, Clone)]
struct QueuedDtmf {
    digit: char,
    method: DtmfMethod,
}

pub struct SoftPhone {
    transport: SipTransport,
    dialog: Option<SipDialog>,
    rtp_session: Option<RtpSession>,
    call_id: Option<String>,
    local_tag: String,
    local_ip: String,
    live_recorder: Option<AudioRecorder>,
    pending_record_path: Option<String>,
    remote_sip_addr: Option<SocketAddr>,
    remote_dtmf_payload_type: Option<u8>,
    local_dtmf_payload_type: u8,
    dtmf_queue: VecDeque<QueuedDtmf>,
    last_announced_rfc2833: Option<(char, u32)>,
}

impl SoftPhone {
    pub async fn new(bind_addr: &str) -> Result<Self, PhoneError> {
        let transport = SipTransport::bind(bind_addr).await?;
        let local_addr = transport.local_addr();
        let local_ip = local_addr.ip().to_string();
        let local_tag = generate_tag();

        Ok(Self {
            transport,
            dialog: None,
            rtp_session: None,
            call_id: None,
            local_tag,
            local_ip,
            live_recorder: None,
            pending_record_path: None,
            remote_sip_addr: None,
            remote_dtmf_payload_type: None,
            local_dtmf_payload_type: 101,
            dtmf_queue: VecDeque::new(),
            last_announced_rfc2833: None,
        })
    }

    pub async fn register(
        &mut self,
        server: &str,
        user: &str,
        _password: &str,
    ) -> Result<(), PhoneError> {
        let call_id = Uuid::new_v4().to_string();
        let branch = generate_branch();
        let local_addr = self.transport.local_addr();

        let server_uri = if server.starts_with("sip:") {
            server.to_string()
        } else {
            format!("sip:{}", server)
        };

        let request = RequestBuilder::new(SipMethod::Register, &server_uri)
            .header(
                HeaderName::Via,
                format!(
                    "SIP/2.0/UDP {};branch={};rport",
                    local_addr, branch
                ),
            )
            .header(HeaderName::MaxForwards, "70")
            .header(
                HeaderName::From,
                format!("<sip:{}@{}>;tag={}", user, server, self.local_tag),
            )
            .header(
                HeaderName::To,
                format!("<sip:{}@{}>", user, server),
            )
            .header(HeaderName::CallId, &call_id)
            .header(HeaderName::CSeq, "1 REGISTER")
            .header(
                HeaderName::Contact,
                format!("<sip:{}@{}>", user, local_addr),
            )
            .header(HeaderName::Expires, "3600")
            .header(HeaderName::UserAgent, "siphone/0.1.0")
            .build();

        // Resolve server address
        let server_addr = resolve_server_addr(server)?;
        self.transport.send_to(&request, server_addr).await?;
        self.call_id = Some(call_id);

        Ok(())
    }

    pub async fn call(
        &mut self,
        uri: &str,
        server: Option<&str>,
        user: Option<&str>,
    ) -> Result<(), PhoneError> {
        let target_uri = if uri.starts_with("sip:") {
            uri.to_string()
        } else {
            format!("sip:{}", uri)
        };
        let caller_user = user
            .map(|u| u.to_string())
            .or_else(|| extract_user_from_uri(&target_uri))
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_else(|| "siphone".to_string());

        // Extract host from URI if no server given: sip:user@host -> host
        let server_host = if let Some(s) = server {
            s.to_string()
        } else {
            extract_host_from_uri(&target_uri)
                .ok_or_else(|| PhoneError::CallFailed(
                    "Cannot extract server from URI. Use --server or sip:user@host format".into()
                ))?
        };

        let call_id = Uuid::new_v4().to_string();
        let branch = generate_branch();
        let local_addr = self.transport.local_addr();

        // Create RTP session for audio
        let rtp_remote = resolve_server_addr(&server_host)?;
        let rtp_config = SessionConfig::new("0.0.0.0:0", rtp_remote, CodecType::Pcmu);
        let rtp_session = RtpSession::new(rtp_config).await?;
        let rtp_port = rtp_session.local_addr().port();

        // Build SDP offer
        let mut sdp = SdpSession::new(&self.local_ip);
        sdp.add_audio_media(rtp_port);
        self.local_dtmf_payload_type = sdp.get_audio_dtmf_payload_type().unwrap_or(101);
        self.remote_dtmf_payload_type = None;
        self.dtmf_queue.clear();
        self.last_announced_rfc2833 = None;
        let sdp_body = sdp.to_string();

        let request = RequestBuilder::new(SipMethod::Invite, &target_uri)
            .header(
                HeaderName::Via,
                format!(
                    "SIP/2.0/UDP {};branch={};rport",
                    local_addr, branch
                ),
            )
            .header(HeaderName::MaxForwards, "70")
            .header(
                HeaderName::From,
                format!("<sip:{}@{}>;tag={}", caller_user, server_host, self.local_tag),
            )
            .header(HeaderName::To, format!("<{}>", target_uri))
            .header(HeaderName::CallId, &call_id)
            .header(HeaderName::CSeq, "1 INVITE")
            .header(
                HeaderName::Contact,
                format!("<sip:{}@{}>", caller_user, local_addr),
            )
            .header(HeaderName::ContentType, "application/sdp")
            .header(HeaderName::UserAgent, "siphone/0.1.0")
            .body(&sdp_body)
            .build();

        let server_addr = resolve_server_addr(&server_host)?;
        self.transport.send_to(&request, server_addr).await?;

        // Create dialog
        let dialog = SipDialog::new_uac(
            call_id.clone(),
            self.local_tag.clone(),
            format!("sip:{}@{}", caller_user, server_host),
            target_uri,
        );

        self.dialog = Some(dialog);
        self.rtp_session = Some(rtp_session);
        self.call_id = Some(call_id);

        Ok(())
    }

    pub async fn hangup(&mut self) -> Result<(), PhoneError> {
        let dialog = self.dialog.as_mut().ok_or(PhoneError::NoDialog)?;

        let branch = generate_branch();
        let local_addr = self.transport.local_addr();
        let cseq = dialog.next_cseq();

        let remote_target = dialog
            .remote_target
            .as_deref()
            .unwrap_or(&dialog.remote_uri);

        let bye = RequestBuilder::new(SipMethod::Bye, remote_target)
            .header(
                HeaderName::Via,
                format!(
                    "SIP/2.0/UDP {};branch={};rport",
                    local_addr, branch
                ),
            )
            .header(HeaderName::MaxForwards, "70")
            .header(
                HeaderName::From,
                format!("<{}>;tag={}", dialog.local_uri, dialog.local_tag),
            )
            .header(
                HeaderName::To,
                format!(
                    "<{}>{}",
                    dialog.remote_uri,
                    dialog.remote_tag.as_ref()
                        .map(|t| format!(";tag={}", t))
                        .unwrap_or_default()
                ),
            )
            .header(HeaderName::CallId, &dialog.call_id)
            .header(HeaderName::CSeq, format!("{} BYE", cseq))
            .build();

        // Send BYE to the remote target
        if let Some(ref target) = dialog.remote_target {
            if let Some(addr) = sip_core::transport::resolve_sip_uri(target) {
                self.transport.send_to(&bye, addr).await?;
            }
        }

        dialog.terminate();
        self.rtp_session = None;

        Ok(())
    }

    pub async fn wait_for_response(&self) -> Result<SipMessage, PhoneError> {
        let incoming = self.transport.recv().await?;
        Ok(incoming.message)
    }

    pub async fn run_call(
        &mut self,
        mut recorder: Option<&mut AudioRecorder>,
        input_device: &str,
        output_device: &str,
    ) -> Result<(), PhoneError> {
        let mut rx = self.transport.start_receiving(32);
        let mut rtp_stop_tx: Option<tokio::sync::mpsc::Sender<()>> = None;
        let mut rtp_event_rx: Option<tokio::sync::mpsc::Receiver<ReceiveEvent>> = None;
        let mut live_recorder: Option<AudioRecorder> = None;
        let mut rtp_connected = false;
        let mut muted = false;
        let call_start = tokio::time::Instant::now();
        let mut recording_active = recorder.is_some();
        let mut announced_audio_tx = false;
        let mut announced_audio_rx = false;
        let mut debugger = SipDebugger::new(false);
        let local_addr = self.transport.local_addr();
        let audio_config = AudioConfig::telephony();
        let input_sel = DeviceSelector::from_arg(input_device);
        let output_sel = DeviceSelector::from_arg(output_device);

        #[cfg(feature = "audio-device")]
        let mut mic_capture: Option<rtp_core::audio_device::AudioCapture> = None;
        #[cfg(feature = "audio-device")]
        let mut speaker_playback: Option<rtp_core::audio_device::AudioPlayback> = None;

        // Interactive stdin reader
        let stdin = tokio::io::stdin();
        let mut stdin_reader = BufReader::new(stdin).lines();

        // Send silence every 20ms to keep NAT pinhole open and trigger remote RTP
        let mut silence_interval = tokio::time::interval(std::time::Duration::from_millis(20));
        silence_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        if recorder.is_some() {
            println!("Recording active.");
        }
        if rtp_core::audio_device::is_audio_available() {
            #[cfg(feature = "audio-device")]
            {
                match rtp_core::audio_device::AudioCapture::start(&input_sel, &audio_config) {
                    Ok(cap) => {
                        mic_capture = Some(cap);
                        println!("Live mic capture enabled: {}", input_sel);
                    }
                    Err(e) => {
                        println!("Mic capture unavailable (using silence TX): {}", e);
                    }
                }
                match rtp_core::audio_device::AudioPlayback::start(&output_sel, &audio_config) {
                    Ok(pb) => {
                        speaker_playback = Some(pb);
                        println!("Live speaker playback enabled: {}", output_sel);
                    }
                    Err(e) => {
                        println!("Speaker playback unavailable: {}", e);
                    }
                }
            }
        } else {
            println!("No live audio device available; RTP will run without local playback/capture.");
        }
        println!("Type 'help' for interactive commands.");

        loop {
            tokio::select! {
                biased;

                // Prioritize draining audio to prevent channel backpressure
                audio = async {
                    if let Some(ref mut arx) = rtp_event_rx {
                        arx.recv().await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    if let Some(event) = audio {
                        match event {
                            ReceiveEvent::Audio(frame) => {
                                if !announced_audio_rx {
                                    if let Some(ref rtp) = self.rtp_session {
                                        let s = rtp.stats();
                                        let codec = s.codec;
                                        println!(
                                            "Audio RX active: codec={} PT={} rate={}Hz",
                                            codec,
                                            codec.payload_type(),
                                            codec.clock_rate()
                                        );
                                    } else {
                                        println!("Audio RX active.");
                                    }
                                    announced_audio_rx = true;
                                }
                                #[cfg(feature = "audio-device")]
                                if let Some(ref playback) = speaker_playback {
                                    let _ = playback.play_frame(frame.clone()).await;
                                }
                                if recording_active {
                                    if let Some(rec) = recorder.as_deref_mut() {
                                        rec.record_frame(&frame);
                                        while let Ok(extra) = rtp_event_rx.as_mut().unwrap().try_recv() {
                                            match extra {
                                                ReceiveEvent::Audio(extra_frame) => {
                                                    rec.record_frame(&extra_frame);
                                                }
                                                ReceiveEvent::Dtmf(dtmf) => {
                                                    if dtmf.end {
                                                        let key = (dtmf.digit, dtmf.timestamp);
                                                        if self.last_announced_rfc2833 != Some(key) {
                                                            self.last_announced_rfc2833 = Some(key);
                                                            println!("DTMF received (RTP RFC2833): {}", dtmf.digit);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    } else if let Some(rec) = live_recorder.as_mut() {
                                        rec.record_frame(&frame);
                                        while let Ok(extra) = rtp_event_rx.as_mut().unwrap().try_recv() {
                                            match extra {
                                                ReceiveEvent::Audio(extra_frame) => {
                                                    rec.record_frame(&extra_frame);
                                                }
                                                ReceiveEvent::Dtmf(dtmf) => {
                                                    if dtmf.end {
                                                        let key = (dtmf.digit, dtmf.timestamp);
                                                        if self.last_announced_rfc2833 != Some(key) {
                                                            self.last_announced_rfc2833 = Some(key);
                                                            println!("DTMF received (RTP RFC2833): {}", dtmf.digit);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            ReceiveEvent::Dtmf(dtmf) => {
                                // RFC2833 sends repeated packets for one digit. Announce once when event ends.
                                if dtmf.end {
                                    let key = (dtmf.digit, dtmf.timestamp);
                                    if self.last_announced_rfc2833 != Some(key) {
                                        self.last_announced_rfc2833 = Some(key);
                                        println!("DTMF received (RTP RFC2833): {}", dtmf.digit);
                                    }
                                }
                            }
                        }
                    } else {
                        break;
                    }
                }

                sip_msg = rx.recv() => {
                    let Some(incoming) = sip_msg else { break };
                    let msg = incoming.message;

                    // Feed to SIP debugger if sniffing
                    debugger.capture_incoming(&msg, incoming.source, local_addr);

                    if let Some(ref mut dialog) = self.dialog {
                        if msg.is_response() {
                            dialog.process_response(&msg);

                            if let Some(status) = msg.status() {
                                if status.is_provisional() {
                                    println!("Call progress: {} {}", status, status.reason_phrase());
                                } else if status.is_success() {
                                    println!("Call connected!");
                                    self.remote_sip_addr = Some(incoming.source);

                                    // Parse SDP from response to get remote RTP addr
                                    if let Some(body) = msg.body() {
                                        if let Ok(sdp) = SdpSession::parse(body) {
                                            let rtp_port = sdp.get_audio_port().unwrap_or(0);
                                            let rtp_host = sdp.get_connection_address()
                                                .unwrap_or("0.0.0.0");
                                            self.remote_dtmf_payload_type = sdp.get_audio_dtmf_payload_type();
                                            if let Some(pt) = self.remote_dtmf_payload_type {
                                                println!("Remote DTMF RTP payload type: {}", pt);
                                            }
                                            if let Ok(addr) = format!("{}:{}", rtp_host, rtp_port)
                                                .parse::<SocketAddr>()
                                            {
                                                if let Some(ref mut rtp) = self.rtp_session {
                                                    rtp.set_remote_addr(addr);
                                                    println!("RTP remote: {}", addr);
                                                    let (erx, stx) = rtp.start_receiving_events(
                                                        1024,
                                                        self.remote_dtmf_payload_type,
                                                    );
                                                    rtp_event_rx = Some(erx);
                                                    rtp_stop_tx = Some(stx);
                                                    rtp_connected = true;
                                                }
                                            }
                                        }
                                    }

                                    // Send ACK
                                    let ack = build_ack_msg(dialog, &self.transport.local_addr());
                                    debugger.capture_outgoing(&ack, local_addr, incoming.source);
                                    self.transport.send_to(&ack, incoming.source).await?;
                                } else if status.is_error() {
                                    println!("Call failed: {} {}", status, status.reason_phrase());
                                    return Err(PhoneError::CallFailed(format!(
                                        "{} {}",
                                        status,
                                        status.reason_phrase()
                                    )));
                                }
                            }
                        } else if let Some(method) = msg.method() {
                            if *method == SipMethod::Bye {
                                println!("Remote party hung up");
                                dialog.process_bye(&msg);
                                break;
                            } else if *method == SipMethod::Info {
                                if let Some((digit, duration)) = parse_info_dtmf(msg.body()) {
                                    println!(
                                        "DTMF received (SIP INFO): {} (duration {})",
                                        digit, duration
                                    );
                                } else {
                                    println!("SIP INFO received");
                                }
                                if let SipMessage::Request(ref req) = msg {
                                    let ok = ResponseBuilder::from_request(req, StatusCode::OK).build();
                                    debugger.capture_outgoing(&ok, local_addr, incoming.source);
                                    self.transport.send_to(&ok, incoming.source).await?;
                                }
                            }
                        }

                        if dialog.is_terminated() {
                            break;
                        }
                    }
                }

                // Interactive CLI commands from stdin
                line = stdin_reader.next_line() => {
                    match line {
                        Ok(Some(input)) => {
                            let parts: Vec<&str> = input.trim().split_whitespace().collect();
                            if parts.is_empty() { continue; }

                            match parts[0].to_lowercase().as_str() {
                                "help" | "h" | "?" => {
                                    println!("Commands:");
                                    println!("  record <file.wav>  Start recording to WAV file");
                                    println!("  stop               Stop recording");
                                    println!("  mute               Mute outgoing audio");
                                    println!("  unmute             Unmute outgoing audio");
                                    println!("  stats              Show call statistics");
                                    println!("  sniff              Start SIP packet tracing");
                                    println!("  sniff verbose      Start with full headers/bodies");
                                    println!("  sniff stop         Stop SIP packet tracing");
                                    println!("  flows              Show call flow ladder diagrams");
                                    println!("  dtmf <digits>      Queue DTMF over RTP (RFC2833)");
                                    println!("  dtmf-info <digits> Queue DTMF over SIP INFO");
                                    println!("  dtmf-send          Send queued DTMF digits now");
                                    println!("  dtmf-queue         Show queued DTMF digits count");
                                    println!("  hangup | bye       End the call");
                                    println!("  help               Show this help");
                                }
                                "record" | "rec" => {
                                    if recorder.is_some() || live_recorder.is_some() {
                                        recording_active = true;
                                        println!("Recording resumed.");
                                    } else if parts.len() < 2 {
                                        println!("Usage: record <filename.wav>");
                                    } else {
                                        // Create a new recorder on the fly
                                        self.pending_record_path = Some(parts[1].to_string());
                                        live_recorder = Some(AudioRecorder::new(8000));
                                        recording_active = true;
                                        println!("Recording to: {}", parts[1]);
                                    }
                                }
                                "stop" => {
                                    if recording_active {
                                        recording_active = false;
                                        if let Some(rec) = recorder.as_deref() {
                                            println!("Recording paused ({:.1}s captured, {} frames)",
                                                rec.duration_ms() as f64 / 1000.0, rec.frame_count());
                                        } else if let Some(rec) = live_recorder.as_ref() {
                                            println!("Recording paused ({:.1}s captured, {} frames)",
                                                rec.duration_ms() as f64 / 1000.0, rec.frame_count());
                                        }
                                    } else {
                                        println!("Not recording.");
                                    }
                                }
                                "mute" => {
                                    muted = true;
                                    println!("Muted (outgoing audio silenced)");
                                }
                                "unmute" => {
                                    muted = false;
                                    println!("Unmuted");
                                }
                                "stats" | "info" => {
                                    let elapsed = call_start.elapsed();
                                    let mins = elapsed.as_secs() / 60;
                                    let secs = elapsed.as_secs() % 60;
                                    println!("--- Call Stats ---");
                                    println!("  Duration:  {:02}:{:02}", mins, secs);
                                    println!("  Muted:     {}", if muted { "yes" } else { "no" });
                                    if let Some(ref rtp) = self.rtp_session {
                                        let s = rtp.stats();
                                        println!("  Codec:     {}", s.codec);
                                        println!("  RTP local: {}", s.local_addr);
                                        println!("  RTP remote:{}", s.remote_addr);
                                        println!("  Packets TX:{}", s.packets_sent);
                                        println!("  SSRC:      0x{:08X}", s.ssrc);
                                    }
                                    if let Some(rec) = recorder.as_deref() {
                                        println!("  Recording: {} ({:.1}s, {} frames)",
                                            if recording_active { "active" } else { "paused" },
                                            rec.duration_ms() as f64 / 1000.0, rec.frame_count());
                                    } else if let Some(rec) = live_recorder.as_ref() {
                                        println!("  Recording: {} ({:.1}s, {} frames)",
                                            if recording_active { "active" } else { "paused" },
                                            rec.duration_ms() as f64 / 1000.0, rec.frame_count());
                                    }
                                    println!("  Sniffing:  {} ({} messages)",
                                        if debugger.is_active() { "active" } else { "off" },
                                        debugger.message_count());
                                    println!("------------------");
                                }
                                "sniff" | "trace" | "debug" => {
                                    if parts.get(1).map(|s| s.to_lowercase()) == Some("stop".into()) {
                                        debugger.stop();
                                        println!("SIP tracing stopped ({} messages captured).", debugger.message_count());
                                    } else {
                                        let verbose = parts.get(1).map(|s| s.to_lowercase()) == Some("verbose".into());
                                        debugger.start(verbose);
                                        println!("SIP tracing started{}. All SIP messages will be displayed.",
                                            if verbose { " (verbose)" } else { "" });
                                        println!("  'sniff stop' to stop, 'flows' to show diagrams.");
                                    }
                                }
                                "flows" | "flow" | "ladder" => {
                                    if debugger.message_count() == 0 {
                                        println!("No SIP messages captured. Start tracing with 'sniff' first.");
                                    } else {
                                        debugger.print_summary();
                                        debugger.print_flows();
                                    }
                                }
                                "dtmf" => {
                                    if parts.len() < 2 {
                                        println!("Usage: dtmf <digits>");
                                    } else {
                                        let queued = self.queue_dtmf_digits(parts[1], DtmfMethod::Rfc2833);
                                        if queued > 0 {
                                            println!("Queued {} DTMF digit(s) for RTP RFC2833.", queued);
                                        }
                                    }
                                }
                                "dtmf-info" | "dtmf_info" => {
                                    if parts.len() < 2 {
                                        println!("Usage: dtmf-info <digits>");
                                    } else {
                                        let queued = self.queue_dtmf_digits(parts[1], DtmfMethod::SipInfo);
                                        if queued > 0 {
                                            println!("Queued {} DTMF digit(s) for SIP INFO.", queued);
                                        }
                                    }
                                }
                                "dtmf-send" | "send-dtmf" => {
                                    let sent = self.flush_dtmf_queue(local_addr, &mut debugger).await?;
                                    println!("Sent {} queued DTMF digit(s).", sent);
                                }
                                "dtmf-queue" => {
                                    println!("Queued DTMF digits: {}", self.dtmf_queue.len());
                                }
                                "hangup" | "bye" | "quit" | "exit" | "q" => {
                                    println!("Hanging up...");
                                    // Capture outgoing BYE if sniffing
                                    if debugger.is_active() {
                                        if let Some(ref dialog) = self.dialog {
                                            let bye_msg = build_bye_msg(dialog, &local_addr);
                                            if let Some(ref target) = dialog.remote_target {
                                                if let Some(addr) = sip_core::transport::resolve_sip_uri(target) {
                                                    debugger.capture_outgoing(&bye_msg, local_addr, addr);
                                                }
                                            }
                                        }
                                    }
                                    self.hangup().await?;
                                    // Print final flows if we were sniffing
                                    if debugger.message_count() > 0 {
                                        debugger.print_summary();
                                        debugger.print_flows();
                                    }
                                    break;
                                }
                                _ => {
                                    println!("Unknown command '{}'. Type 'help' for commands.", parts[0]);
                                }
                            }
                        }
                        Ok(None) => break, // EOF
                        Err(_) => {} // ignore stdin errors
                    }
                }

                // Send silence frames to keep RTP flowing (NAT traversal)
                _ = silence_interval.tick(), if rtp_connected => {
                    if let Some(ref mut rtp) = self.rtp_session {
                        let mut tx_frame: Option<Vec<i16>> = None;
                        if !muted {
                            #[cfg(feature = "audio-device")]
                            {
                                if let Some(capture) = mic_capture.as_mut() {
                                    if let Ok(Some(frame)) = tokio::time::timeout(
                                        std::time::Duration::from_millis(5),
                                        capture.next_frame(),
                                    )
                                    .await
                                    {
                                        tx_frame = Some(frame);
                                    }
                                }
                            }
                        }
                        let frame = tx_frame.unwrap_or_else(|| vec![0i16; 160]);
                        if let Ok(sent) = rtp.send_audio(&frame).await {
                            if !announced_audio_tx && sent > 0 {
                                let s = rtp.stats();
                                let codec = s.codec;
                                println!(
                                    "Audio TX active: codec={} PT={} rate={}Hz",
                                    codec,
                                    codec.payload_type(),
                                    codec.clock_rate()
                                );
                                announced_audio_tx = true;
                            }
                        }
                    }
                    let _ = self.flush_dtmf_queue(local_addr, &mut debugger).await;
                }
            }
        }

        // Stop RTP receiver
        if let Some(stop) = rtp_stop_tx {
            let _ = stop.send(()).await;
        }

        self.live_recorder = live_recorder;

        Ok(())
    }

    pub fn queue_rfc2833_dtmf(&mut self, digits: &str) -> usize {
        self.queue_dtmf_digits(digits, DtmfMethod::Rfc2833)
    }

    pub fn queue_sip_info_dtmf(&mut self, digits: &str) -> usize {
        self.queue_dtmf_digits(digits, DtmfMethod::SipInfo)
    }

    pub fn queued_dtmf_count(&self) -> usize {
        self.dtmf_queue.len()
    }

    fn queue_dtmf_digits(&mut self, digits: &str, method: DtmfMethod) -> usize {
        let mut count = 0usize;
        for ch in digits.chars().filter(|c| !c.is_whitespace()) {
            if is_valid_dtmf_digit(ch) {
                self.dtmf_queue.push_back(QueuedDtmf {
                    digit: ch.to_ascii_uppercase(),
                    method,
                });
                count += 1;
            } else {
                println!("Ignoring unsupported DTMF digit '{}'", ch);
            }
        }
        count
    }

    async fn flush_dtmf_queue(
        &mut self,
        local_addr: SocketAddr,
        debugger: &mut SipDebugger,
    ) -> Result<usize, PhoneError> {
        if self.dtmf_queue.is_empty() {
            return Ok(0);
        }
        let mut sent = 0usize;
        while let Some(item) = self.dtmf_queue.pop_front() {
            match item.method {
                DtmfMethod::Rfc2833 => {
                    let Some(ref mut rtp) = self.rtp_session else {
                        break;
                    };
                    let payload_type = self.remote_dtmf_payload_type.unwrap_or(self.local_dtmf_payload_type);
                    if let Err(e) = rtp.send_rfc2833_digit(item.digit, payload_type).await {
                        println!("Failed to send RTP DTMF '{}': {}", item.digit, e);
                    } else {
                        println!("DTMF sent (RTP RFC2833): {}", item.digit);
                        sent += 1;
                    }
                }
                DtmfMethod::SipInfo => {
                    if let Some(info) = self.build_dtmf_info(item.digit) {
                        if let Some(target) = self.remote_sip_addr {
                            debugger.capture_outgoing(&info, local_addr, target);
                            self.transport.send_to(&info, target).await?;
                            println!("DTMF sent (SIP INFO): {}", item.digit);
                            sent += 1;
                        }
                    }
                }
            }
        }
        Ok(sent)
    }

    fn build_dtmf_info(&mut self, digit: char) -> Option<SipMessage> {
        let dialog = self.dialog.as_mut()?;
        let cseq = dialog.next_cseq();
        let remote_target = dialog
            .remote_target
            .as_deref()
            .unwrap_or(&dialog.remote_uri);
        let body = format!("Signal={}\r\nDuration=160", digit);
        Some(
            RequestBuilder::new(SipMethod::Info, remote_target)
                .header(
                    HeaderName::Via,
                    format!(
                        "SIP/2.0/UDP {};branch={};rport",
                        self.transport.local_addr(),
                        generate_branch()
                    ),
                )
                .header(HeaderName::MaxForwards, "70")
                .header(
                    HeaderName::From,
                    format!("<{}>;tag={}", dialog.local_uri, dialog.local_tag),
                )
                .header(
                    HeaderName::To,
                    format!(
                        "<{}>{}",
                        dialog.remote_uri,
                        dialog.remote_tag
                            .as_ref()
                            .map(|t| format!(";tag={}", t))
                            .unwrap_or_default()
                    ),
                )
                .header(HeaderName::CallId, &dialog.call_id)
                .header(HeaderName::CSeq, format!("{} INFO", cseq))
                .header(HeaderName::ContentType, "application/dtmf-relay")
                .header(HeaderName::Other("Info-Package".to_string()), "dtmf")
                .header(HeaderName::UserAgent, "siphone/0.1.0")
                .body(body)
                .build(),
        )
    }

    /// Get the path and recorder for any live-started recording
    pub fn take_live_recording(&mut self) -> Option<(String, AudioRecorder)> {
        let path = self.pending_record_path.take()?;
        let rec = self.live_recorder.take()?;
        Some((path, rec))
    }

    #[allow(dead_code)]
    pub fn dialog(&self) -> Option<&SipDialog> {
        self.dialog.as_ref()
    }

    #[allow(dead_code)]
    pub fn local_addr(&self) -> SocketAddr {
        self.transport.local_addr()
    }
}

/// Extract the host part from a SIP URI: sip:user@host[:port] -> host[:port]
fn extract_host_from_uri(uri: &str) -> Option<String> {
    let without_scheme = uri.strip_prefix("sip:").unwrap_or(uri);
    if let Some((_user, host)) = without_scheme.split_once('@') {
        Some(host.to_string())
    } else {
        Some(without_scheme.to_string())
    }
}

/// Extract user from SIP URI: sip:user@host -> user
fn extract_user_from_uri(uri: &str) -> Option<String> {
    let without_scheme = uri.strip_prefix("sip:").unwrap_or(uri);
    let (user, _) = without_scheme.split_once('@')?;
    if user.is_empty() {
        None
    } else {
        Some(user.to_string())
    }
}

fn build_ack_msg(dialog: &SipDialog, local_addr: &SocketAddr) -> SipMessage {
    let branch = generate_branch();
    let remote_target = dialog
        .remote_target
        .as_deref()
        .unwrap_or(&dialog.remote_uri);

    RequestBuilder::new(SipMethod::Ack, remote_target)
        .header(
            HeaderName::Via,
            format!("SIP/2.0/UDP {};branch={};rport", local_addr, branch),
        )
        .header(HeaderName::MaxForwards, "70")
        .header(
            HeaderName::From,
            format!("<{}>;tag={}", dialog.local_uri, dialog.local_tag),
        )
        .header(
            HeaderName::To,
            format!(
                "<{}>{}",
                dialog.remote_uri,
                dialog
                    .remote_tag
                    .as_ref()
                    .map(|t| format!(";tag={}", t))
                    .unwrap_or_default()
            ),
        )
        .header(HeaderName::CallId, &dialog.call_id)
        .header(HeaderName::CSeq, "1 ACK")
        .build()
}

fn build_bye_msg(dialog: &SipDialog, local_addr: &SocketAddr) -> SipMessage {
    let branch = generate_branch();
    let remote_target = dialog
        .remote_target
        .as_deref()
        .unwrap_or(&dialog.remote_uri);

    RequestBuilder::new(SipMethod::Bye, remote_target)
        .header(
            HeaderName::Via,
            format!("SIP/2.0/UDP {};branch={};rport", local_addr, branch),
        )
        .header(HeaderName::MaxForwards, "70")
        .header(
            HeaderName::From,
            format!("<{}>;tag={}", dialog.local_uri, dialog.local_tag),
        )
        .header(
            HeaderName::To,
            format!(
                "<{}>{}",
                dialog.remote_uri,
                dialog
                    .remote_tag
                    .as_ref()
                    .map(|t| format!(";tag={}", t))
                    .unwrap_or_default()
            ),
        )
        .header(HeaderName::CallId, &dialog.call_id)
        .header(HeaderName::CSeq, "2 BYE")
        .build()
}

fn resolve_server_addr(server: &str) -> Result<SocketAddr, PhoneError> {
    // Strip sip: prefix
    let addr_str = server
        .strip_prefix("sip:")
        .unwrap_or(server);

    // Try parsing as SocketAddr first
    if let Ok(addr) = addr_str.parse::<SocketAddr>() {
        return Ok(addr);
    }

    // Try parsing as IP:port
    if let Some((host, port_str)) = addr_str.rsplit_once(':') {
        if let Ok(port) = port_str.parse::<u16>() {
            if let Ok(ip) = host.parse::<std::net::IpAddr>() {
                return Ok(SocketAddr::new(ip, port));
            }
        }
    }

    // Try parsing as just an IP
    if let Ok(ip) = addr_str.parse::<std::net::IpAddr>() {
        return Ok(SocketAddr::new(ip, 5060));
    }

    // Try DNS resolution
    use std::net::ToSocketAddrs;
    let addr_with_port = if addr_str.contains(':') {
        addr_str.to_string()
    } else {
        format!("{}:5060", addr_str)
    };
    if let Ok(mut addrs) = addr_with_port.to_socket_addrs() {
        if let Some(addr) = addrs.next() {
            return Ok(addr);
        }
    }

    Err(PhoneError::CallFailed(format!(
        "Cannot resolve server address: {}",
        server
    )))
}

fn is_valid_dtmf_digit(ch: char) -> bool {
    matches!(ch.to_ascii_uppercase(), '0'..='9' | '*' | '#' | 'A' | 'B' | 'C' | 'D')
}

fn parse_info_dtmf(body: Option<&str>) -> Option<(char, u16)> {
    let body = body?;
    let mut signal: Option<char> = None;
    let mut duration: Option<u16> = None;
    for line in body.lines() {
        let line = line.trim();
        if let Some((k, v)) = line.split_once('=') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim();
            if key == "signal" {
                let d = val.chars().next()?;
                if is_valid_dtmf_digit(d) {
                    signal = Some(d.to_ascii_uppercase());
                }
            } else if key == "duration" {
                duration = val.parse::<u16>().ok();
            }
        } else if let Some(val) = line.strip_prefix("Signal=") {
            let d = val.trim().chars().next()?;
            if is_valid_dtmf_digit(d) {
                signal = Some(d.to_ascii_uppercase());
            }
        }
    }
    signal.map(|d| (d, duration.unwrap_or(160)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_server_addr() {
        let addr = resolve_server_addr("192.168.1.1:5060").unwrap();
        assert_eq!(addr.to_string(), "192.168.1.1:5060");

        let addr = resolve_server_addr("192.168.1.1").unwrap();
        assert_eq!(addr.to_string(), "192.168.1.1:5060");

        let addr = resolve_server_addr("sip:192.168.1.1:5060").unwrap();
        assert_eq!(addr.to_string(), "192.168.1.1:5060");

        let addr = resolve_server_addr("sip:10.0.0.1").unwrap();
        assert_eq!(addr.to_string(), "10.0.0.1:5060");
    }

    #[test]
    fn test_resolve_server_addr_invalid() {
        assert!(resolve_server_addr("not-a-valid-address").is_err());
    }

    #[test]
    fn test_extract_host_from_uri() {
        assert_eq!(extract_host_from_uri("sip:bob@example.com"), Some("example.com".into()));
        assert_eq!(extract_host_from_uri("sip:2234@135.125.159.46"), Some("135.125.159.46".into()));
        assert_eq!(extract_host_from_uri("sip:bob@10.0.0.1:5060"), Some("10.0.0.1:5060".into()));
        assert_eq!(extract_host_from_uri("sip:example.com"), Some("example.com".into()));
    }

    #[test]
    fn test_extract_user_from_uri() {
        assert_eq!(extract_user_from_uri("sip:bob@example.com"), Some("bob".into()));
        assert_eq!(extract_user_from_uri("sip:2234@135.125.159.46"), Some("2234".into()));
        assert_eq!(extract_user_from_uri("sip:example.com"), None);
    }

    #[test]
    fn test_parse_info_dtmf_body() {
        let parsed = parse_info_dtmf(Some("Signal=5\r\nDuration=240"));
        assert_eq!(parsed, Some(('5', 240)));

        let parsed = parse_info_dtmf(Some("Signal=#"));
        assert_eq!(parsed, Some(('#', 160)));

        assert_eq!(parse_info_dtmf(Some("Signal=Z\r\nDuration=10")), None);
        assert_eq!(parse_info_dtmf(None), None);
    }

    #[test]
    fn test_is_valid_dtmf_digit() {
        assert!(is_valid_dtmf_digit('0'));
        assert!(is_valid_dtmf_digit('*'));
        assert!(is_valid_dtmf_digit('D'));
        assert!(is_valid_dtmf_digit('a'));
        assert!(!is_valid_dtmf_digit('Z'));
    }

    #[tokio::test]
    async fn test_softphone_creation() {
        let phone = SoftPhone::new("127.0.0.1:0").await.unwrap();
        assert!(phone.dialog().is_none());
        assert!(phone.local_addr().port() > 0);
    }

    #[tokio::test]
    async fn test_softphone_dtmf_queue_api() {
        let mut phone = SoftPhone::new("127.0.0.1:0").await.unwrap();
        assert_eq!(phone.queue_rfc2833_dtmf("12#"), 3);
        assert_eq!(phone.queue_sip_info_dtmf("A"), 1);
        assert_eq!(phone.queued_dtmf_count(), 4);
    }

    #[tokio::test]
    async fn test_softphone_register() {
        let mut phone = SoftPhone::new("127.0.0.1:0").await.unwrap();

        // Create a mock server to receive the REGISTER
        let server_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_socket.local_addr().unwrap();

        phone
            .register(&server_addr.to_string(), "alice", "secret")
            .await
            .unwrap();

        // Verify the server received a REGISTER
        let mut buf = vec![0u8; 65535];
        let (len, _source) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            server_socket.recv_from(&mut buf),
        )
        .await
        .unwrap()
        .unwrap();

        let msg = SipMessage::parse(&String::from_utf8_lossy(&buf[..len])).unwrap();
        assert!(msg.is_request());
        if let SipMessage::Request(req) = &msg {
            assert_eq!(req.method, SipMethod::Register);
        }
    }

    #[tokio::test]
    async fn test_softphone_call() {
        let mut phone = SoftPhone::new("127.0.0.1:0").await.unwrap();

        let server_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_socket.local_addr().unwrap();

        phone
            .call(
                "sip:bob@example.com",
                Some(&server_addr.to_string()),
                Some("alice"),
            )
            .await
            .unwrap();

        // Verify dialog was created
        assert!(phone.dialog().is_some());
        let dialog = phone.dialog().unwrap();
        assert!(dialog.is_early());

        // Verify server received INVITE
        let mut buf = vec![0u8; 65535];
        let (len, _) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            server_socket.recv_from(&mut buf),
        )
        .await
        .unwrap()
        .unwrap();

        let msg = SipMessage::parse(&String::from_utf8_lossy(&buf[..len])).unwrap();
        if let SipMessage::Request(req) = &msg {
            assert_eq!(req.method, SipMethod::Invite);
            assert!(req.body.is_some()); // Should have SDP body
            let sdp = SdpSession::parse(req.body.as_ref().unwrap()).unwrap();
            assert!(sdp.get_audio_port().is_some());
        } else {
            panic!("Expected INVITE request");
        }
    }

    #[tokio::test]
    async fn test_softphone_call_no_server() {
        let mut phone = SoftPhone::new("127.0.0.1:0").await.unwrap();

        let server_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_socket.local_addr().unwrap();

        // Call with server extracted from URI
        let uri = format!("sip:bob@{}", server_addr);
        phone.call(&uri, None, Some("alice")).await.unwrap();

        assert!(phone.dialog().is_some());
    }

    #[tokio::test]
    async fn test_softphone_call_auto_user_from_uri() {
        let mut phone = SoftPhone::new("127.0.0.1:0").await.unwrap();
        let server_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server_socket.local_addr().unwrap();

        let uri = format!("sip:bob@{}", server_addr);
        phone.call(&uri, None, None).await.unwrap();
        assert!(phone.dialog().is_some());
    }
}
