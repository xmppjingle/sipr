//! AMR-WB SDP + RTP e2e: selectable octet-align and real audio quality.

use rtp_core::amrwb::OctetAlign as RtpOa;
use rtp_core::codec::{CodecPipeline, CodecType};
use rtp_core::session::{RtpSession, SessionConfig};
use rtp_core::wav::{compute_snr, cross_correlation, generate_sine_tone};
use sip_core::header::{generate_branch, generate_tag, HeaderName};
use sip_core::message::{RequestBuilder, ResponseBuilder, SipMethod, StatusCode};
use sip_core::sdp::{AudioOffer, OctetAlign, SdpSession};
use sip_core::transport::SipTransport;

fn tone_frame() -> Vec<i16> {
    generate_sine_tone(700.0, 16000, 20, 14000)
}

#[test]
fn e2e_amrwb_sdp_oa1_oa0_omitted() {
    for (oa, must, must_not) in [
        (OctetAlign::One, Some("octet-align=1"), Some("octet-align=0")),
        (OctetAlign::Zero, Some("octet-align=0"), Some("octet-align=1")),
        (OctetAlign::Omitted, None, Some("octet-align=")),
    ] {
        let mut sdp = SdpSession::new("127.0.0.1");
        sdp.add_audio_media_offer(5072, &AudioOffer::amrwb_then_g711(oa));
        let wire = sdp.to_string();
        assert!(wire.contains("a=rtpmap:102 AMR-WB/16000"), "{wire}");
        if let Some(tok) = must {
            assert!(wire.contains(tok), "missing {tok} in {wire}");
        }
        if let Some(tok) = must_not {
            if oa == OctetAlign::Omitted {
                assert!(!wire.contains(tok), "unexpected {tok} in {wire}");
            } else {
                assert!(!wire.contains(tok), "unexpected {tok} in {wire}");
            }
        }
        assert_eq!(sdp.amr_wb_octet_align(), Some(oa));
        let parsed = SdpSession::parse(&wire).unwrap();
        assert_eq!(parsed.amr_wb_octet_align(), Some(oa));
        assert_eq!(parsed.audio_codec_names()[0], "AMR-WB");
    }
}

#[test]
fn e2e_amrwb_answer_copies_offer_oa() {
    let mut offer = SdpSession::new("10.0.0.1");
    offer.add_audio_media_offer(4000, &AudioOffer::amrwb_then_g711(OctetAlign::Zero));
    let answer = AudioOffer::answer_from_remote(&offer);
    assert_eq!(answer.primary_name(), Some("AMR-WB"));
    assert_eq!(answer.octet_align(), OctetAlign::Zero);
}

async fn rtp_loopback(oa: RtpOa) -> (f64, f64, usize) {
    let codec = CodecType::AmrWb;
    let sender_placeholder = RtpSession::new(
        SessionConfig::new("127.0.0.1:0", "127.0.0.1:0".parse().unwrap(), codec).with_octet_align(oa),
    )
    .await
    .unwrap();
    let sender_addr = sender_placeholder.local_addr();
    drop(sender_placeholder);

    let receiver_cfg =
        SessionConfig::new("127.0.0.1:0", sender_addr, codec).with_octet_align(oa);
    let mut receiver = RtpSession::new(receiver_cfg).await.unwrap();
    let receiver_addr = receiver.local_addr();

    let sender_cfg =
        SessionConfig::new("127.0.0.1:0", receiver_addr, codec).with_octet_align(oa);
    let mut sender = RtpSession::new(sender_cfg).await.unwrap();

    let mut original = Vec::new();
    let mut decoded = Vec::new();
    for i in 0..40 {
        let frame = generate_sine_tone(700.0, 16000, 20, 14000);
        let _ = i;
        original.extend_from_slice(&frame);
        sender.send_audio(&frame).await.unwrap();
        let (pkt, _) = receiver.recv_packet().await.unwrap();
        assert_eq!(pkt.payload_type, 102);
        decoded.extend_from_slice(&receiver.decode_packet(&pkt).unwrap());
    }
    let mut best = (f64::NEG_INFINITY, 0.0);
    for lag in 0..=640 {
        if decoded.len() <= lag + 1600 {
            continue;
        }
        let n = original.len().min(decoded.len() - lag);
        let corr = cross_correlation(&original[..n], &decoded[lag..lag + n]);
        if corr > best.0 {
            best = (corr, compute_snr(&original[..n], &decoded[lag..lag + n]));
        }
    }
    (best.1, best.0, original.len())
}

#[tokio::test]
async fn e2e_amrwb_rtp_oa1_audio_quality() {
    let (snr, corr, n) = rtp_loopback(RtpOa::One).await;
    println!("AMR-WB OA=1 loopback SNR={snr:.1} dB corr={corr:.4} samples={n}");
    assert!(n >= 320 * 40);
    assert!(snr > 20.0, "OA=1 SNR {snr:.1}");
    assert!(corr > 0.95, "OA=1 corr {corr:.4}");
}

#[tokio::test]
async fn e2e_amrwb_rtp_oa0_audio_quality() {
    let (snr, corr, n) = rtp_loopback(RtpOa::Zero).await;
    println!("AMR-WB OA=0 loopback SNR={snr:.1} dB corr={corr:.4} samples={n}");
    assert!(n >= 320 * 40);
    assert!(snr > 20.0, "OA=0 SNR {snr:.1}");
    assert!(corr > 0.95, "OA=0 corr {corr:.4}");
}

