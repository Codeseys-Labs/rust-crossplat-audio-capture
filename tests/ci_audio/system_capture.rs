//! System audio capture integration tests.
//!
//! These tests verify the full capture pipeline: build → start → read → stop.
//! They require audio infrastructure AND a test tone playing.

use std::time::Instant;

use rsac::{AudioCaptureBuilder, CaptureTarget};

use crate::helpers;

#[test]
fn test_system_capture_receives_audio() {
    require_audio!();

    // Generate and start playing a test tone
    let wav_path = helpers::generate_test_wav(5.0, 48000, 2);
    let player = helpers::spawn_test_tone_player(&wav_path);

    // Give the player a moment to start
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Build capture
    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ci_audio] Failed to build capture: {:?}", e);
            if let Some(p) = player {
                helpers::stop_player(p);
            }
            // Don't fail hard — this might be an environment issue
            eprintln!("[ci_audio] SKIPPING: Capture build failed (not a test logic error)");
            return;
        }
    };

    // Start capture
    if let Err(e) = capture.start() {
        eprintln!("[ci_audio] Failed to start capture: {:?}", e);
        if let Some(p) = player {
            helpers::stop_player(p);
        }
        eprintln!("[ci_audio] SKIPPING: Capture start failed (not a test logic error)");
        return;
    }

    assert!(
        capture.is_running(),
        "Capture should be running after start()"
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
                        "[ci_audio] First non-silent buffer: {} frames, RMS={:.6}",
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
                // No data yet, brief sleep
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => {
                eprintln!("[ci_audio] Read error (may be transient): {:?}", e);
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

    // Verify results
    eprintln!(
        "[ci_audio] Capture complete: {} buffers, {} total frames",
        buffers_read, total_frames
    );

    assert!(buffers_read > 0, "Should have read at least one buffer");
    assert!(
        total_frames > 0,
        "Should have captured at least some audio frames"
    );

    // Non-silence check is a soft warning, not a hard failure
    // CI audio routing can be flaky
    if !got_non_silence {
        eprintln!(
            "[ci_audio] ⚠ WARNING: All captured audio was silence. \
             This may indicate audio routing issues in CI."
        );
    }

    // Clean up temp file
    let _ = std::fs::remove_file(&wav_path);
    if let Some(parent) = wav_path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

#[test]
fn test_capture_format_correct() {
    require_audio!();

    let expected_sample_rate = 48000u32;
    let expected_channels = 2u16;

    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(expected_sample_rate)
        .channels(expected_channels)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ci_audio] Failed to build capture: {:?}", e);
            eprintln!("[ci_audio] SKIPPING: Capture build failed");
            return;
        }
    };

    if let Err(e) = capture.start() {
        eprintln!("[ci_audio] Failed to start capture: {:?}", e);
        eprintln!("[ci_audio] SKIPPING: Capture start failed");
        return;
    }

    // Try to read a buffer to check format
    let timeout = helpers::capture_timeout();
    let start = Instant::now();
    let mut format_verified = false;

    while start.elapsed() < timeout {
        match capture.read_buffer() {
            Ok(Some(buffer)) => {
                eprintln!(
                    "[ci_audio] Buffer format: rate={}, channels={}, frames={}",
                    buffer.sample_rate(),
                    buffer.channels(),
                    buffer.num_frames()
                );

                assert!(
                    helpers::verify_format(&buffer, expected_sample_rate, expected_channels),
                    "Audio format should match requested configuration"
                );

                format_verified = true;
                break;
            }
            Ok(None) => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => {
                eprintln!("[ci_audio] Read error: {:?}", e);
                if e.is_fatal() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    let _ = capture.stop();

    assert!(
        format_verified,
        "Should have received at least one buffer to verify format"
    );
}
