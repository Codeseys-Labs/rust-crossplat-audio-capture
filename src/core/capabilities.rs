// src/core/capabilities.rs

//! Platform capability reporting.
//!
//! [`PlatformCapabilities`] provides honest reporting of what each platform's
//! audio backend supports — never pretend a platform can do something it cannot.

use super::config::SampleFormat;

/// Reports what the current platform's audio backend supports.
///
/// Used for honest capability reporting — never pretend a platform
/// can do something it cannot. Query capabilities at runtime via
/// [`PlatformCapabilities::query()`] and check before attempting
/// operations that may not be available on all platforms.
///
/// # Example
///
/// ```
/// use rsac::core::capabilities::PlatformCapabilities;
///
/// let caps = PlatformCapabilities::query();
/// if caps.supports_application_capture {
///     // Safe to use CaptureTarget::Application(..)
/// }
/// ```
#[derive(Debug, Clone)]
pub struct PlatformCapabilities {
    /// Whether system-wide audio capture is supported.
    pub supports_system_capture: bool,
    /// Whether per-application audio capture is supported.
    pub supports_application_capture: bool,
    /// Whether process-tree audio capture is supported.
    pub supports_process_tree_capture: bool,
    /// Whether device selection is supported.
    pub supports_device_selection: bool,
    /// Supported sample formats.
    pub supported_sample_formats: Vec<SampleFormat>,
    /// Supported sample rate range (min, max) in Hz.
    pub sample_rate_range: (u32, u32),
    /// Maximum number of channels supported.
    pub max_channels: u16,
    /// Name of the audio backend (e.g., "WASAPI", "CoreAudio", "PipeWire").
    pub backend_name: &'static str,
}

impl PlatformCapabilities {
    /// Query the capabilities of the current platform's audio backend.
    ///
    /// This is determined at compile time based on the target OS.
    pub fn query() -> Self {
        #[cfg(target_os = "windows")]
        {
            Self::windows()
        }

        #[cfg(target_os = "macos")]
        {
            Self::macos()
        }

        #[cfg(target_os = "linux")]
        {
            Self::linux()
        }

        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            Self::unsupported()
        }
    }

    /// Check if a specific sample format is supported.
    pub fn supports_format(&self, format: SampleFormat) -> bool {
        self.supported_sample_formats.contains(&format)
    }

    /// Check if a specific sample rate is supported.
    pub fn supports_sample_rate(&self, rate: u32) -> bool {
        rate >= self.sample_rate_range.0 && rate <= self.sample_rate_range.1
    }

    /// Check if a specific channel count is supported.
    pub fn supports_channels(&self, channels: u16) -> bool {
        channels > 0 && channels <= self.max_channels
    }

    // ── Platform constructors (private) ──────────────────────────────────

    #[cfg(target_os = "windows")]
    fn windows() -> Self {
        Self {
            supports_system_capture: true,
            supports_application_capture: true, // WASAPI session capture
            supports_process_tree_capture: false,
            supports_device_selection: true,
            supported_sample_formats: vec![
                SampleFormat::I16,
                SampleFormat::I24,
                SampleFormat::I32,
                SampleFormat::F32,
            ],
            sample_rate_range: (8000, 384000),
            max_channels: 8,
            backend_name: "WASAPI",
        }
    }

    #[cfg(target_os = "macos")]
    fn macos() -> Self {
        Self {
            supports_system_capture: true,
            supports_application_capture: true, // CoreAudio Process Tap
            supports_process_tree_capture: true, // Process Tap supports trees
            supports_device_selection: true,
            supported_sample_formats: vec![SampleFormat::I16, SampleFormat::I32, SampleFormat::F32],
            sample_rate_range: (8000, 192000),
            max_channels: 8,
            backend_name: "CoreAudio",
        }
    }

    #[cfg(target_os = "linux")]
    fn linux() -> Self {
        Self {
            supports_system_capture: true,
            supports_application_capture: true, // PipeWire node targeting
            supports_process_tree_capture: false,
            supports_device_selection: true,
            supported_sample_formats: vec![SampleFormat::I16, SampleFormat::I32, SampleFormat::F32],
            sample_rate_range: (8000, 384000),
            max_channels: 32, // PipeWire supports many channels
            backend_name: "PipeWire",
        }
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    fn unsupported() -> Self {
        Self {
            supports_system_capture: false,
            supports_application_capture: false,
            supports_process_tree_capture: false,
            supports_device_selection: false,
            supported_sample_formats: vec![],
            sample_rate_range: (0, 0),
            max_channels: 0,
            backend_name: "unsupported",
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_returns_valid_capabilities() {
        let caps = PlatformCapabilities::query();

        // We're on Linux, so these should be the PipeWire values
        #[cfg(target_os = "linux")]
        {
            assert_eq!(caps.backend_name, "PipeWire");
            assert!(caps.supports_system_capture);
            assert!(caps.supports_application_capture);
            assert!(!caps.supports_process_tree_capture);
            assert!(caps.supports_device_selection);
            assert_eq!(caps.max_channels, 32);
            assert_eq!(caps.sample_rate_range, (8000, 384000));
            assert!(!caps.supported_sample_formats.is_empty());
        }
    }

    #[test]
    fn supports_format_f32() {
        let caps = PlatformCapabilities::query();
        assert!(caps.supports_format(SampleFormat::F32));
    }

    #[test]
    fn supports_format_missing() {
        let caps = PlatformCapabilities {
            supports_system_capture: false,
            supports_application_capture: false,
            supports_process_tree_capture: false,
            supports_device_selection: false,
            supported_sample_formats: vec![SampleFormat::I16],
            sample_rate_range: (8000, 48000),
            max_channels: 2,
            backend_name: "test",
        };
        assert!(!caps.supports_format(SampleFormat::F32));
    }

    #[test]
    fn supports_sample_rate_48000() {
        let caps = PlatformCapabilities::query();
        assert!(caps.supports_sample_rate(48000));
    }

    #[test]
    fn supports_sample_rate_zero_is_false() {
        let caps = PlatformCapabilities::query();
        // 0 is below the minimum range for any real platform
        assert!(!caps.supports_sample_rate(0));
    }

    #[test]
    fn supports_channels_stereo() {
        let caps = PlatformCapabilities::query();
        assert!(caps.supports_channels(2));
    }

    #[test]
    fn supports_channels_zero_is_false() {
        let caps = PlatformCapabilities::query();
        assert!(!caps.supports_channels(0));
    }
}
