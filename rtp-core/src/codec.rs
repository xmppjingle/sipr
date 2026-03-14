use std::fmt;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecType {
    /// G.711 mu-law (PCMU), payload type 0
    Pcmu,
    /// G.711 A-law (PCMA), payload type 8
    Pcma,
    /// Opus codec, payload type 111 (dynamic)
    Opus,
}

impl fmt::Display for CodecType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodecType::Pcmu => write!(f, "PCMU (G.711 mu-law)"),
            CodecType::Pcma => write!(f, "PCMA (G.711 A-law)"),
            CodecType::Opus => write!(f, "Opus"),
        }
    }
}

impl CodecType {
    pub fn payload_type(&self) -> u8 {
        match self {
            CodecType::Pcmu => 0,
            CodecType::Pcma => 8,
            CodecType::Opus => 111,
        }
    }

    pub fn clock_rate(&self) -> u32 {
        match self {
            CodecType::Pcmu => 8000,
            CodecType::Pcma => 8000,
            CodecType::Opus => 48000,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            CodecType::Pcmu => "PCMU",
            CodecType::Pcma => "PCMA",
            CodecType::Opus => "opus",
        }
    }

    pub fn from_payload_type(pt: u8) -> Option<Self> {
        match pt {
            0 => Some(CodecType::Pcmu),
            8 => Some(CodecType::Pcma),
            111 => Some(CodecType::Opus),
            _ => None,
        }
    }

    /// Samples per frame at 20ms ptime
    pub fn samples_per_frame(&self) -> usize {
        match self {
            CodecType::Pcmu => 160,  // 8000 * 0.020
            CodecType::Pcma => 160,
            CodecType::Opus => 960,  // 48000 * 0.020
        }
    }
}

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("encoding error: {0}")]
    EncodingError(String),
    #[error("decoding error: {0}")]
    DecodingError(String),
    #[error("unsupported codec")]
    Unsupported,
}

/// Codec pipeline for encoding/decoding audio frames.
///
/// For PCMU/PCMA, we implement the G.711 codec directly.
/// For Opus, uses the audiopus crate when the "opus" feature is enabled,
/// otherwise falls back to a raw-bytes stub.
pub struct CodecPipeline {
    codec: CodecType,
    #[cfg(feature = "opus")]
    opus_encoder: Option<audiopus::coder::Encoder>,
    #[cfg(feature = "opus")]
    opus_decoder: Option<audiopus::coder::Decoder>,
}

impl CodecPipeline {
    pub fn new(codec: CodecType) -> Self {
        #[cfg(feature = "opus")]
        let (opus_encoder, opus_decoder) = if codec == CodecType::Opus {
            let enc = audiopus::coder::Encoder::new(
                audiopus::SampleRate::Hz48000,
                audiopus::Channels::Mono,
                audiopus::Application::Voip,
            ).ok();
            let dec = audiopus::coder::Decoder::new(
                audiopus::SampleRate::Hz48000,
                audiopus::Channels::Mono,
            ).ok();
            (enc, dec)
        } else {
            (None, None)
        };

        Self {
            codec,
            #[cfg(feature = "opus")]
            opus_encoder,
            #[cfg(feature = "opus")]
            opus_decoder,
        }
    }

    pub fn codec_type(&self) -> CodecType {
        self.codec
    }

    /// Encode PCM samples (16-bit linear, mono) to codec format
    pub fn encode(&mut self, pcm_samples: &[i16]) -> Result<Vec<u8>, CodecError> {
        match self.codec {
            CodecType::Pcmu => Ok(pcm_samples.iter().map(|&s| linear_to_ulaw(s)).collect()),
            CodecType::Pcma => Ok(pcm_samples.iter().map(|&s| linear_to_alaw(s)).collect()),
            CodecType::Opus => {
                #[cfg(feature = "opus")]
                {
                    if let Some(ref mut enc) = self.opus_encoder {
                        let mut output = vec![0u8; 4000]; // max opus frame
                        let len = enc.encode(pcm_samples, &mut output)
                            .map_err(|e| CodecError::EncodingError(format!("opus encode: {}", e)))?;
                        output.truncate(len);
                        return Ok(output);
                    }
                }
                // Stub fallback: encode as raw bytes
                let mut bytes = Vec::with_capacity(pcm_samples.len() * 2);
                for &sample in pcm_samples {
                    bytes.extend_from_slice(&sample.to_le_bytes());
                }
                Ok(bytes)
            }
        }
    }

