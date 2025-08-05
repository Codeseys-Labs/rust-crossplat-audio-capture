// src/core/config.rs

/// Specifies the format of audio samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleFormat {
    /// Signed 8-bit integer.
    S8,
    /// Unsigned 8-bit integer.
    U8,
    /// Signed 16-bit integer, little-endian.
    S16LE,
    /// Signed 16-bit integer, big-endian.
    S16BE,
    /// Unsigned 16-bit integer, little-endian.
    U16LE,
    /// Unsigned 16-bit integer, big-endian.
    U16BE,
    /// Signed 24-bit integer (often packed in 32 bits), little-endian.
    S24LE,
    /// Signed 24-bit integer (often packed in 32 bits), big-endian.
    S24BE,
    /// Unsigned 24-bit integer (often packed in 32 bits), little-endian.
    U24LE,
    /// Unsigned 24-bit integer (often packed in 32 bits), big-endian.
    U24BE,
    /// Signed 32-bit integer, little-endian.
    S32LE,
    /// Signed 32-bit integer, big-endian.
    S32BE,
    /// Unsigned 32-bit integer, little-endian.
    U32LE,
    /// Unsigned 32-bit integer, big-endian.
    U32BE,
    /// 32-bit floating-point, little-endian.
    F32LE,
    /// 32-bit floating-point, big-endian.
    F32BE,
    /// 64-bit floating-point, little-endian.
    F64LE,
    /// 64-bit floating-point, big-endian.
    F64BE,
}

impl Default for SampleFormat {
    /// Returns a default sample format, typically S16LE.
    fn default() -> Self {
        SampleFormat::S16LE
    }
}

/// Represents an audio format, detailing sample rate, channels, and sample type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AudioFormat {
    /// The number of samples per second (e.g., 44100, 48000).
    pub sample_rate: u32,
    /// The number of audio channels (e.g., 1 for mono, 2 for stereo).
    pub channels: u16,
    /// The number of bits per sample (e.g., 16, 24, 32).
    /// This should correspond to the `SampleFormat`.
    pub bits_per_sample: u16,
    /// The specific type and endianness of samples.
    pub sample_format: SampleFormat,
}

impl Default for AudioFormat {
    /// Provides a default `AudioFormat`.
    ///
    /// - Sample Rate: 44100 Hz
    /// - Channels: 2 (stereo)
    /// - Bits Per Sample: 16
    /// - Sample Format: S16LE (Signed 16-bit Little Endian)
    fn default() -> Self {
        AudioFormat {
            sample_rate: 44100,
            channels: 2,
            bits_per_sample: 16,
            sample_format: SampleFormat::S16LE,
        }
    }
}

/// Defines preferred latency modes for an audio stream.
///
/// Backends will attempt to honor this preference, but actual latency
/// may vary based on system capabilities and load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LatencyMode {
    /// Prioritizes the lowest possible latency, potentially at the cost of higher CPU usage or power consumption.
    LowLatency,
    /// Aims for a balance between latency, CPU usage, and power consumption. This is often the default.
    Balanced,
    /// Prioritizes lower power consumption, potentially at the cost of higher latency.
    PowerSaving,
}

impl Default for LatencyMode {
    /// Returns the default latency mode, `Balanced`.
    fn default() -> Self {
        LatencyMode::Balanced
    }
}

/// Configuration for an audio stream.
///
/// This struct specifies the desired audio format, buffer size, and latency preferences
/// when opening an audio stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamConfig {
    /// The desired audio format for the stream.
    pub format: AudioFormat,
    /// Optional: Desired buffer size in frames.
    /// If `None`, the backend will choose a suitable default.
    /// The actual buffer size used by the backend may differ from this request.
    pub buffer_size_frames: Option<u32>,
    /// Preferred latency mode for the stream.
    pub latency_mode: LatencyMode,
    // Potentially other parameters like desired latency in ms, specific device flags, etc.
}

impl Default for StreamConfig {
    /// Provides a default `StreamConfig`.
    ///
    /// - Format: Default `AudioFormat` (44.1kHz, 2ch, S16LE)
    /// - Buffer Size: `None` (backend default)
    /// - Latency Mode: `Balanced`
    fn default() -> Self {
        StreamConfig {
            format: AudioFormat::default(),
            buffer_size_frames: None,
            latency_mode: LatencyMode::default(),
        }
    }
}

/// Specifies criteria for selecting an audio device.
///
/// This enum allows users to request devices by default role, unique ID, or human-readable name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DeviceSelector {
    /// Selects the system's default input device.
    DefaultInput,
    /// Selects the system's default output device.
    DefaultOutput,
    /// Selects a device by its unique, platform-specific identifier.
    ById(String), // Assuming DeviceId can often be represented or converted to a String.
    // If DeviceId is more complex, this might need adjustment or a generic parameter.
    /// Selects a device by its human-readable name.
    /// Note: Names may not be unique; the first match is typically chosen.
    ByName(String),
}

impl Default for DeviceSelector {
    /// Returns a default device selector, `DefaultInput`.
    fn default() -> Self {
        DeviceSelector::DefaultInput
    }
}
/// Specifies the audio file format for recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioFileFormat {
    /// Waveform Audio File Format.
    #[default]
    Wav,
    // Placeholder for future formats like Mp3, Ogg, etc.
    // Mp3,
    // Ogg,
}
