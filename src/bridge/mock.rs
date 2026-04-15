//! Mock audio backend for testing without platform audio hardware.
//!
//! Provides [`MockAudioDevice`], [`MockDeviceEnumerator`], and a synthetic audio
//! producer thread that generates a 440 Hz sine wave through the real
//! `BridgeProducer → BridgeStream` pipeline. This exercises the full data plane
//! (ring buffer, state machine, `CapturingStream` trait) on any platform,
//! without requiring OS audio services.
//!
//! # Usage
//!
//! Available behind `#[cfg(any(test, feature = "test-utils"))]`.
//!
//! ```rust,ignore
//! use rsac::bridge::mock::{MockDeviceEnumerator, MockAudioDevice};
//! use rsac::core::interface::{DeviceEnumerator, AudioDevice};
//!
//! let enumerator = MockDeviceEnumerator::new();
//! let device = enumerator.default_device().unwrap();
//! let stream = device.create_stream(&StreamConfig::default()).unwrap();
//!
//! // Stream delivers 440 Hz sine wave buffers
//! let buffer = stream.read_chunk().unwrap();
//! assert!(buffer.data().iter().any(|&s| s.abs() > 0.01)); // non-silence
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::bridge::ring_buffer::create_bridge;
use crate::bridge::state::StreamState;
use crate::bridge::stream::{BridgeStream, PlatformStream};
use crate::core::buffer::AudioBuffer;
use crate::core::config::{AudioFormat, DeviceId, SampleFormat, StreamConfig};
use crate::core::error::{AudioError, AudioResult};
use crate::core::interface::{AudioDevice, CapturingStream, DeviceEnumerator};

// ── MockPlatformStream ──────────────────────────────────────────────────

/// Mock platform stream that manages a synthetic audio producer thread.
///
/// When stopped, signals the producer thread to terminate.
pub(crate) struct MockPlatformStream {
    active: Arc<AtomicBool>,
    producer_thread: Option<std::thread::JoinHandle<()>>,
}

impl PlatformStream for MockPlatformStream {
    fn stop_capture(&self) -> AudioResult<()> {
        self.active.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}

impl Drop for MockPlatformStream {
    fn drop(&mut self) {
        self.active.store(false, Ordering::SeqCst);
        if let Some(handle) = self.producer_thread.take() {
            let _ = handle.join();
        }
    }
}

// ── Sine Wave Generator ─────────────────────────────────────────────────

/// Generates interleaved stereo samples of a sine wave at the given frequency.
fn generate_sine_buffer(
    frequency: f32,
    sample_rate: u32,
    channels: u16,
    frames_per_buffer: usize,
    phase: &mut f32,
) -> Vec<f32> {
    let total_samples = frames_per_buffer * channels as usize;
    let mut data = Vec::with_capacity(total_samples);
    let phase_increment = 2.0 * std::f32::consts::PI * frequency / sample_rate as f32;

    for _ in 0..frames_per_buffer {
        let sample = 0.5 * phase.sin(); // 50% amplitude to avoid clipping
        for _ in 0..channels {
            data.push(sample);
        }
        *phase += phase_increment;
        if *phase > 2.0 * std::f32::consts::PI {
            *phase -= 2.0 * std::f32::consts::PI;
        }
    }
    data
}

// ── MockAudioDevice ─────────────────────────────────────────────────────

/// A mock audio device that creates a `BridgeStream` with a synthetic producer.
///
/// The producer thread generates a 440 Hz sine wave at the requested sample rate
/// and pushes buffers through the real ring buffer bridge. The stream behaves
/// identically to a real platform stream from the consumer's perspective.
pub struct MockAudioDevice {
    id: DeviceId,
    name: String,
    is_default: bool,
    /// Frequency of the generated sine wave (default: 440 Hz).
    pub frequency: f32,
}

impl MockAudioDevice {
    /// Creates a new mock audio device with the given name.
    pub fn new(name: &str) -> Self {
        Self {
            id: DeviceId("mock-device-0".to_string()),
            name: name.to_string(),
            is_default: true,
            frequency: 440.0,
        }
    }

