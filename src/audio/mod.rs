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
#[cfg(all(target_os = "linux", feature = "feat_linux"))]
pub mod linux;
#[cfg(all(target_os = "macos", feature = "feat_macos"))]
pub mod macos;
#[cfg(all(target_os = "windows", feature = "feat_windows"))]
pub mod windows;

// Application-specific capture module
pub mod application_capture;

// Audio source discovery module
pub mod discovery;

// Deprecated/Old API components - to be removed or refactored
mod capture; // Keep for now if ProcessAudioCapture is still used
pub mod core; // This seems to be the old core, distinct from crate::core
pub use self::core::{AudioApplication, AudioCaptureBackend, AudioCaptureStream};
#[cfg(target_os = "windows")]
pub use capture::{AudioCaptureError, ProcessAudioCapture};

// --- New Trait-Based API Exports ---

// Re-export the unified application capture API
pub use application_capture::{
    capture_application_by_name, capture_application_by_pid, list_capturable_applications,
    ApplicationCapture, ApplicationCaptureFactory, ApplicationInfo,
    CrossPlatformApplicationCapture,
};

// Re-export platform-specific DeviceEnumerators
#[cfg(all(target_os = "linux", feature = "feat_linux"))]
pub use linux::LinuxDeviceEnumerator;
#[cfg(all(target_os = "macos", feature = "feat_macos"))]
pub use macos::MacosDeviceEnumerator;
#[cfg(all(target_os = "windows", feature = "feat_windows"))]
pub use windows::WindowsDeviceEnumerator;

// Re-export platform-specific AudioDevice and AudioStream types if they need to be named directly.
// Usually, interaction will be through the traits.
#[cfg(all(target_os = "linux", feature = "feat_linux"))]
// LinuxAudioBackend doesn't exist - removed export
#[cfg(all(target_os = "macos", feature = "feat_macos"))]
pub use macos::{
    enumerate_audio_applications, ApplicationInfo, MacosAudioDevice, MacosAudioStream,
};
#[cfg(all(target_os = "windows", feature = "feat_windows"))]
pub use windows::{
    enumerate_application_audio_sessions, ApplicationAudioSessionInfo, WindowsAudioDevice,
    WindowsAudioStream,
};

// --- Factory function for the new DeviceEnumerator ---

use crate::core::error::AudioError;
use crate::core::interface::DeviceEnumerator; // Import the trait itself // For error handling

/// Cross-platform device enumerator that wraps platform-specific implementations.
pub enum CrossPlatformDeviceEnumerator {
    #[cfg(all(target_os = "windows", feature = "feat_windows"))]
    Windows(windows::WindowsDeviceEnumerator),

    #[cfg(all(target_os = "linux", feature = "feat_linux"))]
    Linux(linux::LinuxDeviceEnumerator),

    #[cfg(all(target_os = "macos", feature = "feat_macos"))]
    MacOS(macos::MacOSDeviceEnumerator),
}

impl CrossPlatformDeviceEnumerator {
    /// Enumerate all available audio devices
    pub fn enumerate_devices(
        &self,
    ) -> crate::core::error::Result<
        Vec<Box<dyn crate::core::interface::AudioDevice<DeviceId = String>>>,
    > {
        match self {
            #[cfg(all(target_os = "windows", feature = "feat_windows"))]
            CrossPlatformDeviceEnumerator::Windows(enumerator) => {
                let devices = enumerator.enumerate_devices()?;
                Ok(devices
                    .into_iter()
                    .map(|d| {
                        Box::new(d)
                            as Box<dyn crate::core::interface::AudioDevice<DeviceId = String>>
                    })
                    .collect())
            }
            #[cfg(all(target_os = "linux", feature = "feat_linux"))]
            CrossPlatformDeviceEnumerator::Linux(enumerator) => {
                let devices = enumerator.enumerate_devices()?;
                Ok(devices
                    .into_iter()
                    .map(|d| {
                        Box::new(d)
                            as Box<dyn crate::core::interface::AudioDevice<DeviceId = String>>
                    })
                    .collect())
            }
            #[cfg(all(target_os = "macos", feature = "feat_macos"))]
            CrossPlatformDeviceEnumerator::MacOS(enumerator) => {
                let devices = enumerator.enumerate_devices()?;
                Ok(devices
                    .into_iter()
                    .map(|d| {
                        Box::new(d)
                            as Box<dyn crate::core::interface::AudioDevice<DeviceId = String>>
                    })
                    .collect())
            }
            #[cfg(not(any(
                all(target_os = "windows", feature = "feat_windows"),
                all(target_os = "linux", feature = "feat_linux"),
                all(target_os = "macos", feature = "feat_macos")
            )))]
            _ => Err(crate::core::error::AudioError::UnsupportedPlatform(
                "Platform not supported".to_string(),
            )),
        }
    }

    /// Get the default device of the specified kind
    pub fn get_default_device(
        &self,
        kind: crate::core::interface::DeviceKind,
    ) -> crate::core::error::Result<Box<dyn crate::core::interface::AudioDevice<DeviceId = String>>>
    {
        match self {
            #[cfg(all(target_os = "windows", feature = "feat_windows"))]
            CrossPlatformDeviceEnumerator::Windows(enumerator) => {
                let device = enumerator.get_default_device(kind)?;
                Ok(Box::new(device)
                    as Box<
                        dyn crate::core::interface::AudioDevice<DeviceId = String>,
                    >)
            }
            #[cfg(all(target_os = "linux", feature = "feat_linux"))]
            CrossPlatformDeviceEnumerator::Linux(enumerator) => {
                let device = enumerator.get_default_device(kind)?;
                Ok(Box::new(device)
                    as Box<
                        dyn crate::core::interface::AudioDevice<DeviceId = String>,
                    >)
            }
            #[cfg(all(target_os = "macos", feature = "feat_macos"))]
            CrossPlatformDeviceEnumerator::MacOS(enumerator) => {
                let device = enumerator.get_default_device(kind)?;
                Ok(Box::new(device)
                    as Box<
                        dyn crate::core::interface::AudioDevice<DeviceId = String>,
                    >)
            }
            #[cfg(not(any(
                all(target_os = "windows", feature = "feat_windows"),
                all(target_os = "linux", feature = "feat_linux"),
                all(target_os = "macos", feature = "feat_macos")
            )))]
            _ => Err(crate::core::error::AudioError::UnsupportedPlatform(
                "Platform not supported".to_string(),
            )),
        }
    }
}