    /// Decode codec format to PCM samples (16-bit linear, mono)
    pub fn decode(&mut self, data: &[u8]) -> Result<Vec<i16>, CodecError> {
        match self.codec {
            CodecType::Pcmu => Ok(data.iter().map(|&b| ulaw_to_linear(b)).collect()),
            CodecType::Pcma => Ok(data.iter().map(|&b| alaw_to_linear(b)).collect()),
            CodecType::Opus => {
                #[cfg(feature = "opus")]
                {
                    if let Some(ref mut dec) = self.opus_decoder {
                        let mut output = vec![0i16; 5760]; // max decode buffer
                        let packet: audiopus::packet::Packet<'_> = data.try_into()
                            .map_err(|e: audiopus::Error| CodecError::DecodingError(format!("opus packet: {}", e)))?;
                        let signals: audiopus::MutSignals<'_, i16> = output.as_mut_slice().try_into()
                            .map_err(|e: audiopus::Error| CodecError::DecodingError(format!("opus signals: {}", e)))?;
                        let samples = dec.decode(Some(packet), signals, false)
                            .map_err(|e| CodecError::DecodingError(format!("opus decode: {}", e)))?;
                        output.truncate(samples);
                        return Ok(output);
                    }
                }
                // Stub fallback: decode raw bytes back to samples
                if data.len() % 2 != 0 {
                    return Err(CodecError::DecodingError(
                        "invalid opus stub data length".to_string(),
                    ));
                }
                let samples: Vec<i16> = data
                    .chunks_exact(2)
                    .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                    .collect();
                Ok(samples)
            }
        }
    }

    /// Generate silence for one frame
    pub fn silence_frame(&self) -> Vec<u8> {
        match self.codec {
            CodecType::Pcmu => vec![0xFF; self.codec.samples_per_frame()], // mu-law silence
            CodecType::Pcma => vec![0xD5; self.codec.samples_per_frame()], // A-law silence
            CodecType::Opus => vec![0; self.codec.samples_per_frame() * 2], // stub silence
        }
    }
}

// G.711 mu-law encoding/decoding tables and functions

const ULAW_BIAS: i32 = 0x84;
const ULAW_CLIP: i32 = 32635;

fn linear_to_ulaw(sample: i16) -> u8 {
    let mut pcm_val = sample as i32;
    let sign = if pcm_val < 0 {
        pcm_val = -pcm_val;
        0x80
    } else {
        0
    };

    if pcm_val > ULAW_CLIP {
        pcm_val = ULAW_CLIP;
    }
    pcm_val += ULAW_BIAS;

    let exponent = match pcm_val {
        0..=0xFF => 0,
        0x100..=0x1FF => 1,
        0x200..=0x3FF => 2,
        0x400..=0x7FF => 3,
        0x800..=0xFFF => 4,
        0x1000..=0x1FFF => 5,
        0x2000..=0x3FFF => 6,
        _ => 7,
    };

    let mantissa = (pcm_val >> (exponent + 3)) & 0x0F;
    let ulaw_byte = !(sign | (exponent << 4) as i32 | mantissa) as u8;
    ulaw_byte
}

