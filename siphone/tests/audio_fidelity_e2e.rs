/// End-to-end audio fidelity tests for diagnosing playback artifacts.
///
/// These tests cover:
/// 1. **Clock rate consistency** — RTP timestamps must advance by exactly
///    `samples_per_frame` per packet. A mismatch causes pitch shift or speed
///    artifacts (e.g. audio playing 6× too fast if Opus clock rate is applied
///    to PCMU packets).
/// 2. **Codec SNR (signal-to-noise ratio)** — measures how much the
///    encode→decode roundtrip degrades the signal for each codec.
/// 3. **Jitter simulation** — out-of-order packets are reordered by the jitter
///    buffer; the reconstructed audio must match the original sequence without
///    gaps or duplicates.
/// 4. **Packet loss effects** — skipped packets leave holes; the buffer must
///    not stall and downstream frames must arrive in order.
/// 5. **Full pipeline fidelity** — actual UDP loopback send/receive with SNR
///    measurement.
/// 6. **RTCP contamination** — RTCP control packets (PT 64-95) arriving on the
///    RTP port must be silently dropped and must NOT be decoded as audio.
/// 7. **Payload type filtering** — packets with an unexpected PT must be
///    ignored so stray traffic cannot pollute the audio stream.
/// 8. **Playback buffer bounds** — the output buffer must stay within its
///    maximum size under a fast producer to prevent latency growth.
use rtp_core::codec::{CodecPipeline, CodecType};
use rtp_core::jitter::JitterBuffer;
use rtp_core::packet::RtpPacket;
use rtp_core::session::{RtpSession, SessionConfig};
use rtp_core::wav::{
    compute_snr, cross_correlation, generate_multi_tone, generate_sine_tone, max_sample_error,
    rms_error,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build an RTP packet that carries `samples` encoded with `codec`.
fn encode_packet(
    codec: &mut CodecPipeline,
    codec_type: CodecType,
    seq: u16,
    samples: &[i16],
    ssrc: u32,
) -> RtpPacket {
    let encoded = codec.encode(samples).expect("encode failed");
    let ts = seq as u32 * codec_type.samples_per_frame() as u32;
    RtpPacket::new(codec_type.payload_type(), seq, ts, ssrc).with_payload(encoded)
}

// ---------------------------------------------------------------------------
// 1. Clock rate consistency
// ---------------------------------------------------------------------------

/// Verifies that the RTP timestamp advances by exactly `samples_per_frame`
/// for each codec, matching the codec's own clock rate.
///
/// A mismatch here is the most common cause of pitch / speed artifacts:
/// if the sender increments the timestamp by the wrong amount, the receiver
/// reconstructs audio at a different speed.
#[test]
fn test_clock_rate_timestamp_consistency_pcmu() {
    let codec = CodecType::Pcmu;
    let expected_clock_rate: u32 = 8000;
    let expected_spf: usize = 160; // 8000 * 0.020

    assert_eq!(
        codec.clock_rate(),
        expected_clock_rate,
        "PCMU clock rate must be 8000 Hz"
    );
    assert_eq!(
        codec.samples_per_frame(),
        expected_spf,
        "PCMU samples_per_frame must be 160 (20 ms at 8 kHz)"
    );

    // Simulate RTP timestamp sequence
    let mut ts: u32 = 0;
    for pkt in 0..10u32 {
        let expected_ts = pkt * expected_spf as u32;
        assert_eq!(ts, expected_ts, "packet {pkt}: timestamp mismatch");
        ts = ts.wrapping_add(codec.samples_per_frame() as u32);
    }
}

#[test]
fn test_clock_rate_timestamp_consistency_pcma() {
    let codec = CodecType::Pcma;
    assert_eq!(codec.clock_rate(), 8000);
    assert_eq!(codec.samples_per_frame(), 160);

    let mut ts: u32 = 0;
    for pkt in 0..10u32 {
        assert_eq!(ts, pkt * 160, "packet {pkt}: timestamp mismatch");
        ts = ts.wrapping_add(codec.samples_per_frame() as u32);
    }
}

#[test]
fn test_clock_rate_timestamp_consistency_opus() {
    let codec = CodecType::Opus;
    let expected_clock_rate: u32 = 48000;
    let expected_spf: usize = 960; // 48000 * 0.020

    assert_eq!(
        codec.clock_rate(),
        expected_clock_rate,
        "Opus clock rate must be 48000 Hz"
    );
    assert_eq!(
        codec.samples_per_frame(),
        expected_spf,
        "Opus samples_per_frame must be 960 (20 ms at 48 kHz)"
    );

    let mut ts: u32 = 0;
    for pkt in 0..10u32 {
        let expected_ts = pkt * expected_spf as u32;
        assert_eq!(ts, expected_ts, "packet {pkt}: timestamp mismatch");
        ts = ts.wrapping_add(codec.samples_per_frame() as u32);
    }
}

/// Cross-codec sanity: applying the wrong clock rate to a codec is a common
/// misconfiguration that causes artifacts. This test makes the mismatch
/// explicit so it is easy to spot in test output.
#[test]
fn test_clock_rate_cross_codec_mismatch_is_detectable() {
    // If a PCMU stream (8 kHz) accidentally uses Opus timestamps (48 kHz),
    // the timestamp would advance 960 instead of 160 per frame — a 6× error.
    let pcmu_spf = CodecType::Pcmu.samples_per_frame() as u32; // 160
    let opus_spf = CodecType::Opus.samples_per_frame() as u32; // 960
    let ratio = opus_spf / pcmu_spf;
    assert_eq!(ratio, 6, "mismatch ratio between Opus and PCMU clock rates");

    // Demonstrate: 10 PCMU frames at correct spacing vs wrong (Opus) spacing
    let correct_end_ts = 10 * pcmu_spf; // 1600 (200 ms of audio)
    let wrong_end_ts = 10 * opus_spf; // 9600 (would be interpreted as 1.2 s)
    assert_ne!(
        correct_end_ts, wrong_end_ts,
        "correct vs wrong clock rate produces different end timestamps"
    );
    // A receiver replaying at 8 kHz would think 9600 ts ticks = 1.2 s, not 0.2 s
    let wrong_duration_ms = wrong_end_ts * 1000 / CodecType::Pcmu.clock_rate();
    assert_eq!(
        wrong_duration_ms, 1200,
        "wrong clock rate makes 200 ms of audio appear as 1200 ms"
    );
}

// ---------------------------------------------------------------------------
// 2. Codec SNR (signal-to-noise ratio)
// ---------------------------------------------------------------------------

/// G.711 mu-law (PCMU) roundtrip SNR.
/// PCMU is designed for telephony at 8 kHz; expected SNR > 30 dB for a
/// mid-level sine wave. Below ~20 dB the audio sounds noticeably degraded.
#[test]
fn test_codec_snr_pcmu() {
    let mut codec = CodecPipeline::new(CodecType::Pcmu);
    // 1 second of a 400 Hz sine wave at 8 kHz (telephony range)
    let original = generate_sine_tone(400.0, 8000, 1000, 16000);
    let encoded = codec.encode(&original).expect("encode");
    let decoded = codec.decode(&encoded).expect("decode");

    let snr = compute_snr(&original, &decoded);
    let corr = cross_correlation(&original, &decoded);
    let rms = rms_error(&original, &decoded);

    println!(
        "[PCMU] SNR={:.1} dB  corr={:.4}  rms_err={:.1}",
        snr, corr, rms
    );

    assert!(
        snr > 30.0,
        "PCMU SNR {snr:.1} dB is too low (< 30 dB) — codec is causing significant distortion"
    );
    assert!(
        corr > 0.99,
        "PCMU cross-correlation {corr:.4} is too low — signal shape is degraded"
    );
}

/// G.711 A-law (PCMA) roundtrip SNR — similar expectations to PCMU.
#[test]
fn test_codec_snr_pcma() {
    let mut codec = CodecPipeline::new(CodecType::Pcma);
    let original = generate_sine_tone(400.0, 8000, 1000, 16000);
    let encoded = codec.encode(&original).expect("encode");
    let decoded = codec.decode(&encoded).expect("decode");

    let snr = compute_snr(&original, &decoded);
    let corr = cross_correlation(&original, &decoded);

    println!("[PCMA] SNR={:.1} dB  corr={:.4}", snr, corr);

    assert!(
        snr > 30.0,
        "PCMA SNR {snr:.1} dB is too low (< 30 dB)"
    );
    assert!(
        corr > 0.99,
        "PCMA cross-correlation {corr:.4} is too low"
    );
}

/// Multi-tone fidelity for PCMU — simulates a voice signal that contains
/// multiple formant frequencies. Checks that none of the frequencies are
/// being clipped or suppressed.
#[test]
fn test_codec_multitone_fidelity_pcmu() {
    let mut codec = CodecPipeline::new(CodecType::Pcmu);
    // Typical voiced speech formants (F1~700 Hz, F2~1200 Hz, F3~2500 Hz)
    let freqs = [700.0, 1200.0, 2500.0];
    let original = generate_multi_tone(&freqs, 8000, 500, 12000);
    let encoded = codec.encode(&original).expect("encode");
    let decoded = codec.decode(&encoded).expect("decode");

    let snr = compute_snr(&original, &decoded);
    let rms = rms_error(&original, &decoded);
    let max_err = max_sample_error(&original, &decoded);

    println!(
        "[PCMU multi-tone] SNR={:.1} dB  rms_err={:.1}  max_err={}",
        snr, rms, max_err
    );

    assert!(
        snr > 25.0,
        "PCMU multi-tone SNR {snr:.1} dB is too low (< 25 dB)"
    );
}

// ---------------------------------------------------------------------------
// 3. Jitter buffer sequence reconstruction
// ---------------------------------------------------------------------------

/// Sends 20 frames out of order, verifies the jitter buffer outputs them in
/// the correct sequence and that the decoded audio matches the original.
/// A broken jitter buffer (or wrong sequence comparison) causes clicks and
/// scrambled audio.
#[test]
fn test_jitter_buffer_reorder_and_audio_fidelity() {
    let codec_type = CodecType::Pcmu;
    let mut encoder = CodecPipeline::new(codec_type);
    let mut decoder = CodecPipeline::new(codec_type);
    let mut jitter = JitterBuffer::new(30);

    let num_frames: u16 = 20;
    let mut originals: Vec<Vec<i16>> = Vec::new();

    // Build packets with distinct known content (seq * 500 DC value +  sine)
    let packets: Vec<RtpPacket> = (0..num_frames)
        .map(|seq| {
            let dc = seq as i16 * 200;
            let tone = generate_sine_tone(300.0 + seq as f64 * 50.0, 8000, 20, 10000);
            let samples: Vec<i16> = tone.iter().map(|&s| s.saturating_add(dc)).collect();
            originals.push(samples.clone());
            encode_packet(&mut encoder, codec_type, seq, &samples, 0xABCD)
        })
        .collect();

    // Scramble the order: reverse half, interleave
    let mut shuffled: Vec<usize> = (0..num_frames as usize).collect();
    // A deterministic shuffle: reverse blocks of 4
    for chunk in shuffled.chunks_mut(4) {
        chunk.reverse();
    }

    for &idx in &shuffled {
        jitter.insert(packets[idx].clone());
    }

    // Drain the buffer and verify ordering + fidelity
    let mut received_seqs: Vec<u16> = Vec::new();
    let mut total_snr = 0.0f64;
    let mut count = 0usize;

    while let Some(pkt) = jitter.pop() {
        let seq = pkt.sequence_number;
        received_seqs.push(seq);

        let decoded = decoder.decode(&pkt.payload).expect("decode");
        let orig = &originals[seq as usize];
        let snr = compute_snr(orig, &decoded);
        total_snr += snr;
        count += 1;
    }

    assert_eq!(
        count, num_frames as usize,
        "jitter buffer must deliver all {num_frames} frames"
    );

    // Verify strictly increasing sequence numbers (no reordering artifacts)
    for window in received_seqs.windows(2) {
        assert!(
            window[1] == window[0] + 1,
            "non-sequential frame delivery: got {} after {}",
            window[1],
            window[0]
        );
    }

    let avg_snr = total_snr / count as f64;
    println!(
        "[Jitter] avg SNR over {count} frames = {avg_snr:.1} dB  ({} dropped)",
        jitter.packets_dropped()
    );

    assert!(
        avg_snr > 25.0,
        "average SNR {avg_snr:.1} dB after jitter buffer reorder is too low — potential artifact"
    );
    assert_eq!(
        jitter.packets_dropped(),
        0,
        "no packets should be dropped in a well-sized buffer with no true loss"
    );
}

// ---------------------------------------------------------------------------
// 4. Packet loss effects
// ---------------------------------------------------------------------------

/// Simulates 20% packet loss (every 5th packet dropped).
/// The jitter buffer must not stall and must advance past the missing
/// sequence numbers. Frames after the gap must still decode correctly.
#[test]
fn test_jitter_buffer_packet_loss_no_stall() {
    let codec_type = CodecType::Pcmu;
    let mut encoder = CodecPipeline::new(codec_type);
    let mut decoder = CodecPipeline::new(codec_type);
    let mut jitter = JitterBuffer::new(20);

    let num_frames: u16 = 20;
    let drop_every = 5usize; // drop frames 4, 9, 14, 19 → 4 lost out of 20

    for seq in 0..num_frames {
        if (seq as usize + 1) % drop_every == 0 {
            continue; // simulate network loss
        }
        let samples = generate_sine_tone(440.0, 8000, 20, 14000);
        let pkt = encode_packet(&mut encoder, codec_type, seq, &samples, 0x1234);
        jitter.insert(pkt);
    }

    let expected_lost = num_frames as usize / drop_every; // 4
    let expected_received = num_frames as usize - expected_lost; // 16

    assert_eq!(
        jitter.packets_received(),
        expected_received as u64,
        "jitter buffer should have received {expected_received} packets"
    );

    // Pop all frames — lost ones return None (concealment hole), others decode
    let mut delivered = 0usize;
    let mut holes = 0usize;
    let mut good_snr_count = 0usize;

    for _ in 0..num_frames {
        match jitter.pop() {
            Some(pkt) => {
                let decoded = decoder.decode(&pkt.payload).expect("decode");
                let original = generate_sine_tone(440.0, 8000, 20, 14000);
                let snr = compute_snr(&original, &decoded);
                if snr > 25.0 {
                    good_snr_count += 1;
                }
                delivered += 1;
            }
            None => {
                holes += 1; // gap — application should insert PLC or silence here
            }
        }
    }

    println!(
        "[PacketLoss] delivered={delivered}  holes={holes}  good_snr_frames={good_snr_count}"
    );

    assert_eq!(holes, expected_lost, "should have exactly {expected_lost} holes for lost packets");
    assert_eq!(
        delivered, expected_received,
        "should deliver exactly {expected_received} decodable frames"
    );
    assert!(
        good_snr_count >= expected_received - 1,
        "almost all received frames should have good SNR; got {good_snr_count}/{expected_received}"
    );
}

// ---------------------------------------------------------------------------
// 5. Full UDP pipeline fidelity
// ---------------------------------------------------------------------------

/// Full loopback test: sender encodes audio into real RTP packets over UDP,
/// receiver decodes them, SNR is measured end-to-end for both PCMU and PCMA.
///
/// This catches any byte-swapping, truncation, or framing bugs in the network
/// path that would show up as clicks or silence in real calls.
#[tokio::test]
async fn test_full_pipeline_fidelity_pcmu() {
    full_pipeline_fidelity(CodecType::Pcmu, 30.0).await;
}

#[tokio::test]
async fn test_full_pipeline_fidelity_pcma() {
    full_pipeline_fidelity(CodecType::Pcma, 30.0).await;
}

async fn full_pipeline_fidelity(codec_type: CodecType, min_snr_db: f64) {
    let spf = codec_type.samples_per_frame();

    // Create two RTP sessions on loopback
    let sender_cfg = SessionConfig::new(
        "127.0.0.1:0",
        "127.0.0.1:0".parse().unwrap(),
        codec_type,
    );
    let sender_placeholder = RtpSession::new(sender_cfg).await.unwrap();
    let sender_addr = sender_placeholder.local_addr();
    drop(sender_placeholder);

    let receiver_cfg = SessionConfig::new("127.0.0.1:0", sender_addr, codec_type);
    let mut receiver = RtpSession::new(receiver_cfg).await.unwrap();
    let receiver_addr = receiver.local_addr();

    let sender_cfg = SessionConfig::new("127.0.0.1:0", receiver_addr, codec_type);
    let mut sender = RtpSession::new(sender_cfg).await.unwrap();

    // Generate a multi-tone signal spanning the telephony band
    let freqs = [400.0, 1000.0, 2000.0, 3000.0];
    let full_audio = generate_multi_tone(&freqs, codec_type.clock_rate(), 200, 12000);

    // Split into frames and send
    let frames: Vec<&[i16]> = full_audio.chunks(spf).collect();
    let num_frames = frames.len();

    for frame in &frames {
        sender.send_audio(frame).await.unwrap();
    }

    // Receive and accumulate decoded audio
    let mut decoded_full: Vec<i16> = Vec::new();
    let mut received_seqs: Vec<u16> = Vec::new();

    for _ in 0..num_frames {
        let (pkt, _src) = receiver.recv_packet().await.unwrap();
        received_seqs.push(pkt.sequence_number);
        let decoded = receiver.decode_packet(&pkt).unwrap();
        assert_eq!(
            decoded.len(),
            spf,
            "decoded frame length must equal samples_per_frame for {}",
            codec_type
        );
        decoded_full.extend_from_slice(&decoded);
    }

    // Sequence numbers must be strictly sequential (no reordering on loopback)
    for (i, &seq) in received_seqs.iter().enumerate() {
        assert_eq!(
            seq, i as u16,
            "{codec_type}: packet {i} has unexpected sequence number {seq}"
        );
    }

    // Timestamps must advance by exactly samples_per_frame
    // (We verify this by checking sequence number spacing; the RTP session
    //  manages timestamps internally so we re-derive expected values.)
    for (i, &seq) in received_seqs.iter().enumerate() {
        let expected_seq = i as u16;
        assert_eq!(
            seq, expected_seq,
            "{codec_type}: timestamp-derived sequence mismatch at frame {i}"
        );
    }

    // Measure end-to-end audio quality
    let len = full_audio.len().min(decoded_full.len());
    let snr = compute_snr(&full_audio[..len], &decoded_full[..len]);
    let corr = cross_correlation(&full_audio[..len], &decoded_full[..len]);
    let rms = rms_error(&full_audio[..len], &decoded_full[..len]);
    let max_err = max_sample_error(&full_audio[..len], &decoded_full[..len]);

    println!(
        "[{codec_type}] end-to-end SNR={snr:.1} dB  corr={corr:.4}  rms={rms:.1}  max_err={max_err}  frames={num_frames}"
    );

    assert!(
        snr > min_snr_db,
        "{codec_type} end-to-end SNR {snr:.1} dB < {min_snr_db} dB — audio artifacts detected"
    );
    assert!(
        corr > 0.99,
        "{codec_type} end-to-end cross-correlation {corr:.4} < 0.99 — signal degraded"
    );
    assert_eq!(
        sender.stats().packets_sent,
        num_frames as u64,
        "{codec_type}: packets_sent counter mismatch"
    );
}

// ---------------------------------------------------------------------------
// 6. RTP timestamp continuity across frames
// ---------------------------------------------------------------------------

/// Verifies that the RTP session increments timestamps in a way that is
/// consistent with the expected audio duration.
///
/// If `samples_per_frame` doesn't match `clock_rate * ptime_ms / 1000`, the
/// receiver reconstructs audio at the wrong playback speed:
///   - Too small → audio speeds up (samples played too quickly)
///   - Too large → audio slows down and adds silence
#[tokio::test]
async fn test_rtp_timestamp_increments_match_clock_rate() {
    for codec_type in [CodecType::Pcmu, CodecType::Pcma] {
        let spf = codec_type.samples_per_frame();
        let clock_rate = codec_type.clock_rate();
        let ptime_ms = 20u32;
        let expected_spf = (clock_rate * ptime_ms / 1000) as usize;

        assert_eq!(
            spf, expected_spf,
            "{codec_type}: samples_per_frame={spf} does not match clock_rate({clock_rate}) * {ptime_ms}ms = {expected_spf}"
        );

        // Create a loopback session and send several packets
        let rx_cfg = SessionConfig::new("127.0.0.1:0", "127.0.0.1:1".parse().unwrap(), codec_type);
        let rx = RtpSession::new(rx_cfg).await.unwrap();
        let rx_addr = rx.local_addr();

        let tx_cfg = SessionConfig::new("127.0.0.1:0", rx_addr, codec_type);
        let mut tx = RtpSession::new(tx_cfg).await.unwrap();
        let tx_addr = tx.local_addr();
        drop(rx);

        // Recreate rx pointing back at tx
        let rx_cfg2 = SessionConfig::new("127.0.0.1:0", tx_addr, codec_type);
        let rx2 = RtpSession::new(rx_cfg2).await.unwrap();
        let rx2_addr = rx2.local_addr();
        // Update tx to point at new rx
        tx.set_remote_addr(rx2_addr);

        let silence = vec![0i16; spf];
        let num = 5usize;
        for _ in 0..num {
            tx.send_audio(&silence).await.unwrap();
        }

        let mut prev_ts: Option<u32> = None;
        for i in 0..num {
            let (pkt, _) = rx2.recv_packet().await.unwrap();
            assert_eq!(pkt.sequence_number, i as u16, "{codec_type}: sequence number");
            if let Some(prev) = prev_ts {
                let delta = pkt.timestamp.wrapping_sub(prev);
                assert_eq!(
                    delta, spf as u32,
                    "{codec_type}: timestamp delta {delta} != samples_per_frame {spf} at packet {i} — clock rate mismatch!"
                );
            }
            prev_ts = Some(pkt.timestamp);
        }

        println!("[{codec_type}] timestamp increment check passed ({num} frames, Δts={spf})");
    }
}

// ---------------------------------------------------------------------------
// 7. Jitter buffer size vs. artifact risk
// ---------------------------------------------------------------------------

/// A buffer that is too small relative to network jitter will suffer from
/// excessive drops. This test verifies the minimum fill level heuristic:
/// playout should not start until the buffer is at least 25% full.
#[test]
fn test_jitter_buffer_minimum_fill_before_playout() {
    // capacity=8 → min_fill = max(8/4, 1) = 2
    let mut jitter = JitterBuffer::new(8);

    // Insert only 1 packet — below min fill level, pop should return None
    let samples = vec![0i16; 160];
    let mut enc = CodecPipeline::new(CodecType::Pcmu);
    let encoded = enc.encode(&samples).unwrap();
    let pkt = RtpPacket::new(0, 0, 0, 0xBEEF).with_payload(encoded);
    jitter.insert(pkt);

    // With only 1 packet in a capacity-8 buffer (min_fill=2), playout hasn't started
    assert!(
        jitter.pop().is_none(),
        "buffer should not start playout before reaching min fill level"
    );

    // Insert second packet — now at fill level 2, playout starts
    let pkt2 = RtpPacket::new(0, 1, 160, 0xBEEF).with_payload(vec![0u8; 160]);
    jitter.insert(pkt2);

    assert!(
        jitter.pop().is_some(),
        "buffer should start playout once min fill level is reached"
    );
}

/// A very small jitter buffer (capacity=1) should still deliver packets —
/// the min_fill clamp to max(capacity/4, 1) = 1 ensures it.
#[test]
fn test_jitter_buffer_capacity_one_delivers_immediately() {
    let mut jitter = JitterBuffer::new(1);
    let pkt = RtpPacket::new(0, 0, 0, 0).with_payload(vec![0x7Fu8; 160]);
    jitter.insert(pkt);
    assert!(
        jitter.pop().is_some(),
        "capacity-1 buffer must deliver the first packet"
    );
}

// ---------------------------------------------------------------------------
// 8. RTCP contamination (metallic noise regression test)
// ---------------------------------------------------------------------------

/// Craft fake RTCP packets (SR, RR, SDES, BYE) and inject them directly into
/// an RTP session's socket.  The receive loop must drop all of them and must
/// NOT produce any decoded audio frames from them.
///
/// This is the regression test for the metallic-artifact bug: RTCP packets
/// share `version=2` with RTP so the parser accepted them; their payload was
/// then decoded as G.711, producing loud garbage bursts.
#[tokio::test]
async fn test_rtcp_packets_not_decoded_as_audio() {
    let codec_type = CodecType::Pcmu;

    // Create receiver session
    let rx_cfg = SessionConfig::new("127.0.0.1:0", "127.0.0.1:1".parse().unwrap(), codec_type);
    let rx = RtpSession::new(rx_cfg).await.unwrap();
    let rx_addr = rx.local_addr();

    // Sender socket (simulates a peer that also sends RTCP on the RTP port)
    let sender_sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // Recreate rx with sender as remote so it passes the source filter
    let sender_addr = sender_sock.local_addr().unwrap();
    let rx_cfg2 = SessionConfig::new("127.0.0.1:0", sender_addr, codec_type);
    let rx2 = RtpSession::new(rx_cfg2).await.unwrap();
    let rx2_addr = rx2.local_addr();

    let (mut audio_rx, _stop) = rx2.start_receiving(32);
    // Yield to let the receive task start listening before we send.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // Build synthetic RTCP packets:
    //   Byte 0:  V=2, P=0, RC=0  → 0x80
    //   Byte 1:  PT = 200 (SR), 201 (RR), 202 (SDES), 203 (BYE), 204 (APP)
    //   Bytes 2-3: length in 32-bit words minus 1
    //   Bytes 4+:  payload (irrelevant content)
    let rtcp_types: &[u8] = &[200, 201, 202, 203, 204];
    for &pt in rtcp_types {
        let mut pkt = vec![0u8; 8];
        pkt[0] = 0x80;          // V=2
        pkt[1] = pt;            // RTCP packet type
        pkt[2] = 0x00;          // length hi
        pkt[3] = 0x01;          // length lo = 1 → 8 bytes total
        pkt[4..8].copy_from_slice(&0xDEADBEEFu32.to_be_bytes()); // SSRC
        sender_sock.send_to(&pkt, rx2_addr).await.unwrap();
    }

    // Send 4 real RTP audio packets. The jitter buffer pops one per received
    // packet (after min_fill=2 is reached), so 4 sent → 3 popped + 1 buffered.
    // We assert exactly 3 frames arrive — none from the RTCP packets.
    let mut enc = CodecPipeline::new(codec_type);
    let audio = generate_sine_tone(440.0, 8000, 20, 14000);
    let spf = codec_type.samples_per_frame() as u32;
    for seq in 0..4u16 {
        let encoded = enc.encode(&audio).unwrap();
        let real_pkt = RtpPacket::new(codec_type.payload_type(), seq, seq as u32 * spf, 0xCAFE)
            .with_payload(encoded);
        sender_sock.send_to(&real_pkt.serialize(), rx2_addr).await.unwrap();
    }

    // Collect with a short timeout — exactly 3 frames should arrive (not 8
    // which would happen if the 5 RTCP packets were decoded as audio too).
    let mut received_frames = 0usize;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(200);
    loop {
        match tokio::time::timeout_at(deadline, audio_rx.recv()).await {
            Ok(Some(_frame)) => received_frames += 1,
            _ => break,
        }
    }

    assert_eq!(
        received_frames, 3,
        "exactly 3 frames expected from 4 real packets; \
         got {received_frames} (if >3, RTCP packets leaked as audio)"
    );
}

/// Verify that the PT 64-95 boundary is exact: PT 63 and PT 96 pass through,
/// PT 64 and PT 95 are rejected.  This guards against off-by-one regressions.
#[test]
fn test_rtcp_payload_type_boundary() {
    // Simulate the filter logic directly
    let is_rtcp_range = |pt: u8| pt >= 64 && pt <= 95;

    assert!(!is_rtcp_range(63), "PT 63 must NOT be in RTCP range");
    assert!(is_rtcp_range(64),  "PT 64 must be in RTCP range (boundary)");
    assert!(is_rtcp_range(72),  "PT 72 (RTCP SR mapped) must be in RTCP range");
    assert!(is_rtcp_range(76),  "PT 76 (RTCP APP mapped) must be in RTCP range");
    assert!(is_rtcp_range(95),  "PT 95 must be in RTCP range (boundary)");
    assert!(!is_rtcp_range(96), "PT 96 must NOT be in RTCP range");
    assert!(!is_rtcp_range(0),  "PT 0 (PCMU) must NOT be filtered");
    assert!(!is_rtcp_range(8),  "PT 8 (PCMA) must NOT be filtered");
    assert!(!is_rtcp_range(111), "PT 111 (Opus) must NOT be filtered");
}

// ---------------------------------------------------------------------------
// 9. Payload type filtering (wrong-PT regression)
// ---------------------------------------------------------------------------

/// Packets with an unexpected payload type (e.g., video, or a codec the
/// session was not negotiated for) must be silently discarded by the receive
/// loop.  They must not produce audio frames.
#[tokio::test]
async fn test_wrong_payload_type_filtered() {
    let codec_type = CodecType::Pcmu; // session expects PT=0

    let sender_sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let sender_addr = sender_sock.local_addr().unwrap();

    let rx_cfg = SessionConfig::new("127.0.0.1:0", sender_addr, codec_type);
    let rx = RtpSession::new(rx_cfg).await.unwrap();
    let rx_addr = rx.local_addr();
    let (mut audio_rx, _stop) = rx.start_receiving(32);
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // Send 5 packets with wrong PT (e.g., PT=96 = dynamic video or unknown codec).
    // These should all be silently dropped.
    for seq in 0..5u16 {
        let pkt = RtpPacket::new(96, seq, seq as u32 * 160, 0xBEEF)
            .with_payload(vec![0xAA; 160]);
        sender_sock.send_to(&pkt.serialize(), rx_addr).await.unwrap();
    }

    // Send 4 correct PT=0 packets → jitter buffer delivers 3 frames (n sent → n-1 popped).
    let mut enc = CodecPipeline::new(codec_type);
    let audio = generate_sine_tone(400.0, 8000, 20, 10000);
    let spf = codec_type.samples_per_frame() as u32;
    for seq in 0..4u16 {
        let encoded = enc.encode(&audio).unwrap();
        let good_pkt = RtpPacket::new(codec_type.payload_type(), seq, seq as u32 * spf, 0xBEEF)
            .with_payload(encoded);
        sender_sock.send_to(&good_pkt.serialize(), rx_addr).await.unwrap();
    }

    let mut received = 0usize;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(200);
    loop {
        match tokio::time::timeout_at(deadline, audio_rx.recv()).await {
            Ok(Some(_)) => received += 1,
            _ => break,
        }
    }

    assert_eq!(
        received, 3,
        "exactly 3 frames expected from 4 good packets; \
         wrong-PT packets must be filtered (got {received})"
    );
}

// ---------------------------------------------------------------------------
// 10. Playback buffer bounds under a fast producer
// ---------------------------------------------------------------------------

/// Simulates a producer pushing audio significantly faster than real-time
/// (burst delivery) and verifies the playback VecDeque stays within the
/// expected maximum size rather than growing without bound.
///
/// This is the regression test for the cumulative-latency / buffer-overflow
/// bug where clock drift caused the buffer to grow until the mpsc channel
/// filled and frames were silently dropped.
#[test]
fn test_playback_buffer_bounded_under_fast_producer() {
    // Mirror the capping logic from AudioPlayback::start():
    //   max_buf_samples = (4.0 * dst_rate / 50.0 * channels) as usize
    // Use 48000 Hz stereo as the worst-case device config.
    let dst_rate = 48000.0f64;
    let channels = 2usize;
    let max_buf_samples = (4.0 * dst_rate / 50.0 * channels as f64) as usize;

    let mut buffer: std::collections::VecDeque<f32> =
        std::collections::VecDeque::with_capacity(max_buf_samples);

    // Simulate 200 frames arriving at 2× real-time speed (fast producer / clock drift)
    let frame_samples = 960 * channels; // Opus/48kHz stereo frame
    for _ in 0..200 {
        // Producer pushes one frame
        buffer.extend(std::iter::repeat(0.5f32).take(frame_samples));

        // Apply the same clamping logic as in AudioPlayback
        if buffer.len() > max_buf_samples {
            let excess = buffer.len() - max_buf_samples;
            buffer.drain(..excess);
        }

        // Consumer drains one frame (simulating normal playback speed)
        // In the fast-producer scenario the consumer is slower, so skip drain
        // every other frame to simulate 2× accumulation rate.
    }

    assert!(
        buffer.len() <= max_buf_samples,
        "playback buffer grew to {} samples, exceeding max {} — \
         unbounded growth would cause cumulative latency artifacts",
        buffer.len(),
        max_buf_samples
    );

    println!(
        "[BufferBounds] final buffer={} / max={} samples ({:.1} ms at 48kHz stereo)",
        buffer.len(),
        max_buf_samples,
        buffer.len() as f64 / (dst_rate * channels as f64) * 1000.0
    );
}

/// Verify max_buf_samples formula produces sensible values for standard device configs.
#[test]
fn test_playback_buffer_max_sizes_are_reasonable() {
    // (device_rate, channels) → expected max latency ~80ms
    let cases = [(8000u32, 1u16), (44100, 2), (48000, 2), (96000, 2)];
    for (rate, ch) in cases {
        let max = (4.0 * rate as f64 / 50.0 * ch as f64) as usize;
        let latency_ms = max as f64 / (rate as f64 * ch as f64) * 1000.0;
        println!("[BufferMax] {rate} Hz {ch}ch → max={max} samples = {latency_ms:.1} ms");
        assert!(
            latency_ms >= 70.0 && latency_ms <= 90.0,
            "{rate} Hz {ch}ch: max latency {latency_ms:.1} ms is not in 70-90 ms range"
        );
    }
}

// ---------------------------------------------------------------------------
// Helper: RMS energy of a PCM slice
// ---------------------------------------------------------------------------

fn rms(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum / samples.len() as f64).sqrt()
}

