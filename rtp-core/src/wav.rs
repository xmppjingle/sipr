//! WAV file reading and writing for audio recording and playback.
//!
//! Supports 16-bit PCM mono WAV files at any sample rate (typically 8000 Hz for telephony).

use std::io;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WavError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid WAV header: {0}")]
    InvalidHeader(String),
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),
}

/// WAV file header parameters
#[derive(Debug, Clone)]
pub struct WavHeader {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub num_samples: usize,
}

impl WavHeader {
    /// Standard telephony WAV: 8kHz mono 16-bit
    pub fn telephony() -> Self {
        Self {
            sample_rate: 8000,
            channels: 1,
            bits_per_sample: 16,
            num_samples: 0,
        }
    }

    /// WAV at a custom sample rate, mono 16-bit
    pub fn mono(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            channels: 1,
            bits_per_sample: 16,
            num_samples: 0,
        }
    }
}

/// Write PCM samples to a WAV file (in memory as bytes).
pub fn encode_wav(samples: &[i16], header: &WavHeader) -> Vec<u8> {
    let data_size = samples.len() * 2; // 16-bit = 2 bytes per sample
    let byte_rate = header.sample_rate * header.channels as u32 * (header.bits_per_sample as u32 / 8);
    let block_align = header.channels * (header.bits_per_sample / 8);

    let mut buf = Vec::with_capacity(44 + data_size);

    // RIFF header
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&((36 + data_size) as u32).to_le_bytes());
    buf.extend_from_slice(b"WAVE");

    // fmt chunk
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    buf.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    buf.extend_from_slice(&header.channels.to_le_bytes());
    buf.extend_from_slice(&header.sample_rate.to_le_bytes());
    buf.extend_from_slice(&byte_rate.to_le_bytes());
    buf.extend_from_slice(&block_align.to_le_bytes());
    buf.extend_from_slice(&header.bits_per_sample.to_le_bytes());

    // data chunk
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&(data_size as u32).to_le_bytes());
    for &sample in samples {
        buf.extend_from_slice(&sample.to_le_bytes());
    }

    buf
}

/// Write PCM samples to a WAV file on disk.
pub fn write_wav(path: &str, samples: &[i16], header: &WavHeader) -> Result<(), WavError> {
    let data = encode_wav(samples, header);
    std::fs::write(path, data)?;
    Ok(())
}

/// Read PCM samples from WAV bytes.
pub fn decode_wav(data: &[u8]) -> Result<(WavHeader, Vec<i16>), WavError> {
    if data.len() < 44 {
        return Err(WavError::InvalidHeader("too short".to_string()));
    }

    // Verify RIFF header
    if &data[0..4] != b"RIFF" {
        return Err(WavError::InvalidHeader("missing RIFF".to_string()));
    }
    if &data[8..12] != b"WAVE" {
        return Err(WavError::InvalidHeader("missing WAVE".to_string()));
    }

    // Parse fmt chunk
    if &data[12..16] != b"fmt " {
        return Err(WavError::InvalidHeader("missing fmt chunk".to_string()));
    }

    let format = u16::from_le_bytes([data[20], data[21]]);
    if format != 1 {
        return Err(WavError::UnsupportedFormat(format!(
            "not PCM (format={})",
            format
        )));
    }

    let channels = u16::from_le_bytes([data[22], data[23]]);
    let sample_rate = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);
    let bits_per_sample = u16::from_le_bytes([data[34], data[35]]);

    if bits_per_sample != 16 {
        return Err(WavError::UnsupportedFormat(format!(
            "not 16-bit (bits={})",
            bits_per_sample
        )));
    }

    // Find data chunk (skip any extra fmt data or other chunks)
    let mut pos = 12;
    loop {
        if pos + 8 > data.len() {
            return Err(WavError::InvalidHeader("missing data chunk".to_string()));
        }
        let chunk_id = &data[pos..pos + 4];
        let chunk_size = u32::from_le_bytes([
            data[pos + 4],
            data[pos + 5],
            data[pos + 6],
            data[pos + 7],
        ]) as usize;

        if chunk_id == b"data" {
            let sample_data = &data[pos + 8..pos + 8 + chunk_size.min(data.len() - pos - 8)];
            let samples: Vec<i16> = sample_data
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();

            let header = WavHeader {
                sample_rate,
                channels,
                bits_per_sample,
                num_samples: samples.len(),
            };

            return Ok((header, samples));
        }

        pos += 8 + chunk_size;
        // Align to even boundary
        if chunk_size % 2 != 0 {
            pos += 1;
        }
    }
}

