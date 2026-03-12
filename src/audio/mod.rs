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
#[cfg(all(target_os = "macos", feature = "feat_macos"))]
pub use macos::{
    enumerate_audio_applications, ApplicationInfo, MacosAudioDevice, MacosAudioStream,
};
#[cfg(all(target_os = "windows", feature = "feat_windows"))]
pub use windows::{
    enumerate_application_audio_sessions, ApplicationAudioSessionInfo, WindowsAudioDevice,
};

// --- Factory function for the new DeviceEnumerator ---

use crate::core::error::AudioError;
use crate::core::interface::DeviceEnumerator; // Import the trait itself

/// Cross-platform device enumerator that wraps platform-specific implementations.
pub enum CrossPlatformDeviceEnumerator {
    #[cfg(all(target_os = "windows", feature = "feat_windows"))]
    Windows(windows::WindowsDeviceEnumerator),

    #[cfg(all(target_os = "linux", feature = "feat_linux"))]
    Linux(linux::LinuxDeviceEnumerator),

    #[cfg(all(target_os = "macos", feature = "feat_macos"))]
    MacOS(macos::MacosDeviceEnumerator),
}

impl CrossPlatformDeviceEnumerator {
    /// Enumerate all available audio devices.
    pub fn enumerate_devices(
        &self,
    ) -> crate::core::error::Result<Vec<Box<dyn crate::core::interface::AudioDevice>>> {
        match self {
            #[cfg(all(target_os = "windows", feature = "feat_windows"))]
            CrossPlatformDeviceEnumerator::Windows(enumerator) => {
                DeviceEnumerator::enumerate_devices(enumerator)
            }
            #[cfg(all(target_os = "linux", feature = "feat_linux"))]
            CrossPlatformDeviceEnumerator::Linux(enumerator) => {
                DeviceEnumerator::enumerate_devices(enumerator)
            }
            #[cfg(all(target_os = "macos", feature = "feat_macos"))]
            CrossPlatformDeviceEnumerator::MacOS(enumerator) => {
                DeviceEnumerator::enumerate_devices(enumerator)
            }
            #[cfg(not(any(
                all(target_os = "windows", feature = "feat_windows"),
                all(target_os = "linux", feature = "feat_linux"),
                all(target_os = "macos", feature = "feat_macos")
            )))]
            _ => Err(crate::core::error::AudioError::PlatformNotSupported {
                feature: "audio device enumeration".to_string(),
                platform: std::env::consts::OS.to_string(),
            }),
        }
    }

    /// Get the default audio device.
    ///
    /// The `_kind` parameter is accepted for backward compatibility but
    /// the underlying `DeviceEnumerator::default_device()` returns the
    /// platform's default capture-relevant device.
    pub fn get_default_device(
        &self,
        _kind: crate::core::interface::DeviceKind,
    ) -> crate::core::error::Result<Box<dyn crate::core::interface::AudioDevice>> {
        match self {
            #[cfg(all(target_os = "windows", feature = "feat_windows"))]
            CrossPlatformDeviceEnumerator::Windows(enumerator) => {
                DeviceEnumerator::default_device(enumerator)
            }
            #[cfg(all(target_os = "linux", feature = "feat_linux"))]
            CrossPlatformDeviceEnumerator::Linux(enumerator) => {
                DeviceEnumerator::default_device(enumerator)
            }
            #[cfg(all(target_os = "macos", feature = "feat_macos"))]
            CrossPlatformDeviceEnumerator::MacOS(enumerator) => {
                DeviceEnumerator::default_device(enumerator)
            }
            #[cfg(not(any(
                all(target_os = "windows", feature = "feat_windows"),
                all(target_os = "linux", feature = "feat_linux"),
                all(target_os = "macos", feature = "feat_macos")
            )))]
            _ => Err(crate::core::error::AudioError::PlatformNotSupported {
                feature: "audio device enumeration".to_string(),
                platform: std::env::consts::OS.to_string(),
            }),
        }
    }
}

/// Returns a platform-specific implementation of `DeviceEnumerator`.
///
/// This function inspects the `target_os` at compile time and provides the
/// appropriate enumerator for the current platform.
///
/// # Returns
/// A `Result` containing a `CrossPlatformDeviceEnumerator` for the current platform,
/// or an `AudioError::PlatformNotSupported` if the OS is not supported.
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
            macos::MacosDeviceEnumerator::new(),
        ))
    }
    #[cfg(not(any(
        all(target_os = "windows", feature = "feat_windows"),
        all(target_os = "linux", feature = "feat_linux"),
        all(target_os = "macos", feature = "feat_macos")
    )))]
    {
        Err::<CrossPlatformDeviceEnumerator, AudioError>(AudioError::PlatformNotSupported {
            feature: "audio capture".to_string(),
            platform: std::env::consts::OS.to_string(),
        })
    }
}
