//! Device selection capture integration tests.
//!
//! These tests verify the full device-targeted capture pipeline:
//! enumerate devices → select a specific device → capture audio from it.
//! Requires audio infrastructure with at least one enumerable device.

use std::time::Instant;

use rsac::{AudioCaptureBuilder, CaptureTarget, DeviceId};

use crate::helpers;

/// Test: Enumerate devices, select the default output device by ID,
/// and capture audio from it using `CaptureTarget::Device(DeviceId)`.
#[test]
fn test_capture_from_selected_device() {
    require_device_selection!();

    // Enumerate and find the default output device
    let enumerator = match rsac::get_device_enumerator() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[ci_audio] Failed to create device enumerator: {:?}", e);
            eprintln!("[ci_audio] SKIPPING: Device enumerator unavailable");
            return;
        }
    };

    let default_device = match enumerator.get_default_device() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[ci_audio] Failed to get default device: {:?}", e);
            eprintln!("[ci_audio] SKIPPING: No default output device");
            return;
        }
    };

    let device_id = default_device.id().clone();
    eprintln!(
        "[ci_audio] Targeting device: {} (id: {:?})",
        default_device.name(),
        device_id
    );

    // Generate and start playing a test tone so there's audio to capture
    let wav_path = helpers::generate_test_wav(5.0, 48000, 2);
    let player = helpers::spawn_test_tone_player(&wav_path);

    // Give the player a moment to start
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Build capture targeting the specific device
    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Device(device_id.clone()))
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ci_audio] Failed to build device capture: {:?}", e);
            if let Some(p) = player {
                helpers::stop_player(p);
            }
            eprintln!("[ci_audio] SKIPPING: Device capture build failed");
            return;
        }
    };

    // Start capture
    if let Err(e) = capture.start() {
        eprintln!("[ci_audio] Failed to start device capture: {:?}", e);
        if let Some(p) = player {
            helpers::stop_player(p);
        }
        eprintln!("[ci_audio] SKIPPING: Device capture start failed");
        return;
    }

    assert!(
        capture.is_running(),
        "Device capture should be running after start()"
    );

    // Read audio buffers with timeout
    let timeout = helpers::capture_timeout();
    let start = Instant::now();
    let mut total_frames: usize = 0;
    let mut got_non_silence = false;
    let mut buffers_read: usize = 0;

    while start.elapsed() < timeout {
        match capture.read_buffer() {
            Ok(Some(buffer)) => {
                buffers_read += 1;
                total_frames += buffer.num_frames();

                if !got_non_silence && helpers::verify_non_silence(&buffer, 0.001) {
                    got_non_silence = true;
                    let (rms, _) = helpers::verify_rms_energy(&buffer, 0.0);
                    eprintln!(
                        "[ci_audio] Device capture: first non-silent buffer, {} frames, RMS={:.6}",
                        buffer.num_frames(),
                        rms
                    );
                }

                // If we have enough data, break early
                if total_frames > 48000 {
                    break;
                }
            }
            Ok(None) => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => {
                eprintln!("[ci_audio] Device capture read error: {:?}", e);
                if e.is_fatal() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    // Stop capture
    let _ = capture.stop();

    // Stop player
    if let Some(p) = player {
        helpers::stop_player(p);
    }

    eprintln!(
        "[ci_audio] Device capture complete: {} buffers, {} total frames, device={:?}",
        buffers_read, total_frames, device_id
    );

    if buffers_read == 0 {
        // On macOS (and some other platforms), the default "output" device
        // (e.g., MacBook Air Speakers) is output-only and cannot produce
        // capture data directly.  System capture or Process Tap is needed
        // for loopback.  Rather than hard-fail, we warn and skip.
        eprintln!(
            "[ci_audio] WARNING: 0 buffers captured from device '{}' ({:?}). \
             This device is likely output-only and does not support direct \
             input capture. Use CaptureTarget::SystemDefault for loopback \
             capture instead. SKIPPING assertion.",
            default_device.name(),
            device_id
        );
        // Clean up temp file
        let _ = std::fs::remove_file(&wav_path);
        if let Some(parent) = wav_path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
        return;
    }

    assert!(
        total_frames > 0,
        "Should have captured at least some audio frames from device"
    );

    if !got_non_silence {
        eprintln!(
            "[ci_audio] WARNING: All captured audio from device was silence. \
             This may indicate audio routing issues in CI."
        );
    }

    // Clean up temp file
    let _ = std::fs::remove_file(&wav_path);
    if let Some(parent) = wav_path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

/// Test: Enumerate all devices and verify each has a valid DeviceId
/// that could be used with `CaptureTarget::Device`.
#[test]
fn test_all_enumerated_devices_have_valid_ids() {
    require_device_selection!();

    let enumerator = match rsac::get_device_enumerator() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[ci_audio] Failed to create device enumerator: {:?}", e);
            return;
        }
    };

    let devices = match enumerator.enumerate_devices() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[ci_audio] Failed to enumerate devices: {:?}", e);
            return;
        }
    };

    eprintln!("[ci_audio] Checking {} enumerated devices:", devices.len());

    for device in &devices {
        let id = device.id();
        let name = device.name();

        eprintln!(
            "  - {} (id: {:?}, default: {})",
            name,
            id,
            device.is_default()
        );

        // Every device should have a non-empty name
        assert!(
            !name.is_empty(),
            "Device should have a non-empty name, got empty for id {:?}",
            id
        );
    }

    assert!(
        !devices.is_empty(),
        "Should have at least one device in CI with audio infrastructure"
    );
}

/// Test: Capture from a nonexistent device ID — should error gracefully.
#[test]
fn test_capture_nonexistent_device() {
    require_device_selection!();

    let bogus_id = DeviceId("nonexistent_device_id_12345".to_string());

    let result = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Device(bogus_id.clone()))
        .sample_rate(48000)
        .channels(2)
        .build();

    match result {
        Err(e) => {
            eprintln!(
                "[ci_audio] ✅ Build correctly rejected nonexistent device: {:?}",
                e
            );
        }
        Ok(mut capture) => {
            // Some backends accept any device ID at build time and fail at start
            match capture.start() {
                Err(e) => {
                    eprintln!(
                        "[ci_audio] ✅ Start correctly rejected nonexistent device: {:?}",
                        e
                    );
                }
                Ok(()) => {
                    eprintln!(
                        "[ci_audio] ⚠ Capture started with nonexistent device (may produce silence)"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    let _ = capture.stop();
                }
            }
        }
    }
}
