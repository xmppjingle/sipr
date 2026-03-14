//! Audio device abstraction for microphone capture and speaker playback.
//!
//! Requires the `audio-device` feature flag (backed by `cpal`).
//! On headless systems without audio hardware, the module compiles but
//! device enumeration returns empty lists.

#[cfg(feature = "audio-device")]
mod cpal_backend;

#[cfg(feature = "audio-device")]
pub use cpal_backend::*;

use std::fmt;

/// Describes an audio device (input or output).
#[derive(Debug, Clone)]
pub struct AudioDeviceInfo {
    pub name: String,
    pub device_type: DeviceType,
    pub sample_rates: Vec<u32>,
    pub channels: Vec<u16>,
    pub is_default: bool,
}

impl fmt::Display for AudioDeviceInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let default_marker = if self.is_default { " (default)" } else { "" };
        let rates: Vec<String> = self.sample_rates.iter().map(|r| format!("{}Hz", r)).collect();
        write!(
            f,
            "{}{} [{}] rates=[{}] channels={:?}",
            self.name,
            default_marker,
            self.device_type,
            rates.join(", "),
            self.channels,
        )
    }
}

/// Type of audio device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    Input,
    Output,
}

impl fmt::Display for DeviceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceType::Input => write!(f, "input"),
            DeviceType::Output => write!(f, "output"),
        }
    }
}

/// Audio device selection criteria.
#[derive(Debug, Clone)]
pub enum DeviceSelector {
    /// Use the system default device.
    Default,
    /// Select by device name (substring match).
    ByName(String),
    /// Select by index from the device list.
    ByIndex(usize),
}

impl DeviceSelector {
    pub fn from_arg(arg: &str) -> Self {
        if arg.eq_ignore_ascii_case("default") {
            DeviceSelector::Default
        } else if let Ok(idx) = arg.parse::<usize>() {
            DeviceSelector::ByIndex(idx)
        } else {
            DeviceSelector::ByName(arg.to_string())
        }
    }
}

impl fmt::Display for DeviceSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeviceSelector::Default => write!(f, "default"),
            DeviceSelector::ByName(name) => write!(f, "\"{}\"", name),
            DeviceSelector::ByIndex(idx) => write!(f, "#{}", idx),
        }
    }
}

/// Configuration for audio capture/playback.
#[derive(Debug, Clone)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub frame_size_ms: u32,
}

impl AudioConfig {
    /// Standard telephony config: 8kHz mono, 20ms frames.
    pub fn telephony() -> Self {
        Self {
            sample_rate: 8000,
            channels: 1,
            frame_size_ms: 20,
        }
    }

    /// Samples per frame.
    pub fn samples_per_frame(&self) -> usize {
        (self.sample_rate as usize * self.frame_size_ms as usize) / 1000
    }
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self::telephony()
    }
}

/// Stub implementations when `audio-device` feature is not enabled.
/// These allow the CLI to compile and provide helpful messages.
#[cfg(not(feature = "audio-device"))]
pub fn list_devices() -> Vec<AudioDeviceInfo> {
    Vec::new()
}

#[cfg(not(feature = "audio-device"))]
pub fn list_input_devices() -> Vec<AudioDeviceInfo> {
    Vec::new()
}

#[cfg(not(feature = "audio-device"))]
pub fn list_output_devices() -> Vec<AudioDeviceInfo> {
    Vec::new()
}

#[cfg(not(feature = "audio-device"))]
pub fn is_audio_available() -> bool {
    false
}

#[cfg(not(feature = "audio-device"))]
pub fn audio_unavailable_reason() -> &'static str {
    "Compiled without audio-device feature. Rebuild with: cargo build --features audio-device"
}

/// Test tone generator that produces audio frames for device testing.
pub struct TestToneGenerator {
    frequency: f64,
    sample_rate: u32,
    amplitude: i16,
    phase: f64,
}

impl TestToneGenerator {
    pub fn new(frequency: f64, sample_rate: u32, amplitude: i16) -> Self {
        Self {
            frequency,
            sample_rate,
            amplitude,
            phase: 0.0,
        }
    }

