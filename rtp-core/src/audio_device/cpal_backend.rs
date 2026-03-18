//! cpal-backed audio device implementation.
//!
//! Provides real audio capture and playback through system audio devices.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

use super::{AudioConfig, AudioDeviceInfo, DeviceSelector, DeviceType};

/// List all available audio devices (input and output).
pub fn list_devices() -> Vec<AudioDeviceInfo> {
    let mut devices = list_input_devices();
    devices.extend(list_output_devices());
    devices
}

/// List available input (microphone) devices.
pub fn list_input_devices() -> Vec<AudioDeviceInfo> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.name().ok());

    let Ok(input_devices) = host.input_devices() else {
        return Vec::new();
    };

    input_devices
        .filter_map(|device| {
            let name = device.name().ok()?;
            let configs = device.supported_input_configs().ok()?;

            let mut sample_rates = Vec::new();
            let mut channels = Vec::new();

            for config in configs {
                let min = config.min_sample_rate().0;
                let max = config.max_sample_rate().0;
                for &rate in &[8000, 16000, 44100, 48000] {
                    if rate >= min && rate <= max && !sample_rates.contains(&rate) {
                        sample_rates.push(rate);
                    }
                }
                let ch = config.channels();
                if !channels.contains(&ch) {
                    channels.push(ch);
                }
            }

            sample_rates.sort();
            channels.sort();

            Some(AudioDeviceInfo {
                is_default: default_name.as_deref() == Some(&name),
                name,
                device_type: DeviceType::Input,
                sample_rates,
                channels,
            })
        })
        .collect()
}

/// List available output (speaker) devices.
pub fn list_output_devices() -> Vec<AudioDeviceInfo> {
    let host = cpal::default_host();
    let default_name = host
        .default_output_device()
        .and_then(|d| d.name().ok());

    let Ok(output_devices) = host.output_devices() else {
        return Vec::new();
    };

    output_devices
        .filter_map(|device| {
            let name = device.name().ok()?;
            let configs = device.supported_output_configs().ok()?;

            let mut sample_rates = Vec::new();
            let mut channels = Vec::new();

            for config in configs {
                let min = config.min_sample_rate().0;
                let max = config.max_sample_rate().0;
                for &rate in &[8000, 16000, 44100, 48000] {
                    if rate >= min && rate <= max && !sample_rates.contains(&rate) {
                        sample_rates.push(rate);
                    }
                }
                let ch = config.channels();
                if !channels.contains(&ch) {
                    channels.push(ch);
                }
            }

            sample_rates.sort();
            channels.sort();

            Some(AudioDeviceInfo {
                is_default: default_name.as_deref() == Some(&name),
                name,
                device_type: DeviceType::Output,
                sample_rates,
                channels,
            })
        })
        .collect()
}

/// Check if any audio device is available.
pub fn is_audio_available() -> bool {
    let host = cpal::default_host();
    host.default_input_device().is_some() || host.default_output_device().is_some()
}

/// Returns a reason why audio is unavailable, or empty string if available.
pub fn audio_unavailable_reason() -> &'static str {
    if is_audio_available() {
        ""
    } else {
        "No audio devices found. Check that a sound card is installed and drivers are loaded."
    }
}

/// Select a cpal device by selector criteria.
fn select_input_device(selector: &DeviceSelector) -> Option<cpal::Device> {
    let host = cpal::default_host();
    match selector {
        DeviceSelector::Default => host.default_input_device(),
        DeviceSelector::ByName(name) => {
            let devices = host.input_devices().ok()?;
            devices
                .into_iter()
                .find(|d| d.name().ok().map(|n| n.contains(name.as_str())).unwrap_or(false))
        }
        DeviceSelector::ByIndex(idx) => {
            let devices: Vec<_> = host.input_devices().ok()?.collect();
            devices.into_iter().nth(*idx)
        }
    }
}

fn select_output_device(selector: &DeviceSelector) -> Option<cpal::Device> {
    let host = cpal::default_host();
    match selector {
        DeviceSelector::Default => host.default_output_device(),
        DeviceSelector::ByName(name) => {
            let devices = host.output_devices().ok()?;
            devices
                .into_iter()
                .find(|d| d.name().ok().map(|n| n.contains(name.as_str())).unwrap_or(false))
        }
        DeviceSelector::ByIndex(idx) => {
            let devices: Vec<_> = host.output_devices().ok()?.collect();
            devices.into_iter().nth(*idx)
        }
    }
}