    /// Sets the frequency of the generated sine wave.
    pub fn with_frequency(mut self, freq: f32) -> Self {
        self.frequency = freq;
        self
    }
}

impl AudioDevice for MockAudioDevice {
    fn id(&self) -> DeviceId {
        self.id.clone()
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn is_default(&self) -> bool {
        self.is_default
    }

    fn supported_formats(&self) -> Vec<AudioFormat> {
        vec![
            AudioFormat {
                sample_rate: 44100,
                channels: 2,
                sample_format: SampleFormat::F32,
            },
            AudioFormat {
                sample_rate: 48000,
                channels: 2,
                sample_format: SampleFormat::F32,
            },
        ]
    }

    fn create_stream(&self, config: &StreamConfig) -> AudioResult<Box<dyn CapturingStream>> {
        let sample_rate = config.sample_rate;
        let channels = config.channels;
        let format = config.to_audio_format();

        // 10ms buffer size = sample_rate / 100 frames
        let frames_per_buffer = (sample_rate / 100) as usize;
        let ring_capacity = 32; // ~320ms of buffering at 10ms per buffer

        let (mut producer, consumer) = create_bridge(ring_capacity, format.clone());

        // Transition to Running
        consumer
            .shared()
            .state
            .transition(StreamState::Created, StreamState::Running)
            .map_err(|e| AudioError::StreamCreationFailed {
                reason: format!("Failed to transition mock stream to Running: {}", e),
                context: None,
            })?;

        let active = Arc::new(AtomicBool::new(true));
        let active_clone = active.clone();
        let frequency = self.frequency;

        // Spawn producer thread that generates sine wave
        let producer_thread = std::thread::Builder::new()
            .name("rsac-mock-producer".to_string())
            .spawn(move || {
                let mut phase: f32 = 0.0;
                let buffer_duration = Duration::from_millis(10);

                while active_clone.load(Ordering::SeqCst) {
                    let data = generate_sine_buffer(
                        frequency,
                        sample_rate,
                        channels,
                        frames_per_buffer,
                        &mut phase,
                    );
                    let buffer = AudioBuffer::new(data, channels, sample_rate);
                    producer.push_or_drop(buffer);
                    std::thread::sleep(buffer_duration);
                }

                // Signal producer done
                producer.signal_done();
            })
            .map_err(|e| AudioError::StreamCreationFailed {
                reason: format!("Failed to spawn mock producer thread: {}", e),
                context: None,
            })?;

        let platform_stream = MockPlatformStream {
            active,
            producer_thread: Some(producer_thread),
        };

        let stream = BridgeStream::new(consumer, platform_stream, format, Duration::from_secs(5));

        Ok(Box::new(stream))
    }
}

// ── MockDeviceEnumerator ────────────────────────────────────────────────

/// A mock device enumerator that returns [`MockAudioDevice`] instances.
///
/// Useful for testing the full `AudioCapture` pipeline without platform audio.
pub struct MockDeviceEnumerator {
    /// Frequency for generated audio (default: 440 Hz).
    pub frequency: f32,
}

impl MockDeviceEnumerator {
    /// Creates a new mock device enumerator.
    pub fn new() -> Self {
        Self { frequency: 440.0 }
    }
}

impl Default for MockDeviceEnumerator {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceEnumerator for MockDeviceEnumerator {
    fn enumerate_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice>>> {
        Ok(vec![
            Box::new(MockAudioDevice::new("Mock Output (Stereo)").with_frequency(self.frequency)),
            Box::new(MockAudioDevice {
                id: DeviceId("mock-device-1".to_string()),
                name: "Mock Input (Mono)".to_string(),
                is_default: false,
                frequency: self.frequency,
            }),
        ])
    }

