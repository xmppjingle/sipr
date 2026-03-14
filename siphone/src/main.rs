mod phone;
mod sip_debug;

use clap::{Parser, Subcommand};
use phone::SoftPhone;
use rtp_core::audio_device::{AudioConfig, DeviceSelector};
#[cfg(feature = "audio-device")]
use rtp_core::audio_device::TestToneGenerator;

#[derive(Parser)]
#[command(
    name = "siphone",
    about = "SIP CLI Softphone - A command-line SIP user agent",
    long_about = "siphone is a SIP softphone that runs entirely from the command line.\n\
                  It supports SIP registration, outbound calls with RTP audio,\n\
                  G.711 mu-law/A-law codecs, and audio device selection.\n\n\
                  Examples:\n  \
                    siphone register --server sip.example.com --user alice --password secret\n  \
                    siphone call sip:bob@example.com --server sip.example.com --user alice\n  \
                    siphone devices\n  \
                    siphone test-audio --duration 3\n  \
                    siphone test-audio --input \"USB Mic\" --output default --duration 5",
    version,
    after_help = "Use 'siphone <COMMAND> --help' for more information about a specific command."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Register with a SIP server
    #[command(long_about = "Register this softphone with a SIP registrar server.\n\
                            This tells the server where to route incoming calls to your account.\n\n\
                            Example:\n  \
                              siphone register --server sip.example.com --user alice --password secret")]
    Register {
        /// SIP server address (e.g., sip.example.com or 192.168.1.1:5060)
        #[arg(long)]
        server: String,
        /// SIP username for authentication
        #[arg(long)]
        user: String,
        /// SIP password for authentication
        #[arg(long)]
        password: String,
        /// Local UDP port to bind for SIP signaling (default: 5060)
        #[arg(long, default_value = "5060")]
        port: u16,
    },

    /// Make an outbound SIP call
    #[command(long_about = "Initiate an outbound SIP call to the specified URI.\n\
                            The call will use RTP for audio transport with G.711 codec.\n\
                            Press Ctrl+C to hang up during an active call.\n\n\
                            Examples:\n  \
                              siphone call sip:2234@135.125.159.46 --user 2234\n  \
                              siphone call sip:bob@example.com --server sip.example.com --user alice\n  \
                              siphone call sip:bob@192.168.1.100 --user alice --record call.wav\n  \
                              siphone call sip:bob@example.com --user alice --codec pcma")]
    Call {
        /// SIP URI to call (e.g., sip:bob@example.com)
        uri: String,
        /// SIP server/proxy address (optional, extracted from URI if omitted)
        #[arg(long)]
        server: Option<String>,
        /// SIP username
        #[arg(long)]
        user: String,
        /// Local UDP port for SIP signaling (default: 5060)
        #[arg(long, default_value = "5060")]
        port: u16,
        /// Audio codec to use: pcmu (G.711 mu-law) or pcma (G.711 A-law)
        #[arg(long, default_value = "pcmu", value_parser = parse_codec)]
        codec: rtp_core::CodecType,
        /// Input audio device (microphone): "default", device index, or name substring
        #[arg(long, default_value = "default")]
        input_device: String,
        /// Output audio device (speaker): "default", device index, or name substring
        #[arg(long, default_value = "default")]
        output_device: String,
        /// Record received audio to a WAV file
        #[arg(long)]
        record: Option<String>,
    },

    /// List available audio devices
    #[command(long_about = "List all audio input (microphone) and output (speaker) devices\n\
                            available on your system. Use device names or indices with the\n\
                            --input-device and --output-device options of the 'call' command.\n\n\
                            Requires the 'audio-device' feature to detect real hardware.\n\
                            Build with: cargo build --features audio-device")]
    Devices,

    /// Test audio input/output devices
    #[command(long_about = "Test your audio setup by playing a tone through the output device\n\
                            and/or capturing audio from the input device.\n\n\
                            Modes:\n  \
                              --mode tone    Play a test tone through speakers (default)\n  \
                              --mode loopback  Capture from mic and play through speakers\n  \
                              --mode capture   Capture from mic and save to WAV file\n\n\
                            Examples:\n  \
                              siphone test-audio\n  \
                              siphone test-audio --mode tone --frequency 440 --duration 3\n  \
                              siphone test-audio --mode capture --output-file recording.wav --duration 5\n  \
                              siphone test-audio --mode loopback --input \"USB Mic\" --output default")]
    TestAudio {
        /// Test mode: tone, loopback, or capture
        #[arg(long, default_value = "tone")]
        mode: String,
        /// Input device for capture/loopback: "default", index, or name
        #[arg(long, default_value = "default")]
        input: String,
        /// Output device for tone/loopback: "default", index, or name
        #[arg(long, default_value = "default")]
        output: String,
        /// Test duration in seconds
        #[arg(long, default_value = "3")]
        duration: u64,
        /// Tone frequency in Hz (for tone mode)
        #[arg(long, default_value = "440")]
        frequency: f64,
        /// Output WAV file path (for capture mode)
        #[arg(long, default_value = "capture.wav")]
        output_file: String,
    },

    /// Show status of current sessions
    #[command(long_about = "Display the status of any active SIP sessions,\n\
                            including registration state and active calls.")]
    Status,

    /// Sniff SIP traffic (sngrep-like debugger)
    #[command(long_about = "Capture and display SIP traffic on a UDP port.\n\
                            Similar to sngrep, shows SIP messages with headers,\n\
                            call flow diagrams, and timing information.\n\n\
                            Examples:\n  \
                              siphone sniff\n  \
                              siphone sniff --port 5060 --verbose\n  \
                              siphone sniff --filter INVITE\n  \
                              siphone sniff --port 5080 --filter REGISTER")]
    Sniff {
        /// UDP port to listen on
        #[arg(long, default_value = "5060")]
        port: u16,
        /// Show full SIP message headers and bodies
        #[arg(long, short)]
        verbose: bool,
        /// Filter by SIP method (e.g., INVITE, REGISTER, BYE)
        #[arg(long)]
        filter: Option<String>,
    },
}