/// Audio capture stream that reads from a microphone.
pub struct AudioCapture {
    _stream: cpal::Stream,
    rx: mpsc::Receiver<Vec<i16>>,
}

impl AudioCapture {
    /// Start capturing audio from the selected input device.
    ///
    /// If the device doesn't support the requested sample rate, we find the
    /// nearest supported rate and downsample in the capture callback.
    pub fn start(
        selector: &DeviceSelector,
        config: &AudioConfig,
    ) -> Result<Self, String> {
        let device = select_input_device(selector)
            .ok_or_else(|| format!("Input device not found: {}", selector))?;

        let device_rate = find_supported_input_rate(&device, config.sample_rate, config.channels)
            .ok_or_else(|| "No supported sample rate found for input device".to_string())?;

        let stream_config = cpal::StreamConfig {
            channels: config.channels,
            sample_rate: cpal::SampleRate(device_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let need_resample = device_rate != config.sample_rate;
        let src_rate = device_rate as f64;
        let dst_rate = config.sample_rate as f64;

        let samples_per_frame = config.samples_per_frame();
        let (tx, rx) = mpsc::channel::<Vec<i16>>(32);
        let buffer = Arc::new(Mutex::new(Vec::with_capacity(samples_per_frame * 2)));
        let buffer_clone = buffer.clone();

        let stream = device
            .build_input_stream(
                &stream_config,
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    // Convert f32 to i16 with clamping
                    let i16_data: Vec<i16> = data
                        .iter()
                        .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
                        .collect();
                    let resampled = if need_resample {
                        resample_linear(&i16_data, src_rate, dst_rate)
                    } else {
                        i16_data
                    };

                    if let Ok(mut buf) = buffer_clone.lock() {
                        buf.extend_from_slice(&resampled);

                        while buf.len() >= samples_per_frame {
                            let frame: Vec<i16> = buf.drain(..samples_per_frame).collect();
                            let _ = tx.try_send(frame);
                        }
                    }
                },
                |err| {
                    tracing::error!("Audio capture error: {}", err);
                },
                None,
            )
            .map_err(|e| format!("Failed to build input stream: {}", e))?;

        stream
            .play()
            .map_err(|e| format!("Failed to start capture: {}", e))?;

        Ok(Self {
            _stream: stream,
            rx,
        })
    }

    /// Receive the next audio frame.
    pub async fn next_frame(&mut self) -> Option<Vec<i16>> {
        self.rx.recv().await
    }
}

/// Audio playback stream that writes to a speaker.
pub struct AudioPlayback {
    _stream: cpal::Stream,
    tx: mpsc::Sender<Vec<i16>>,
}

impl AudioPlayback {
    /// Start playing audio on the selected output device.
    ///
    /// If the device doesn't support the requested sample rate, we find the
    /// nearest supported rate and perform linear interpolation resampling in
    /// the playback callback.
    pub fn start(
        selector: &DeviceSelector,
        config: &AudioConfig,
    ) -> Result<Self, String> {
        let device = select_output_device(selector)
            .ok_or_else(|| format!("Output device not found: {}", selector))?;

        // Find a supported sample rate, preferring the requested one.
        // Try with requested channels first, then fall back to any channel count.
        let (device_rate, device_channels) =
            find_supported_output_config(&device, config.sample_rate, config.channels)
                .ok_or_else(|| "No supported configuration found for output device".to_string())?;

        let stream_config = cpal::StreamConfig {
            channels: device_channels,
            sample_rate: cpal::SampleRate(device_rate),
            buffer_size: cpal::BufferSize::Default,
        };

        let need_resample = device_rate != config.sample_rate;
        let need_channel_convert = device_channels != config.channels;
        let src_rate = config.sample_rate as f64;
        let dst_rate = device_rate as f64;
        let out_channels = device_channels;

        // Playout buffer thresholds (all in output samples).
        //
        // The sender's RTP clock and the hardware audio clock are independent oscillators
        // that drift apart over time.  Without RTCP Sender Reports there is no external
        // reference to correct them, so we do adaptive clock recovery entirely from buffer
        // level:
        //
        //  • high-water (3 frames / 60 ms): buffer is growing → sender faster than hardware.
        //    Drop 1 sample per frame by blending adjacent samples (linear interpolation).
        //    Effect: ~0.6 % speed-up, nearly inaudible.
        //
        //  • low-water  (1 frame / 20 ms): buffer is shrinking → sender slower than hardware.
        //    Duplicate 1 sample per frame.
        //    Effect: ~0.6 % slow-down, nearly inaudible.
        //
        //  • hard cap   (4 frames / 80 ms): safety net for burst arrivals.
        //    When exceeded, trim to the midpoint (2 frames / 40 ms) to create headroom before
        //    the next potential overflow.  This is a last-resort discontinuity; in steady state
        //    the adaptive logic above should keep the buffer between low- and high-water.
        let samples_per_frame_out = (dst_rate / 50.0 * out_channels as f64) as usize;
        let low_water  = samples_per_frame_out;           // 1 frame  (~20 ms)
        let high_water = samples_per_frame_out * 3;       // 3 frames (~60 ms)
        let max_buf_samples = samples_per_frame_out * 4;  // 4 frames (~80 ms) hard cap

        let (tx, mut rx) = mpsc::channel::<Vec<i16>>(32);
        let buffer = Arc::new(Mutex::new(VecDeque::<f32>::new()));
        let buffer_clone = buffer.clone();

        // Spawn a task to receive i16 frames, resample/channel-convert, and buffer as f32.
        // Adaptive clock correction is applied here so the real-time audio callback never
        // needs to do anything but drain the local buffer.
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                let resampled = if need_resample {
                    resample_linear(&frame, src_rate, dst_rate)
                } else {
                    frame
                };
                let converted = if need_channel_convert {
                    mono_to_multi(&resampled, out_channels)
                } else {
                    resampled
                };
                // Convert i16 → f32 [-1.0, 1.0]
                let mut float_samples: Vec<f32> = converted
                    .iter()
                    .map(|&s| (s as f32 / 32768.0).clamp(-1.0, 1.0))
                    .collect();

                if let Ok(mut buf) = buffer_clone.lock() {
                    // Adaptive rate correction based on current buffer occupancy.
                    if buf.len() > high_water && float_samples.len() > 2 {
                        // Buffer growing: drop 1 sample near the middle via linear blend.
                        // Choosing the midpoint minimises audible discontinuity compared
                        // with dropping at the edges.
                        let mid = float_samples.len() / 2;
                        let blended = (float_samples[mid] + float_samples[mid + 1]) / 2.0;
                        float_samples[mid] = blended;
                        float_samples.remove(mid + 1);
                    } else if buf.len() < low_water && !float_samples.is_empty() {
                        // Buffer shrinking: duplicate 1 sample near the middle.
                        let mid = float_samples.len() / 2;
                        let dup = float_samples[mid];
                        float_samples.insert(mid + 1, dup);
                    }

                    buf.extend(float_samples.iter());

                    // Hard cap: burst protection.  Trim to midpoint to leave headroom.
                    if buf.len() > max_buf_samples {
                        let target = max_buf_samples / 2;
                        let excess = buf.len().saturating_sub(target);
                        buf.drain(..excess);
                    }
                }
            }
        });

        let buffer_for_stream = buffer.clone();

        // Local buffer owned exclusively by the audio callback (no lock needed to read it).
        // On each invocation we try to drain the shared Mutex buffer into this local one in a
        // single short critical section, then serve output from the local copy.  This means the
        // real-time audio thread never blocks waiting for the tokio producer.
        //
        // VecDeque is used so that pop_front() and draining from the front are O(1).
        // Vec::drain(..n) from the front shifts all remaining elements, which is O(n) and
        // creates non-deterministic callback timing → occasional underruns → clicks.
        let mut local_buf: VecDeque<f32> = VecDeque::with_capacity(max_buf_samples);

        let stream = device
            .build_output_stream(
                &stream_config,
                move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    // Try to grab all pending samples from the shared buffer in one shot.
                    // try_lock() never blocks: if the producer holds the lock we simply
                    // serve from whatever is already in local_buf.
                    if let Ok(mut shared) = buffer_for_stream.try_lock() {
                        local_buf.extend(shared.drain(..));
                    }

                    for sample in data.iter_mut() {
                        *sample = local_buf.pop_front().unwrap_or(0.0);
                    }
                },
                |err| {
                    tracing::error!("Audio playback error: {}", err);
                },
                None,
            )
            .map_err(|e| format!("Failed to build output stream: {}", e))?;

        stream
            .play()
            .map_err(|e| format!("Failed to start playback: {}", e))?;

        Ok(Self {
            _stream: stream,
            tx,
        })
    }

    /// Send an audio frame for playback.
    pub async fn play_frame(&self, frame: Vec<i16>) -> Result<(), String> {
        self.tx
            .send(frame)
            .await
            .map_err(|_| "Playback channel closed".to_string())
    }
}