#[tokio::test]
async fn e2e_amrwb_rtp_omitted_matches_oa0() {
    let (snr0, corr0, _) = rtp_loopback(RtpOa::Zero).await;
    let (snr_omit, corr_omit, _) = rtp_loopback(RtpOa::Omitted).await;
    assert!((snr0 - snr_omit).abs() < 3.0, "omitted SNR diverged from OA=0");
    assert!((corr0 - corr_omit).abs() < 0.05);
}

#[test]
fn e2e_amrwb_pipeline_oa_mismatch_is_detectable() {
    let pcm = tone_frame();
    let mut enc = CodecPipeline::with_octet_align(CodecType::AmrWb, RtpOa::One);
    let packed = enc.encode(&pcm).unwrap();
    let mut dec0 = CodecPipeline::with_octet_align(CodecType::AmrWb, RtpOa::Zero);
    match dec0.decode(&packed) {
        Err(_) => {}
        Ok(out) => {
            let corr = cross_correlation(&pcm, &out);
            assert!(corr < 0.5, "OA mismatch still correlated {corr:.4}");
        }
    }
}

#[tokio::test]
async fn e2e_amrwb_sip_invite_answer_rtp() {
    let uac = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uas = SipTransport::bind("127.0.0.1:0").await.unwrap();
    let uac_addr = uac.local_addr();
    let uas_addr = uas.local_addr();

    let caller_rtp = RtpSession::new(
        SessionConfig::new(
            "127.0.0.1:0",
            "127.0.0.1:0".parse().unwrap(),
            CodecType::AmrWb,
        )
        .with_octet_align(RtpOa::One),
    )
    .await
    .unwrap();
    let caller_rtp_port = caller_rtp.local_addr().port();

    let mut offer = SdpSession::new("127.0.0.1");
    offer.add_audio_media_offer(caller_rtp_port, &AudioOffer::amrwb_only(OctetAlign::One));
    let offer_body = offer.to_string();
    assert!(offer_body.contains("octet-align=1"));

    let call_id = uuid::Uuid::new_v4().to_string();
    let tag = generate_tag();
    let invite = RequestBuilder::new(SipMethod::Invite, format!("sip:bob@{}", uas_addr))
        .header(
            HeaderName::Via,
            format!("SIP/2.0/UDP {};branch={}", uac_addr, generate_branch()),
        )
        .header(HeaderName::From, format!("<sip:alice@{}>;tag={}", uac_addr, tag))
        .header(HeaderName::To, format!("<sip:bob@{}>", uas_addr))
        .header(HeaderName::CallId, &call_id)
        .header(HeaderName::CSeq, "1 INVITE")
        .header(HeaderName::ContentType, "application/sdp")
        .body(&offer_body)
        .build();

    uac.send_to(&invite, uas_addr).await.unwrap();
    let incoming = uas.recv().await.unwrap();
    let remote_sdp = SdpSession::parse(incoming.message.body().unwrap()).unwrap();
    assert_eq!(remote_sdp.amr_wb_octet_align(), Some(OctetAlign::One));
    assert_eq!(remote_sdp.audio_codec_names()[0], "AMR-WB");

    let answer_offer = AudioOffer::answer_from_remote(&remote_sdp);
    assert_eq!(answer_offer.octet_align(), OctetAlign::One);

    let mut callee_rtp = RtpSession::new(
        SessionConfig::new("127.0.0.1:0", caller_rtp.local_addr(), CodecType::AmrWb)
            .with_octet_align(RtpOa::One),
    )
    .await
    .unwrap();
    let callee_port = callee_rtp.local_addr().port();

    let mut answer = SdpSession::new("127.0.0.1");
    answer.add_audio_media_offer(callee_port, &answer_offer);
    let answer_body = answer.to_string();
    assert!(answer_body.contains("octet-align=1"));
    assert!(!answer_body.contains("PCMU"));

    if let sip_core::message::SipMessage::Request(ref req) = incoming.message {
        let ok = ResponseBuilder::from_request(req, StatusCode::OK)
            .header(HeaderName::ContentType, "application/sdp")
            .body(&answer_body)
            .build();
        uas.send_to(&ok, uac_addr).await.unwrap();
    }
    let resp = uac.recv().await.unwrap();
    assert_eq!(resp.message.status().unwrap(), &StatusCode::OK);
    let answered = SdpSession::parse(resp.message.body().unwrap()).unwrap();
    assert_eq!(answered.amr_wb_octet_align(), Some(OctetAlign::One));

    let mut caller_rtp = RtpSession::new(
        SessionConfig::new("127.0.0.1:0", callee_rtp.local_addr(), CodecType::AmrWb)
            .with_octet_align(RtpOa::One),
    )
    .await
    .unwrap();

    let mut original = Vec::new();
    let mut decoded = Vec::new();
    for _ in 0..40 {
        let frame = tone_frame();
        original.extend_from_slice(&frame);
        caller_rtp.send_audio(&frame).await.unwrap();
        let (pkt, _) = callee_rtp.recv_packet().await.unwrap();
        decoded.extend_from_slice(&callee_rtp.decode_packet(&pkt).unwrap());
    }
    let mut best = (f64::NEG_INFINITY, 0.0);
    for lag in 0..=640 {
        if decoded.len() <= lag + 1600 {
            continue;
        }
        let n = original.len().min(decoded.len() - lag);
        let corr = cross_correlation(&original[..n], &decoded[lag..lag + n]);
        if corr > best.0 {
            best = (corr, compute_snr(&original[..n], &decoded[lag..lag + n]));
        }
    }
    let (corr, snr) = best;
    println!("AMR-WB SIP+RTP SNR={snr:.1} dB corr={corr:.4}");
    assert!(snr > 20.0, "SIP+RTP SNR {snr:.1}");
    assert!(corr > 0.95, "SIP+RTP corr {corr:.4}");
}
