// src/core/config.rs

// ── ID Newtypes ──────────────────────────────────────────────────────────

/// Opaque identifier for an audio device.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceId(pub String);

impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Opaque identifier for an application (audio session).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApplicationId(pub String);

impl std::fmt::Display for ApplicationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Opaque identifier for an OS process (PID).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProcessId(pub u32);

impl std::fmt::Display for ProcessId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ── CaptureTarget ────────────────────────────────────────────────────────

/// Unified capture target model covering all capture modes.
///
/// This enum specifies *what* audio should be captured. It replaces the old
/// combination of `DeviceSelector` + PID/session fields with a single,
/// explicit discriminated union.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CaptureTarget {
    /// Capture from the system default audio device / mix.
    SystemDefault,
    /// Capture from a specific device identified by [`DeviceId`].
    Device(DeviceId),
    /// Capture audio from a specific application session identified by [`ApplicationId`].
    Application(ApplicationId),
    /// Capture audio from the first application whose name matches the given string.
    ApplicationByName(String),
    /// Capture audio from a process and its child processes, identified by [`ProcessId`].
    ProcessTree(ProcessId),
}

impl Default for CaptureTarget {
    fn default() -> Self {
        CaptureTarget::SystemDefault
    }
}

// ── SampleFormat (new canonical 4-variant) ───────────────────────────────

/// Specifies the format of audio samples.
///
/// All audio data is standardized to `f32` internally, but this enum
/// describes the wire/storage format for configuration and capability
/// negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleFormat {
    /// Signed 16-bit integer.
    I16,
    /// Signed 24-bit integer (packed in 32-bit container).
    I24,
    /// Signed 32-bit integer.
    I32,
    /// 32-bit IEEE 754 floating-point.
    F32,
}

impl Default for SampleFormat {
    /// Returns `F32` — the library's internal standard format.
    fn default() -> Self {
        SampleFormat::F32
    }
}

impl SampleFormat {
    /// Returns the number of bits per sample for this format.
    pub fn bits_per_sample(&self) -> u16 {
        match self {
            SampleFormat::I16 => 16,
            SampleFormat::I24 => 24,
            SampleFormat::I32 | SampleFormat::F32 => 32,
        }
    }
}

// ── AudioFormat ──────────────────────────────────────────────────────────

/// Represents a concrete audio format describing sample rate, channels,
/// and sample type.
///
/// Used by [`AudioBuffer`](super::buffer::AudioBuffer) and the
/// [`AudioDevice`](super::interface::AudioDevice) trait to describe
/// the actual format of audio data.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AudioFormat {
    /// The number of samples per second (e.g., 44100, 48000).
    pub sample_rate: u32,
    /// The number of audio channels (e.g., 1 for mono, 2 for stereo).
    pub channels: u16,
    /// The specific sample format.
    pub sample_format: SampleFormat,
}

impl Default for AudioFormat {
    /// Provides a default `AudioFormat`: 48 kHz, stereo, F32.
    fn default() -> Self {
        AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
        }
    }
}

// ── StreamConfig ─────────────────────────────────────────────────────────

/// Configuration for an audio stream.
///
/// Specifies the desired audio format and buffer size when opening a stream.
/// This is a simplified, flat representation — no nested `AudioFormat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamConfig {
    /// The desired sample rate in Hz (e.g., 48000).
    pub sample_rate: u32,
    /// The desired number of audio channels (e.g., 2 for stereo).
    pub channels: u16,
    /// The desired sample format.
    pub sample_format: SampleFormat,
    /// Optional desired buffer size in frames.
    /// If `None`, the backend will choose a suitable default.
    pub buffer_size: Option<usize>,
}

impl Default for StreamConfig {
    /// Provides a default `StreamConfig`: 48 kHz, 2 channels, F32, no buffer size preference.
    fn default() -> Self {
        StreamConfig {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
            buffer_size: None,
        }
    }
}

impl StreamConfig {
    /// Converts this `StreamConfig` into a corresponding [`AudioFormat`].
    pub fn to_audio_format(&self) -> AudioFormat {
        AudioFormat {
            sample_rate: self.sample_rate,
            channels: self.channels,
            sample_format: self.sample_format,
        }
    }
}

// ── AudioCaptureConfig ───────────────────────────────────────────────────

/// Full configuration for an audio capture session.
///
/// Created by [`AudioCaptureBuilder`](crate::api::AudioCaptureBuilder),
/// this struct stores the validated capture parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioCaptureConfig {
    /// What to capture — system default, specific device, application, etc.
    pub target: CaptureTarget,
    /// Stream format and buffer configuration.
    pub stream_config: StreamConfig,
}

impl Default for AudioCaptureConfig {
    fn default() -> Self {
        AudioCaptureConfig {
            target: CaptureTarget::default(),
            stream_config: StreamConfig::default(),
        }
    }
}

// ── Legacy / Compatibility Types ─────────────────────────────────────────

