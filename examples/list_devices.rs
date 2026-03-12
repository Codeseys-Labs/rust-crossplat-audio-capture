//! # List Devices & Platform Capabilities
//!
//! Example showing how to query platform audio capabilities using rsac.
//! This does not require any audio hardware to run — it simply reports
//! what the current platform's audio backend supports.
//!
//! Run with: `cargo run --example list_devices`

use rsac::PlatformCapabilities;

fn main() {
    let caps = PlatformCapabilities::query();

    println!("=== rsac Platform Capabilities ===");
    println!();
    println!("Backend:             {}", caps.backend_name);
    println!(
        "System capture:      {}",
        if caps.supports_system_capture {
            "✓"
        } else {
            "✗"
        }
    );
    println!(
        "Application capture: {}",
        if caps.supports_application_capture {
            "✓"
        } else {
            "✗"
        }
    );
    println!(
        "Process tree:        {}",
        if caps.supports_process_tree_capture {
            "✓"
        } else {
            "✗"
        }
    );
    println!(
        "Device selection:    {}",
        if caps.supports_device_selection {
            "✓"
        } else {
            "✗"
        }
    );
    println!();
    println!(
        "Sample rate range:   {} – {} Hz",
        caps.sample_rate_range.0, caps.sample_rate_range.1
    );
    println!("Max channels:        {}", caps.max_channels);
    println!("Supported formats:   {:?}", caps.supported_sample_formats);
    println!();
    println!("To capture audio, use the builder API:");
    println!("  AudioCaptureBuilder::new()");
    println!("      .with_target(CaptureTarget::SystemDefault)");
    println!("      .sample_rate(48000)");
    println!("      .channels(2)");
    println!("      .build()? → capture.start()? → capture.read_buffer()?");
}