/// Find a supported output sample rate and channel count for the device.
/// Prefers the requested rate+channels; falls back to standard rates and any channel count.
fn find_supported_output_config(device: &cpal::Device, requested: u32, channels: u16) -> Option<(u32, u16)> {
    let configs: Vec<_> = device.supported_output_configs().ok()?.collect();
    let standard_rates = [44100u32, 48000, 16000, 8000, 96000];

    // 1. Exact match: requested rate + requested channels
    for cfg in &configs {
        if cfg.channels() == channels
            && cfg.min_sample_rate().0 <= requested
            && cfg.max_sample_rate().0 >= requested
        {
            return Some((requested, channels));
        }
    }
    // 2. Requested rate, any channel count
    for cfg in &configs {
        if cfg.min_sample_rate().0 <= requested && cfg.max_sample_rate().0 >= requested {
            return Some((requested, cfg.channels()));
        }
    }
    // 3. Standard rate + requested channels
    for &rate in &standard_rates {
        for cfg in &configs {
            if cfg.channels() == channels
                && cfg.min_sample_rate().0 <= rate
                && cfg.max_sample_rate().0 >= rate
            {
                return Some((rate, channels));
            }
        }
    }
    // 4. Standard rate, any channel count
    for &rate in &standard_rates {
        for cfg in &configs {
            if cfg.min_sample_rate().0 <= rate && cfg.max_sample_rate().0 >= rate {
                return Some((rate, cfg.channels()));
            }
        }
    }
    None
}