/// Read PCM samples from a WAV file on disk.
pub fn read_wav(path: &str) -> Result<(WavHeader, Vec<i16>), WavError> {
    let data = std::fs::read(path)?;
    decode_wav(&data)
}

/// An audio recorder that accumulates PCM samples.
#[derive(Debug, Clone)]
pub struct AudioRecorder {
    samples: Vec<i16>,
    sample_rate: u32,
}

impl AudioRecorder {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            samples: Vec::new(),
            sample_rate,
        }
    }

    /// Record a frame of PCM samples.
    pub fn record_frame(&mut self, frame: &[i16]) {
        self.samples.extend_from_slice(frame);
    }

    /// Get all recorded samples.
    pub fn samples(&self) -> &[i16] {
        &self.samples
    }

    /// Get duration in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        (self.samples.len() as u64 * 1000) / self.sample_rate as u64
    }

    /// Get number of frames recorded (assuming 20ms frames).
    pub fn frame_count(&self) -> usize {
        let samples_per_frame = (self.sample_rate as usize * 20) / 1000;
        if samples_per_frame == 0 {
            return 0;
        }
        self.samples.len() / samples_per_frame
    }

    /// Export as WAV bytes.
    pub fn to_wav(&self) -> Vec<u8> {
        let header = WavHeader {
            sample_rate: self.sample_rate,
            channels: 1,
            bits_per_sample: 16,
            num_samples: self.samples.len(),
        };
        encode_wav(&self.samples, &header)
    }

    /// Save to a WAV file.
    pub fn save_wav(&self, path: &str) -> Result<(), WavError> {
        let header = WavHeader::mono(self.sample_rate);
        write_wav(path, &self.samples, &header)
    }

    /// Clear all recorded samples.
    pub fn clear(&mut self) {
        self.samples.clear();
    }

    /// Check if any audio has been recorded.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Total number of samples recorded.
    pub fn len(&self) -> usize {
        self.samples.len()
    }
}

/// Generate a sine wave tone (for testing).
pub fn generate_sine_tone(frequency: f64, sample_rate: u32, duration_ms: u32, amplitude: i16) -> Vec<i16> {
    let num_samples = (sample_rate as u64 * duration_ms as u64 / 1000) as usize;
    (0..num_samples)
        .map(|i| {
            let t = i as f64 / sample_rate as f64;
            (f64::sin(2.0 * std::f64::consts::PI * frequency * t) * amplitude as f64) as i16
        })
        .collect()
}

/// Generate a multi-tone signal with distinct frequencies (for fidelity testing).
pub fn generate_multi_tone(
    frequencies: &[f64],
    sample_rate: u32,
    duration_ms: u32,
    amplitude: i16,
) -> Vec<i16> {
    let num_samples = (sample_rate as u64 * duration_ms as u64 / 1000) as usize;
    let scale = 1.0 / frequencies.len() as f64;
    (0..num_samples)
        .map(|i| {
            let t = i as f64 / sample_rate as f64;
            let sum: f64 = frequencies
                .iter()
                .map(|&freq| f64::sin(2.0 * std::f64::consts::PI * freq * t))
                .sum();
            (sum * scale * amplitude as f64) as i16
        })
        .collect()
}

