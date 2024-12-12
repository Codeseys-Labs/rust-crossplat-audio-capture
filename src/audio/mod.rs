mod capture;
mod core;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

// Re-export trait-based API
pub use core::{
    AudioApplication, AudioCaptureBackend, AudioCaptureStream, AudioConfig, AudioError, AudioFormat,
};

// Re-export platform-specific backends
#[cfg(target_os = "linux")]
pub use linux::{PipeWireBackend, PulseAudioBackend};
#[cfg(target_os = "macos")]
pub use macos::CoreAudioBackend;
#[cfg(target_os = "windows")]
pub use windows::WasapiBackend;

// Re-export ProcessAudioCapture API
#[cfg(target_os = "windows")]
pub use capture::{AudioCaptureError, ProcessAudioCapture};

pub fn get_audio_backend() -> Result<Box<dyn AudioCaptureBackend>, AudioError> {
    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(windows::WasapiBackend::new()?))
    }
    #[cfg(target_os = "linux")]
    {
        if linux::PulseAudioBackend::is_available() {
            Ok(Box::new(linux::PulseAudioBackend::new()?))
        } else if linux::PipeWireBackend::is_available() {
            Ok(Box::new(linux::PipeWireBackend::new()?))
        } else {
            Err(AudioError::BackendUnavailable(
                "No supported audio backend available",
            ))
        }
    }
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(macos::CoreAudioBackend::new()?))
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        Err(AudioError::BackendUnavailable(
            "Unsupported operating system",
        ))
    }
}