    /// Generate the next frame of audio samples.
    pub fn next_frame(&mut self, num_samples: usize) -> Vec<i16> {
        let mut samples = Vec::with_capacity(num_samples);
        let phase_increment = 2.0 * std::f64::consts::PI * self.frequency / self.sample_rate as f64;

        for _ in 0..num_samples {
            let sample = (self.phase.sin() * self.amplitude as f64) as i16;
            samples.push(sample);
            self.phase += phase_increment;
            if self.phase > 2.0 * std::f64::consts::PI {
                self.phase -= 2.0 * std::f64::consts::PI;
            }
        }

        samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_type_display() {
        assert_eq!(DeviceType::Input.to_string(), "input");
        assert_eq!(DeviceType::Output.to_string(), "output");
    }

    #[test]
    fn test_device_selector_from_arg() {
        assert!(matches!(DeviceSelector::from_arg("default"), DeviceSelector::Default));
        assert!(matches!(DeviceSelector::from_arg("0"), DeviceSelector::ByIndex(0)));
        assert!(matches!(DeviceSelector::from_arg("2"), DeviceSelector::ByIndex(2)));
        assert!(matches!(DeviceSelector::from_arg("My Mic"), DeviceSelector::ByName(ref s) if s == "My Mic"));
    }

    #[test]
    fn test_device_selector_display() {
        assert_eq!(DeviceSelector::Default.to_string(), "default");
        assert_eq!(DeviceSelector::ByIndex(3).to_string(), "#3");
        assert_eq!(DeviceSelector::ByName("USB Mic".into()).to_string(), "\"USB Mic\"");
    }

    #[test]
    fn test_audio_config_telephony() {
        let cfg = AudioConfig::telephony();
        assert_eq!(cfg.sample_rate, 8000);
        assert_eq!(cfg.channels, 1);
        assert_eq!(cfg.frame_size_ms, 20);
        assert_eq!(cfg.samples_per_frame(), 160);
    }

    #[test]
    fn test_audio_config_default() {
        let cfg = AudioConfig::default();
        assert_eq!(cfg.sample_rate, 8000);
    }

    #[test]
    fn test_audio_device_info_display() {
        let info = AudioDeviceInfo {
            name: "Test Mic".to_string(),
            device_type: DeviceType::Input,
            sample_rates: vec![8000, 44100, 48000],
            channels: vec![1, 2],
            is_default: true,
        };
        let s = info.to_string();
        assert!(s.contains("Test Mic"));
        assert!(s.contains("(default)"));
        assert!(s.contains("input"));
        assert!(s.contains("8000Hz"));
    }

    #[test]
    fn test_audio_device_info_non_default() {
        let info = AudioDeviceInfo {
            name: "HDMI Output".to_string(),
            device_type: DeviceType::Output,
            sample_rates: vec![48000],
            channels: vec![2],
            is_default: false,
        };
        let s = info.to_string();
        assert!(!s.contains("(default)"));
        assert!(s.contains("output"));
    }

    #[test]
    fn test_test_tone_generator() {
        let mut gen = TestToneGenerator::new(440.0, 8000, 12000);
        let frame1 = gen.next_frame(160);
        assert_eq!(frame1.len(), 160);
        assert!(frame1.iter().any(|&s| s != 0));

        let frame2 = gen.next_frame(160);
        assert_eq!(frame2.len(), 160);
        // Phase should continue, so frame2 != frame1 in general
        // (unless frequency perfectly divides sample rate * frame_size)
    }

    #[test]
    fn test_test_tone_continuous_phase() {
        let mut gen = TestToneGenerator::new(400.0, 8000, 16000);
        // Generate multiple frames and check they have reasonable amplitude
        for _ in 0..50 {
            let frame = gen.next_frame(160);
            let max = frame.iter().map(|s| s.abs()).max().unwrap();
            assert!(max > 10000, "Tone amplitude should stay consistent");
        }
    }

    #[cfg(not(feature = "audio-device"))]
    #[test]
    fn test_stub_no_devices() {
        assert_eq!(list_devices().len(), 0);
        assert_eq!(list_input_devices().len(), 0);
        assert_eq!(list_output_devices().len(), 0);
        assert!(!is_audio_available());
        assert!(audio_unavailable_reason().contains("audio-device"));
    }
}