fn parse_codec(s: &str) -> Result<rtp_core::CodecType, String> {
    match s.to_lowercase().as_str() {
        "pcmu" | "ulaw" | "g711u" => Ok(rtp_core::CodecType::Pcmu),
        "pcma" | "alaw" | "g711a" => Ok(rtp_core::CodecType::Pcma),
        _ => Err(format!("Unknown codec '{}'. Supported: pcmu, pcma", s)),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Register {
            server,
            user,
            password,
            port,
        } => {
            let mut phone = SoftPhone::new(&format!("0.0.0.0:{}", port)).await?;
            phone.register(&server, &user, &password).await?;
            println!("Registration sent to {}", server);

            match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                phone.wait_for_response(),
            )
            .await
            {
                Ok(Ok(msg)) => {
                    if let Some(status) = msg.status() {
                        if status.is_success() {
                            println!("Registered successfully!");
                        } else {
                            println!("Registration failed: {} {}", status, status.reason_phrase());
                        }
                    }
                }
                Ok(Err(e)) => println!("Error: {}", e),
                Err(_) => println!("Registration timed out"),
            }
        }
        Commands::Call {
            uri,
            server,
            user,
            port,
            codec: _codec,
            input_device,
            output_device,
            record,
        } => {
            // Report device selection
            let input_sel = DeviceSelector::from_arg(&input_device);
            let output_sel = DeviceSelector::from_arg(&output_device);
            println!("Audio input:  {}", input_sel);
            println!("Audio output: {}", output_sel);
            if let Some(ref path) = record {
                println!("Recording to: {}", path);
            }

            if !rtp_core::audio_device::is_audio_available() {
                println!(
                    "Warning: {}",
                    rtp_core::audio_device::audio_unavailable_reason()
                );
                println!("Call will proceed without live audio (RTP only).");
            }

            let mut phone = SoftPhone::new(&format!("0.0.0.0:{}", port)).await?;
            phone.call(&uri, server.as_deref(), &user).await?;
            println!("Calling {}...", uri);

            let mut recorder = record.as_ref().map(|_| rtp_core::AudioRecorder::new(8000));
            tokio::select! {
                result = phone.run_call(recorder.as_mut()) => {
                    match result {
                        Ok(_) => println!("Call ended"),
                        Err(e) => println!("Call error: {}", e),
                    }
                }
                _ = tokio::signal::ctrl_c() => {
                    println!("\nHanging up...");
                    phone.hangup().await?;
                    println!("Call ended");
                }
            }

            // Save recording started with --record flag
            if let (Some(path), Some(ref rec)) = (&record, &recorder) {
                if rec.frame_count() > 0 {
                    match rec.save_wav(path) {
                        Ok(_) => println!("Saved recording to {} ({:.1}s, {} frames)",
                            path, rec.duration_ms() as f64 / 1000.0, rec.frame_count()),
                        Err(e) => println!("Failed to save recording: {}", e),
                    }
                } else {
                    println!("No audio frames received, recording not saved.");
                }
            }

            // Save recording started interactively with 'record <file>' command
            if let Some((path, rec)) = phone.take_live_recording() {
                if rec.frame_count() > 0 {
                    match rec.save_wav(&path) {
                        Ok(_) => println!("Saved recording to {} ({:.1}s, {} frames)",
                            path, rec.duration_ms() as f64 / 1000.0, rec.frame_count()),
                        Err(e) => println!("Failed to save recording: {}", e),
                    }
                }
            }
        }
        Commands::Devices => {
            cmd_devices();
        }
        Commands::TestAudio {
            mode,
            input,
            output,
            duration,
            frequency,
            output_file,
        } => {
            cmd_test_audio(&mode, &input, &output, duration, frequency, &output_file).await?;
        }
        Commands::Status => {
            println!("No active sessions");
        }
        Commands::Sniff {
            port,
            verbose,
            filter,
        } => {
            let filter_method = filter.map(|f| sip_core::message::SipMethod::from_str(&f));
            sip_debug::run_sniffer(port, verbose, filter_method).await?;
        }
    }

    Ok(())
}

