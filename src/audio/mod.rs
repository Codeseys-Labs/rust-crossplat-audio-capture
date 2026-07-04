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

// --- Trait-Based API Exports ---

// Re-export platform-specific DeviceEnumerators
#[cfg(all(target_os = "linux", feature = "feat_linux"))]
pub use linux::LinuxDeviceEnumerator;
#[cfg(all(target_os = "macos", feature = "feat_macos"))]
pub use macos::MacosDeviceEnumerator;
#[cfg(all(target_os = "windows", feature = "feat_windows"))]
pub use windows::WindowsDeviceEnumerator;

// Re-export platform-specific AudioDevice types if they need to be named directly.
// Usually, interaction will be through the traits.
#[cfg(all(target_os = "macos", feature = "feat_macos"))]
pub use macos::{enumerate_audio_applications, ApplicationInfo, MacosAudioDevice};
#[cfg(all(target_os = "windows", feature = "feat_windows"))]
pub use windows::{
    enumerate_application_audio_sessions, ApplicationAudioSessionInfo, WindowsAudioDevice,
};

// --- Factory function for the new DeviceEnumerator ---

use crate::core::error::AudioError;
// DeviceEnumerator is used in match arms that are compiled only with platform features
#[cfg(any(
    all(target_os = "windows", feature = "feat_windows"),
    all(target_os = "linux", feature = "feat_linux"),
    all(target_os = "macos", feature = "feat_macos")
))]
use crate::core::interface::DeviceEnumerator;

/// Cross-platform device enumerator that wraps platform-specific implementations.
pub enum CrossPlatformDeviceEnumerator {
    /// WASAPI-backed enumerator (Windows).
    #[cfg(all(target_os = "windows", feature = "feat_windows"))]
    Windows(windows::WindowsDeviceEnumerator),

    /// PipeWire-backed enumerator (Linux).
    #[cfg(all(target_os = "linux", feature = "feat_linux"))]
    Linux(linux::LinuxDeviceEnumerator),

