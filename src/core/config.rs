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