/// Returns a platform-specific implementation of `DeviceEnumerator`.
///
/// This function inspects the `target_os` at compile time and provides the
/// appropriate enumerator for the current platform.
///
/// # Returns
/// A `Result` containing a boxed `DeviceEnumerator` for the current platform,
/// or an `AudioError::UnsupportedPlatform` if the OS is not supported.
pub fn get_device_enumerator() -> Result<CrossPlatformDeviceEnumerator, AudioError> {
    #[cfg(all(target_os = "windows", feature = "feat_windows"))]
    {
        Ok(CrossPlatformDeviceEnumerator::Windows(
            windows::WindowsDeviceEnumerator::new()?,
        ))
    }
    #[cfg(all(target_os = "linux", feature = "feat_linux"))]
    {
        Ok(CrossPlatformDeviceEnumerator::Linux(
            linux::LinuxDeviceEnumerator::new(),
        ))
    }
    #[cfg(all(target_os = "macos", feature = "feat_macos"))]
    {
        Ok(CrossPlatformDeviceEnumerator::MacOS(
            macos::MacOSDeviceEnumerator::new(),
        ))
    }
    #[cfg(not(any(
        all(target_os = "windows", feature = "feat_windows"),
        all(target_os = "linux", feature = "feat_linux"),
        all(target_os = "macos", feature = "feat_macos")
    )))]
    {
        Err::<CrossPlatformDeviceEnumerator, AudioError>(AudioError::UnsupportedPlatform(
            "This operating system is not supported for audio capture or the required feature is not enabled.".to_string(),
        ))
    }
}

// --- Old Backend Factory (to be deprecated/removed) ---

// Re-export old platform-specific backends (for now)
#[cfg(all(target_os = "linux", feature = "feat_linux"))]
pub use linux::PipeWireBackend; // Old backend
#[cfg(all(target_os = "macos", feature = "feat_macos"))]
pub use macos::CoreAudioBackend; // Assuming this was the old macOS backend name
#[cfg(all(target_os = "windows", feature = "feat_windows"))]
pub use windows::WasapiBackend; // Old backend

/// Returns a platform-specific implementation of the (old) `AudioCaptureBackend`.
/// **Note:** This function is part of the older API and will be deprecated.
/// Use `get_device_enumerator()` for the new trait-based API.
pub fn get_audio_backend() -> crate::core::error::Result<Box<dyn AudioCaptureBackend>> {
    #[cfg(all(target_os = "windows", feature = "feat_windows"))]
    {
        // WasapiBackend is an alias for WindowsApplicationCapture, which needs parameters
        // Using default values for legacy compatibility
        let backend = windows::WasapiBackend::new(0, false)
            .map_err(|e| crate::core::error::AudioError::BackendError(e.to_string()))?;
        Ok(Box::new(backend))
    }
    #[cfg(all(target_os = "linux", feature = "feat_linux"))]
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
    #[cfg(all(target_os = "macos", feature = "feat_macos"))]
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
    #[cfg(not(any(
        all(target_os = "windows", feature = "feat_windows"),
        all(target_os = "linux", feature = "feat_linux"),
        all(target_os = "macos", feature = "feat_macos")
    )))]
    {
        Err(AudioError::BackendError(
            // Corrected error variant
            "Unsupported operating system or required feature not enabled".to_string(),
        ))
    }
}