/// Compute the signal-to-noise ratio (SNR) in dB between original and received audio.
/// Higher values mean better fidelity. Typical telephony: >20 dB is acceptable.
pub fn compute_snr(original: &[i16], received: &[i16]) -> f64 {
    let len = original.len().min(received.len());
    if len == 0 {
        return 0.0;
    }

    let mut signal_power = 0.0f64;
    let mut noise_power = 0.0f64;

    for i in 0..len {
        let s = original[i] as f64;
        let n = (original[i] as f64) - (received[i] as f64);
        signal_power += s * s;
        noise_power += n * n;
    }

    if noise_power < 1.0 {
        return 100.0; // Perfect match
    }

    10.0 * (signal_power / noise_power).log10()
}

/// Compute normalized cross-correlation between two signals.
/// Returns a value between -1.0 and 1.0. Values > 0.9 indicate strong similarity.
pub fn cross_correlation(a: &[i16], b: &[i16]) -> f64 {
    let len = a.len().min(b.len());
    if len == 0 {
        return 0.0;
    }

    let mut sum_ab = 0.0f64;
    let mut sum_aa = 0.0f64;
    let mut sum_bb = 0.0f64;

    for i in 0..len {
        let va = a[i] as f64;
        let vb = b[i] as f64;
        sum_ab += va * vb;
        sum_aa += va * va;
        sum_bb += vb * vb;
    }

    let denom = (sum_aa * sum_bb).sqrt();
    if denom < 1.0 {
        return 0.0;
    }

    sum_ab / denom
}

/// Compute the maximum absolute sample-by-sample error.
pub fn max_sample_error(original: &[i16], received: &[i16]) -> i32 {
    let len = original.len().min(received.len());
    let mut max_err = 0i32;
    for i in 0..len {
        let err = (original[i] as i32 - received[i] as i32).abs();
        if err > max_err {
            max_err = err;
        }
    }
    max_err
}