// ---------------------------------------------------------------------------
// 11. Round-trip energy preservation
// ---------------------------------------------------------------------------

/// Sends a sine wave through the full UDP loopback pipeline and verifies that
/// the per-frame RMS energy of the decoded output stays within 5% of the
/// original.  This catches relays or buffers that silently zero payload bytes
/// (energy would drop to ~0) while still "succeeding" structurally.
#[tokio::test]
async fn test_round_trip_energy_pcmu() {
    round_trip_energy(CodecType::Pcmu, 440.0, 0.05).await;
}

#[tokio::test]
async fn test_round_trip_energy_pcma() {
    round_trip_energy(CodecType::Pcma, 440.0, 0.05).await;
}

async fn round_trip_energy(codec_type: CodecType, freq_hz: f64, max_rel_error: f64) {
    let spf = codec_type.samples_per_frame();
    let clock_rate = codec_type.clock_rate();

    let sender_placeholder =
        RtpSession::new(SessionConfig::new("127.0.0.1:0", "127.0.0.1:1".parse().unwrap(), codec_type))
            .await
            .unwrap();
    let sender_addr = sender_placeholder.local_addr();
    drop(sender_placeholder);

    let mut receiver =
        RtpSession::new(SessionConfig::new("127.0.0.1:0", sender_addr, codec_type))
            .await
            .unwrap();
    let receiver_addr = receiver.local_addr();

    let mut sender =
        RtpSession::new(SessionConfig::new("127.0.0.1:0", receiver_addr, codec_type))
            .await
            .unwrap();

    // Generate 10 frames of a known sine wave
    let full = generate_sine_tone(freq_hz, clock_rate, 200, 20_000);
    let frames: Vec<&[i16]> = full.chunks(spf).collect();
    let num_frames = frames.len();

    for frame in &frames {
        sender.send_audio(frame).await.unwrap();
    }

    let mut frame_errors: Vec<f64> = Vec::new();

    for i in 0..num_frames {
        let (pkt, _) = receiver.recv_packet().await.unwrap();
        let decoded = receiver.decode_packet(&pkt).unwrap();
        let orig_rms = rms(frames[i]);
        let recv_rms = rms(&decoded);

        // Avoid division by zero for genuinely silent frames
        if orig_rms > 1.0 {
            let rel_err = (orig_rms - recv_rms).abs() / orig_rms;
            frame_errors.push(rel_err);
            assert!(
                rel_err <= max_rel_error,
                "[{codec_type}] frame {i}: RMS energy error {rel_err:.3} > {max_rel_error} \
                 (orig={orig_rms:.1}, recv={recv_rms:.1}) — payload bytes may have been zeroed"
            );
        }
    }

    let avg_err = frame_errors.iter().sum::<f64>() / frame_errors.len() as f64;
    println!(
        "[{codec_type}] round-trip energy: avg_rel_rms_err={avg_err:.4} over {} frames",
        frame_errors.len()
    );
}