/// Find a supported input sample rate for the device.
fn find_supported_input_rate(device: &cpal::Device, requested: u32, channels: u16) -> Option<u32> {
    let configs: Vec<_> = device.supported_input_configs().ok()?.collect();
    for cfg in &configs {
        if cfg.channels() == channels
            && cfg.min_sample_rate().0 <= requested
            && cfg.max_sample_rate().0 >= requested
        {
            return Some(requested);
        }
    }
    let standard_rates = [44100u32, 48000, 16000, 8000, 96000];
    for &rate in &standard_rates {
        for cfg in &configs {
            if cfg.channels() == channels
                && cfg.min_sample_rate().0 <= rate
                && cfg.max_sample_rate().0 >= rate
            {
                return Some(rate);
            }
        }
    }
    for &rate in &standard_rates {
        for cfg in &configs {
            if cfg.min_sample_rate().0 <= rate && cfg.max_sample_rate().0 >= rate {
                return Some(rate);
            }
        }
    }
    None
}

/// Duplicate mono samples to fill multiple channels (e.g., mono→stereo).
fn mono_to_multi(samples: &[i16], channels: u16) -> Vec<i16> {
    let ch = channels as usize;
    let mut out = Vec::with_capacity(samples.len() * ch);
    for &s in samples {
        for _ in 0..ch {
            out.push(s);
        }
    }
    out
}

/// Linear interpolation resampling from src_rate to dst_rate.
fn resample_linear(samples: &[i16], src_rate: f64, dst_rate: f64) -> Vec<i16> {
    if samples.is_empty() {
        return Vec::new();
    }
    let ratio = src_rate / dst_rate;
    let out_len = ((samples.len() as f64) / ratio).ceil() as usize;
    let mut output = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 * ratio;
        let idx = src_pos as usize;
        let frac = src_pos - idx as f64;
        let sample = if idx + 1 < samples.len() {
            let a = samples[idx] as f64;
            let b = samples[idx + 1] as f64;
            (a + frac * (b - a)) as i16
        } else {
            samples[samples.len() - 1]
        };
        output.push(sample);
    }
    output
}
