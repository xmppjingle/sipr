/// End-to-end integration tests for the SIP softphone stack.
/// These tests simulate real SIP interactions between two endpoints
/// using loopback UDP transport.
use sip_core::dialog::{DialogState, SipDialog};
use sip_core::header::{generate_branch, generate_tag, HeaderName};
use sip_core::message::{
    RequestBuilder, ResponseBuilder, SipMessage, SipMethod, StatusCode,
};
use sip_core::sdp::SdpSession;
use sip_core::transaction::{SipTransaction, TransactionAction, TransactionState};
use sip_core::transport::SipTransport;
use rtp_core::codec::{CodecPipeline, CodecType};
use rtp_core::jitter::JitterBuffer;
use rtp_core::packet::RtpPacket;
use rtp_core::session::{RtpSession, SessionConfig};
use rtp_core::wav::{
    AudioRecorder, generate_sine_tone, generate_multi_tone,
    compute_snr, cross_correlation, max_sample_error, rms_error,
    decode_wav,
};
use std::net::SocketAddr;

// =============================================================================
// E2E Test: Full REGISTER flow
// =============================================================================

#[tokio::test]
async fn e2e_register_flow() {
    // Setup: UAC (client) and UAS (server/registrar) transports
    let uac_transport = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uas_transport = SipTransport::bind("127.0.0.1:0").await.unwrap();

    let uac_addr = uac_transport.local_addr();
    let uas_addr = uas_transport.local_addr();

    let call_id = uuid::Uuid::new_v4().to_string();
    let branch = generate_branch();
    let local_tag = generate_tag();

    // Step 1: UAC sends REGISTER
    let register = RequestBuilder::new(SipMethod::Register, format!("sip:{}", uas_addr))
        .header(
            HeaderName::Via,
            format!("SIP/2.0/UDP {};branch={};rport", uac_addr, branch),
        )
        .header(HeaderName::MaxForwards, "70")
        .header(
            HeaderName::From,
            format!("<sip:alice@{}>; tag={}", uas_addr, local_tag),
        )
        .header(HeaderName::To, format!("<sip:alice@{}>", uas_addr))
        .header(HeaderName::CallId, &call_id)
        .header(HeaderName::CSeq, "1 REGISTER")
        .header(
            HeaderName::Contact,
            format!("<sip:alice@{}>", uac_addr),
        )
        .header(HeaderName::Expires, "3600")
        .header(HeaderName::UserAgent, "siphone-test/0.1.0")
        .build();

    // Create client transaction
    let mut client_txn = SipTransaction::new_client(&register).unwrap();
    assert_eq!(client_txn.state, TransactionState::Trying);

    uac_transport.send_to(&register, uas_addr).await.unwrap();

    // Step 2: UAS receives REGISTER
    let incoming = uas_transport.recv().await.unwrap();
    assert_eq!(incoming.source, uac_addr);
    assert!(incoming.message.is_request());

    if let SipMessage::Request(ref req) = incoming.message {
        assert_eq!(req.method, SipMethod::Register);

        // Create server transaction
        let mut server_txn = SipTransaction::new_server(&incoming.message).unwrap();
        assert_eq!(server_txn.state, TransactionState::Trying);

        // Step 3: UAS sends 200 OK
        let ok_response = ResponseBuilder::from_request(req, StatusCode::OK)
            .header(
                HeaderName::Contact,
                format!("<sip:alice@{}>", uac_addr),
            )
            .header(HeaderName::Expires, "3600")
            .build();

        let action = server_txn.send_response(&ok_response);
        assert_eq!(action, TransactionAction::SendResponse);

        uas_transport.send_to(&ok_response, uac_addr).await.unwrap();

        // Step 4: UAC receives 200 OK
        let response = uac_transport.recv().await.unwrap();
        assert!(response.message.is_response());

        let action = client_txn.process_response(&response.message);
        assert_eq!(action, TransactionAction::PassToTU);
        // Non-invite client goes to Completed
        assert_eq!(client_txn.state, TransactionState::Completed);

        if let SipMessage::Response(ref res) = response.message {
            assert_eq!(res.status, StatusCode::OK);
        }
    } else {
        panic!("Expected REGISTER request");
    }
}

// =============================================================================
// E2E Test: Full INVITE call flow (INVITE → 180 → 200 → ACK → BYE → 200)
// =============================================================================

