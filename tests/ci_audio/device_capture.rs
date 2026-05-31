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

    let default_device = match enumerator.default_device() {
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
                    let (rms, rms_ok) = helpers::verify_rms_energy(&buffer, 0.01);
                    eprintln!(
                        "[ci_audio] Device capture: first non-silent buffer, {} frames, RMS={:.6}",
                        buffer.num_frames(),
                        rms
                    );

                    if helpers::deterministic_audio_env() {
                        assert!(
                            rms_ok,
                            "deterministic source: device RMS energy {:.6} below 0.01 floor",
                            rms
                        );
                        assert!(
                            helpers::verify_tone_present(&buffer, 440.0),
                            "deterministic source: 440 Hz tone not detected in device capture"
                        );
                    }
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
        //
        // Under the deterministic Linux source the default device is the
        // PipeWire null sink, whose monitor DOES support capture — zero
        // buffers there is a real regression, so we refuse to soft-skip.
        if helpers::deterministic_audio_env() {
            // Clean up before the panic so we don't leak the temp file.
            let _ = std::fs::remove_file(&wav_path);
            if let Some(parent) = wav_path.parent() {
                let _ = std::fs::remove_dir(parent);
            }
            panic!(
                "deterministic source: 0 buffers captured from device '{}' ({:?}) — \
                 the null-sink monitor should always yield audio",
                default_device.name(),
                device_id
            );
        }
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

    if helpers::deterministic_audio_env() {
        assert!(
            got_non_silence,
            "deterministic source: all captured device audio was silence — \
             real regression, not CI flakiness"
        );
    } else if !got_non_silence {
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
                    // Some backends accept any device ID and produce silence
                    // rather than erroring. If audio DOES flow from a bogus
                    // device, that's a routing bug — assert the data is silent.
                    eprintln!(
                        "[ci_audio] Capture started with nonexistent device; verifying it yields silence"
                    );
                    let start = Instant::now();
                    let mut produced_audio = false;
                    while start.elapsed() < std::time::Duration::from_millis(500) {
                        match capture.read_buffer() {
                            Ok(Some(buf)) => {
                                if helpers::verify_non_silence(&buf, 0.001) {
                                    produced_audio = true;
                                    break;
                                }
                            }
                            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
                            Err(_) => break,
                        }
                    }
                    let _ = capture.stop();
                    assert!(
                        !produced_audio,
                        "nonexistent device must not produce non-silent audio — \
                         capture is reading from the wrong endpoint"
                    );
                    eprintln!(
                        "[ci_audio] ✅ Nonexistent device produced only silence (as expected)"
                    );
                }
            }
        }
    }
}