    /// CoreAudio-backed enumerator (macOS).
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
    /// All platform backends (WASAPI, PipeWire, CoreAudio) return the default
    /// output device here, since rsac is a loopback-capture library and the
    /// output device is what consumers need for system audio capture.
    ///
    /// This is the canonical facade name, matching the
    /// [`DeviceEnumerator::default_device`]
    /// trait method and the sibling [`enumerate_devices`](Self::enumerate_devices)
    /// (AEG-3, rsac-0113). The historical
    /// [`get_default_device`](Self::get_default_device) name is retained as a
    /// thin alias for one release so existing callers and bindings keep
    /// compiling while they migrate.
    pub fn default_device(
        &self,
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

    /// Deprecated alias for [`default_device`](Self::default_device).
    ///
    /// AEG-3 (rsac-0113) renamed the facade to `default_device()` to match the
    /// [`DeviceEnumerator`] trait
    /// method and the sibling [`enumerate_devices`](Self::enumerate_devices) —
    /// `default_device` was the lone `get_`-prefixed divergence. This alias is
    /// kept for one release so existing callers and bindings migrate without a
    /// hard break; prefer [`default_device`](Self::default_device) in new code.
    ///
    /// AEG-3 finish (rsac-9d51): every in-crate caller (the demo binary,
    /// `smoke_alpine`, the examples, the integration tests, and the FFI/napi
    /// bindings) has been migrated to [`default_device`](Self::default_device),
    /// so the `#[deprecated]` attribute can now be applied without tripping CI's
    /// `cargo clippy --all-targets -- -D warnings` gate (no in-crate call site
    /// emits the lint). The remaining in-crate use is the alias-forwarding test
    /// below, which suppresses the lint locally with `#[allow(deprecated)]`.
    #[deprecated(
        since = "0.3.0",
        note = "renamed to default_device() to match the DeviceEnumerator trait; \
                this alias will be removed in a future release"
    )]
    pub fn get_default_device(
        &self,
    ) -> crate::core::error::Result<Box<dyn crate::core::interface::AudioDevice>> {
        self.default_device()
    }

    /// Subscribe to device hot-plug / default-change notifications.
    ///
    /// This is the real public entry point for device-change watching (the
    /// `CrossPlatformDeviceEnumerator` does not itself implement the
    /// [`DeviceEnumerator`] trait). It dispatches to the active backend's
    /// [`DeviceEnumerator::watch`] implementation; backends that have not wired
    /// up an OS listener return
    /// [`AudioError::PlatformNotSupported`],
    /// consistent with their
    /// [`supports_device_change_notifications`](crate::core::capabilities::PlatformCapabilities::supports_device_change_notifications)
    /// flag.
    ///
    /// `on_event` runs on the backend's OS notification thread (never the
    /// real-time audio callback thread); the returned
    /// [`DeviceWatcher`](crate::core::interface::DeviceWatcher) unregisters the
    /// listener and joins that thread on drop.
    pub fn watch(
        &self,
        on_event: crate::core::interface::DeviceEventHandler,
    ) -> crate::core::error::Result<crate::core::interface::DeviceWatcher> {
        match self {
            #[cfg(all(target_os = "windows", feature = "feat_windows"))]
            CrossPlatformDeviceEnumerator::Windows(enumerator) => {
                DeviceEnumerator::watch(enumerator, on_event)
            }
            #[cfg(all(target_os = "linux", feature = "feat_linux"))]
            CrossPlatformDeviceEnumerator::Linux(enumerator) => {
                DeviceEnumerator::watch(enumerator, on_event)
            }
            #[cfg(all(target_os = "macos", feature = "feat_macos"))]
            CrossPlatformDeviceEnumerator::MacOS(enumerator) => {
                DeviceEnumerator::watch(enumerator, on_event)
            }
            #[cfg(not(any(
                all(target_os = "windows", feature = "feat_windows"),
                all(target_os = "linux", feature = "feat_linux"),
                all(target_os = "macos", feature = "feat_macos")
            )))]
            _ => {
                drop(on_event);
                Err(crate::core::error::AudioError::PlatformNotSupported {
                    feature: "device change notifications".to_string(),
                    platform: std::env::consts::OS.to_string(),
                })
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// AEG-3 (rsac-0113): the deprecated `get_default_device()` alias must
    /// forward to the canonical `default_device()` and return an
    /// observably-equivalent result. We construct an enumerator only where the
    /// active platform feature makes one available; on a backend-less build the
    /// factory errors and there is nothing to compare, so the test is a no-op
    /// there. The comparison is device-free-tolerant: it asserts the two names
    /// agree on success-vs-failure and, when both fail, that they surface the
    /// same `AudioError` discriminant — never that hardware is present.
    // The deprecated `get_default_device()` alias is exercised here on purpose:
    // this test exists to prove the alias still forwards correctly for the one
    // release it remains in the public API. The `#[allow(deprecated)]` keeps the
    // intentional call from tripping CI's `-D warnings` gate.
    #[test]
    #[allow(deprecated)]
    fn get_default_device_alias_matches_default_device() {
        let enumerator = match get_device_enumerator() {
            Ok(e) => e,
            // No backend on this build (or no platform feature): nothing to
            // compare. The alias-forwarding is still proven to compile.
            Err(_) => return,
        };

        let canonical = enumerator.default_device();
        let aliased = enumerator.get_default_device();

        assert_eq!(
            canonical.is_ok(),
            aliased.is_ok(),
            "get_default_device() (alias) and default_device() (canonical) must \
             agree on success vs failure"
        );

        // When both fail (e.g. a device-free CI box), they must fail the same
        // way — the alias is a pure forward, so it cannot change the error.
        if let (Err(c), Err(a)) = (&canonical, &aliased) {
            assert_eq!(
                std::mem::discriminant(c),
                std::mem::discriminant(a),
                "alias must surface the same AudioError variant as the canonical \
                 method (canonical={c:?}, alias={a:?})"
            );
        }
    }
}