fn cmd_devices() {
    if !rtp_core::audio_device::is_audio_available() {
        println!(
            "No audio devices available.\n{}",
            rtp_core::audio_device::audio_unavailable_reason()
        );
        println!();
        println!("On this system, siphone operates in RTP-only mode.");
        println!("Audio is encoded/decoded and sent/received via RTP,");
        println!("but no local microphone or speaker is used.");
        println!();
        println!("To enable audio device support, rebuild with:");
        println!("  cargo build --features audio-device");
        return;
    }

    let input_devices = rtp_core::audio_device::list_input_devices();
    let output_devices = rtp_core::audio_device::list_output_devices();

    println!("=== Input Devices (Microphones) ===");
    if input_devices.is_empty() {
        println!("  (none found)");
    } else {
        for (i, dev) in input_devices.iter().enumerate() {
            println!("  [{}] {}", i, dev);
        }
    }

    println!();
    println!("=== Output Devices (Speakers) ===");
    if output_devices.is_empty() {
        println!("  (none found)");
    } else {
        for (i, dev) in output_devices.iter().enumerate() {
            println!("  [{}] {}", i, dev);
        }
    }

    println!();
    println!("Use --input-device / --output-device with 'call' or 'test-audio'.");
    println!("Values: \"default\", device index (e.g., \"0\"), or name substring.");
}

