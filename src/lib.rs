pub mod api; // Added api module
pub mod audio;
pub mod core; // Added core module
#[path = "../tests/mod.rs"]
pub mod tests;
pub mod utils;

// Re-export trait-based API
pub use audio::{
    get_audio_backend,     // Old API
    get_device_enumerator, // New API
    AudioApplication,      // Old API
    AudioCaptureBackend,   // Old API
    AudioCaptureStream,    // Old API
};
// Core type re-exports
pub use crate::core::buffer::VecAudioBuffer; // Added re-export for VecAudioBuffer
pub use crate::core::config::{
    AudioFileFormat, AudioFormat, DeviceSelector, LatencyMode, SampleFormat, StreamConfig,
}; // Explicitly re-export StreamConfig and AudioFileFormat
pub use crate::core::error::{AudioError, Result as CoreAudioResult}; // Alias core::error::Result
pub use crate::core::interface::{
    AudioBuffer,
    AudioDevice,
    AudioStream,
    DeviceEnumerator,
    DeviceKind,
    StreamDataCallback,
    // SampleType is in core::interface but also SampleFormat in core::config. Clarify if needed.
    // For now, keeping SampleType from interface as it's used by AudioBuffer trait.
    // SampleFormat from config is more detailed for format specification.
};
// Re-exporting SampleType from interface.rs as it's used by AudioBuffer.
// SampleFormat from config.rs is for detailed format specification.
pub use crate::core::interface::SampleType;

// Re-export new API types
pub use crate::api::{AudioCapture, AudioCaptureBuilder, AudioCaptureConfig};

// Re-export platform-specific components for the new API
#[cfg(target_os = "linux")]
pub use audio::LinuxDeviceEnumerator;
#[cfg(target_os = "macos")]
pub use audio::MacosDeviceEnumerator;
#[cfg(target_os = "windows")]
pub use audio::WindowsDeviceEnumerator;

// Re-export old platform-specific backends (to be deprecated)
#[cfg(target_os = "macos")]
pub use audio::CoreAudioBackend; // Assuming this was the old name
#[cfg(target_os = "linux")]
pub use audio::PipeWireBackend;
#[cfg(target_os = "windows")]
pub use audio::WasapiBackend;

// Re-export ProcessAudioCapture API (Windows-only)
#[cfg(target_os = "windows")]
pub use audio::{AudioCaptureError, ProcessAudioCapture};

// Re-export test utils if the feature is enabled
#[cfg(feature = "test-utils")]
pub use utils::test_utils;

/// Error type for the library
pub type Error = color_eyre::Report;
/// Result type for the library
pub type Result<T> = std::result::Result<T, Error>;

/// Initialize the library
pub fn init() -> Result<()> {
    color_eyre::install()?;
    Ok(())
}
