//! System audio capture integration tests.
//!
//! These tests verify the full capture pipeline: build → start → read → stop.
//! They require audio infrastructure AND a test tone playing.

use std::time::Instant;

use rsac::{AudioCaptureBuilder, CaptureTarget};

use crate::helpers;

#[test]
fn test_system_capture_receives_audio() {
    require_system_capture!();

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
            if let Some(p) = player {
                helpers::stop_player(p);
            }
            // On a deterministic source, build() must succeed — a failure here
            // is a real regression in the pre-streaming path, not an
            // environment quirk, so hard-fail instead of silently skipping.
            if helpers::deterministic_audio_env() {
                panic!("deterministic source: capture build failed: {:?}", e);
            }
            eprintln!("[ci_audio] Failed to build capture: {:?}", e);
            eprintln!("[ci_audio] SKIPPING: Capture build failed (not a test logic error)");
            return;
        }
    };

    // Start capture
    if let Err(e) = capture.start() {
        if let Some(p) = player {
            helpers::stop_player(p);
        }
        // Same rationale as build(): a deterministic source must start cleanly.
        if helpers::deterministic_audio_env() {
            panic!("deterministic source: capture start failed: {:?}", e);
        }
        eprintln!("[ci_audio] Failed to start capture: {:?}", e);
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
                    let (rms, rms_ok) = helpers::verify_rms_energy(&buffer, 0.01);
                    eprintln!(
                        "[ci_audio] First non-silent buffer: {} frames, RMS={:.6}",
                        buffer.num_frames(),
                        rms
                    );

                    // Under the deterministic Linux source (PipeWire null sink +
                    // 440 Hz/0.8 sine tone) silence is impossible if capture works,
                    // so promote the checks to hard asserts: RMS must clear the
                    // 0.01 floor and the 440 Hz tone must dominate the spectrum.
                    if helpers::deterministic_audio_env() {
                        assert!(
                            rms_ok,
                            "deterministic source: RMS energy {:.6} below 0.01 floor",
                            rms
                        );
                        assert!(
                            helpers::verify_tone_present(&buffer, 440.0),
                            "deterministic source: 440 Hz test tone not detected in capture"
                        );
                    }
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

    if buffers_read == 0 {
        // The dbus-less PipeWire VM in CI may not route SystemDefault capture
        // at all (0 buffers). Under a deterministic source that's a real
        // regression; otherwise skip honestly — we do NOT claim to have verified
        // capture here (content is verified on real hardware and on the
        // deterministic Windows VB-CABLE runner).
        if helpers::deterministic_audio_env() {
            panic!(
                "deterministic source: 0 buffers captured — the 440 Hz tone never \
                 reached the SystemDefault capture path (real regression, not flakiness)"
            );
        }
        eprintln!(
            "[ci_audio] SKIPPED: 0 buffers captured in this environment \
             (SystemDefault capture not verifiable here)"
        );
        return;
    }

    assert!(buffers_read > 0, "Should have read at least one buffer");
    assert!(
        total_frames > 0,
        "Should have captured at least some audio frames"
    );

    if helpers::deterministic_audio_env() {
        // The Linux deterministic source guarantees audible, tonal output.
        // Anything less than non-silence here is a genuine capture regression.
        assert!(
            got_non_silence,
            "deterministic source: all captured audio was silence — \
             the 440 Hz tone never reached the capture path (real regression, \
             not CI flakiness)"
        );
    } else if !got_non_silence {
        // Non-deterministic hosts (Windows VB-CABLE, macOS BlackHole/TCC):
        // keep the soft warning — routing here is genuinely flaky.
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
    require_system_capture!();

    let expected_sample_rate = 48000u32;
    let expected_channels = 2u16;

    // Generate + start playing a test tone so SystemDefault loopback
    // has audio to capture. Without this, VB-CABLE on Windows / null-
    // sinks on Linux produce no buffers and the assertion panics.
    // Mirrors the pattern in test_system_capture_receives_audio above.
    let wav_path = helpers::generate_test_wav(5.0, expected_sample_rate, expected_channels);
    let player = helpers::spawn_test_tone_player(&wav_path);
    std::thread::sleep(std::time::Duration::from_millis(500));

    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(expected_sample_rate)
        .channels(expected_channels)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            if let Some(p) = player {
                helpers::stop_player(p);
            }
            if helpers::deterministic_audio_env() {
                panic!("deterministic source: capture build failed: {:?}", e);
            }
            eprintln!("[ci_audio] Failed to build capture: {:?}", e);
            eprintln!("[ci_audio] SKIPPING: Capture build failed");
            return;
        }
    };

    if let Err(e) = capture.start() {
        if let Some(p) = player {
            helpers::stop_player(p);
        }
        if helpers::deterministic_audio_env() {
            panic!("deterministic source: capture start failed: {:?}", e);
        }
        eprintln!("[ci_audio] Failed to start capture: {:?}", e);
        eprintln!("[ci_audio] SKIPPING: Capture start failed");
        return;
    }

    // Try to read a buffer to check format
    let timeout = helpers::capture_timeout();
    let start = Instant::now();
    let mut format_verified = false;

    // Track overrun_count across successive reads — it is a cumulative
    // counter and must be monotonically non-decreasing for the lifetime
    // of the stream. Anything else indicates a bookkeeping regression.
    let mut prev_overrun: Option<u64> = None;
    let mut monotonic_samples: usize = 0;

    while start.elapsed() < timeout {
        match capture.read_buffer() {
            Ok(Some(buffer)) => {
                eprintln!(
                    "[ci_audio] Buffer format: rate={}, channels={}, frames={}",
                    buffer.sample_rate(),
                    buffer.channels(),
                    buffer.num_frames()
                );

                // rsac does not resample: the delivered format equals the
                // *request* only under a deterministic source. On an arbitrary
                // host the device negotiates its own mix format, so we assert
                // self-consistency and only require exact equality where the
                // source format is controlled. See `helpers::assert_buffer_format`.
                helpers::assert_buffer_format(&buffer, expected_sample_rate, expected_channels);

                // Monotonic non-decreasing overrun_count check — property
                // assertion alongside the no-panic backbone.
                let current_overrun = capture.overrun_count();
                if let Some(prev) = prev_overrun {
                    assert!(
                        current_overrun >= prev,
                        "overrun_count must be monotonically non-decreasing: \
                         previous={}, current={}",
                        prev,
                        current_overrun
                    );
                    monotonic_samples += 1;
                }
                prev_overrun = Some(current_overrun);

                // Need at least two observations to have checked monotonicity;
                // keep reading until we have that, then we can bail.
                if monotonic_samples >= 1 {
                    format_verified = true;
                    break;
                }
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

    // If we only got one buffer (CI hardware can be flaky), the format
    // was still verified on that single read — mark it as such without
    // forcing a second read that may never come on a quiet fixture.
    if !format_verified && prev_overrun.is_some() {
        format_verified = true;
    }

    let _ = capture.stop();
    if let Some(p) = player {
        helpers::stop_player(p);
    }

    // If NO buffer arrived: under a deterministic source the tone is
    // guaranteed to flow (Linux null sink / Windows VB-CABLE), so zero
    // buffers means the producer stopped pushing — a real regression, not
    // flakiness. Hard-fail there, mirroring the sibling
    // `test_system_capture_receives_audio` (above). On non-deterministic
    // hosts this is the "no functional loopback" case, so skip honestly.
    // Without this branch the deterministic run was a vacuous PASS: it could
    // return without ever examining a buffer.
    if !format_verified {
        if helpers::deterministic_audio_env() {
            panic!(
                "deterministic source: no buffer arrived within {:?} — the \
                 format-path capture producer stopped pushing under the \
                 null sink / VB-CABLE (real regression, not flakiness)",
                timeout
            );
        }
        eprintln!(
            "[ci_audio] test_capture_format_correct: no buffer arrived \
             within {:?}; environment lacks a functional audio loopback — \
             skipping",
            timeout
        );
    }
}
