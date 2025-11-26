pub mod api; // Added api module
pub mod audio;
pub mod core; // Added core module
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
pub use crate::core::buffer::AudioBuffer; // Changed to re-export the new AudioBuffer struct
pub use crate::core::config::{
    AudioFileFormat, AudioFormat, DeviceSelector, LatencyMode, SampleFormat, StreamConfig,
}; // Explicitly re-export StreamConfig and AudioFileFormat
pub use crate::core::error::{AudioError, ProcessError, Result as CoreAudioResult}; // Added ProcessError, Alias core::error::Result
pub use crate::core::interface::{
    // AudioBuffer trait removed from interface, struct is re-exported from core::buffer
    AudioDevice,
    AudioStream,
    CapturingStream, // Added CapturingStream
    DeviceEnumerator,
    DeviceKind,
    SampleType, // SampleType is still relevant for some generic contexts if used
    StreamDataCallback,
};
pub use crate::core::processing::AudioProcessor; // Added AudioProcessor re-export

// Re-export new API types
pub use crate::api::{AudioCapture, AudioCaptureBuilder, AudioCaptureConfig};

// Re-export platform-specific components for the new API
#[cfg(all(target_os = "linux", feature = "feat_linux"))]
pub use audio::LinuxDeviceEnumerator;
#[cfg(all(target_os = "windows", feature = "feat_windows"))]
pub use audio::{
    enumerate_application_audio_sessions, ApplicationAudioSessionInfo, WindowsDeviceEnumerator,
};
#[cfg(all(target_os = "macos", feature = "feat_macos"))]
pub use audio::{enumerate_audio_applications, ApplicationInfo, MacosDeviceEnumerator};

// Re-export old platform-specific backends (to be deprecated)
#[cfg(all(target_os = "macos", feature = "feat_macos"))]
pub use audio::CoreAudioBackend; // Assuming this was the old name
#[cfg(all(target_os = "linux", feature = "feat_linux"))]
pub use audio::PipeWireBackend;
#[cfg(all(target_os = "windows", feature = "feat_windows"))]
pub use audio::WasapiBackend;

// Re-export ProcessAudioCapture API (Windows-only)
#[cfg(all(target_os = "windows", feature = "feat_windows"))]
pub use audio::{AudioCaptureError, ProcessAudioCapture};

// Re-export test utils if the feature is enabled
#[cfg(feature = "test-utils")]
pub use utils::test_utils;

// Re-export CapturingStream from core::interface directly for convenience

/// Error type for the library
pub type Error = color_eyre::Report;
/// Result type for the library
pub type Result<T> = std::result::Result<T, Error>;

/// Initialize the library
pub fn init() -> Result<()> {
    color_eyre::install()?;
    Ok(())
}
