//! The `audio` module serves as the primary facade for audio operations.
//! It conditionally compiles and exposes platform-specific implementations
//! based on the `target_os` compilation flag.
//!
//! For each supported platform (Windows, Linux, macOS), there's a corresponding
//! module (`windows`, `linux`, `macos`) that implements the core audio traits
//! defined in `crate::core::interface`.
//!
//! The main way to interact with platform-specific audio capabilities is by
//! obtaining a `DeviceEnumerator` through the `get_device_enumerator()` function.

// Conditional module declarations
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

// Deprecated/Old API components - to be removed or refactored
mod capture; // Keep for now if ProcessAudioCapture is still used
pub mod core; // This seems to be the old core, distinct from crate::core
pub use self::core::{AudioApplication, AudioCaptureBackend, AudioCaptureStream};
#[cfg(target_os = "windows")]
pub use capture::{AudioCaptureError, ProcessAudioCapture};

// --- New Trait-Based API Exports ---

// Re-export platform-specific DeviceEnumerators
#[cfg(target_os = "linux")]
pub use linux::LinuxDeviceEnumerator;
#[cfg(target_os = "macos")]
pub use macos::MacosDeviceEnumerator;
#[cfg(target_os = "windows")]
pub use windows::WindowsDeviceEnumerator;

// Re-export platform-specific AudioDevice and AudioStream types if they need to be named directly.
// Usually, interaction will be through the traits.
#[cfg(target_os = "linux")]
pub use linux::LinuxAudioBackend;
#[cfg(target_os = "macos")]
pub use macos::{
    enumerate_audio_applications, ApplicationInfo, MacosAudioDevice, MacosAudioStream,
};
#[cfg(target_os = "windows")]
pub use windows::{
    enumerate_application_audio_sessions, ApplicationAudioSessionInfo, WindowsAudioDevice,
    WindowsAudioStream,
};

// --- Factory function for the new DeviceEnumerator ---

use crate::core::error::AudioError;
use crate::core::interface::DeviceEnumerator; // Import the trait itself // For error handling

/// Returns a platform-specific implementation of `DeviceEnumerator`.
///
/// This function inspects the `target_os` at compile time and provides the
/// appropriate enumerator for the current platform.
///
/// # Returns
/// A `Result` containing a boxed `DeviceEnumerator` for the current platform,
/// or an `AudioError::UnsupportedPlatform` if the OS is not supported.
pub fn get_device_enumerator() -> Result<
    Box<dyn DeviceEnumerator<Device = impl crate::core::interface::AudioDevice + 'static>>,
    AudioError,
> {
    #[cfg(target_os = "windows")]
    {
        // WindowsDeviceEnumerator itself is the concrete type.
        // If it needs initialization that can fail, it should have a `new()` method returning Result.
        Ok(Box::new(windows::WindowsDeviceEnumerator))
    }
    #[cfg(target_os = "linux")]
    {
        // LinuxDeviceEnumerator itself is the concrete type.
        Ok(Box::new(linux::LinuxDeviceEnumerator))
    }
    #[cfg(target_os = "macos")]
    {
        // MacosDeviceEnumerator itself is the concrete type.
        Ok(Box::new(macos::MacosDeviceEnumerator))
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        Err(AudioError::UnsupportedPlatform(
            "This operating system is not supported for audio capture.".to_string(),
        ))
    }
}

// --- Old Backend Factory (to be deprecated/removed) ---

// Re-export old platform-specific backends (for now)
#[cfg(target_os = "linux")]
pub use linux::PipeWireBackend; // Old backend
#[cfg(target_os = "macos")]
pub use macos::CoreAudioBackend; // Assuming this was the old macOS backend name
#[cfg(target_os = "windows")]
pub use windows::WasapiBackend; // Old backend

/// Returns a platform-specific implementation of the (old) `AudioCaptureBackend`.
/// **Note:** This function is part of the older API and will be deprecated.
/// Use `get_device_enumerator()` for the new trait-based API.
pub fn get_audio_backend() -> crate::core::error::Result<Box<dyn AudioCaptureBackend>> {
    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(windows::WasapiBackend::new()?))
    }
    #[cfg(target_os = "linux")]
    {
        // Assuming PipeWireBackend::is_available() and new() are part of the old API
        if linux::PipeWireBackend::is_available() {
            Ok(Box::new(linux::PipeWireBackend::new()?))
        } else {
            Err(AudioError::BackendError(
                // Corrected error variant
                "PipeWire is not available. Audio capture is not supported on this system."
                    .to_string(),
            ))
        }
    }
    #[cfg(target_os = "macos")]
    {
        // Assuming CoreAudioBackend::new() was the old way
        // This will likely cause an error if CoreAudioBackend doesn't exist or have a `new` method.
        // For now, let's assume it might exist in macos.rs from a previous structure.
        // If not, this part needs to be removed or adapted.
        // Ok(Box::new(macos::CoreAudioBackend::new()?))
        Err(AudioError::BackendError(
            "Old macOS backend not yet adapted for this example.".to_string(),
        ))
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        Err(AudioError::BackendError(
            // Corrected error variant
            "Unsupported operating system".to_string(),
        ))
    }
}
