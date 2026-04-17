//! Device enumeration integration tests.
//!
//! These tests verify that rsac can enumerate audio devices and find
//! defaults when audio infrastructure is available.

use rsac::get_device_enumerator;

#[test]
fn test_enumerate_devices_finds_at_least_one() {
    require_audio!();

    let enumerator = get_device_enumerator().expect("Failed to create device enumerator");

    let devices = enumerator
        .enumerate_devices()
        .expect("Failed to enumerate devices");

    eprintln!("[ci_audio] Found {} devices:", devices.len());
    for device in &devices {
        eprintln!(
            "  - {} (id: {:?}, default: {})",
            device.name(),
            device.id(),
            device.is_default()
        );
    }

    assert!(
        !devices.is_empty(),
        "Expected at least one audio device in CI environment"
    );
}

#[test]
fn test_default_device_exists() {
    require_audio!();

    let enumerator = get_device_enumerator().expect("Failed to create device enumerator");

    let default_device = enumerator
        .get_default_device()
        .expect("Failed to get default output device");

    eprintln!(
        "[ci_audio] Default output device: {} (id: {:?})",
        default_device.name(),
        default_device.id()
    );

    // Device should have a non-empty name
    assert!(
        !default_device.name().is_empty(),
        "Default device should have a name"
    );

    // Device should report supported formats
    let formats = default_device.supported_formats();
    eprintln!("[ci_audio] Supported formats: {:?}", formats);
    // Note: formats might be empty on some backends, so we just log it
}

#[test]
fn test_platform_capabilities_reasonable() {
    // This test doesn't require audio infrastructure —
    // PlatformCapabilities::query() returns static data about what the
    // current platform supports.

    let caps = rsac::PlatformCapabilities::query();

    eprintln!("[ci_audio] Platform capabilities:");
    eprintln!("  Backend:                {}", caps.backend_name);
    eprintln!("  System capture:         {}", caps.supports_system_capture);
    eprintln!(
        "  Application capture:    {}",
        caps.supports_application_capture
    );
    eprintln!(
        "  Process tree capture:   {}",
        caps.supports_process_tree_capture
    );
    eprintln!(
        "  Device selection:       {}",
        caps.supports_device_selection
    );
    eprintln!(
        "  Sample formats:         {:?}",
        caps.supported_sample_formats
    );
    eprintln!("  Sample rate range:      {:?}", caps.sample_rate_range);
    eprintln!("  Max channels:           {}", caps.max_channels);

    // Backend name should not be empty
    assert!(
        !caps.backend_name.is_empty(),
        "Backend name should not be empty"
    );

    // Should support at least one sample format
    assert!(
        !caps.supported_sample_formats.is_empty(),
        "Should support at least one sample format"
    );

    // Sample rate range should be valid
    assert!(
        caps.sample_rate_range.0 <= caps.sample_rate_range.1,
        "Sample rate min ({}) should be <= max ({})",
        caps.sample_rate_range.0,
        caps.sample_rate_range.1
    );

    // Max channels should be positive
    assert!(
        caps.max_channels > 0,
        "Max channels should be > 0, got {}",
        caps.max_channels
    );

    // Linux-specific: should report PipeWire backend
    #[cfg(target_os = "linux")]
    {
        assert_eq!(
            caps.backend_name, "PipeWire",
            "Linux backend should be PipeWire"
        );
        assert!(
            caps.supports_system_capture,
            "Linux should support system capture"
        );
    }
}