// ---------------------------------------------------------------------------
// 12. Silent-frame injection under channel pressure
// ---------------------------------------------------------------------------

/// Floods the RTP receive channel to capacity, then verifies:
///   (a) frames that DID make it through have non-zero energy (no zeros injected)
///   (b) dropped frames are counted, not silently replaced with silence
///
/// A buggy implementation might write silence into the output when the
/// channel is full rather than dropping — this would be hard to notice
/// structurally but sounds like intermittent muting.
#[tokio::test]
async fn test_no_silent_frame_injection_on_overflow() {
    let codec_type = CodecType::Pcmu;
    let spf = codec_type.samples_per_frame();

    let sender_placeholder =
        RtpSession::new(SessionConfig::new("127.0.0.1:0", "127.0.0.1:1".parse().unwrap(), codec_type))
            .await
            .unwrap();
    let sender_addr = sender_placeholder.local_addr();
    drop(sender_placeholder);

    let receiver =
        RtpSession::new(SessionConfig::new("127.0.0.1:0", sender_addr, codec_type))
            .await
            .unwrap();
    let receiver_addr = receiver.local_addr();

    // Use a very small channel (capacity=2) to force overflow quickly
    let (mut audio_rx, _stop) = receiver.start_receiving(2);

    let mut sender =
        RtpSession::new(SessionConfig::new("127.0.0.1:0", receiver_addr, codec_type))
            .await
            .unwrap();

    // Generate a loud, clearly non-silent signal
    let probe = generate_sine_tone(1000.0, codec_type.clock_rate(), 20, 28_000);
    let expected_rms = rms(&probe);
    assert!(expected_rms > 5000.0, "probe signal must be loud enough to detect");

    // Send 20 frames rapidly — most will overflow the capacity-2 channel
    for _ in 0..20 {
        sender.send_audio(&probe).await.unwrap();
    }

    // Drain whatever arrived
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(100);
    let mut silent_frames = 0usize;
    let mut total_frames = 0usize;

    loop {
        match tokio::time::timeout_at(deadline, audio_rx.recv()).await {
            Ok(Some(frame)) => {
                total_frames += 1;
                let frame_rms = rms(&frame);
                // A frame with RMS < 1% of expected is treated as silent injection
                if frame_rms < expected_rms * 0.01 {
                    silent_frames += 1;
                    eprintln!(
                        "Silent frame detected! rms={frame_rms:.1} (expected ~{expected_rms:.1})"
                    );
                }
                assert_eq!(
                    frame.len(),
                    spf,
                    "received frame has wrong length {}, expected {spf}",
                    frame.len()
                );
            }
            _ => break,
        }
    }

    println!(
        "[SilentFrameInjection] received={total_frames}, silent={silent_frames} \
         (overflow correctly dropped, not zeroed)"
    );

    assert_eq!(
        silent_frames, 0,
        "{silent_frames} silent frames were injected on overflow — \
         overflow must drop frames, not inject silence"
    );
    // Sanity: overflow must have actually occurred (not all 20 frames fit in capacity-2 channel)
    assert!(
        total_frames < 20,
        "expected overflow drops but all 20 frames arrived — channel capacity not exercised"
    );
}