/// Defines preferred latency modes for an audio stream.
///
/// Backends will attempt to honor this preference, but actual latency
/// may vary based on system capabilities and load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LatencyMode {
    /// Prioritizes the lowest possible latency.
    LowLatency,
    /// Aims for a balance (default).
    Balanced,
    /// Prioritizes lower power consumption.
    PowerSaving,
}

impl Default for LatencyMode {
    fn default() -> Self {
        LatencyMode::Balanced
    }
}

/// Specifies criteria for selecting an audio device.
///
/// **Deprecated** — prefer [`CaptureTarget`] for new code.
/// Retained for backward compatibility during the API transition.
#[deprecated(note = "Use CaptureTarget instead")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DeviceSelector {
    /// Selects the system's default input device.
    DefaultInput,
    /// Selects the system's default output device.
    DefaultOutput,
    /// Selects a device by its platform-specific identifier.
    ById(String),
    /// Selects a device by name (first match).
    ByName(String),
}

#[allow(deprecated)]
impl Default for DeviceSelector {
    fn default() -> Self {
        DeviceSelector::DefaultInput
    }
}

#[allow(deprecated)]
impl std::fmt::Display for DeviceSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceSelector::DefaultInput => write!(f, "DefaultInput"),
            DeviceSelector::DefaultOutput => write!(f, "DefaultOutput"),
            DeviceSelector::ById(id) => write!(f, "ById({})", id),
            DeviceSelector::ByName(name) => write!(f, "ByName({})", name),
        }
    }
}