/// Compute root-mean-square error between two signals.
pub fn rms_error(original: &[i16], received: &[i16]) -> f64 {
    let len = original.len().min(received.len());
    if len == 0 {
        return 0.0;
    }
    let sum: f64 = (0..len)
        .map(|i| {
            let diff = original[i] as f64 - received[i] as f64;
            diff * diff
        })
        .sum();
    (sum / len as f64).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wav_roundtrip() {
        let samples: Vec<i16> = (0..8000)
            .map(|i| ((i as f64 / 8000.0 * std::f64::consts::TAU * 440.0).sin() * 16000.0) as i16)
            .collect();

        let header = WavHeader::telephony();
        let encoded = encode_wav(&samples, &header);
        let (decoded_header, decoded_samples) = decode_wav(&encoded).unwrap();

        assert_eq!(decoded_header.sample_rate, 8000);
        assert_eq!(decoded_header.channels, 1);
        assert_eq!(decoded_header.bits_per_sample, 16);
        assert_eq!(decoded_samples, samples);
    }

    #[test]
    fn test_wav_file_roundtrip() {
        let samples = generate_sine_tone(440.0, 8000, 100, 16000);
        let header = WavHeader::telephony();

        let path = "/tmp/siphone_test_wav_roundtrip.wav";
        write_wav(path, &samples, &header).unwrap();
        let (_, read_samples) = read_wav(path).unwrap();
        assert_eq!(read_samples, samples);

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_wav_invalid() {
        assert!(decode_wav(b"NOT A WAV").is_err());
        assert!(decode_wav(&[0; 10]).is_err());
    }

    #[test]
    fn test_generate_sine_tone() {
        let tone = generate_sine_tone(440.0, 8000, 100, 16000);
        assert_eq!(tone.len(), 800); // 8000 * 0.1 = 800 samples

        // Should have non-zero samples
        assert!(tone.iter().any(|&s| s != 0));

        // Max amplitude should be close to 16000
        let max = tone.iter().map(|s| s.abs()).max().unwrap();
        assert!(max > 15000 && max <= 16000);
    }

    #[test]
    fn test_generate_multi_tone() {
        let tone = generate_multi_tone(&[300.0, 500.0, 700.0], 8000, 100, 16000);
        assert_eq!(tone.len(), 800);
        assert!(tone.iter().any(|&s| s != 0));
    }

    #[test]
    fn test_audio_recorder() {
        let mut recorder = AudioRecorder::new(8000);
        assert!(recorder.is_empty());
        assert_eq!(recorder.duration_ms(), 0);

        let frame = vec![1000i16; 160];
        recorder.record_frame(&frame);
        assert_eq!(recorder.len(), 160);
        assert_eq!(recorder.duration_ms(), 20); // 160 / 8000 * 1000 = 20ms
        assert_eq!(recorder.frame_count(), 1);

        recorder.record_frame(&frame);
        assert_eq!(recorder.len(), 320);
        assert_eq!(recorder.frame_count(), 2);
        assert_eq!(recorder.duration_ms(), 40);
    }

    #[test]
    fn test_recorder_to_wav() {
        let mut recorder = AudioRecorder::new(8000);
        let tone = generate_sine_tone(440.0, 8000, 100, 16000);
        for frame in tone.chunks(160) {
            recorder.record_frame(frame);
        }

        let wav = recorder.to_wav();
        let (header, samples) = decode_wav(&wav).unwrap();
        assert_eq!(header.sample_rate, 8000);
        assert_eq!(samples, recorder.samples());
    }

    #[test]
    fn test_compute_snr_identical() {
        let signal = generate_sine_tone(440.0, 8000, 100, 16000);
        let snr = compute_snr(&signal, &signal);
        assert!(snr > 90.0, "SNR for identical signals should be very high, got {}", snr);
    }

    #[test]
    fn test_compute_snr_with_noise() {
        let signal = generate_sine_tone(440.0, 8000, 100, 16000);
        let noisy: Vec<i16> = signal
            .iter()
            .enumerate()
            .map(|(i, &s)| {
                let noise = ((i as f64 * 0.1).sin() * 100.0) as i16;
                s.saturating_add(noise)
            })
            .collect();

        let snr = compute_snr(&signal, &noisy);
        assert!(snr > 20.0, "SNR should be decent, got {}", snr);
    }

    #[test]
    fn test_cross_correlation_identical() {
        let signal = generate_sine_tone(440.0, 8000, 100, 16000);
        let corr = cross_correlation(&signal, &signal);
        assert!((corr - 1.0).abs() < 0.001, "Self-correlation should be ~1.0, got {}", corr);
    }

    #[test]
    fn test_cross_correlation_different() {
        let sig_a = generate_sine_tone(440.0, 8000, 100, 16000);
        let sig_b = generate_sine_tone(880.0, 8000, 100, 16000);
        let corr = cross_correlation(&sig_a, &sig_b);
        // Different frequencies should have lower correlation
        assert!(corr < 0.5, "Different tones should have low correlation, got {}", corr);
    }

    #[test]
    fn test_max_sample_error() {
        let a = vec![100i16, 200, 300, 400, 500];
        let b = vec![110i16, 190, 310, 350, 510];
        let err = max_sample_error(&a, &b);
        assert_eq!(err, 50); // 400 - 350
    }

    #[test]
    fn test_rms_error_identical() {
        let a = generate_sine_tone(440.0, 8000, 100, 16000);
        let rms = rms_error(&a, &a);
        assert!(rms < 0.001, "RMS error for identical signals should be ~0, got {}", rms);
    }

    #[test]
    fn test_recorder_clear() {
        let mut recorder = AudioRecorder::new(8000);
        recorder.record_frame(&[100i16; 160]);
        assert!(!recorder.is_empty());
        recorder.clear();
        assert!(recorder.is_empty());
    }

    #[test]
    fn test_wav_header_constructors() {
        let h = WavHeader::telephony();
        assert_eq!(h.sample_rate, 8000);
        assert_eq!(h.channels, 1);

        let h = WavHeader::mono(48000);
        assert_eq!(h.sample_rate, 48000);
        assert_eq!(h.channels, 1);
    }
}