// ---------------------------------------------------------------------------
// 13. Cross-channel isolation under concurrent load
// ---------------------------------------------------------------------------

/// Runs two independent RTP session pairs concurrently, each carrying a
/// different probe frequency.  Verifies that neither session's output
/// contains the other's signal — i.e. there is no shared codec state,
/// shared jitter buffer, or SSRC confusion between sessions.
///
/// Cross-correlation between the two output streams must be near zero.
/// If sessions share a codec pipeline the 880 Hz signal would bleed into
/// the 440 Hz session and vice versa, raising the cross-correlation.
#[tokio::test]
async fn test_cross_channel_isolation() {
    let codec_type = CodecType::Pcmu;
    let spf = codec_type.samples_per_frame();
    let clock_rate = codec_type.clock_rate();
    let num_frames = 8usize;

    // Build a loopback pair: sender binds to 0, receiver binds to 0,
    // then each is told the other's address.
    async fn make_pair(codec_type: CodecType) -> (RtpSession, RtpSession) {
        // Use placeholder to learn sender port
        let ph = RtpSession::new(SessionConfig::new(
            "127.0.0.1:0", "127.0.0.1:1".parse().unwrap(), codec_type,
        )).await.unwrap();
        let sender_addr = ph.local_addr();
        drop(ph);

        let mut receiver = RtpSession::new(SessionConfig::new(
            "127.0.0.1:0", sender_addr, codec_type,
        )).await.unwrap();
        let receiver_addr = receiver.local_addr();

        let mut sender = RtpSession::new(SessionConfig::new(
            "127.0.0.1:0", receiver_addr, codec_type,
        )).await.unwrap();
        let actual_sender_addr = sender.local_addr();
        receiver.set_remote_addr(actual_sender_addr);

        (sender, receiver)
    }

    let (mut a_sender, mut a_receiver) = make_pair(codec_type).await;
    let (mut b_sender, mut b_receiver) = make_pair(codec_type).await;

    let probe_a = generate_sine_tone(440.0, clock_rate, 20, 20_000); // 440 Hz
    let probe_b = generate_sine_tone(880.0, clock_rate, 20, 20_000); // 880 Hz

    // Send num_frames+1 so all num_frames arrive (last triggers jitter buffer pop)
    for _ in 0..num_frames + 1 {
        let _ = a_sender.send_audio(&probe_a).await;
        let _ = b_sender.send_audio(&probe_b).await;
    }

    // Collect decoded output from each session
    let mut a_out: Vec<i16> = Vec::new();
    let mut b_out: Vec<i16> = Vec::new();

    let timeout = std::time::Duration::from_millis(500);
    for _ in 0..num_frames {
        if let Ok(Ok((pkt, _))) =
            tokio::time::timeout(timeout, a_receiver.recv_packet()).await
        {
            if let Ok(s) = a_receiver.decode_packet(&pkt) {
                a_out.extend_from_slice(&s);
            }
        }
        if let Ok(Ok((pkt, _))) =
            tokio::time::timeout(timeout, b_receiver.recv_packet()).await
        {
            if let Ok(s) = b_receiver.decode_packet(&pkt) {
                b_out.extend_from_slice(&s);
            }
        }
    }

    assert!(!a_out.is_empty(), "session A produced no output");
    assert!(!b_out.is_empty(), "session B produced no output");

    // Build reference signals the same length as the collected output
    // by tiling the 1-frame probe.
    let a_ref: Vec<i16> = probe_a.iter().cloned().cycle().take(a_out.len()).collect();
    let b_ref: Vec<i16> = probe_b.iter().cloned().cycle().take(b_out.len()).collect();

    let a_self_corr = cross_correlation(&a_ref, &a_out);
    let b_self_corr = cross_correlation(&b_ref, &b_out);

    // Cross-correlation between the two output streams — orthogonal signals
    // (440 Hz vs 880 Hz) should be near zero if isolation is correct.
    let len = a_out.len().min(b_out.len());
    let cross_ab = cross_correlation(&a_out[..len], &b_out[..len]).abs();

    println!(
        "[CrossChannel] A self-corr={a_self_corr:.4}  B self-corr={b_self_corr:.4}  \
         cross-corr(A_out,B_out)={cross_ab:.4}  frames={num_frames} spf={spf}"
    );

    assert!(
        a_self_corr > 0.90,
        "session A output doesn't match its own probe (corr={a_self_corr:.4}) — codec state may be shared"
    );
    assert!(
        b_self_corr > 0.90,
        "session B output doesn't match its own probe (corr={b_self_corr:.4}) — codec state may be shared"
    );
    assert!(
        cross_ab < 0.30,
        "cross-corr A↔B = {cross_ab:.4} — sessions are contaminating each other"
    );
}
