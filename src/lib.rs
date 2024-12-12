pub mod audio;

// Re-export trait-based API
pub use audio::{
    get_audio_backend, AudioApplication, AudioCaptureBackend, AudioCaptureStream, AudioConfig,
    AudioError, AudioFormat,
};

// Re-export platform-specific backends
#[cfg(target_os = "macos")]
pub use audio::CoreAudioBackend;
#[cfg(target_os = "windows")]
pub use audio::WasapiBackend;
#[cfg(target_os = "linux")]
pub use audio::{PipeWireBackend, PulseAudioBackend};

// Re-export ProcessAudioCapture API (Windows-only)
#[cfg(target_os = "windows")]
pub use audio::{AudioCaptureError, ProcessAudioCapture};

/// Error type for the library
pub type Error = color_eyre::Report;
/// Result type for the library
pub type Result<T> = std::result::Result<T, Error>;

/// Initialize the library
pub fn init() -> Result<()> {
    color_eyre::install()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_library_initialization() {
        assert!(init().is_ok());
    }
}