    fn default_device(&self) -> AudioResult<Box<dyn AudioDevice>> {
        Ok(Box::new(
            MockAudioDevice::new("Mock Output (Stereo)").with_frequency(self.frequency),
        ))
    }
}

// ── Standalone Test Helpers ─────────────────────────────────────────────

/// Creates a mock `CapturingStream` that delivers 440 Hz sine wave buffers.
///
/// This is the simplest way to get a test stream without going through
/// `AudioCaptureBuilder`. Returns the stream directly.
///
/// # Arguments
///
/// * `sample_rate` — Sample rate in Hz (e.g., 48000)
/// * `channels` — Number of channels (e.g., 2 for stereo)
pub fn create_mock_stream(
    sample_rate: u32,
    channels: u16,
) -> AudioResult<Box<dyn CapturingStream>> {
    let device = MockAudioDevice::new("Mock Test Stream");
    let config = StreamConfig {
        sample_rate,
        channels,
        sample_format: SampleFormat::F32,
        buffer_size: None,
        capture_target: crate::core::config::CaptureTarget::SystemDefault,
    };
    device.create_stream(&config)
}

/// Verifies that audio data contains non-silence (at least one sample above threshold).
///
/// Returns the RMS energy of the buffer for further analysis.
pub fn verify_non_silence(data: &[f32], threshold: f32) -> (bool, f32) {
    if data.is_empty() {
        return (false, 0.0);
    }
    let sum_squares: f32 = data.iter().map(|&s| s * s).sum();
    let rms = (sum_squares / data.len() as f32).sqrt();
    (rms > threshold, rms)
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sine_wave_generation() {
        let mut phase = 0.0f32;
        let data = generate_sine_buffer(440.0, 48000, 2, 480, &mut phase);
        assert_eq!(data.len(), 960); // 480 frames * 2 channels
        let (non_silent, rms) = verify_non_silence(&data, 0.01);
        assert!(non_silent, "Sine wave should not be silence (rms={})", rms);
    }

    #[test]
    fn test_mock_device_create_stream() {
        let device = MockAudioDevice::new("Test Device");
        let config = StreamConfig::default();
        let stream = device.create_stream(&config).unwrap();

        assert!(stream.is_running());
        assert_eq!(stream.format().sample_rate, 48000);
        assert_eq!(stream.format().channels, 2);

        // Read a few buffers
        for _ in 0..3 {
            let buffer = stream.read_chunk().unwrap();
            assert_eq!(buffer.channels(), 2);
            assert_eq!(buffer.sample_rate(), 48000);
            let (non_silent, rms) = verify_non_silence(buffer.data(), 0.01);
            assert!(
                non_silent,
                "Mock stream should deliver non-silent audio (rms={})",
                rms
            );
        }

        // Stop
        stream.stop().unwrap();
        assert!(!stream.is_running());
    }

    #[test]
    fn test_mock_enumerator() {
        let enumerator = MockDeviceEnumerator::new();
        let devices = enumerator.enumerate_devices().unwrap();
        assert_eq!(devices.len(), 2);
        assert!(devices[0].is_default());
        assert!(!devices[1].is_default());

        let default = enumerator.default_device().unwrap();
        assert_eq!(default.name(), "Mock Output (Stereo)");
    }

    #[test]
    fn test_create_mock_stream_helper() {
        let stream = create_mock_stream(48000, 2).unwrap();
        let buffer = stream.read_chunk().unwrap();
        assert_eq!(buffer.sample_rate(), 48000);
        assert_eq!(buffer.channels(), 2);
        let (non_silent, _) = verify_non_silence(buffer.data(), 0.01);
        assert!(non_silent);
        stream.stop().unwrap();
    }

    #[test]
    fn test_overrun_count_starts_at_zero() {
        let stream = create_mock_stream(48000, 2).unwrap();
        assert_eq!(stream.overrun_count(), 0);
        stream.stop().unwrap();
    }

    #[test]
    fn test_mock_stream_format() {
        let stream = create_mock_stream(44100, 1).unwrap();
        let format = stream.format();
        assert_eq!(format.sample_rate, 44100);
        assert_eq!(format.channels, 1);
        assert_eq!(format.sample_format, SampleFormat::F32);
        stream.stop().unwrap();
    }
}