fn ulaw_to_linear(ulaw_byte: u8) -> i16 {
    let ulaw = !ulaw_byte;
    let sign = ulaw & 0x80;
    let exponent = ((ulaw >> 4) & 0x07) as i32;
    let mantissa = (ulaw & 0x0F) as i32;

    let mut sample = ((mantissa << 1) | 0x21) << (exponent + 2);
    sample -= ULAW_BIAS as i32;

    if sign != 0 {
        -sample as i16
    } else {
        sample as i16
    }
}

fn linear_to_alaw(sample: i16) -> u8 {
    let mut pcm_val = sample as i32;
    let sign = if pcm_val < 0 {
        pcm_val = -pcm_val;
        0x80i32
    } else {
        0
    };

    if pcm_val > 32767 {
        pcm_val = 32767;
    }

    let (exponent, mantissa) = if pcm_val >= 256 {
        let exp = match pcm_val {
            256..=511 => 1,
            512..=1023 => 2,
            1024..=2047 => 3,
            2048..=4095 => 4,
            4096..=8191 => 5,
            8192..=16383 => 6,
            _ => 7,
        };
        let man = (pcm_val >> (exp + 3)) & 0x0F;
        (exp, man)
    } else {
        (0, pcm_val >> 4)
    };

    let alaw_byte = (sign | (exponent << 4) | mantissa) as u8;
    alaw_byte ^ 0x55
}

