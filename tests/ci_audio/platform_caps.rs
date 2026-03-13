//! Platform capabilities integration tests.
//!
//! These tests verify that PlatformCapabilities::query() returns
//! valid, consistent data for the current platform.

use rsac::{PlatformCapabilities, SampleFormat};

#[test]
fn test_capabilities_query() {
    // This test does NOT require audio infrastructure —
    // it validates the static capability reporting.

    let caps = PlatformCapabilities::query();

    eprintln!("[ci_audio] === Platform Capabilities ===");
    eprintln!("  backend_name:                {}", caps.backend_name);
    eprintln!(
        "  supports_system_capture:     {}",
        caps.supports_system_capture
    );
    eprintln!(
        "  supports_application_capture:{}",
        caps.supports_application_capture
    );
    eprintln!(
        "  supports_process_tree_capture:{}",
        caps.supports_process_tree_capture
    );
    eprintln!(
        "  supports_device_selection:   {}",
        caps.supports_device_selection
    );
    eprintln!(
        "  supported_sample_formats:    {:?}",
        caps.supported_sample_formats
    );
    eprintln!(
        "  sample_rate_range:           {:?}",
        caps.sample_rate_range
    );
    eprintln!("  max_channels:                {}", caps.max_channels);

    // Every platform should support F32 (internal format)
    assert!(
        caps.supported_sample_formats.contains(&SampleFormat::F32),
        "All platforms should support F32 sample format, got: {:?}",
        caps.supported_sample_formats
    );

    // Sample rate range should include 48000 (standard rate)
    assert!(
        caps.sample_rate_range.0 <= 48000 && caps.sample_rate_range.1 >= 48000,
        "Sample rate range {:?} should include 48000",
        caps.sample_rate_range
    );

    // Max channels should be at least 2 (stereo)
    assert!(
        caps.max_channels >= 2,
        "Max channels should be at least 2, got {}",
        caps.max_channels
    );
}

#[test]
fn test_backend_name_matches_platform() {
    let caps = PlatformCapabilities::query();

    #[cfg(target_os = "linux")]
    {
        assert_eq!(
            caps.backend_name, "PipeWire",
            "Linux should report PipeWire backend"
        );
        // Linux/PipeWire should support system capture
        assert!(caps.supports_system_capture);
        // Linux/PipeWire should support application capture
        assert!(caps.supports_application_capture);
    }

    #[cfg(target_os = "windows")]
    {
        assert_eq!(
            caps.backend_name, "WASAPI",
            "Windows should report WASAPI backend"
        );
    }

    #[cfg(target_os = "macos")]
    {
        assert_eq!(
            caps.backend_name, "CoreAudio",
            "macOS should report CoreAudio backend"
        );
    }

    eprintln!(
        "[ci_audio] Backend name '{}' matches expected for this platform",
        caps.backend_name
    );
}