/// Specifies the audio file format for recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioFileFormat {
    /// Waveform Audio File Format.
    #[default]
    Wav,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── DeviceId ─────────────────────────────────────────────────────

    #[test]
    fn device_id_construction_and_display() {
        let id = DeviceId("hw:0,0".to_string());
        assert_eq!(id.to_string(), "hw:0,0");
    }

    #[test]
    fn device_id_clone_and_eq() {
        let id = DeviceId("device-1".to_string());
        let cloned = id.clone();
        assert_eq!(id, cloned);
    }

    #[test]
    fn device_id_inequality() {
        let a = DeviceId("a".to_string());
        let b = DeviceId("b".to_string());
        assert_ne!(a, b);
    }

    #[test]
    fn device_id_debug() {
        let id = DeviceId("test".to_string());
        let dbg = format!("{:?}", id);
        assert!(dbg.contains("DeviceId"));
        assert!(dbg.contains("test"));
    }

    // ── ApplicationId ────────────────────────────────────────────────

    #[test]
    fn application_id_construction_and_display() {
        let id = ApplicationId("com.example.app".to_string());
        assert_eq!(id.to_string(), "com.example.app");
    }

    #[test]
    fn application_id_clone_and_eq() {
        let id = ApplicationId("session-42".to_string());
        let cloned = id.clone();
        assert_eq!(id, cloned);
    }

    #[test]
    fn application_id_debug() {
        let id = ApplicationId("app".to_string());
        let dbg = format!("{:?}", id);
        assert!(dbg.contains("ApplicationId"));
    }

    // ── ProcessId ────────────────────────────────────────────────────

    #[test]
    fn process_id_construction_and_display() {
        let pid = ProcessId(12345);
        assert_eq!(pid.to_string(), "12345");
    }

    #[test]
    fn process_id_clone_and_eq() {
        let pid = ProcessId(42);
        let cloned = pid.clone();
        assert_eq!(pid, cloned);
    }

    #[test]
    fn process_id_debug() {
        let pid = ProcessId(1);
        let dbg = format!("{:?}", pid);
        assert!(dbg.contains("ProcessId"));
        assert!(dbg.contains("1"));
    }

    // ── CaptureTarget ────────────────────────────────────────────────

    #[test]
    fn capture_target_default_is_system_default() {
        assert_eq!(CaptureTarget::default(), CaptureTarget::SystemDefault);
    }

    #[test]
    fn capture_target_all_variants_constructible() {
        let _ = CaptureTarget::SystemDefault;
        let _ = CaptureTarget::Device(DeviceId("d".to_string()));
        let _ = CaptureTarget::Application(ApplicationId("a".to_string()));
        let _ = CaptureTarget::ApplicationByName("Firefox".to_string());
        let _ = CaptureTarget::ProcessTree(ProcessId(1));
    }

    #[test]
    fn capture_target_clone() {
        let target = CaptureTarget::Device(DeviceId("hw:1".to_string()));
        let cloned = target.clone();
        assert_eq!(target, cloned);
    }

    #[test]
    fn capture_target_eq_same_variant() {
        let a = CaptureTarget::ApplicationByName("Spotify".to_string());
        let b = CaptureTarget::ApplicationByName("Spotify".to_string());
        assert_eq!(a, b);
    }

    #[test]
    fn capture_target_ne_different_variant() {
        assert_ne!(
            CaptureTarget::SystemDefault,
            CaptureTarget::ProcessTree(ProcessId(1))
        );
    }

    #[test]
    fn capture_target_debug() {
        let dbg = format!("{:?}", CaptureTarget::SystemDefault);
        assert!(dbg.contains("SystemDefault"));
    }

    // ── SampleFormat ─────────────────────────────────────────────────

    #[test]
    fn sample_format_default_is_f32() {
        assert_eq!(SampleFormat::default(), SampleFormat::F32);
    }

    #[test]
    fn sample_format_bits_per_sample() {
        assert_eq!(SampleFormat::I16.bits_per_sample(), 16);
        assert_eq!(SampleFormat::I24.bits_per_sample(), 24);
        assert_eq!(SampleFormat::I32.bits_per_sample(), 32);
        assert_eq!(SampleFormat::F32.bits_per_sample(), 32);
    }

    #[test]
    fn sample_format_copy() {
        let fmt = SampleFormat::I16;
        let copied = fmt; // Copy — not move
        assert_eq!(fmt, copied);
    }

    #[test]
    fn sample_format_all_variants_eq() {
        assert_eq!(SampleFormat::I16, SampleFormat::I16);
        assert_eq!(SampleFormat::I24, SampleFormat::I24);
        assert_eq!(SampleFormat::I32, SampleFormat::I32);
        assert_eq!(SampleFormat::F32, SampleFormat::F32);
        assert_ne!(SampleFormat::I16, SampleFormat::F32);
    }

    // ── AudioFormat ──────────────────────────────────────────────────

    #[test]
    fn audio_format_default() {
        let fmt = AudioFormat::default();
        assert_eq!(fmt.sample_rate, 48000);
        assert_eq!(fmt.channels, 2);
        assert_eq!(fmt.sample_format, SampleFormat::F32);
    }

    #[test]
    fn audio_format_custom_construction() {
        let fmt = AudioFormat {
            sample_rate: 44100,
            channels: 1,
            sample_format: SampleFormat::I16,
        };
        assert_eq!(fmt.sample_rate, 44100);
        assert_eq!(fmt.channels, 1);
        assert_eq!(fmt.sample_format, SampleFormat::I16);
    }

    #[test]
    fn audio_format_clone_and_eq() {
        let fmt = AudioFormat {
            sample_rate: 96000,
            channels: 8,
            sample_format: SampleFormat::I32,
        };
        let cloned = fmt.clone();
        assert_eq!(fmt, cloned);
    }

    // ── StreamConfig ─────────────────────────────────────────────────

    #[test]
    fn stream_config_default() {
        let cfg = StreamConfig::default();
        assert_eq!(cfg.sample_rate, 48000);
        assert_eq!(cfg.channels, 2);
        assert_eq!(cfg.sample_format, SampleFormat::F32);
        assert_eq!(cfg.buffer_size, None);
    }

    #[test]
    fn stream_config_to_audio_format() {
        let cfg = StreamConfig {
            sample_rate: 44100,
            channels: 6,
            sample_format: SampleFormat::I24,
            buffer_size: Some(1024),
        };
        let fmt = cfg.to_audio_format();
        assert_eq!(fmt.sample_rate, 44100);
        assert_eq!(fmt.channels, 6);
        assert_eq!(fmt.sample_format, SampleFormat::I24);
    }

    #[test]
    fn stream_config_to_audio_format_default_roundtrip() {
        let cfg = StreamConfig::default();
        let fmt = cfg.to_audio_format();
        assert_eq!(fmt, AudioFormat::default());
    }

    #[test]
    fn stream_config_custom_with_buffer_size() {
        let cfg = StreamConfig {
            sample_rate: 22050,
            channels: 1,
            sample_format: SampleFormat::I16,
            buffer_size: Some(512),
        };
        assert_eq!(cfg.buffer_size, Some(512));
    }

    // ── AudioCaptureConfig ───────────────────────────────────────────

    #[test]
    fn audio_capture_config_default() {
        let cfg = AudioCaptureConfig::default();
        assert_eq!(cfg.target, CaptureTarget::SystemDefault);
        assert_eq!(cfg.stream_config, StreamConfig::default());
    }

    #[test]
    fn audio_capture_config_custom() {
        let cfg = AudioCaptureConfig {
            target: CaptureTarget::ProcessTree(ProcessId(999)),
            stream_config: StreamConfig {
                sample_rate: 96000,
                channels: 2,
                sample_format: SampleFormat::F32,
                buffer_size: Some(2048),
            },
        };
        assert_eq!(cfg.target, CaptureTarget::ProcessTree(ProcessId(999)));
        assert_eq!(cfg.stream_config.sample_rate, 96000);
        assert_eq!(cfg.stream_config.buffer_size, Some(2048));
    }

    #[test]
    fn audio_capture_config_clone_and_eq() {
        let cfg = AudioCaptureConfig {
            target: CaptureTarget::ApplicationByName("VLC".to_string()),
            stream_config: StreamConfig::default(),
        };
        let cloned = cfg.clone();
        assert_eq!(cfg, cloned);
    }

    // ── Edge cases ───────────────────────────────────────────────────

    #[test]
    fn device_id_empty_string() {
        let id = DeviceId(String::new());
        assert_eq!(id.to_string(), "");
    }

    #[test]
    fn process_id_zero() {
        let pid = ProcessId(0);
        assert_eq!(pid.to_string(), "0");
    }
}