fn alaw_to_linear(alaw_byte: u8) -> i16 {
    let alaw = alaw_byte ^ 0x55;
    let sign = alaw & 0x80;
    let exponent = ((alaw >> 4) & 0x07) as i32;
    let mantissa = (alaw & 0x0F) as i32;

    let sample = if exponent == 0 {
        (mantissa << 4) | 0x08
    } else {
        ((mantissa << 1) | 0x21) << (exponent + 2)
    };

    if sign != 0 {
        -(sample as i16)
    } else {
        sample as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_codec_type_properties() {
        assert_eq!(CodecType::Pcmu.payload_type(), 0);
        assert_eq!(CodecType::Pcma.payload_type(), 8);
        assert_eq!(CodecType::Opus.payload_type(), 111);

        assert_eq!(CodecType::Pcmu.clock_rate(), 8000);
        assert_eq!(CodecType::Opus.clock_rate(), 48000);

        assert_eq!(CodecType::Pcmu.name(), "PCMU");
        assert_eq!(CodecType::Pcma.name(), "PCMA");
        assert_eq!(CodecType::Opus.name(), "opus");
    }

    #[test]
    fn test_codec_from_payload_type() {
        assert_eq!(CodecType::from_payload_type(0), Some(CodecType::Pcmu));
        assert_eq!(CodecType::from_payload_type(8), Some(CodecType::Pcma));
        assert_eq!(CodecType::from_payload_type(111), Some(CodecType::Opus));
        assert_eq!(CodecType::from_payload_type(99), None);
    }

    #[test]
    fn test_samples_per_frame() {
        assert_eq!(CodecType::Pcmu.samples_per_frame(), 160);
        assert_eq!(CodecType::Pcma.samples_per_frame(), 160);
        assert_eq!(CodecType::Opus.samples_per_frame(), 960);
    }

    #[test]
    fn test_pcmu_encode_decode_roundtrip() {
        let mut codec = CodecPipeline::new(CodecType::Pcmu);

        // Test with various sample values
        let samples: Vec<i16> = vec![0, 100, -100, 1000, -1000, 8000, -8000, 32000, -32000];
        let encoded = codec.encode(&samples).unwrap();
        let decoded = codec.decode(&encoded).unwrap();

        assert_eq!(samples.len(), decoded.len());
        // G.711 is lossy, so values won't be exact but should be close
        for (original, decoded) in samples.iter().zip(decoded.iter()) {
            let diff = (*original as i32 - *decoded as i32).abs();
            // Allow some quantization error
            assert!(
                diff < 500,
                "Too much error: original={}, decoded={}, diff={}",
                original,
                decoded,
                diff
            );
        }
    }

    #[test]
    fn test_pcma_encode_decode_roundtrip() {
        let mut codec = CodecPipeline::new(CodecType::Pcma);

        let samples: Vec<i16> = vec![0, 100, -100, 1000, -1000, 8000, -8000, 32000, -32000];
        let encoded = codec.encode(&samples).unwrap();
        let decoded = codec.decode(&encoded).unwrap();

        assert_eq!(samples.len(), decoded.len());
        for (original, decoded) in samples.iter().zip(decoded.iter()) {
            let diff = (*original as i32 - *decoded as i32).abs();
            assert!(
                diff < 500,
                "Too much error: original={}, decoded={}, diff={}",
                original,
                decoded,
                diff
            );
        }
    }

    #[test]
    fn test_opus_roundtrip() {
        let mut codec = CodecPipeline::new(CodecType::Opus);
        // Generate a simple tone for testing
        let samples: Vec<i16> = (0..960)
            .map(|i| ((i as f64 / 960.0 * std::f64::consts::TAU).sin() * 16000.0) as i16)
            .collect();
        let encoded = codec.encode(&samples).unwrap();
        let decoded = codec.decode(&encoded).unwrap();
        // Opus is lossy but output should have correct number of samples
        assert_eq!(decoded.len(), 960);
        // Verify the decoded audio isn't silence (some energy preserved)
        let max_sample = decoded.iter().map(|s| s.abs()).max().unwrap_or(0);
        assert!(max_sample > 1000, "Expected audio energy, max sample was {}", max_sample);
    }

    #[cfg(not(feature = "opus"))]
    #[test]
    fn test_opus_stub_decode_odd_length() {
        let mut codec = CodecPipeline::new(CodecType::Opus);
        let result = codec.decode(&[0, 1, 2]); // Odd number of bytes
        assert!(result.is_err());
    }

    #[test]
    fn test_pcmu_silence() {
        let codec = CodecPipeline::new(CodecType::Pcmu);
        let silence = codec.silence_frame();
        assert_eq!(silence.len(), 160);
        assert!(silence.iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn test_pcma_silence() {
        let codec = CodecPipeline::new(CodecType::Pcma);
        let silence = codec.silence_frame();
        assert_eq!(silence.len(), 160);
        assert!(silence.iter().all(|&b| b == 0xD5));
    }

    #[test]
    fn test_pcmu_encode_silence() {
        let mut codec = CodecPipeline::new(CodecType::Pcmu);
        let silence_pcm = vec![0i16; 160];
        let encoded = codec.encode(&silence_pcm).unwrap();
        assert_eq!(encoded.len(), 160);
    }

    #[test]
    fn test_ulaw_known_values() {
        // Silence (0) should encode to 0xFF
        assert_eq!(linear_to_ulaw(0), 0xFF);
        // Decode 0xFF should be close to 0
        let decoded = ulaw_to_linear(0xFF);
        assert!(decoded.abs() < 10, "Expected near-zero, got {}", decoded);
    }

    #[test]
    fn test_codec_pipeline_type() {
        let pipeline = CodecPipeline::new(CodecType::Pcmu);
        assert_eq!(pipeline.codec_type(), CodecType::Pcmu);

        let pipeline = CodecPipeline::new(CodecType::Pcma);
        assert_eq!(pipeline.codec_type(), CodecType::Pcma);
    }

    #[test]
    fn test_pcmu_full_frame() {
        let mut codec = CodecPipeline::new(CodecType::Pcmu);
        // Generate a sine-like wave
        let samples: Vec<i16> = (0..160)
            .map(|i| ((i as f64 / 160.0 * std::f64::consts::TAU).sin() * 16000.0) as i16)
            .collect();

        let encoded = codec.encode(&samples).unwrap();
        assert_eq!(encoded.len(), 160); // 1 byte per sample for G.711

        let decoded = codec.decode(&encoded).unwrap();
        assert_eq!(decoded.len(), 160);
    }
}