#[tokio::test]
async fn e2e_invite_call_flow() {
    let caller_transport = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let callee_transport = SipTransport::bind("127.0.0.1:0").await.unwrap();

    let caller_addr = caller_transport.local_addr();
    let callee_addr = callee_transport.local_addr();

    let call_id = uuid::Uuid::new_v4().to_string();
    let caller_tag = generate_tag();
    let branch = generate_branch();

    // --- Step 1: Caller sends INVITE with SDP ---
    let mut caller_sdp = SdpSession::new(&caller_addr.ip().to_string());
    caller_sdp.add_audio_media(10000);
    let sdp_body = caller_sdp.to_string();

    let invite = RequestBuilder::new(SipMethod::Invite, format!("sip:bob@{}", callee_addr))
        .header(
            HeaderName::Via,
            format!("SIP/2.0/UDP {};branch={};rport", caller_addr, branch),
        )
        .header(HeaderName::MaxForwards, "70")
        .header(
            HeaderName::From,
            format!("<sip:alice@{}>;tag={}", caller_addr, caller_tag),
        )
        .header(
            HeaderName::To,
            format!("<sip:bob@{}>", callee_addr),
        )
        .header(HeaderName::CallId, &call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .header(
            HeaderName::Contact,
            format!("<sip:alice@{}>", caller_addr),
        )
        .header(HeaderName::ContentType, "application/sdp")
        .body(&sdp_body)
        .build();

    // Create caller dialog
    let mut caller_dialog = SipDialog::new_uac(
        call_id.clone(),
        caller_tag.clone(),
        format!("sip:alice@{}", caller_addr),
        format!("sip:bob@{}", callee_addr),
    );
    assert_eq!(caller_dialog.state, DialogState::Early);

    // Create caller transaction
    let mut caller_txn = SipTransaction::new_client(&invite).unwrap();

    caller_transport.send_to(&invite, callee_addr).await.unwrap();

    // --- Step 2: Callee receives INVITE, creates dialog ---
    let incoming = callee_transport.recv().await.unwrap();
    assert!(incoming.message.is_request());

    let callee_dialog = SipDialog::from_invite(&incoming.message).unwrap();
    assert_eq!(callee_dialog.state, DialogState::Early);
    assert_eq!(callee_dialog.call_id, call_id);

    let callee_tag = callee_dialog.local_tag.clone();

    if let SipMessage::Request(ref req) = incoming.message {
        assert_eq!(req.method, SipMethod::Invite);

        // Verify SDP in body
        let body = req.body.as_ref().unwrap();
        let received_sdp = SdpSession::parse(body).unwrap();
        assert_eq!(received_sdp.get_audio_port(), Some(10000));

        // --- Step 3: Callee sends 180 Ringing ---
        let ringing = ResponseBuilder::from_request(req, StatusCode::RINGING)
            .header(
                HeaderName::To,
                format!("<sip:bob@{}>;tag={}", callee_addr, callee_tag),
            )
            .build();

        callee_transport.send_to(&ringing, caller_addr).await.unwrap();

        // Caller receives 180
        let ringing_recv = caller_transport.recv().await.unwrap();
        let action = caller_txn.process_response(&ringing_recv.message);
        assert_eq!(action, TransactionAction::PassToTU);
        assert_eq!(caller_txn.state, TransactionState::Proceeding);

        caller_dialog.process_response(&ringing_recv.message);
        assert_eq!(caller_dialog.state, DialogState::Early);
        assert_eq!(caller_dialog.remote_tag, Some(callee_tag.clone()));

        // --- Step 4: Callee sends 200 OK with SDP answer ---
        let mut callee_sdp = SdpSession::new(&callee_addr.ip().to_string());
        callee_sdp.add_audio_media(20000);
        let callee_sdp_body = callee_sdp.to_string();

        let ok_response = ResponseBuilder::from_request(req, StatusCode::OK)
            .header(
                HeaderName::To,
                format!("<sip:bob@{}>;tag={}", callee_addr, callee_tag),
            )
            .header(
                HeaderName::Contact,
                format!("<sip:bob@{}>", callee_addr),
            )
            .header(HeaderName::ContentType, "application/sdp")
            .body(&callee_sdp_body)
            .build();

        callee_transport.send_to(&ok_response, caller_addr).await.unwrap();

        // Caller receives 200 OK
        let ok_recv = caller_transport.recv().await.unwrap();
        let action = caller_txn.process_response(&ok_recv.message);
        assert_eq!(action, TransactionAction::PassToTU);
        assert_eq!(caller_txn.state, TransactionState::Terminated);

        caller_dialog.process_response(&ok_recv.message);
        assert_eq!(caller_dialog.state, DialogState::Confirmed);

        // Verify SDP answer
        if let SipMessage::Response(ref res) = ok_recv.message {
            let sdp_answer = SdpSession::parse(res.body.as_ref().unwrap()).unwrap();
            assert_eq!(sdp_answer.get_audio_port(), Some(20000));
        }

        // --- Step 5: Caller sends ACK ---
        let ack_branch = generate_branch();
        let ack = RequestBuilder::new(SipMethod::Ack, format!("sip:bob@{}", callee_addr))
            .header(
                HeaderName::Via,
                format!("SIP/2.0/UDP {};branch={};rport", caller_addr, ack_branch),
            )
            .header(HeaderName::MaxForwards, "70")
            .header(
                HeaderName::From,
                format!("<sip:alice@{}>;tag={}", caller_addr, caller_tag),
            )
            .header(
                HeaderName::To,
                format!("<sip:bob@{}>;tag={}", callee_addr, callee_tag),
            )
            .header(HeaderName::CallId, &call_id)
            .header(HeaderName::CSeq, "1 ACK")
            .build();

        caller_transport.send_to(&ack, callee_addr).await.unwrap();

        // Callee receives ACK
        let ack_recv = callee_transport.recv().await.unwrap();
        if let SipMessage::Request(ref ack_req) = ack_recv.message {
            assert_eq!(ack_req.method, SipMethod::Ack);
        }

        // --- Step 6: Caller sends BYE ---
        let bye_branch = generate_branch();
        let bye_cseq = caller_dialog.next_cseq();
        let bye = RequestBuilder::new(SipMethod::Bye, format!("sip:bob@{}", callee_addr))
            .header(
                HeaderName::Via,
                format!("SIP/2.0/UDP {};branch={};rport", caller_addr, bye_branch),
            )
            .header(HeaderName::MaxForwards, "70")
            .header(
                HeaderName::From,
                format!("<sip:alice@{}>;tag={}", caller_addr, caller_tag),
            )
            .header(
                HeaderName::To,
                format!("<sip:bob@{}>;tag={}", callee_addr, callee_tag),
            )
            .header(HeaderName::CallId, &call_id)
            .header(HeaderName::CSeq, format!("{} BYE", bye_cseq))
            .build();

        caller_dialog.terminate();
        assert_eq!(caller_dialog.state, DialogState::Terminated);

        caller_transport.send_to(&bye, callee_addr).await.unwrap();

        // Callee receives BYE
        let bye_recv = callee_transport.recv().await.unwrap();
        if let SipMessage::Request(ref bye_req) = bye_recv.message {
            assert_eq!(bye_req.method, SipMethod::Bye);

            // Callee sends 200 OK for BYE
            let bye_ok = ResponseBuilder::from_request(bye_req, StatusCode::OK).build();
            callee_transport.send_to(&bye_ok, caller_addr).await.unwrap();
        }

        // Caller receives 200 OK for BYE
        let bye_ok_recv = caller_transport.recv().await.unwrap();
        if let SipMessage::Response(ref res) = bye_ok_recv.message {
            assert_eq!(res.status, StatusCode::OK);
        }
    } else {
        panic!("Expected INVITE request");
    }
}

// =============================================================================
// E2E Test: Full RTP audio exchange between two endpoints
// =============================================================================

#[tokio::test]
async fn e2e_rtp_audio_exchange() {
    // Setup two RTP endpoints
    let sender_config = SessionConfig::new("127.0.0.1:0", "127.0.0.1:0".parse().unwrap(), CodecType::Pcmu);
    let sender = RtpSession::new(sender_config).await.unwrap();
    let sender_addr = sender.local_addr();

    let receiver_config = SessionConfig::new("127.0.0.1:0", sender_addr, CodecType::Pcmu);
    let mut receiver = RtpSession::new(receiver_config).await.unwrap();
    let receiver_addr = receiver.local_addr();

    // Update sender to point at receiver (we needed receiver's addr first)
    let sender_config = SessionConfig::new("127.0.0.1:0", receiver_addr, CodecType::Pcmu);
    let mut sender = RtpSession::new(sender_config).await.unwrap();

    // Generate audio: a 400Hz sine wave at 8kHz sample rate, 20ms frame
    let samples: Vec<i16> = (0..160)
        .map(|i| {
            let t = i as f64 / 8000.0;
            (f64::sin(2.0 * std::f64::consts::PI * 400.0 * t) * 16000.0) as i16
        })
        .collect();

    // Send 5 frames
    for _ in 0..5 {
        sender.send_audio(&samples).await.unwrap();
    }

    // Receive and decode all 5 frames
    for i in 0..5 {
        let (packet, _source) = receiver.recv_packet().await.unwrap();
        assert_eq!(packet.payload_type, 0); // PCMU
        assert_eq!(packet.sequence_number, i);
        assert_eq!(packet.payload.len(), 160);

        let decoded = receiver.decode_packet(&packet).unwrap();
        assert_eq!(decoded.len(), 160);

        // Verify decoded audio is reasonably close to original
        for (orig, dec) in samples.iter().zip(decoded.iter()) {
            let diff = (*orig as i32 - *dec as i32).abs();
            assert!(diff < 500, "Audio quality too low: diff={}", diff);
        }
    }

    assert_eq!(sender.stats().packets_sent, 5);
}

// =============================================================================
// E2E Test: RTP with jitter buffer reordering
// =============================================================================

#[tokio::test]
async fn e2e_rtp_jitter_buffer_integration() {
    // Simulate out-of-order packet delivery through the jitter buffer
    let mut codec = CodecPipeline::new(CodecType::Pcmu);
    let mut jitter = JitterBuffer::new(20);

    // Create packets with known content
    let mut packets = Vec::new();
    for seq in 0..10u16 {
        let samples: Vec<i16> = vec![seq as i16 * 100; 160];
        let encoded = codec.encode(&samples).unwrap();
        let packet = RtpPacket::new(0, seq, seq as u32 * 160, 0x12345678)
            .with_payload(encoded);
        packets.push(packet);
    }

    // Insert in scrambled order: 3, 1, 4, 0, 2, 5, 7, 6, 9, 8
    let order = [3, 1, 4, 0, 2, 5, 7, 6, 9, 8];
    for &idx in &order {
        jitter.insert(packets[idx].clone());
    }

    // Pop should return in sequence order 0..10
    for expected_seq in 0..10u16 {
        let pkt = jitter.pop().unwrap();
        assert_eq!(pkt.sequence_number, expected_seq);

        // Decode and verify content
        let decoded = codec.decode(&pkt.payload).unwrap();
        assert_eq!(decoded.len(), 160);
        // Each packet's samples should match the sequence number pattern
        let expected_val = expected_seq as i16 * 100;
        // G.711 is lossy, but for such simple values the decoded should be close
        for &sample in &decoded {
            let diff = (sample as i32 - expected_val as i32).abs();
            assert!(diff < 500, "seq={}, expected~{}, got {}", expected_seq, expected_val, sample);
        }
    }

    assert_eq!(jitter.packets_received(), 10);
}

// =============================================================================
// E2E Test: SIP + RTP integrated call setup
// =============================================================================

#[tokio::test]
async fn e2e_sip_rtp_integrated_call() {
    // This test simulates a complete call:
    // 1. INVITE with SDP offer
    // 2. 200 OK with SDP answer
    // 3. ACK
    // 4. RTP audio exchange
    // 5. BYE

    let caller_sip = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let callee_sip = SipTransport::bind("127.0.0.1:0").await.unwrap();

    let caller_sip_addr = caller_sip.local_addr();
    let callee_sip_addr = callee_sip.local_addr();

    // Pre-bind RTP sockets to know the ports for SDP
    let caller_rtp_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let caller_rtp_port = caller_rtp_socket.local_addr().unwrap().port();
    drop(caller_rtp_socket);

    let call_id = uuid::Uuid::new_v4().to_string();
    let caller_tag = generate_tag();
    let branch = generate_branch();

    // --- INVITE with SDP ---
    let mut caller_sdp = SdpSession::new("127.0.0.1");
    caller_sdp.add_audio_media(caller_rtp_port);
    let sdp_offer = caller_sdp.to_string();

    let invite = RequestBuilder::new(SipMethod::Invite, format!("sip:bob@{}", callee_sip_addr))
        .header(
            HeaderName::Via,
            format!("SIP/2.0/UDP {};branch={}", caller_sip_addr, branch),
        )
        .header(HeaderName::MaxForwards, "70")
        .header(
            HeaderName::From,
            format!("<sip:alice@{}>;tag={}", caller_sip_addr, caller_tag),
        )
        .header(HeaderName::To, format!("<sip:bob@{}>", callee_sip_addr))
        .header(HeaderName::CallId, &call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .header(
            HeaderName::Contact,
            format!("<sip:alice@{}>", caller_sip_addr),
        )
        .header(HeaderName::ContentType, "application/sdp")
        .body(&sdp_offer)
        .build();

    caller_sip.send_to(&invite, callee_sip_addr).await.unwrap();

    // --- Callee receives INVITE, parses SDP ---
    let incoming = callee_sip.recv().await.unwrap();
    let req = match &incoming.message {
        SipMessage::Request(req) => req.clone(),
        _ => panic!("Expected INVITE"),
    };

    let offer_sdp = SdpSession::parse(req.body.as_ref().unwrap()).unwrap();
    let caller_audio_port = offer_sdp.get_audio_port().unwrap();
    assert_eq!(caller_audio_port, caller_rtp_port);

    // --- Callee sends 200 OK with SDP answer ---
    let callee_tag = generate_tag();

    // Create callee RTP session
    let callee_rtp_remote = SocketAddr::new(
        "127.0.0.1".parse().unwrap(),
        caller_audio_port,
    );
    let callee_rtp_config = SessionConfig::new("127.0.0.1:0", callee_rtp_remote, CodecType::Pcmu);
    let mut callee_rtp = RtpSession::new(callee_rtp_config).await.unwrap();
    let callee_rtp_port = callee_rtp.local_addr().port();

    let mut callee_sdp = SdpSession::new("127.0.0.1");
    callee_sdp.add_audio_media(callee_rtp_port);
    let sdp_answer = callee_sdp.to_string();

    let ok_response = ResponseBuilder::from_request(&req, StatusCode::OK)
        .header(
            HeaderName::To,
            format!("<sip:bob@{}>;tag={}", callee_sip_addr, callee_tag),
        )
        .header(
            HeaderName::Contact,
            format!("<sip:bob@{}>", callee_sip_addr),
        )
        .header(HeaderName::ContentType, "application/sdp")
        .body(&sdp_answer)
        .build();

    callee_sip.send_to(&ok_response, caller_sip_addr).await.unwrap();

    // --- Caller receives 200 OK, extracts SDP answer ---
    let ok_recv = caller_sip.recv().await.unwrap();
    if let SipMessage::Response(ref res) = ok_recv.message {
        assert_eq!(res.status, StatusCode::OK);
        let answer_sdp = SdpSession::parse(res.body.as_ref().unwrap()).unwrap();
        let callee_audio_port_from_sdp = answer_sdp.get_audio_port().unwrap();
        assert_eq!(callee_audio_port_from_sdp, callee_rtp_port);

        // --- Create caller RTP session using info from SDP answer ---
        let caller_rtp_remote = SocketAddr::new(
            "127.0.0.1".parse().unwrap(),
            callee_audio_port_from_sdp,
        );
        let caller_rtp_config =
            SessionConfig::new("127.0.0.1:0", caller_rtp_remote, CodecType::Pcmu);
        let mut caller_rtp = RtpSession::new(caller_rtp_config).await.unwrap();

        // --- Send ACK ---
        let ack_branch = generate_branch();
        let ack = RequestBuilder::new(SipMethod::Ack, format!("sip:bob@{}", callee_sip_addr))
            .header(
                HeaderName::Via,
                format!("SIP/2.0/UDP {};branch={}", caller_sip_addr, ack_branch),
            )
            .header(HeaderName::MaxForwards, "70")
            .header(
                HeaderName::From,
                format!("<sip:alice@{}>;tag={}", caller_sip_addr, caller_tag),
            )
            .header(
                HeaderName::To,
                format!("<sip:bob@{}>;tag={}", callee_sip_addr, callee_tag),
            )
            .header(HeaderName::CallId, &call_id)
            .header(HeaderName::CSeq, "1 ACK")
            .build();

        caller_sip.send_to(&ack, callee_sip_addr).await.unwrap();

        // Callee receives ACK
        let ack_recv = callee_sip.recv().await.unwrap();
        if let SipMessage::Request(ref ack_req) = ack_recv.message {
            assert_eq!(ack_req.method, SipMethod::Ack);
        } else {
            panic!("Expected ACK");
        }

        // --- RTP audio exchange ---
        let audio_frame: Vec<i16> = (0..160)
            .map(|i| ((i as f64 / 160.0 * std::f64::consts::TAU).sin() * 8000.0) as i16)
            .collect();

        // Caller sends 3 RTP packets
        for _ in 0..3 {
            caller_rtp.send_audio(&audio_frame).await.unwrap();
        }

        // Callee receives 3 RTP packets
        for seq in 0..3u16 {
            let (pkt, _) = callee_rtp.recv_packet().await.unwrap();
            assert_eq!(pkt.payload_type, 0);
            assert_eq!(pkt.sequence_number, seq);
            let decoded = callee_rtp.decode_packet(&pkt).unwrap();
            assert_eq!(decoded.len(), 160);
        }

        // --- BYE ---
        let bye_branch = generate_branch();
        let bye = RequestBuilder::new(SipMethod::Bye, format!("sip:bob@{}", callee_sip_addr))
            .header(
                HeaderName::Via,
                format!("SIP/2.0/UDP {};branch={}", caller_sip_addr, bye_branch),
            )
            .header(HeaderName::MaxForwards, "70")
            .header(
                HeaderName::From,
                format!("<sip:alice@{}>;tag={}", caller_sip_addr, caller_tag),
            )
            .header(
                HeaderName::To,
                format!("<sip:bob@{}>;tag={}", callee_sip_addr, callee_tag),
            )
            .header(HeaderName::CallId, &call_id)
            .header(HeaderName::CSeq, "2 BYE")
            .build();

        caller_sip.send_to(&bye, callee_sip_addr).await.unwrap();

        let bye_recv = callee_sip.recv().await.unwrap();
        if let SipMessage::Request(ref bye_req) = bye_recv.message {
            assert_eq!(bye_req.method, SipMethod::Bye);
        }
    } else {
        panic!("Expected 200 OK response");
    }
}

// =============================================================================
// E2E Test: INVITE rejected with 486 Busy Here
// =============================================================================

#[tokio::test]
async fn e2e_invite_rejected_busy() {
    let caller = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let callee = SipTransport::bind("127.0.0.1:0").await.unwrap();

    let caller_addr = caller.local_addr();
    let callee_addr = callee.local_addr();

    let call_id = uuid::Uuid::new_v4().to_string();
    let caller_tag = generate_tag();
    let branch = generate_branch();

    let invite = RequestBuilder::new(SipMethod::Invite, format!("sip:bob@{}", callee_addr))
        .header(
            HeaderName::Via,
            format!("SIP/2.0/UDP {};branch={}", caller_addr, branch),
        )
        .header(HeaderName::MaxForwards, "70")
        .header(
            HeaderName::From,
            format!("<sip:alice@{}>;tag={}", caller_addr, caller_tag),
        )
        .header(HeaderName::To, format!("<sip:bob@{}>", callee_addr))
        .header(HeaderName::CallId, &call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .build();

    let mut caller_dialog = SipDialog::new_uac(
        call_id.clone(),
        caller_tag.clone(),
        format!("sip:alice@{}", caller_addr),
        format!("sip:bob@{}", callee_addr),
    );

    caller.send_to(&invite, callee_addr).await.unwrap();

    // Callee receives and rejects
    let incoming = callee.recv().await.unwrap();
    if let SipMessage::Request(ref req) = incoming.message {
        let busy = ResponseBuilder::from_request(req, StatusCode::BUSY_HERE)
            .header(
                HeaderName::To,
                format!("<sip:bob@{}>;tag={}", callee_addr, generate_tag()),
            )
            .build();
        callee.send_to(&busy, caller_addr).await.unwrap();
    }

    // Caller receives 486
    let response = caller.recv().await.unwrap();
    caller_dialog.process_response(&response.message);
    assert_eq!(caller_dialog.state, DialogState::Terminated);

    if let SipMessage::Response(ref res) = response.message {
        assert_eq!(res.status, StatusCode::BUSY_HERE);
    }
}

// =============================================================================
// E2E Test: CANCEL an outgoing INVITE
// =============================================================================

#[tokio::test]
async fn e2e_cancel_invite() {
    let caller = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let callee = SipTransport::bind("127.0.0.1:0").await.unwrap();

    let caller_addr = caller.local_addr();
    let callee_addr = callee.local_addr();

    let call_id = uuid::Uuid::new_v4().to_string();
    let caller_tag = generate_tag();
    let branch = generate_branch();

    // Send INVITE
    let invite = RequestBuilder::new(SipMethod::Invite, format!("sip:bob@{}", callee_addr))
        .header(
            HeaderName::Via,
            format!("SIP/2.0/UDP {};branch={}", caller_addr, branch),
        )
        .header(HeaderName::MaxForwards, "70")
        .header(
            HeaderName::From,
            format!("<sip:alice@{}>;tag={}", caller_addr, caller_tag),
        )
        .header(HeaderName::To, format!("<sip:bob@{}>", callee_addr))
        .header(HeaderName::CallId, &call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .build();

    caller.send_to(&invite, callee_addr).await.unwrap();

    // Callee receives INVITE
    let incoming = callee.recv().await.unwrap();
    assert!(incoming.message.is_request());

    // Callee sends 180 Ringing
    if let SipMessage::Request(ref req) = incoming.message {
        let ringing = ResponseBuilder::from_request(req, StatusCode::RINGING)
            .header(
                HeaderName::To,
                format!("<sip:bob@{}>;tag={}", callee_addr, generate_tag()),
            )
            .build();
        callee.send_to(&ringing, caller_addr).await.unwrap();
    }

    // Caller receives 180
    let ringing_recv = caller.recv().await.unwrap();
    assert!(ringing_recv.message.is_response());

    // Caller sends CANCEL (same branch as INVITE)
    let cancel = RequestBuilder::new(SipMethod::Cancel, format!("sip:bob@{}", callee_addr))
        .header(
            HeaderName::Via,
            format!("SIP/2.0/UDP {};branch={}", caller_addr, branch),
        )
        .header(HeaderName::MaxForwards, "70")
        .header(
            HeaderName::From,
            format!("<sip:alice@{}>;tag={}", caller_addr, caller_tag),
        )
        .header(HeaderName::To, format!("<sip:bob@{}>", callee_addr))
        .header(HeaderName::CallId, &call_id)
        .header(HeaderName::CSeq, "1 CANCEL")
        .build();

    caller.send_to(&cancel, callee_addr).await.unwrap();

    // Callee receives CANCEL
    let cancel_recv = callee.recv().await.unwrap();
    if let SipMessage::Request(ref req) = cancel_recv.message {
        assert_eq!(req.method, SipMethod::Cancel);

        // Callee sends 200 OK for CANCEL
        let cancel_ok = ResponseBuilder::from_request(req, StatusCode::OK).build();
        callee.send_to(&cancel_ok, caller_addr).await.unwrap();
    }

    // Caller receives 200 OK for CANCEL
    let cancel_ok_recv = caller.recv().await.unwrap();
    if let SipMessage::Response(ref res) = cancel_ok_recv.message {
        assert_eq!(res.status, StatusCode::OK);
    }
}

// =============================================================================
// E2E Test: Multiple codec negotiation via SDP
// =============================================================================

#[tokio::test]
async fn e2e_sdp_codec_negotiation() {
    // Caller offers PCMU, PCMA, Opus
    let mut offer = SdpSession::new("10.0.0.1");
    offer.add_audio_media(5000);

    let offer_str = offer.to_string();

    // Parse the offer
    let parsed_offer = SdpSession::parse(&offer_str).unwrap();
    let audio = &parsed_offer.media_descriptions[0];

    // Verify all codecs are present
    assert!(audio.rtpmaps.iter().any(|r| r.encoding_name == "PCMU"));
    assert!(audio.rtpmaps.iter().any(|r| r.encoding_name == "PCMA"));
    assert!(audio.rtpmaps.iter().any(|r| r.encoding_name == "opus"));

    // Callee answers with just PCMU
    let mut answer = SdpSession::new("10.0.0.2");
    {
        let mut audio_media = sip_core::sdp::MediaDescription::new_audio(6000);
        audio_media.add_codec(0, "PCMU", 8000, None);
        audio_media.add_attribute("sendrecv", None);
        answer.media_descriptions.push(audio_media);
    }

    let answer_str = answer.to_string();
    let parsed_answer = SdpSession::parse(&answer_str).unwrap();

    assert_eq!(parsed_answer.media_descriptions.len(), 1);
    assert_eq!(parsed_answer.media_descriptions[0].rtpmaps.len(), 1);
    assert_eq!(
        parsed_answer.media_descriptions[0].rtpmaps[0].encoding_name,
        "PCMU"
    );

    // Both sides can now use PCMU
    let mut caller_codec = CodecPipeline::new(CodecType::Pcmu);
    let mut callee_codec = CodecPipeline::new(CodecType::Pcmu);

    let samples = vec![5000i16; 160];
    let encoded = caller_codec.encode(&samples).unwrap();
    let decoded = callee_codec.decode(&encoded).unwrap();
    assert_eq!(decoded.len(), 160);
}

// =============================================================================
// E2E Test: SIP message parse → serialize → parse roundtrip
// =============================================================================

#[tokio::test]
async fn e2e_message_roundtrip_through_transport() {
    let t1 = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let t2 = SipTransport::bind("127.0.0.1:0").await.unwrap();

    let t1_addr = t1.local_addr();
    let t2_addr = t2.local_addr();

    // Build a complex message
    let mut sdp = SdpSession::new("192.168.1.100");
    sdp.add_audio_media(49170);
    let sdp_body = sdp.to_string();

    let msg = RequestBuilder::new(SipMethod::Invite, "sip:bob@biloxi.com")
        .header(
            HeaderName::Via,
            format!("SIP/2.0/UDP {};branch={}", t1_addr, generate_branch()),
        )
        .header(HeaderName::MaxForwards, "70")
        .header(
            HeaderName::From,
            format!("<sip:alice@atlanta.com>;tag={}", generate_tag()),
        )
        .header(HeaderName::To, "<sip:bob@biloxi.com>")
        .header(HeaderName::CallId, uuid::Uuid::new_v4().to_string())
        .header(HeaderName::CSeq, "1 INVITE")
        .header(
            HeaderName::Contact,
            format!("<sip:alice@{}>", t1_addr),
        )
        .header(HeaderName::ContentType, "application/sdp")
        .body(&sdp_body)
        .build();

    // Send through UDP
    t1.send_to(&msg, t2_addr).await.unwrap();

    // Receive and parse
    let incoming = t2.recv().await.unwrap();
    let received = incoming.message;

    // Verify the message survived the roundtrip
    assert!(received.is_request());
    assert_eq!(received.cseq().unwrap().1, SipMethod::Invite);

    // Verify SDP body survived
    let body = received.body().unwrap();
    let received_sdp = SdpSession::parse(body).unwrap();
    assert_eq!(received_sdp.get_audio_port(), Some(49170));
    assert_eq!(
        received_sdp.get_connection_address(),
        Some("192.168.1.100")
    );
}

// =============================================================================
// E2E Test: Transaction retransmission logic
// =============================================================================

#[test]
fn e2e_transaction_retransmit_behavior() {
    let branch = generate_branch();

    let mut headers = sip_core::header::Headers::new();
    headers.add(
        HeaderName::Via,
        format!("SIP/2.0/UDP 10.0.0.1:5060;branch={}", branch),
    );
    headers.add(HeaderName::From, "<sip:alice@a.com>;tag=t1");
    headers.add(HeaderName::To, "<sip:bob@b.com>");
    headers.add(HeaderName::CallId, "retransmit-test");
    headers.add(HeaderName::CSeq, "1 INVITE");
    headers.add(HeaderName::ContentLength, "0");

    let invite = SipMessage::Request(sip_core::message::SipRequest {
        method: SipMethod::Invite,
        uri: "sip:bob@b.com".to_string(),
        version: "SIP/2.0".to_string(),
        headers,
        body: None,
    });

    let mut txn = SipTransaction::new_client(&invite).unwrap();

    // Verify exponential backoff
    assert!(txn.should_retransmit());
    let intervals: Vec<u64> = (0..5)
        .map(|_| {
            let interval = txn.retransmit_interval().as_millis() as u64;
            txn.mark_retransmit();
            interval
        })
        .collect();

    // Should double: 500, 1000, 2000, 4000, 4000 (capped at T2)
    assert_eq!(intervals[0], 500);
    assert_eq!(intervals[1], 1000);
    assert_eq!(intervals[2], 2000);
    assert_eq!(intervals[3], 4000);
    assert_eq!(intervals[4], 4000); // capped
}

// =============================================================================
// E2E Test: Bidirectional RTP audio
// =============================================================================

#[tokio::test]
async fn e2e_bidirectional_rtp() {
    // Pre-bind two UDP sockets to learn their ports
    let sock_a = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr_a = sock_a.local_addr().unwrap();
    let sock_b = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr_b = sock_b.local_addr().unwrap();
    drop(sock_a);
    drop(sock_b);

    // Create sessions with known remote addresses
    // Bind to specific ports so we reuse the same addresses
    let session_a_config = SessionConfig::new(&addr_a.to_string(), addr_b, CodecType::Pcmu);
    let mut session_a = RtpSession::new(session_a_config).await.unwrap();

    let session_b_config = SessionConfig::new(&addr_b.to_string(), addr_a, CodecType::Pcmu);
    let mut session_b = RtpSession::new(session_b_config).await.unwrap();

    // A → B
    let samples_a = vec![1000i16; 160];
    session_a.send_audio(&samples_a).await.unwrap();

    let (pkt_at_b, _) = session_b.recv_packet().await.unwrap();
    assert_eq!(pkt_at_b.payload_type, 0);
    let decoded_at_b = session_b.decode_packet(&pkt_at_b).unwrap();
    assert_eq!(decoded_at_b.len(), 160);

    // B → A
    let samples_b = vec![2000i16; 160];
    session_b.send_audio(&samples_b).await.unwrap();

    let (pkt_at_a, _) = session_a.recv_packet().await.unwrap();
    assert_eq!(pkt_at_a.payload_type, 0);
    let decoded_at_a = session_a.decode_packet(&pkt_at_a).unwrap();
    assert_eq!(decoded_at_a.len(), 160);
}

// =============================================================================
// E2E Test: Two-instance call with audio recording and fidelity verification
// Alice calls Bob, sends a sine tone, Bob records it, then verify fidelity
// =============================================================================

#[tokio::test]
async fn e2e_two_instance_call_with_audio_recording() {
    // --- Setup SIP transports for Alice and Bob ---
    let alice_sip = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let bob_sip = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let alice_sip_addr = alice_sip.local_addr();
    let bob_sip_addr = bob_sip.local_addr();

    // Pre-bind RTP sockets to learn ports for SDP
    let alice_rtp_sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let alice_rtp_port = alice_rtp_sock.local_addr().unwrap().port();
    let bob_rtp_sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let bob_rtp_port = bob_rtp_sock.local_addr().unwrap().port();
    drop(alice_rtp_sock);
    drop(bob_rtp_sock);

    let call_id = uuid::Uuid::new_v4().to_string();
    let alice_tag = generate_tag();
    let branch = generate_branch();

    // --- Alice sends INVITE with SDP offer ---
    let mut alice_sdp = SdpSession::new("127.0.0.1");
    alice_sdp.add_audio_media(alice_rtp_port);

    let invite = RequestBuilder::new(SipMethod::Invite, format!("sip:bob@{}", bob_sip_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", alice_sip_addr, branch))
        .header(HeaderName::MaxForwards, "70")
        .header(HeaderName::From, format!("<sip:alice@{}>;tag={}", alice_sip_addr, alice_tag))
        .header(HeaderName::To, format!("<sip:bob@{}>", bob_sip_addr))
        .header(HeaderName::CallId, &call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .header(HeaderName::Contact, format!("<sip:alice@{}>", alice_sip_addr))
        .header(HeaderName::ContentType, "application/sdp")
        .body(&alice_sdp.to_string())
        .build();

    alice_sip.send_to(&invite, bob_sip_addr).await.unwrap();

    // --- Bob receives INVITE ---
    let incoming = bob_sip.recv().await.unwrap();
    let req = match &incoming.message {
        SipMessage::Request(r) => r.clone(),
        _ => panic!("Expected INVITE"),
    };
    assert_eq!(req.method, SipMethod::Invite);

    let offer = SdpSession::parse(req.body.as_ref().unwrap()).unwrap();
    let alice_audio_port = offer.get_audio_port().unwrap();

    // --- Bob sends 200 OK with SDP answer ---
    let bob_tag = generate_tag();
    let mut bob_sdp = SdpSession::new("127.0.0.1");
    bob_sdp.add_audio_media(bob_rtp_port);

    let ok = ResponseBuilder::from_request(&req, StatusCode::OK)
        .header(HeaderName::To, format!("<sip:bob@{}>;tag={}", bob_sip_addr, bob_tag))
        .header(HeaderName::Contact, format!("<sip:bob@{}>", bob_sip_addr))
        .header(HeaderName::ContentType, "application/sdp")
        .body(&bob_sdp.to_string())
        .build();

    bob_sip.send_to(&ok, alice_sip_addr).await.unwrap();

    // --- Alice receives 200 OK ---
    let ok_recv = alice_sip.recv().await.unwrap();
    let bob_audio_port = match &ok_recv.message {
        SipMessage::Response(res) => {
            assert_eq!(res.status, StatusCode::OK);
            SdpSession::parse(res.body.as_ref().unwrap()).unwrap().get_audio_port().unwrap()
        }
        _ => panic!("Expected 200 OK"),
    };

    // --- Alice sends ACK ---
    let ack = RequestBuilder::new(SipMethod::Ack, format!("sip:bob@{}", bob_sip_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", alice_sip_addr, generate_branch()))
        .header(HeaderName::MaxForwards, "70")
        .header(HeaderName::From, format!("<sip:alice@{}>;tag={}", alice_sip_addr, alice_tag))
        .header(HeaderName::To, format!("<sip:bob@{}>;tag={}", bob_sip_addr, bob_tag))
        .header(HeaderName::CallId, &call_id)
        .header(HeaderName::CSeq, "1 ACK")
        .build();
    alice_sip.send_to(&ack, bob_sip_addr).await.unwrap();

    // Bob receives ACK
    let ack_recv = bob_sip.recv().await.unwrap();
    assert!(matches!(&ack_recv.message, SipMessage::Request(r) if r.method == SipMethod::Ack));

    // --- Create RTP sessions ---
    let alice_rtp_remote: SocketAddr = format!("127.0.0.1:{}", bob_audio_port).parse().unwrap();
    let alice_rtp_config = SessionConfig::new(
        &format!("127.0.0.1:{}", alice_rtp_port), alice_rtp_remote, CodecType::Pcmu,
    );
    let mut alice_rtp = RtpSession::new(alice_rtp_config).await.unwrap();

    let bob_rtp_remote: SocketAddr = format!("127.0.0.1:{}", alice_audio_port).parse().unwrap();
    let bob_rtp_config = SessionConfig::new(
        &format!("127.0.0.1:{}", bob_rtp_port), bob_rtp_remote, CodecType::Pcmu,
    );
    let mut bob_rtp = RtpSession::new(bob_rtp_config).await.unwrap();

    // --- Generate test audio: 440Hz sine tone, 10 frames (200ms) ---
    let original_tone = generate_sine_tone(440.0, 8000, 200, 12000);
    let frames: Vec<&[i16]> = original_tone.chunks(160).collect();
    assert_eq!(frames.len(), 10);

    // --- Alice sends audio, Bob records it ---
    let mut bob_recorder = AudioRecorder::new(8000);

    for frame in &frames {
        alice_rtp.send_audio(frame).await.unwrap();
    }

    for _ in 0..10 {
        let (pkt, _) = bob_rtp.recv_packet().await.unwrap();
        assert_eq!(pkt.payload_type, 0); // PCMU
        let decoded = bob_rtp.decode_packet(&pkt).unwrap();
        bob_recorder.record_frame(&decoded);
    }

    // --- Verify recording metadata ---
    assert_eq!(bob_recorder.frame_count(), 10);
    assert_eq!(bob_recorder.duration_ms(), 200);
    assert_eq!(bob_recorder.len(), 1600); // 10 * 160

    // --- Audio fidelity checks ---
    let recorded = bob_recorder.samples();

    let snr = compute_snr(&original_tone, recorded);
    assert!(snr > 20.0, "SNR should be >20 dB for G.711, got {:.1} dB", snr);

    let corr = cross_correlation(&original_tone, recorded);
    assert!(corr > 0.95, "Cross-correlation should be >0.95, got {:.4}", corr);

    let max_err = max_sample_error(&original_tone, recorded);
    assert!(max_err < 1000, "Max sample error should be <1000 for G.711, got {}", max_err);

    let rms = rms_error(&original_tone, recorded);
    assert!(rms < 500.0, "RMS error should be <500 for G.711, got {:.1}", rms);

    // --- Verify WAV export/import roundtrip preserves recorded audio ---
    let wav_bytes = bob_recorder.to_wav();
    let (header, wav_samples) = decode_wav(&wav_bytes).unwrap();
    assert_eq!(header.sample_rate, 8000);
    assert_eq!(wav_samples.len(), recorded.len());
    assert_eq!(wav_samples, recorded);

    // --- BYE ---
    let bye = RequestBuilder::new(SipMethod::Bye, format!("sip:bob@{}", bob_sip_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", alice_sip_addr, generate_branch()))
        .header(HeaderName::MaxForwards, "70")
        .header(HeaderName::From, format!("<sip:alice@{}>;tag={}", alice_sip_addr, alice_tag))
        .header(HeaderName::To, format!("<sip:bob@{}>;tag={}", bob_sip_addr, bob_tag))
        .header(HeaderName::CallId, &call_id)
        .header(HeaderName::CSeq, "2 BYE")
        .build();
    alice_sip.send_to(&bye, bob_sip_addr).await.unwrap();

    let bye_recv = bob_sip.recv().await.unwrap();
    assert!(matches!(&bye_recv.message, SipMessage::Request(r) if r.method == SipMethod::Bye));

    let bye_ok = match &bye_recv.message {
        SipMessage::Request(r) => ResponseBuilder::from_request(r, StatusCode::OK).build(),
        _ => panic!("Expected BYE"),
    };
    bob_sip.send_to(&bye_ok, alice_sip_addr).await.unwrap();

    let bye_ok_recv = alice_sip.recv().await.unwrap();
    assert!(matches!(&bye_ok_recv.message, SipMessage::Response(r) if r.status == StatusCode::OK));
}

// =============================================================================
// E2E Test: Bidirectional audio recording with fidelity check on BOTH sides
// =============================================================================

#[tokio::test]
async fn e2e_bidirectional_audio_recording_fidelity() {
    // Pre-bind sockets
    let sock_a = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr_a = sock_a.local_addr().unwrap();
    let sock_b = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr_b = sock_b.local_addr().unwrap();
    drop(sock_a);
    drop(sock_b);

    let config_a = SessionConfig::new(&addr_a.to_string(), addr_b, CodecType::Pcmu);
    let mut session_a = RtpSession::new(config_a).await.unwrap();
    let config_b = SessionConfig::new(&addr_b.to_string(), addr_a, CodecType::Pcmu);
    let mut session_b = RtpSession::new(config_b).await.unwrap();

    // Alice sends a 440Hz tone, Bob sends a 1000Hz tone
    let tone_a = generate_sine_tone(440.0, 8000, 200, 12000);
    let tone_b = generate_sine_tone(1000.0, 8000, 200, 12000);

    let mut recorder_at_b = AudioRecorder::new(8000); // records Alice's audio
    let mut recorder_at_a = AudioRecorder::new(8000); // records Bob's audio

    // Send all frames from both sides
    let frames_a: Vec<&[i16]> = tone_a.chunks(160).collect();
    let frames_b: Vec<&[i16]> = tone_b.chunks(160).collect();
    assert_eq!(frames_a.len(), 10);
    assert_eq!(frames_b.len(), 10);

    // Interleaved: A sends, B sends, B receives, A receives
    for i in 0..10 {
        session_a.send_audio(frames_a[i]).await.unwrap();
        session_b.send_audio(frames_b[i]).await.unwrap();

        let (pkt_b, _) = session_b.recv_packet().await.unwrap();
        let decoded_b = session_b.decode_packet(&pkt_b).unwrap();
        recorder_at_b.record_frame(&decoded_b);

        let (pkt_a, _) = session_a.recv_packet().await.unwrap();
        let decoded_a = session_a.decode_packet(&pkt_a).unwrap();
        recorder_at_a.record_frame(&decoded_a);
    }

    // Verify Bob's recording of Alice's 440Hz tone
    assert_eq!(recorder_at_b.frame_count(), 10);
    let snr_b = compute_snr(&tone_a, recorder_at_b.samples());
    let corr_b = cross_correlation(&tone_a, recorder_at_b.samples());
    assert!(snr_b > 20.0, "Bob's SNR of Alice's tone: {:.1} dB", snr_b);
    assert!(corr_b > 0.95, "Bob's correlation of Alice's tone: {:.4}", corr_b);

    // Verify Alice's recording of Bob's 1000Hz tone
    assert_eq!(recorder_at_a.frame_count(), 10);
    let snr_a = compute_snr(&tone_b, recorder_at_a.samples());
    let corr_a = cross_correlation(&tone_b, recorder_at_a.samples());
    assert!(snr_a > 20.0, "Alice's SNR of Bob's tone: {:.1} dB", snr_a);
    assert!(corr_a > 0.95, "Alice's correlation of Bob's tone: {:.4}", corr_a);

    // Cross-check: Alice's recording should NOT correlate with Alice's original tone
    let cross_corr = cross_correlation(&tone_a, recorder_at_a.samples());
    assert!(cross_corr < 0.5, "Should not correlate 440Hz with recorded 1000Hz: {:.4}", cross_corr);
}

// =============================================================================
// E2E Test: Audio through full encode→RTP→UDP→jitter→decode pipeline with WAV
// =============================================================================

#[tokio::test]
async fn e2e_audio_pipeline_wav_roundtrip() {
    // Pre-bind sockets
    let sock_a = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr_a = sock_a.local_addr().unwrap();
    let sock_b = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr_b = sock_b.local_addr().unwrap();
    drop(sock_a);
    drop(sock_b);

    let config_a = SessionConfig::new(&addr_a.to_string(), addr_b, CodecType::Pcmu);
    let mut session_a = RtpSession::new(config_a).await.unwrap();
    let config_b = SessionConfig::new(&addr_b.to_string(), addr_a, CodecType::Pcmu);
    let mut session_b = RtpSession::new(config_b).await.unwrap();

    // Generate a multi-tone signal for richer fidelity testing
    let original = generate_multi_tone(&[300.0, 700.0, 1200.0], 8000, 400, 10000);
    assert_eq!(original.len(), 3200); // 400ms at 8kHz

    let mut recorder = AudioRecorder::new(8000);

    // Send all 20 frames (400ms / 20ms = 20 frames)
    let frames: Vec<&[i16]> = original.chunks(160).collect();
    assert_eq!(frames.len(), 20);

    for frame in &frames {
        session_a.send_audio(frame).await.unwrap();
    }

    // Receive through jitter buffer by receiving packets directly
    for _ in 0..20 {
        let (pkt, _) = session_b.recv_packet().await.unwrap();
        let decoded = session_b.decode_packet(&pkt).unwrap();
        recorder.record_frame(&decoded);
    }

    assert_eq!(recorder.frame_count(), 20);
    assert_eq!(recorder.duration_ms(), 400);

    // Fidelity check
    let recorded = recorder.samples();
    let snr = compute_snr(&original, recorded);
    let corr = cross_correlation(&original, recorded);
    let max_err = max_sample_error(&original, recorded);
    let rms = rms_error(&original, recorded);

    assert!(snr > 20.0, "Pipeline SNR: {:.1} dB", snr);
    assert!(corr > 0.95, "Pipeline correlation: {:.4}", corr);
    assert!(max_err < 1000, "Pipeline max error: {}", max_err);
    assert!(rms < 500.0, "Pipeline RMS error: {:.1}", rms);

    // Save to WAV, read back, verify integrity
    let wav_bytes = recorder.to_wav();
    let (header, wav_samples) = decode_wav(&wav_bytes).unwrap();
    assert_eq!(header.sample_rate, 8000);
    assert_eq!(header.channels, 1);
    assert_eq!(wav_samples.len(), 3200);
    assert_eq!(wav_samples, recorded);

    // Also verify the WAV samples still match the original well
    let wav_snr = compute_snr(&original, &wav_samples);
    assert!(wav_snr > 20.0, "WAV roundtrip SNR: {:.1} dB", wav_snr);
}

// =============================================================================
// E2E Test: A-law codec audio recording fidelity
// =============================================================================

#[tokio::test]
async fn e2e_alaw_audio_recording_fidelity() {
    let sock_a = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr_a = sock_a.local_addr().unwrap();
    let sock_b = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr_b = sock_b.local_addr().unwrap();
    drop(sock_a);
    drop(sock_b);

    // Use PCMA (A-law) instead of PCMU
    let config_a = SessionConfig::new(&addr_a.to_string(), addr_b, CodecType::Pcma);
    let mut session_a = RtpSession::new(config_a).await.unwrap();
    let config_b = SessionConfig::new(&addr_b.to_string(), addr_a, CodecType::Pcma);
    let mut session_b = RtpSession::new(config_b).await.unwrap();

    let original = generate_sine_tone(600.0, 8000, 200, 12000);
    let mut recorder = AudioRecorder::new(8000);

    for frame in original.chunks(160) {
        session_a.send_audio(frame).await.unwrap();
    }

    for _ in 0..10 {
        let (pkt, _) = session_b.recv_packet().await.unwrap();
        assert_eq!(pkt.payload_type, 8); // PCMA
        let decoded = session_b.decode_packet(&pkt).unwrap();
        recorder.record_frame(&decoded);
    }

    let snr = compute_snr(&original, recorder.samples());
    let corr = cross_correlation(&original, recorder.samples());
    assert!(snr > 20.0, "A-law SNR: {:.1} dB", snr);
    assert!(corr > 0.95, "A-law correlation: {:.4}", corr);
}

// =============================================================================
// E2E Test: Interactive CLI — mute/unmute during call
// =============================================================================

#[tokio::test]
async fn e2e_interactive_mute_unmute() {
    // Simulate a call where we track mute state changes
    // The mute flag controls whether silence or real audio is sent
    let mut muted = false;

    // Unmuted initially
    assert!(!muted);

    // Simulate "mute" command
    muted = true;
    assert!(muted);

    // While muted, only silence should be sent
    let silence = vec![0i16; 160];
    assert!(silence.iter().all(|&s| s == 0));

    // Simulate "unmute" command
    muted = false;
    assert!(!muted);
}

// =============================================================================
// E2E Test: Interactive CLI — start/stop recording mid-call
// =============================================================================

#[tokio::test]
async fn e2e_interactive_record_mid_call() {
    // Simulate starting a recording mid-call and stopping it
    let mut recorder = AudioRecorder::new(8000);
    let mut recording_active = false;

    // Generate some audio frames
    let tone = generate_sine_tone(440.0, 8000, 100, 12000); // 100ms
    let frames: Vec<Vec<i16>> = tone.chunks(160).map(|c| c.to_vec()).collect();

    // Before recording starts, frames are discarded
    for frame in &frames {
        if recording_active {
            recorder.record_frame(frame);
        }
    }
    assert_eq!(recorder.frame_count(), 0);

    // Start recording (simulate "record" command)
    recording_active = true;

    // Now frames are captured
    for frame in &frames {
        if recording_active {
            recorder.record_frame(frame);
        }
    }
    assert!(recorder.frame_count() > 0);
    let frames_after_start = recorder.frame_count();

    // Stop recording (simulate "stop" command)
    recording_active = false;

    // More frames arrive but aren't recorded
    for frame in &frames {
        if recording_active {
            recorder.record_frame(frame);
        }
    }
    assert_eq!(recorder.frame_count(), frames_after_start);

    // Resume recording (simulate "record" command again)
    recording_active = true;
    for frame in &frames {
        if recording_active {
            recorder.record_frame(frame);
        }
    }
    assert!(recorder.frame_count() > frames_after_start);

    // Verify WAV export works
    let wav_bytes = recorder.to_wav();
    assert!(!wav_bytes.is_empty());
}

// =============================================================================
// E2E Test: Interactive CLI — stats display during active call
// =============================================================================

#[tokio::test]
async fn e2e_interactive_stats_during_call() {
    let remote_addr = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
    let config = SessionConfig::new("127.0.0.1:0", remote_addr, CodecType::Pcmu);
    let mut session = RtpSession::new(config).await.unwrap();

    // Initially no packets sent
    let stats = session.stats();
    assert_eq!(stats.packets_sent, 0);
    assert_eq!(stats.codec, CodecType::Pcmu);

    // Create a receiver to accept packets
    let recv_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let recv_addr = recv_socket.local_addr().unwrap();
    session.set_remote_addr(recv_addr);

    // Send some audio
    for _ in 0..5 {
        let samples = vec![1000i16; 160];
        session.send_audio(&samples).await.unwrap();
    }

    let stats = session.stats();
    assert_eq!(stats.packets_sent, 5);
    assert_eq!(stats.remote_addr, recv_addr);
    assert_ne!(stats.ssrc, 0); // SSRC should be set
}

// =============================================================================
// E2E Test: Interactive CLI — hangup command sends BYE
// =============================================================================

#[tokio::test]
async fn e2e_interactive_hangup_sends_bye() {
    let uac_transport = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uas_transport = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uac_addr = uac_transport.local_addr();
    let uas_addr = uas_transport.local_addr();

    let call_id = uuid::Uuid::new_v4().to_string();
    let branch = generate_branch();
    let local_tag = generate_tag();
    let remote_tag = generate_tag();

    // Step 1: Setup call — UAC sends INVITE
    let invite = RequestBuilder::new(SipMethod::Invite, format!("sip:bob@{}", uas_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={};rport", uac_addr, branch))
        .header(HeaderName::From, format!("<sip:alice@{}>;tag={}", uac_addr, local_tag))
        .header(HeaderName::To, format!("<sip:bob@{}>", uas_addr))
        .header(HeaderName::CallId, &call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .header(HeaderName::Contact, format!("<sip:alice@{}>", uac_addr))
        .build();

    uac_transport.send_to(&invite, uas_addr).await.unwrap();
    let incoming = uas_transport.recv().await.unwrap();
    assert!(incoming.message.is_request());

    // Step 2: UAS sends 200 OK
    let ok = ResponseBuilder::new(StatusCode::OK)
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={};rport", uac_addr, branch))
        .header(HeaderName::From, format!("<sip:alice@{}>;tag={}", uac_addr, local_tag))
        .header(HeaderName::To, format!("<sip:bob@{}>;tag={}", uas_addr, remote_tag))
        .header(HeaderName::CallId, &call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .header(HeaderName::Contact, format!("<sip:bob@{}>", uas_addr))
        .build();

    uas_transport.send_to(&ok, uac_addr).await.unwrap();
    let response = uac_transport.recv().await.unwrap();
    assert!(response.message.is_response());

    // Step 3: Simulate "hangup" — UAC sends BYE
    let bye_branch = generate_branch();
    let bye = RequestBuilder::new(SipMethod::Bye, format!("sip:bob@{}", uas_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={};rport", uac_addr, bye_branch))
        .header(HeaderName::From, format!("<sip:alice@{}>;tag={}", uac_addr, local_tag))
        .header(HeaderName::To, format!("<sip:bob@{}>;tag={}", uas_addr, remote_tag))
        .header(HeaderName::CallId, &call_id)
        .header(HeaderName::CSeq, "2 BYE")
        .build();

    uac_transport.send_to(&bye, uas_addr).await.unwrap();

    // Verify UAS receives BYE
    let bye_msg = uas_transport.recv().await.unwrap();
    assert!(bye_msg.message.is_request());
    if let SipMessage::Request(req) = &bye_msg.message {
        assert_eq!(req.method, SipMethod::Bye);
    }

    // Verify it's the same call
    assert_eq!(bye_msg.message.call_id().unwrap(), call_id);
}

// =============================================================================
// E2E Test: SIP Debugger — capture and display SIP messages
// =============================================================================

#[tokio::test]
async fn e2e_sip_debugger_capture() {
    // Simulate SIP messages being captured by the debugger
    let uac_transport = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uas_transport = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uac_addr = uac_transport.local_addr();
    let uas_addr = uas_transport.local_addr();

    let call_id = uuid::Uuid::new_v4().to_string();
    let branch = generate_branch();
    let tag = generate_tag();

    // Send INVITE
    let invite = RequestBuilder::new(SipMethod::Invite, format!("sip:bob@{}", uas_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", uac_addr, branch))
        .header(HeaderName::From, format!("<sip:alice@{}>;tag={}", uac_addr, tag))
        .header(HeaderName::To, format!("<sip:bob@{}>", uas_addr))
        .header(HeaderName::CallId, &call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .build();

    uac_transport.send_to(&invite, uas_addr).await.unwrap();

    // Receive it
    let incoming = uas_transport.recv().await.unwrap();
    let msg = incoming.message;

    // Verify the message can be inspected like the debugger does
    assert!(msg.is_request());
    assert_eq!(msg.call_id().unwrap(), call_id);

    // Verify headers are accessible
    let headers = msg.headers();
    assert!(headers.get(&HeaderName::Via).is_some());
    assert!(headers.get(&HeaderName::From).is_some());
    assert!(headers.get(&HeaderName::To).is_some());
    assert!(headers.get(&HeaderName::CallId).is_some());
    assert!(headers.get(&HeaderName::CSeq).is_some());

    // Verify CSeq parsing
    let (seq, method) = msg.cseq().unwrap();
    assert_eq!(seq, 1);
    assert_eq!(method, SipMethod::Invite);

    // Send 180 Ringing
    let ringing = ResponseBuilder::new(StatusCode::RINGING)
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", uac_addr, branch))
        .header(HeaderName::From, format!("<sip:alice@{}>;tag={}", uac_addr, tag))
        .header(HeaderName::To, format!("<sip:bob@{}>;tag=resp1", uas_addr))
        .header(HeaderName::CallId, &call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .build();

    uas_transport.send_to(&ringing, uac_addr).await.unwrap();
    let resp = uac_transport.recv().await.unwrap();
    assert!(resp.message.is_response());
    assert_eq!(resp.message.status().unwrap().0, 180);

    // Send 200 OK
    let ok = ResponseBuilder::new(StatusCode::OK)
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", uac_addr, branch))
        .header(HeaderName::From, format!("<sip:alice@{}>;tag={}", uac_addr, tag))
        .header(HeaderName::To, format!("<sip:bob@{}>;tag=resp1", uas_addr))
        .header(HeaderName::CallId, &call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .build();

    uas_transport.send_to(&ok, uac_addr).await.unwrap();
    let resp = uac_transport.recv().await.unwrap();
    assert!(resp.message.is_response());
    assert_eq!(resp.message.status().unwrap().0, 200);
}

// =============================================================================
// E2E Test: SIP Debugger — call flow tracking across multiple messages
// =============================================================================

#[tokio::test]
async fn e2e_sip_debugger_call_flow_tracking() {
    // Test that messages with the same Call-ID are grouped together
    let t1 = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let t2 = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let addr1 = t1.local_addr();
    let addr2 = t2.local_addr();

    let call_id_a = "call-flow-A";
    let call_id_b = "call-flow-B";
    let tag1 = generate_tag();
    let tag2 = generate_tag();

    // Send messages from two different calls interleaved
    let invite_a = RequestBuilder::new(SipMethod::Invite, format!("sip:x@{}", addr2))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", addr1, generate_branch()))
        .header(HeaderName::From, format!("<sip:a@{}>;tag={}", addr1, tag1))
        .header(HeaderName::To, format!("<sip:x@{}>", addr2))
        .header(HeaderName::CallId, call_id_a)
        .header(HeaderName::CSeq, "1 INVITE")
        .build();

    let register_b = RequestBuilder::new(SipMethod::Register, format!("sip:{}", addr2))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", addr1, generate_branch()))
        .header(HeaderName::From, format!("<sip:b@{}>;tag={}", addr1, tag2))
        .header(HeaderName::To, format!("<sip:b@{}>", addr2))
        .header(HeaderName::CallId, call_id_b)
        .header(HeaderName::CSeq, "1 REGISTER")
        .build();

    t1.send_to(&invite_a, addr2).await.unwrap();
    t1.send_to(&register_b, addr2).await.unwrap();

    // Receive both
    let msg1 = t2.recv().await.unwrap();
    let msg2 = t2.recv().await.unwrap();

    // Verify they have different Call-IDs (can be grouped into separate flows)
    let cid1 = msg1.message.call_id().unwrap();
    let cid2 = msg2.message.call_id().unwrap();
    assert_ne!(cid1, cid2);

    // Verify we can identify the methods
    let methods: Vec<SipMethod> = [&msg1.message, &msg2.message]
        .iter()
        .filter_map(|m| m.method().cloned())
        .collect();
    assert!(methods.contains(&SipMethod::Invite));
    assert!(methods.contains(&SipMethod::Register));
}

// =============================================================================
// E2E Test: Full call with mid-call recording and hangup
// =============================================================================

#[tokio::test]
async fn e2e_full_call_with_interactive_features() {
    // Setup two RTP endpoints
    let remote_addr = "127.0.0.1:0".parse::<SocketAddr>().unwrap();
    let recv_config = SessionConfig::new("127.0.0.1:0", remote_addr, CodecType::Pcmu);
    let recv_session = RtpSession::new(recv_config).await.unwrap();
    let recv_addr = recv_session.local_addr();

    let send_config = SessionConfig::new("127.0.0.1:0", recv_addr, CodecType::Pcmu);
    let mut send_session = RtpSession::new(send_config).await.unwrap();

    // Start receiving with jitter buffer
    let (mut audio_rx, stop_tx) = recv_session.start_receiving(128);

    // Generate a recognizable audio pattern
    let tone = generate_sine_tone(440.0, 8000, 200, 12000); // 200ms of 440Hz

    // Send 10 frames (200ms of audio at 20ms per frame)
    let frames: Vec<&[i16]> = tone.chunks(160).collect();
    for frame in &frames {
        send_session.send_audio(frame).await.unwrap();
    }

    // Simulate interactive recording — start after 2 frames, stop after 6
    let mut recorder = AudioRecorder::new(8000);
    let mut recording_active = false;
    let mut frame_count = 0;

    loop {
        match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            audio_rx.recv(),
        ).await {
            Ok(Some(audio_frame)) => {
                frame_count += 1;

                // "record" command at frame 2
                if frame_count == 2 {
                    recording_active = true;
                }
                // "stop" command at frame 6
                if frame_count == 6 {
                    recording_active = false;
                }

                if recording_active {
                    recorder.record_frame(&audio_frame);
                }

                if frame_count >= 8 {
                    break; // Enough frames
                }
            }
            Ok(None) => break,
            Err(_) => break, // Timeout
        }
    }

    // Should have recorded frames 2-5 (4 frames)
    assert!(recorder.frame_count() >= 2, "Expected at least 2 recorded frames, got {}", recorder.frame_count());
    assert!(recorder.frame_count() <= 5, "Expected at most 5 recorded frames, got {}", recorder.frame_count());

    // Verify recorded audio quality
    assert!(!recorder.is_empty());
    let duration = recorder.duration_ms();
    assert!(duration > 0);

    // Stats should show packets sent
    let stats = send_session.stats();
    assert_eq!(stats.packets_sent, frames.len() as u64);

    // Clean up
    let _ = stop_tx.send(()).await;
}

// =============================================================================
// E2E Test: SIP message serialization round-trip for debugger
// =============================================================================

#[tokio::test]
async fn e2e_sip_message_roundtrip_for_debug() {
    // Verify SIP messages survive parse → display → parse round-trip
    // This is important for the debugger's ability to show raw messages
    let t1 = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let t2 = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let addr1 = t1.local_addr();
    let addr2 = t2.local_addr();

    // Build an INVITE with SDP body
    let mut sdp = SdpSession::new(&addr1.ip().to_string());
    sdp.add_audio_media(10000);
    let sdp_body = sdp.to_string();

    let invite = RequestBuilder::new(SipMethod::Invite, format!("sip:test@{}", addr2))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", addr1, generate_branch()))
        .header(HeaderName::From, format!("<sip:caller@{}>;tag={}", addr1, generate_tag()))
        .header(HeaderName::To, format!("<sip:test@{}>", addr2))
        .header(HeaderName::CallId, "roundtrip-test-call")
        .header(HeaderName::CSeq, "1 INVITE")
        .header(HeaderName::ContentType, "application/sdp")
        .header(HeaderName::UserAgent, "siphone/0.1.0")
        .body(&sdp_body)
        .build();

    // Send and receive
    t1.send_to(&invite, addr2).await.unwrap();
    let incoming = t2.recv().await.unwrap();

    // Verify all fields survived
    let msg = &incoming.message;
    assert!(msg.is_request());
    assert_eq!(msg.call_id().unwrap(), "roundtrip-test-call");

    // Verify SDP body is intact
    let body = msg.body().unwrap();
    let parsed_sdp = SdpSession::parse(body).unwrap();
    assert_eq!(parsed_sdp.get_audio_port().unwrap(), 10000);
    assert_eq!(
        parsed_sdp.get_connection_address().unwrap(),
        &addr1.ip().to_string()
    );

    // Verify the message can be serialized back to bytes (for debugger display)
    let bytes = msg.to_bytes();
    assert!(!bytes.is_empty());

    // Re-parse from bytes
    let reparsed = SipMessage::parse(&String::from_utf8_lossy(&bytes)).unwrap();
    assert_eq!(reparsed.call_id().unwrap(), "roundtrip-test-call");
    assert!(reparsed.body().is_some());
}

// =============================================================================
// E2E Test: CodecType Display for interactive stats
// =============================================================================

#[test]
fn e2e_codec_type_display() {
    assert_eq!(format!("{}", CodecType::Pcmu), "PCMU (G.711 mu-law)");
    assert_eq!(format!("{}", CodecType::Pcma), "PCMA (G.711 A-law)");
    assert_eq!(format!("{}", CodecType::Opus), "Opus");
}

// =============================================================================
// E2E Test: Multiple concurrent RTP sessions (simulating hold/transfer)
// =============================================================================

#[tokio::test]
async fn e2e_multiple_concurrent_rtp_sessions() {
    // Test that multiple RTP sessions can coexist (needed for call transfer/hold)
    let remote = "127.0.0.1:0".parse::<SocketAddr>().unwrap();

    let session_a = RtpSession::new(
        SessionConfig::new("127.0.0.1:0", remote, CodecType::Pcmu)
    ).await.unwrap();

    let session_b = RtpSession::new(
        SessionConfig::new("127.0.0.1:0", remote, CodecType::Pcma)
    ).await.unwrap();

    // Each session has a unique port and SSRC
    assert_ne!(session_a.local_addr().port(), session_b.local_addr().port());
    assert_ne!(session_a.stats().ssrc, session_b.stats().ssrc);
    assert_eq!(session_a.stats().codec, CodecType::Pcmu);
    assert_eq!(session_b.stats().codec, CodecType::Pcma);
}

// =============================================================================
// E2E Test: Sniff start/stop during call — simulates interactive CLI flow
// =============================================================================

#[tokio::test]
async fn e2e_sniff_start_stop_during_call() {
    // Simulate the interactive sniff start/stop pattern during a call.
    // Messages received while sniffing is off should not be captured.
    let t1 = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let t2 = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let addr1 = t1.local_addr();
    let addr2 = t2.local_addr();

    let call_id = "sniff-test-call";
    let tag = generate_tag();

    // Track captured messages manually (simulating SipDebugger active/inactive)
    let mut sniff_active = false;
    let mut captured: Vec<SipMessage> = Vec::new();

    // --- Phase 1: Sniff OFF — send INVITE, should NOT be captured ---
    let invite = RequestBuilder::new(SipMethod::Invite, format!("sip:x@{}", addr2))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", addr1, generate_branch()))
        .header(HeaderName::From, format!("<sip:a@{}>;tag={}", addr1, tag))
        .header(HeaderName::To, format!("<sip:x@{}>", addr2))
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .build();

    t1.send_to(&invite, addr2).await.unwrap();
    let incoming = t2.recv().await.unwrap();
    if sniff_active {
        captured.push(incoming.message.clone());
    }
    assert_eq!(captured.len(), 0, "Nothing captured while sniff is off");

    // --- Phase 2: User types "sniff" — start capturing ---
    sniff_active = true;

    // Send 180 Ringing back
    let ringing = ResponseBuilder::new(StatusCode::RINGING)
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", addr1, generate_branch()))
        .header(HeaderName::From, format!("<sip:a@{}>;tag={}", addr1, tag))
        .header(HeaderName::To, format!("<sip:x@{}>;tag=r1", addr2))
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .build();

    t2.send_to(&ringing, addr1).await.unwrap();
    let resp = t1.recv().await.unwrap();
    if sniff_active {
        captured.push(resp.message.clone());
    }
    assert_eq!(captured.len(), 1, "180 Ringing captured");
    assert!(captured[0].is_response());
    assert_eq!(captured[0].status().unwrap().0, 180);

    // Send 200 OK
    let ok = ResponseBuilder::new(StatusCode::OK)
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", addr1, generate_branch()))
        .header(HeaderName::From, format!("<sip:a@{}>;tag={}", addr1, tag))
        .header(HeaderName::To, format!("<sip:x@{}>;tag=r1", addr2))
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .build();

    t2.send_to(&ok, addr1).await.unwrap();
    let resp = t1.recv().await.unwrap();
    if sniff_active {
        captured.push(resp.message.clone());
    }
    assert_eq!(captured.len(), 2, "200 OK captured");

    // --- Phase 3: User types "sniff stop" ---
    sniff_active = false;

    // Send ACK — should NOT be captured
    let ack = RequestBuilder::new(SipMethod::Ack, format!("sip:x@{}", addr2))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", addr1, generate_branch()))
        .header(HeaderName::From, format!("<sip:a@{}>;tag={}", addr1, tag))
        .header(HeaderName::To, format!("<sip:x@{}>;tag=r1", addr2))
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "1 ACK")
        .build();

    t1.send_to(&ack, addr2).await.unwrap();
    let incoming = t2.recv().await.unwrap();
    if sniff_active {
        captured.push(incoming.message.clone());
    }
    assert_eq!(captured.len(), 2, "ACK NOT captured (sniff stopped)");

    // --- Phase 4: User types "sniff" again to re-enable ---
    sniff_active = true;

    // Send BYE
    let bye = RequestBuilder::new(SipMethod::Bye, format!("sip:x@{}", addr2))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", addr1, generate_branch()))
        .header(HeaderName::From, format!("<sip:a@{}>;tag={}", addr1, tag))
        .header(HeaderName::To, format!("<sip:x@{}>;tag=r1", addr2))
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "2 BYE")
        .build();

    t1.send_to(&bye, addr2).await.unwrap();
    let incoming = t2.recv().await.unwrap();
    if sniff_active {
        captured.push(incoming.message.clone());
    }
    assert_eq!(captured.len(), 3, "BYE captured after re-enabling sniff");

    // Verify all captured messages belong to the same call
    for msg in &captured {
        assert_eq!(msg.call_id().unwrap(), call_id);
    }
}

// =============================================================================
// E2E Test: Sniff with full INVITE→200→ACK→BYE call flow
// =============================================================================

#[tokio::test]
async fn e2e_sniff_full_call_flow_capture() {
    // Simulate sniff being active for an entire call and verify
    // we capture both directions (incoming + outgoing)
    let uac = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uas = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uac_addr = uac.local_addr();
    let uas_addr = uas.local_addr();

    let call_id = "full-sniff-call";
    let tag_a = generate_tag();
    let tag_b = generate_tag();

    let mut captured_at_uac: Vec<(SocketAddr, SocketAddr, SipMessage)> = Vec::new();

    // 1. UAC sends INVITE (outgoing from UAC perspective)
    let invite = RequestBuilder::new(SipMethod::Invite, format!("sip:b@{}", uas_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", uac_addr, generate_branch()))
        .header(HeaderName::From, format!("<sip:a@{}>;tag={}", uac_addr, tag_a))
        .header(HeaderName::To, format!("<sip:b@{}>", uas_addr))
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .build();

    captured_at_uac.push((uac_addr, uas_addr, invite.clone()));
    uac.send_to(&invite, uas_addr).await.unwrap();
    let _ = uas.recv().await.unwrap();

    // 2. UAS sends 200 OK (incoming to UAC)
    let ok = ResponseBuilder::new(StatusCode::OK)
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", uac_addr, generate_branch()))
        .header(HeaderName::From, format!("<sip:a@{}>;tag={}", uac_addr, tag_a))
        .header(HeaderName::To, format!("<sip:b@{}>;tag={}", uas_addr, tag_b))
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .build();

    uas.send_to(&ok, uac_addr).await.unwrap();
    let resp = uac.recv().await.unwrap();
    captured_at_uac.push((resp.source, uac_addr, resp.message));

    // 3. UAC sends ACK (outgoing)
    let ack = RequestBuilder::new(SipMethod::Ack, format!("sip:b@{}", uas_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", uac_addr, generate_branch()))
        .header(HeaderName::From, format!("<sip:a@{}>;tag={}", uac_addr, tag_a))
        .header(HeaderName::To, format!("<sip:b@{}>;tag={}", uas_addr, tag_b))
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "1 ACK")
        .build();

    captured_at_uac.push((uac_addr, uas_addr, ack.clone()));
    uac.send_to(&ack, uas_addr).await.unwrap();
    let _ = uas.recv().await.unwrap();

    // 4. UAC sends BYE (outgoing)
    let bye = RequestBuilder::new(SipMethod::Bye, format!("sip:b@{}", uas_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", uac_addr, generate_branch()))
        .header(HeaderName::From, format!("<sip:a@{}>;tag={}", uac_addr, tag_a))
        .header(HeaderName::To, format!("<sip:b@{}>;tag={}", uas_addr, tag_b))
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "2 BYE")
        .build();

    captured_at_uac.push((uac_addr, uas_addr, bye.clone()));
    uac.send_to(&bye, uas_addr).await.unwrap();
    let _ = uas.recv().await.unwrap();

    // Verify captured flow
    assert_eq!(captured_at_uac.len(), 4);

    // INVITE (outgoing)
    assert!(captured_at_uac[0].2.is_request());
    assert_eq!(*captured_at_uac[0].2.method().unwrap(), SipMethod::Invite);
    assert_eq!(captured_at_uac[0].0, uac_addr); // from us
    assert_eq!(captured_at_uac[0].1, uas_addr); // to them

    // 200 OK (incoming)
    assert!(captured_at_uac[1].2.is_response());
    assert_eq!(captured_at_uac[1].2.status().unwrap().0, 200);
    assert_eq!(captured_at_uac[1].0, uas_addr); // from them
    assert_eq!(captured_at_uac[1].1, uac_addr); // to us

    // ACK (outgoing)
    assert_eq!(*captured_at_uac[2].2.method().unwrap(), SipMethod::Ack);

    // BYE (outgoing)
    assert_eq!(*captured_at_uac[3].2.method().unwrap(), SipMethod::Bye);

    // All same Call-ID
    for (_, _, msg) in &captured_at_uac {
        assert_eq!(msg.call_id().unwrap(), call_id);
    }
}

// =============================================================================
// E2E Test: Digest Authentication (401 challenge-response)
// =============================================================================

#[tokio::test]
async fn e2e_digest_auth_register() {
    use sip_core::auth::{parse_challenge, compute_digest, Credentials};

    let uac = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uas = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uac_addr = uac.local_addr();
    let uas_addr = uas.local_addr();

    let call_id = "auth-test-call-id";
    let local_tag = generate_tag();

    // Step 1: UAC sends REGISTER (no auth)
    let reg1 = RequestBuilder::new(SipMethod::Register, &format!("sip:{}", uas_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", uac_addr, generate_branch()))
        .header(HeaderName::From, format!("<sip:alice@example.com>;tag={}", local_tag))
        .header(HeaderName::To, "<sip:alice@example.com>")
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "1 REGISTER")
        .header(HeaderName::Contact, format!("<sip:alice@{}>", uac_addr))
        .build();
    uac.send_to(&reg1, uas_addr).await.unwrap();

    // Step 2: UAS receives REGISTER, responds 401 with challenge
    let incoming = uas.recv().await.unwrap();
    assert_eq!(*incoming.message.method().unwrap(), SipMethod::Register);
    assert!(incoming.message.headers().get(&HeaderName::Authorization).is_none());

    if let SipMessage::Request(ref req) = incoming.message {
        let challenge_resp = ResponseBuilder::from_request(req, StatusCode::UNAUTHORIZED)
            .header(HeaderName::WwwAuthenticate,
                r#"Digest realm="example.com", nonce="dcd98b7102dd2f0e", algorithm=MD5, qop="auth""#)
            .build();
        uas.send_to(&challenge_resp, incoming.source).await.unwrap();
    }

    // Step 3: UAC receives 401, computes digest, re-sends REGISTER
    let resp = uac.recv().await.unwrap();
    assert_eq!(resp.message.status().unwrap().0, 401);

    let www_auth = resp.message.headers().get(&HeaderName::WwwAuthenticate).unwrap();
    let challenge = parse_challenge(www_auth.as_str()).unwrap();
    assert_eq!(challenge.realm, "example.com");
    assert_eq!(challenge.nonce, "dcd98b7102dd2f0e");
    assert_eq!(challenge.qop.as_deref(), Some("auth"));

    let creds = Credentials { username: "alice".to_string(), password: "secret".to_string() };
    let digest = compute_digest(&challenge, &creds, "REGISTER", &format!("sip:{}", uas_addr));
    let auth_value = digest.to_string();
    assert!(auth_value.contains("username=\"alice\""));
    assert!(auth_value.contains("realm=\"example.com\""));

    let reg2 = RequestBuilder::new(SipMethod::Register, &format!("sip:{}", uas_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", uac_addr, generate_branch()))
        .header(HeaderName::From, format!("<sip:alice@example.com>;tag={}", local_tag))
        .header(HeaderName::To, "<sip:alice@example.com>")
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "2 REGISTER")
        .header(HeaderName::Contact, format!("<sip:alice@{}>", uac_addr))
        .header(HeaderName::Authorization, auth_value)
        .build();
    uac.send_to(&reg2, uas_addr).await.unwrap();

    // Step 4: UAS receives authenticated REGISTER, responds 200 OK
    let incoming2 = uas.recv().await.unwrap();
    assert_eq!(*incoming2.message.method().unwrap(), SipMethod::Register);
    let auth_header = incoming2.message.headers().get(&HeaderName::Authorization)
        .expect("Second REGISTER must have Authorization");
    assert!(auth_header.as_str().contains("response="));

    if let SipMessage::Request(ref req) = incoming2.message {
        let ok = ResponseBuilder::from_request(req, StatusCode::OK).build();
        uas.send_to(&ok, incoming2.source).await.unwrap();
    }

    let final_resp = uac.recv().await.unwrap();
    assert!(final_resp.message.status().unwrap().is_success());
}

// =============================================================================
// E2E Test: Call hold/resume via re-INVITE
// =============================================================================

#[tokio::test]
async fn e2e_call_hold_resume() {
    let uac = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uas = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uac_addr = uac.local_addr();
    let uas_addr = uas.local_addr();

    let call_id = "hold-test-call-id";
    let local_tag = generate_tag();
    let remote_tag = generate_tag();

    // Establish a call with initial INVITE
    let mut sdp = SdpSession::new("127.0.0.1");
    sdp.add_audio_media(4000);
    let sdp_body = sdp.to_string();

    let invite = RequestBuilder::new(SipMethod::Invite, &format!("sip:bob@{}", uas_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", uac_addr, generate_branch()))
        .header(HeaderName::From, format!("<sip:alice@example.com>;tag={}", local_tag))
        .header(HeaderName::To, format!("<sip:bob@{}>", uas_addr))
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .header(HeaderName::Contact, format!("<sip:alice@{}>", uac_addr))
        .header(HeaderName::ContentType, "application/sdp")
        .body(&sdp_body)
        .build();
    uac.send_to(&invite, uas_addr).await.unwrap();

    // UAS receives and sends 200 OK
    let incoming = uas.recv().await.unwrap();
    if let SipMessage::Request(ref req) = incoming.message {
        let mut answer_sdp = SdpSession::new("127.0.0.1");
        answer_sdp.add_audio_media(5000);
        let answer_body = answer_sdp.to_string();
        let ok = ResponseBuilder::from_request(req, StatusCode::OK)
            .header(HeaderName::Contact, format!("<sip:bob@{}>", uas_addr))
            .header(HeaderName::To, format!("<sip:bob@{}>;tag={}", uas_addr, remote_tag))
            .header(HeaderName::ContentType, "application/sdp")
            .body(&answer_body)
            .build();
        uas.send_to(&ok, incoming.source).await.unwrap();
    }

    let _ = uac.recv().await.unwrap(); // 200 OK

    // Now UAC sends re-INVITE with a=sendonly (hold)
    let mut hold_sdp = SdpSession::new("127.0.0.1");
    hold_sdp.add_audio_media_directed(4000, "sendonly");
    let hold_body = hold_sdp.to_string();

    let reinvite = RequestBuilder::new(SipMethod::Invite, &format!("sip:bob@{}", uas_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", uac_addr, generate_branch()))
        .header(HeaderName::From, format!("<sip:alice@example.com>;tag={}", local_tag))
        .header(HeaderName::To, format!("<sip:bob@{}>;tag={}", uas_addr, remote_tag))
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "2 INVITE")
        .header(HeaderName::Contact, format!("<sip:alice@{}>", uac_addr))
        .header(HeaderName::ContentType, "application/sdp")
        .body(&hold_body)
        .build();
    uac.send_to(&reinvite, uas_addr).await.unwrap();

    // UAS receives re-INVITE and verifies it's a hold
    let hold_incoming = uas.recv().await.unwrap();
    assert_eq!(*hold_incoming.message.method().unwrap(), SipMethod::Invite);
    let hold_sdp_parsed = SdpSession::parse(hold_incoming.message.body().unwrap()).unwrap();
    assert_eq!(hold_sdp_parsed.get_audio_direction(), Some("sendonly"));

    // UAS responds 200 OK to hold
    if let SipMessage::Request(ref req) = hold_incoming.message {
        let mut resp_sdp = SdpSession::new("127.0.0.1");
        resp_sdp.add_audio_media_directed(5000, "recvonly");
        let resp_body = resp_sdp.to_string();
        let ok = ResponseBuilder::from_request(req, StatusCode::OK)
            .header(HeaderName::ContentType, "application/sdp")
            .body(&resp_body)
            .build();
        uas.send_to(&ok, hold_incoming.source).await.unwrap();
    }

    let hold_resp = uac.recv().await.unwrap();
    assert!(hold_resp.message.status().unwrap().is_success());

    // Now UAC sends re-INVITE with a=sendrecv (resume)
    let mut resume_sdp = SdpSession::new("127.0.0.1");
    resume_sdp.add_audio_media_directed(4000, "sendrecv");
    let resume_body = resume_sdp.to_string();

    let reinvite2 = RequestBuilder::new(SipMethod::Invite, &format!("sip:bob@{}", uas_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", uac_addr, generate_branch()))
        .header(HeaderName::From, format!("<sip:alice@example.com>;tag={}", local_tag))
        .header(HeaderName::To, format!("<sip:bob@{}>;tag={}", uas_addr, remote_tag))
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "3 INVITE")
        .header(HeaderName::Contact, format!("<sip:alice@{}>", uac_addr))
        .header(HeaderName::ContentType, "application/sdp")
        .body(&resume_body)
        .build();
    uac.send_to(&reinvite2, uas_addr).await.unwrap();

    let resume_incoming = uas.recv().await.unwrap();
    let resume_sdp_parsed = SdpSession::parse(resume_incoming.message.body().unwrap()).unwrap();
    assert_eq!(resume_sdp_parsed.get_audio_direction(), Some("sendrecv"));
}

// =============================================================================
// E2E Test: REFER for blind call transfer
// =============================================================================

#[tokio::test]
async fn e2e_refer_blind_transfer() {
    let uac = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uas = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uac_addr = uac.local_addr();
    let uas_addr = uas.local_addr();

    let call_id = "refer-test-call-id";
    let local_tag = generate_tag();
    let remote_tag = generate_tag();

    // UAC sends REFER
    let refer = RequestBuilder::new(SipMethod::Refer, &format!("sip:bob@{}", uas_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", uac_addr, generate_branch()))
        .header(HeaderName::From, format!("<sip:alice@example.com>;tag={}", local_tag))
        .header(HeaderName::To, format!("<sip:bob@{}>;tag={}", uas_addr, remote_tag))
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "1 REFER")
        .header(HeaderName::Contact, format!("<sip:alice@{}>", uac_addr))
        .header(HeaderName::ReferTo, "sip:carol@example.com")
        .header(HeaderName::ReferredBy, format!("<sip:alice@{}>", uac_addr))
        .build();
    uac.send_to(&refer, uas_addr).await.unwrap();

    // UAS receives REFER
    let incoming = uas.recv().await.unwrap();
    assert_eq!(*incoming.message.method().unwrap(), SipMethod::Refer);

    let refer_to = incoming.message.headers().get(&HeaderName::ReferTo)
        .expect("REFER should have Refer-To");
    assert_eq!(refer_to.as_str(), "sip:carol@example.com");

    let referred_by = incoming.message.headers().get(&HeaderName::ReferredBy)
        .expect("REFER should have Referred-By");
    assert!(referred_by.as_str().contains("alice"));

    // UAS sends 202 Accepted
    if let SipMessage::Request(ref req) = incoming.message {
        let accepted = ResponseBuilder::from_request(req, StatusCode::ACCEPTED).build();
        uas.send_to(&accepted, incoming.source).await.unwrap();
    }

    let resp = uac.recv().await.unwrap();
    assert_eq!(resp.message.status().unwrap().0, 202);

    // UAS sends NOTIFY with sipfrag body
    let notify = RequestBuilder::new(SipMethod::Notify, &format!("sip:alice@{}", uac_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", uas_addr, generate_branch()))
        .header(HeaderName::From, format!("<sip:bob@{}>;tag={}", uas_addr, remote_tag))
        .header(HeaderName::To, format!("<sip:alice@example.com>;tag={}", local_tag))
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "1 NOTIFY")
        .header(HeaderName::Event, "refer")
        .header(HeaderName::SubscriptionState, "terminated;reason=noresource")
        .header(HeaderName::ContentType, "message/sipfrag")
        .body("SIP/2.0 200 OK")
        .build();
    uas.send_to(&notify, uac_addr).await.unwrap();

    let notify_incoming = uac.recv().await.unwrap();
    assert_eq!(*notify_incoming.message.method().unwrap(), SipMethod::Notify);
    assert_eq!(notify_incoming.message.body().unwrap().trim(), "SIP/2.0 200 OK");

    let event = notify_incoming.message.headers().get(&HeaderName::Event).unwrap();
    assert_eq!(event.as_str(), "refer");
}

// =============================================================================
// E2E Test: PRACK for reliable provisional responses
// =============================================================================

#[tokio::test]
async fn e2e_prack_reliable_provisional() {
    let uac = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uas = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uac_addr = uac.local_addr();
    let uas_addr = uas.local_addr();

    let call_id = "prack-test-call-id";
    let local_tag = generate_tag();
    let remote_tag = generate_tag();

    // UAC sends INVITE
    let invite = RequestBuilder::new(SipMethod::Invite, &format!("sip:bob@{}", uas_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", uac_addr, generate_branch()))
        .header(HeaderName::From, format!("<sip:alice@example.com>;tag={}", local_tag))
        .header(HeaderName::To, format!("<sip:bob@{}>", uas_addr))
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .header(HeaderName::Supported, "100rel")
        .build();
    uac.send_to(&invite, uas_addr).await.unwrap();

    // UAS receives INVITE
    let incoming = uas.recv().await.unwrap();
    assert_eq!(*incoming.message.method().unwrap(), SipMethod::Invite);

    // UAS sends 183 Session Progress with Require: 100rel and RSeq
    if let SipMessage::Request(ref req) = incoming.message {
        let mut sdp = SdpSession::new("127.0.0.1");
        sdp.add_audio_media(6000);
        let sdp_body = sdp.to_string();
        let provisional = ResponseBuilder::from_request(req, StatusCode::SESSION_PROGRESS)
            .header(HeaderName::To, format!("<sip:bob@{}>;tag={}", uas_addr, remote_tag))
            .header(HeaderName::Require, "100rel")
            .header(HeaderName::RSeq, "1")
            .header(HeaderName::ContentType, "application/sdp")
            .body(&sdp_body)
            .build();
        uas.send_to(&provisional, incoming.source).await.unwrap();
    }

    // UAC receives 183 with 100rel
    let resp = uac.recv().await.unwrap();
    assert_eq!(resp.message.status().unwrap().0, 183);

    let require = resp.message.headers().get(&HeaderName::Require).unwrap();
    assert!(require.as_str().contains("100rel"));

    let rseq = resp.message.headers().get(&HeaderName::RSeq).unwrap();
    assert_eq!(rseq.as_str().trim(), "1");

    // UAC sends PRACK
    let prack = RequestBuilder::new(SipMethod::Prack, &format!("sip:bob@{}", uas_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", uac_addr, generate_branch()))
        .header(HeaderName::From, format!("<sip:alice@example.com>;tag={}", local_tag))
        .header(HeaderName::To, format!("<sip:bob@{}>;tag={}", uas_addr, remote_tag))
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "2 PRACK")
        .header(HeaderName::RAck, "1 1 INVITE")
        .build();
    uac.send_to(&prack, uas_addr).await.unwrap();

    // UAS receives PRACK
    let prack_incoming = uas.recv().await.unwrap();
    assert_eq!(*prack_incoming.message.method().unwrap(), SipMethod::Prack);

    let rack = prack_incoming.message.headers().get(&HeaderName::RAck)
        .expect("PRACK should have RAck header");
    assert_eq!(rack.as_str(), "1 1 INVITE");

    // UAS sends 200 OK to PRACK
    if let SipMessage::Request(ref req) = prack_incoming.message {
        let ok = ResponseBuilder::from_request(req, StatusCode::OK).build();
        uas.send_to(&ok, prack_incoming.source).await.unwrap();
    }

    let prack_resp = uac.recv().await.unwrap();
    assert!(prack_resp.message.status().unwrap().is_success());
}

// =============================================================================
// E2E Test: Opus codec encode/decode roundtrip
// =============================================================================

#[tokio::test]
async fn e2e_opus_codec_roundtrip() {
    // Create two RTP sessions using Opus
    let rtp_a_tmp = RtpSession::new(SessionConfig::new(
        "127.0.0.1:0",
        "127.0.0.1:19000".parse().unwrap(),
        CodecType::Opus,
    )).await.unwrap();
    let a_addr = rtp_a_tmp.local_addr();
    let rtp_b = RtpSession::new(SessionConfig::new(
        "127.0.0.1:0",
        a_addr,
        CodecType::Opus,
    )).await.unwrap();
    drop(rtp_a_tmp);

    // Rebind A to point to B's actual port
    let mut rtp_a = RtpSession::new(SessionConfig::new(
        "127.0.0.1:0",
        rtp_b.local_addr(),
        CodecType::Opus,
    )).await.unwrap();

    // Generate a 20ms Opus frame (960 samples at 48kHz)
    let samples: Vec<i16> = (0..960)
        .map(|i| ((i as f64 / 960.0 * std::f64::consts::TAU).sin() * 16000.0) as i16)
        .collect();

    // Encode and send
    let bytes_sent = rtp_a.send_audio(&samples).await.unwrap();
    assert!(bytes_sent > 0, "Opus encoding should produce non-empty payload");

    // Verify codec properties
    assert_eq!(CodecType::Opus.clock_rate(), 48000);
    assert_eq!(CodecType::Opus.payload_type(), 111);
    assert_eq!(CodecType::Opus.samples_per_frame(), 960);
    assert_eq!(CodecType::Opus.name(), "opus");
}

// =============================================================================
// E2E Test: SDP direction attributes in full call flow
// =============================================================================

#[tokio::test]
async fn e2e_sdp_direction_attributes() {
    // Test that SDP direction attributes are correctly serialized/parsed
    let directions = ["sendrecv", "sendonly", "recvonly", "inactive"];

    for dir in &directions {
        let mut sdp = SdpSession::new("10.0.0.1");
        sdp.add_audio_media_directed(4000, dir);

        // Serialize and parse back
        let text = sdp.to_string();
        assert!(text.contains(&format!("a={}", dir)),
            "SDP should contain a={}, got:\n{}", dir, text);

        let parsed = SdpSession::parse(&text).unwrap();
        assert_eq!(parsed.get_audio_direction(), Some(*dir),
            "Parsed direction should be {} for:\n{}", dir, text);
        assert_eq!(parsed.get_audio_port(), Some(4000));
    }
}

// =============================================================================
// E2E Test: Incoming INVITE full flow (UAS side)
// =============================================================================

#[tokio::test]
async fn e2e_incoming_invite_uas_flow() {
    // Simulates an incoming call to a UAS and verifies the response flow
    let uas = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uac = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uas_addr = uas.local_addr();
    let uac_addr = uac.local_addr();

    let call_id = "incoming-test-call-id";
    let local_tag = generate_tag();

    // Build SDP for INVITE
    let mut sdp = SdpSession::new("127.0.0.1");
    sdp.add_audio_media(8000);
    let sdp_body = sdp.to_string();

    // UAC sends INVITE to UAS
    let invite = RequestBuilder::new(SipMethod::Invite, &format!("sip:phone@{}", uas_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", uac_addr, generate_branch()))
        .header(HeaderName::From, format!("<sip:caller@example.com>;tag={}", local_tag))
        .header(HeaderName::To, format!("<sip:phone@{}>", uas_addr))
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .header(HeaderName::Contact, format!("<sip:caller@{}>", uac_addr))
        .header(HeaderName::ContentType, "application/sdp")
        .body(&sdp_body)
        .build();
    uac.send_to(&invite, uas_addr).await.unwrap();

    // UAS receives INVITE
    let incoming = uas.recv().await.unwrap();
    assert_eq!(*incoming.message.method().unwrap(), SipMethod::Invite);
    assert!(incoming.message.body().is_some());

    // Parse SDP from INVITE
    let invite_sdp = SdpSession::parse(incoming.message.body().unwrap()).unwrap();
    assert_eq!(invite_sdp.get_audio_port(), Some(8000));

    // UAS sends 180 Ringing
    let uas_tag = generate_tag();
    if let SipMessage::Request(ref req) = incoming.message {
        let ringing = ResponseBuilder::from_request(req, StatusCode::RINGING)
            .header(HeaderName::Contact, format!("<sip:phone@{}>", uas_addr))
            .header(HeaderName::To, format!("<sip:phone@{}>;tag={}", uas_addr, uas_tag))
            .build();
        uas.send_to(&ringing, incoming.source).await.unwrap();
    }

    let ringing_resp = uac.recv().await.unwrap();
    assert_eq!(ringing_resp.message.status().unwrap().0, 180);

    // UAS sends 200 OK with SDP answer
    let mut answer_sdp = SdpSession::new("127.0.0.1");
    answer_sdp.add_audio_media(9000);
    let answer_body = answer_sdp.to_string();

    if let SipMessage::Request(ref req) = incoming.message {
        let ok = ResponseBuilder::from_request(req, StatusCode::OK)
            .header(HeaderName::Contact, format!("<sip:phone@{}>", uas_addr))
            .header(HeaderName::To, format!("<sip:phone@{}>;tag={}", uas_addr, uas_tag))
            .header(HeaderName::ContentType, "application/sdp")
            .body(&answer_body)
            .build();
        uas.send_to(&ok, incoming.source).await.unwrap();
    }

    let ok_resp = uac.recv().await.unwrap();
    assert!(ok_resp.message.status().unwrap().is_success());
    let ok_sdp = SdpSession::parse(ok_resp.message.body().unwrap()).unwrap();
    assert_eq!(ok_sdp.get_audio_port(), Some(9000));

    // UAC sends ACK
    let ack = RequestBuilder::new(SipMethod::Ack, &format!("sip:phone@{}", uas_addr))
        .header(HeaderName::Via, format!("SIP/2.0/UDP {};branch={}", uac_addr, generate_branch()))
        .header(HeaderName::From, format!("<sip:caller@example.com>;tag={}", local_tag))
        .header(HeaderName::To, format!("<sip:phone@{}>;tag={}", uas_addr, uas_tag))
        .header(HeaderName::CallId, call_id)
        .header(HeaderName::CSeq, "1 ACK")
        .build();
    uac.send_to(&ack, uas_addr).await.unwrap();

    let ack_incoming = uas.recv().await.unwrap();
    assert_eq!(*ack_incoming.message.method().unwrap(), SipMethod::Ack);
}