async fn cmd_test_audio(
    mode: &str,
    _input: &str,
    _output: &str,
    duration: u64,
    frequency: f64,
    output_file: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let audio_config = AudioConfig::telephony();

    match mode {
        "tone" => {
            println!(
                "Generating {:.0}Hz test tone for {}s at {}Hz sample rate...",
                frequency, duration, audio_config.sample_rate
            );

            if rtp_core::audio_device::is_audio_available() {
                #[cfg(feature = "audio-device")]
                {
                    let output_sel = DeviceSelector::from_arg(_output);
                    let playback =
                        rtp_core::audio_device::AudioPlayback::start(&output_sel, &audio_config)
                            .map_err(|e| format!("Playback error: {}", e))?;

                    let mut gen = TestToneGenerator::new(
                        frequency,
                        audio_config.sample_rate,
                        12000,
                    );

                    let frames = (duration * 1000) / audio_config.frame_size_ms as u64;
                    for _ in 0..frames {
                        let frame = gen.next_frame(audio_config.samples_per_frame());
                        playback
                            .play_frame(frame)
                            .await
                            .map_err(|e| format!("Playback error: {}", e))?;
                        tokio::time::sleep(std::time::Duration::from_millis(
                            audio_config.frame_size_ms as u64,
                        ))
                        .await;
                    }

                    println!("Tone playback complete.");
                }
            } else {
                // No audio device — generate and save to file
                println!("No audio device available. Saving tone to WAV file instead.");
                let samples = rtp_core::generate_sine_tone(
                    frequency,
                    audio_config.sample_rate,
                    (duration * 1000) as u32,
                    12000,
                );
                let header = rtp_core::WavHeader::telephony();
                rtp_core::write_wav(output_file, &samples, &header)?;
                println!(
                    "Saved {:.1}s tone to {} ({} samples)",
                    duration,
                    output_file,
                    samples.len()
                );
            }
        }
        "capture" => {
            if !rtp_core::audio_device::is_audio_available() {
                println!("Cannot capture: no audio devices available.");
                println!("{}", rtp_core::audio_device::audio_unavailable_reason());
                return Ok(());
            }

            #[cfg(feature = "audio-device")]
            {
                let input_sel = DeviceSelector::from_arg(_input);
                println!(
                    "Capturing from {} for {}s...",
                    input_sel, duration
                );

                let mut capture =
                    rtp_core::audio_device::AudioCapture::start(&input_sel, &audio_config)
                        .map_err(|e| format!("Capture error: {}", e))?;

                let mut recorder = rtp_core::AudioRecorder::new(audio_config.sample_rate);
                let deadline =
                    tokio::time::Instant::now() + std::time::Duration::from_secs(duration);

                while tokio::time::Instant::now() < deadline {
                    if let Ok(Some(frame)) = tokio::time::timeout(
                        std::time::Duration::from_millis(100),
                        capture.next_frame(),
                    )
                    .await
                    {
                        recorder.record_frame(&frame);
                    }
                }

                recorder.save_wav(output_file)?;
                println!(
                    "Captured {} frames ({:.1}s) to {}",
                    recorder.frame_count(),
                    recorder.duration_ms() as f64 / 1000.0,
                    output_file
                );
            }

            #[cfg(not(feature = "audio-device"))]
            {
                println!("Audio device support not compiled in.");
                println!("Rebuild with: cargo build --features audio-device");
            }
        }
        "loopback" => {
            if !rtp_core::audio_device::is_audio_available() {
                println!("Cannot run loopback: no audio devices available.");
                println!("{}", rtp_core::audio_device::audio_unavailable_reason());
                return Ok(());
            }

            #[cfg(feature = "audio-device")]
            {
                let input_sel = DeviceSelector::from_arg(_input);
                let output_sel = DeviceSelector::from_arg(_output);
                println!(
                    "Loopback: {} -> {} for {}s (Ctrl+C to stop)...",
                    input_sel, output_sel, duration
                );

                let mut capture =
                    rtp_core::audio_device::AudioCapture::start(&input_sel, &audio_config)
                        .map_err(|e| format!("Capture error: {}", e))?;
                let playback =
                    rtp_core::audio_device::AudioPlayback::start(&output_sel, &audio_config)
                        .map_err(|e| format!("Playback error: {}", e))?;

                let deadline =
                    tokio::time::Instant::now() + std::time::Duration::from_secs(duration);
                let mut frames = 0u64;

                while tokio::time::Instant::now() < deadline {
                    if let Ok(Some(frame)) = tokio::time::timeout(
                        std::time::Duration::from_millis(100),
                        capture.next_frame(),
                    )
                    .await
                    {
                        playback
                            .play_frame(frame)
                            .await
                            .map_err(|e| format!("Playback error: {}", e))?;
                        frames += 1;
                    }
                }

                println!("Loopback complete. {} frames processed.", frames);
            }

            #[cfg(not(feature = "audio-device"))]
            {
                println!("Audio device support not compiled in.");
                println!("Rebuild with: cargo build --features audio-device");
            }
        }
        _ => {
            println!("Unknown test mode '{}'. Use: tone, capture, or loopback", mode);
        }
    }

    Ok(())
}
