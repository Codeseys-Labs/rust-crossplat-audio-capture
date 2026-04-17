//! # List Devices & Platform Capabilities
//!
//! Example showing how to query platform audio capabilities and enumerate
//! real audio devices using rsac.
//!
//! Run with: `cargo run --example list_devices`
//! (add `--features feat_linux` on Linux, `--features feat_windows` on Windows, etc.)

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

    // Real device enumeration
    println!("=== Audio Devices ===");
    println!();

    match rsac::get_device_enumerator() {
        Ok(enumerator) => {
            // Default device
            match enumerator.get_default_device() {
                Ok(device) => {
                    println!("Default device: {} (ID: {})", device.name(), device.id());
                }
                Err(e) => {
                    println!("Default device: unavailable ({})", e);
                }
            }
            println!();

            // All devices
            match enumerator.enumerate_devices() {
                Ok(devices) => {
                    if devices.is_empty() {
                        println!("No audio devices found.");
                    } else {
                        println!("Found {} device(s):", devices.len());
                        println!();
                        for device in &devices {
                            let default_marker = if device.is_default() {
                                " [default]"
                            } else {
                                ""
                            };
                            println!("  • {}{}", device.name(), default_marker);
                            println!("    ID: {}", device.id());
                            let formats = device.supported_formats();
                            if !formats.is_empty() {
                                for fmt in &formats {
                                    println!(
                                        "    Format: {}ch {}Hz {:?}",
                                        fmt.channels, fmt.sample_rate, fmt.sample_format
                                    );
                                }
                            }
                            println!();
                        }
                    }
                }
                Err(e) => {
                    println!("Failed to enumerate devices: {}", e);
                }
            }
        }
        Err(e) => {
            println!("Device enumeration unavailable: {}", e);
            println!();
            println!("Use the builder API with CaptureTarget::SystemDefault");
            println!("to capture from the system default device.");
        }
    }
}
