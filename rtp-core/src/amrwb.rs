//! AMR-WB (G.722.2) RTP payload: real 3GPP encoder/decoder + RFC 4867 packing.
//!
//! Speech bits come from `rvoip-codec-core` (bit-exact vs the 3GPP reference).
//! RTP wrapping is RFC 4867 octet-aligned (`octet-align=1`) or bandwidth-efficient
//! (`octet-align=0` or omitted; omitted packs as 0).

use codec_core::codecs::amr::{
    AmrCodec, AmrFrameType, AmrPacket, AmrPayloadCodec, AmrPayloadConfig, AmrPayloadFrame,
    AmrVariant,
};
use codec_core::types::{AudioCodec, CodecConfig};

pub const SAMPLES_PER_FRAME: usize = 320; // 16000 Hz * 20 ms
pub const PAYLOAD_TYPE: u8 = 102;
/// Encoder mode 8 = 23.85 kbit/s (highest AMR-WB rate).
pub const MODE_INDEX: u8 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OctetAlign {
    /// `octet-align=1`
    #[default]
    One,
    /// `octet-align=0`
    Zero,
    /// Parameter omitted (RFC 4867 default = bandwidth-efficient / 0)
    Omitted,
}

impl OctetAlign {
    /// RTP packing: omitted is bandwidth-efficient.
    pub fn packing(self) -> OctetAlign {
        match self {
            OctetAlign::Omitted => OctetAlign::Zero,
            other => other,
        }
    }

    pub fn is_octet_aligned(self) -> bool {
        matches!(self.packing(), OctetAlign::One)
    }

    pub fn parse_token(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "1" | "one" | "oa1" | "octet" | "octet-aligned" => Ok(OctetAlign::One),
            "0" | "zero" | "oa0" | "be" | "bandwidth-efficient" => Ok(OctetAlign::Zero),
            "omit" | "omitted" | "none" | "rfc" => Ok(OctetAlign::Omitted),
            other => Err(format!(
                "Unknown octet-align '{}'. Use 1, 0, or omit",
                other
            )),
        }
    }
}

impl std::fmt::Display for OctetAlign {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OctetAlign::One => write!(f, "1"),
            OctetAlign::Zero => write!(f, "0"),
            OctetAlign::Omitted => write!(f, "omitted"),
        }
    }
}

/// Full-duplex AMR-WB encoder + RFC 4867 RTP framer for one negotiated OA.
pub struct AmrWbRtp {
    codec: AmrCodec,
    payload: AmrPayloadCodec,
}

impl AmrWbRtp {
    pub fn new(octet_align: OctetAlign) -> Result<Self, String> {
        let aligned = octet_align.is_octet_aligned();
        let config = CodecConfig::amr_wb()
            .with_amr_octet_align(aligned)
            .with_amr_mode_set(&[MODE_INDEX])
            .with_amr_dtx(false);
        let codec = AmrCodec::new(&config).map_err(|e| e.to_string())?;
        let payload_cfg = if aligned {
            AmrPayloadConfig::octet_aligned(AmrVariant::WideBand)
        } else {
            AmrPayloadConfig::bandwidth_efficient(AmrVariant::WideBand)
        };
        let payload = AmrPayloadCodec::new(payload_cfg).map_err(|e| e.to_string())?;
        Ok(Self { codec, payload })
    }

    pub fn encode_rtp(&mut self, pcm_samples: &[i16]) -> Result<Vec<u8>, String> {
        let samples = match pcm_samples.len() {
            SAMPLES_PER_FRAME => pcm_samples.to_vec(),
            160 => upsample_2x(pcm_samples),
            other => {
                return Err(format!(
                    "AMR-WB encode needs {} samples (16 kHz / 20 ms), got {}",
                    SAMPLES_PER_FRAME, other
                ))
            }
        };
        let speech = AudioCodec::encode(&mut self.codec, &samples).map_err(|e| e.to_string())?;
        let frame = AmrPayloadFrame::new(AmrFrameType::Speech(self.codec.mode()), true, speech)
            .map_err(|e| e.to_string())?;
        self.payload
            .pack(&AmrPacket::single(frame))
            .map_err(|e| e.to_string())
    }

