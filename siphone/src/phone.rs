use crate::sip_debug::SipDebugger;
use rtp_core::{AudioRecorder, CodecType, RtpSession, SessionConfig};
use sip_core::header::{generate_branch, generate_tag, HeaderName};
use sip_core::message::{RequestBuilder, SipMessage, SipMethod};
use sip_core::sdp::SdpSession;
use sip_core::dialog::SipDialog;
use sip_core::transport::SipTransport;
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

pub struct SoftPhone {
    transport: SipTransport,
    dialog: Option<SipDialog>,
    rtp_session: Option<RtpSession>,
    call_id: Option<String>,
    local_tag: String,
    local_ip: String,
    live_recorder: Option<AudioRecorder>,
    pending_record_path: Option<String>,
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
        user: &str,
    ) -> Result<(), PhoneError> {
        let target_uri = if uri.starts_with("sip:") {
            uri.to_string()
        } else {
            format!("sip:{}", uri)
        };

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
                format!("<sip:{}@{}>;tag={}", user, server_host, self.local_tag),
            )
            .header(HeaderName::To, format!("<{}>", target_uri))
            .header(HeaderName::CallId, &call_id)
            .header(HeaderName::CSeq, "1 INVITE")
            .header(
                HeaderName::Contact,
                format!("<sip:{}@{}>", user, local_addr),
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
            format!("sip:{}@{}", user, server_host),
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

    pub async fn run_call(&mut self, mut recorder: Option<&mut AudioRecorder>) -> Result<(), PhoneError> {
        let mut rx = self.transport.start_receiving(32);
        let mut rtp_stop_tx: Option<tokio::sync::mpsc::Sender<()>> = None;
        let mut rtp_audio_rx: Option<tokio::sync::mpsc::Receiver<Vec<i16>>> = None;
        let mut rtp_connected = false;
        let mut muted = false;
        let call_start = tokio::time::Instant::now();
        let mut recording_active = recorder.is_some();
        let mut debugger = SipDebugger::new(false);
        let local_addr = self.transport.local_addr();

        // Interactive stdin reader
        let stdin = tokio::io::stdin();
        let mut stdin_reader = BufReader::new(stdin).lines();

        // Send silence every 20ms to keep NAT pinhole open and trigger remote RTP
        let mut silence_interval = tokio::time::interval(std::time::Duration::from_millis(20));
        silence_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        if recorder.is_some() {
            println!("Recording active.");
        }
        println!("Type 'help' for interactive commands.");

        loop {
            tokio::select! {
                biased;

                // Prioritize draining audio to prevent channel backpressure
                audio = async {
                    if let Some(ref mut arx) = rtp_audio_rx {
                        arx.recv().await
                    } else {
                        std::future::pending().await
                    }
                } => {
                    if let Some(frame) = audio {
                        if recording_active {
                            if let Some(ref mut rec) = recorder {
                                rec.record_frame(&frame);
                                while let Ok(extra) = rtp_audio_rx.as_mut().unwrap().try_recv() {
                                    rec.record_frame(&extra);
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

                                    // Parse SDP from response to get remote RTP addr
                                    if let Some(body) = msg.body() {
                                        if let Ok(sdp) = SdpSession::parse(body) {
                                            let rtp_port = sdp.get_audio_port().unwrap_or(0);
                                            let rtp_host = sdp.get_connection_address()
                                                .unwrap_or("0.0.0.0");
                                            if let Ok(addr) = format!("{}:{}", rtp_host, rtp_port)
                                                .parse::<SocketAddr>()
                                            {
                                                if let Some(ref mut rtp) = self.rtp_session {
                                                    rtp.set_remote_addr(addr);
                                                    println!("RTP remote: {}", addr);
                                                    let (arx, stx) = rtp.start_receiving(1024);
                                                    rtp_audio_rx = Some(arx);
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
                                    println!("  hangup | bye       End the call");
                                    println!("  help               Show this help");
                                }
                                "record" | "rec" => {
                                    if recorder.is_some() {
                                        recording_active = true;
                                        println!("Recording resumed.");
                                    } else if parts.len() < 2 {
                                        println!("Usage: record <filename.wav>");
                                    } else {
                                        // Create a new recorder on the fly
                                        self.pending_record_path = Some(parts[1].to_string());
                                        recorder = Some(self.live_recorder.insert(AudioRecorder::new(8000)));
                                        recording_active = true;
                                        println!("Recording to: {}", parts[1]);
                                    }
                                }
                                "stop" => {
                                    if recording_active {
                                        recording_active = false;
                                        if let Some(ref rec) = recorder {
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
                                    if let Some(ref rec) = recorder {
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
                        if muted {
                            let silence = vec![0i16; 160];
                            let _ = rtp.send_audio(&silence).await;
                        } else {
                            let silence = vec![0i16; 160]; // TODO: send mic audio when capture is wired up
                            let _ = rtp.send_audio(&silence).await;
                        }
                    }
                }
            }
        }

        // Stop RTP receiver
        if let Some(stop) = rtp_stop_tx {
            let _ = stop.send(()).await;
        }

        Ok(())
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

    #[tokio::test]
    async fn test_softphone_creation() {
        let phone = SoftPhone::new("127.0.0.1:0").await.unwrap();
        assert!(phone.dialog().is_none());
        assert!(phone.local_addr().port() > 0);
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
                "alice",
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
        phone.call(&uri, None, "alice").await.unwrap();

        assert!(phone.dialog().is_some());
    }
}