    pub fn decode_rtp(&mut self, payload: &[u8]) -> Result<Vec<i16>, String> {
        let packet = self.payload.unpack(payload).map_err(|e| e.to_string())?;
        let Some(frame) = packet.frames.first() else {
            return Err("AMR-WB RTP payload contained no frames".into());
        };
        if frame.data.is_empty() {
            return Ok(vec![0i16; SAMPLES_PER_FRAME]);
        }
        AudioCodec::decode(&mut self.codec, &frame.data).map_err(|e| e.to_string())
    }

    pub fn silence_rtp(&mut self) -> Vec<u8> {
        self.encode_rtp(&[0i16; SAMPLES_PER_FRAME])
            .unwrap_or_else(|_| vec![0u8; 2])
    }
}

fn upsample_2x(narrow: &[i16]) -> Vec<i16> {
    let mut out = Vec::with_capacity(narrow.len() * 2);
    for (i, &s) in narrow.iter().enumerate() {
        out.push(s);
        let next = narrow.get(i + 1).copied().unwrap_or(s);
        out.push(((s as i32 + next as i32) / 2) as i16);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wav::{compute_snr, cross_correlation, generate_sine_tone};

    fn tone_stream(frames: usize) -> Vec<i16> {
        generate_sine_tone(700.0, 16000, (frames * 20) as u32, 14000)
    }

    fn aligned_quality(orig: &[i16], dec: &[i16]) -> (f64, f64, isize) {
        let mut best = (f64::NEG_INFINITY, 0isize);
        for lag in 0isize..=640 {
            let l = lag as usize;
            if dec.len() <= l + 1600 {
                continue;
            }
            let n = orig.len().min(dec.len() - l);
            let corr = cross_correlation(&orig[..n], &dec[l..l + n]);
            if corr > best.0 {
                best = (corr, lag);
            }
        }
        let l = best.1 as usize;
        let n = orig.len().min(dec.len().saturating_sub(l));
        let snr = compute_snr(&orig[..n], &dec[l..l + n]);
        (snr, best.0, best.1)
    }

    fn roundtrip(oa: OctetAlign, frames: usize) -> (Vec<u8>, Vec<i16>, Vec<i16>) {
        let pcm = tone_stream(frames);
        let mut enc = AmrWbRtp::new(oa).unwrap();
        let mut dec = AmrWbRtp::new(oa).unwrap();
        let mut decoded = Vec::new();
        let mut first_payload = Vec::new();
        for frame in pcm.chunks(SAMPLES_PER_FRAME) {
            let rtp = enc.encode_rtp(frame).unwrap();
            if first_payload.is_empty() {
                first_payload = rtp.clone();
            }
            decoded.extend_from_slice(&dec.decode_rtp(&rtp).unwrap());
        }
        (first_payload, pcm, decoded)
    }

    #[test]
    fn oa1_roundtrip_is_audible() {
        let (rtp, pcm, out) = roundtrip(OctetAlign::One, 40);
        assert!(rtp.len() > 2, "OA=1 payload too short: {}", rtp.len());
        assert_eq!(rtp[0], 0xF0, "OA=1 CMR byte");
        assert_eq!(rtp[1] & 0xFC, 0x44, "OA=1 TOC FT=8 Q=1");
        assert_eq!(out.len(), pcm.len());
        let (snr, corr, lag) = aligned_quality(&pcm, &out);
        println!("OA=1 SNR={snr:.1} corr={corr:.4} lag={lag}");
        let energy: i64 = out.iter().map(|s| (*s as i32).abs() as i64).sum();
        assert!(energy > 10_000, "decoded OA=1 audio was silence, energy={energy}");
        assert!(snr > 20.0, "OA=1 SNR {snr:.1} dB too low");
        assert!(corr > 0.95, "OA=1 correlation {corr:.4} too low");
    }

    #[test]
    fn oa0_roundtrip_is_audible() {
        let (rtp, pcm, out) = roundtrip(OctetAlign::Zero, 40);
        assert_ne!(rtp[0], 0xF0, "OA=0 must not look like OA=1 CMR byte-aligned header");
        let (snr, corr, lag) = aligned_quality(&pcm, &out);
        println!("OA=0 SNR={snr:.1} corr={corr:.4} lag={lag}");
        assert!(snr > 20.0, "OA=0 SNR {snr:.1} dB too low");
        assert!(corr > 0.95, "OA=0 correlation {corr:.4} too low");
    }

    #[test]
    fn omitted_packs_identically_to_oa0() {
        let pcm = generate_sine_tone(700.0, 16000, 20, 14000);
        let a = AmrWbRtp::new(OctetAlign::Omitted)
            .unwrap()
            .encode_rtp(&pcm)
            .unwrap();
        let b = AmrWbRtp::new(OctetAlign::Zero)
            .unwrap()
            .encode_rtp(&pcm)
            .unwrap();
        assert_eq!(a, b);
        let oa1 = AmrWbRtp::new(OctetAlign::One)
            .unwrap()
            .encode_rtp(&pcm)
            .unwrap();
        assert_ne!(a, oa1);
    }

    #[test]
    fn oa_mismatch_does_not_roundtrip() {
        let pcm = generate_sine_tone(700.0, 16000, 20, 14000);
        let rtp = AmrWbRtp::new(OctetAlign::One)
            .unwrap()
            .encode_rtp(&pcm)
            .unwrap();
        let mismatched = AmrWbRtp::new(OctetAlign::Zero)
            .unwrap()
            .decode_rtp(&rtp);
        match mismatched {
            Err(_) => {}
            Ok(out) => {
                let corr = cross_correlation(&pcm, &out);
                assert!(
                    corr < 0.5,
                    "OA mismatch still correlated ({corr:.4}); packing is not OA-specific"
                );
            }
        }
    }

    #[test]
    fn speech_bits_roundtrip_without_rtp() {
        let config = CodecConfig::amr_wb()
            .with_amr_mode_set(&[MODE_INDEX])
            .with_amr_dtx(false);
        let mut enc = AmrCodec::new(&config).unwrap();
        let mut dec = AmrCodec::new(&config).unwrap();
        let mut original = Vec::new();
        let mut decoded = Vec::new();
        let mut last_len = 0usize;
        for _ in 0..50 {
            let pcm = generate_sine_tone(700.0, 16000, 20, 14000);
            original.extend_from_slice(&pcm);
            let bits = AudioCodec::encode(&mut enc, &pcm).unwrap();
            last_len = bits.len();
            let out = AudioCodec::decode(&mut dec, &bits).unwrap();
            decoded.extend_from_slice(&out);
        }
        println!("AMR-WB speech frame bytes={last_len} samples={} vs {}", original.len(), decoded.len());
        let mut best = (f64::NEG_INFINITY, 0isize);
        for lag in -640isize..=640 {
            let (a, b) = align(&original, &decoded, lag);
            if a.len() < 1600 {
                continue;
            }
            let corr = cross_correlation(a, b);
            if corr > best.0 {
                best = (corr, lag);
            }
        }
        let (a, b) = align(&original, &decoded, best.1);
        let snr = compute_snr(a, b);
        println!("speech-only best_lag={} SNR={snr:.1} corr={:.4}", best.1, best.0);
        assert_eq!(last_len, 60, "mode 8 octet-aligned speech bytes");
        assert!(best.0 > 0.85, "speech-only corr {}", best.0);
        assert!(snr > 6.0, "speech-only SNR {snr:.1}");
    }

    fn align<'a>(orig: &'a [i16], dec: &'a [i16], lag: isize) -> (&'a [i16], &'a [i16]) {
        // lag > 0: decoded is delayed (drop leading decoded samples)
        if lag >= 0 {
            let l = lag as usize;
            let n = orig.len().min(dec.len().saturating_sub(l));
            (&orig[..n], &dec[l..l + n])
        } else {
            let l = (-lag) as usize;
            let n = dec.len().min(orig.len().saturating_sub(l));
            (&orig[l..l + n], &dec[..n])
        }
    }

    #[test]
    fn parse_token() {
        assert_eq!(OctetAlign::parse_token("1").unwrap(), OctetAlign::One);
        assert_eq!(OctetAlign::parse_token("0").unwrap(), OctetAlign::Zero);
        assert_eq!(OctetAlign::parse_token("omit").unwrap(), OctetAlign::Omitted);
        assert!(OctetAlign::parse_token("xyz").is_err());
    }
}
