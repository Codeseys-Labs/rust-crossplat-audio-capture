//! Application-specific audio capture integration tests.
//!
//! These tests spawn an audio player as a child process and capture
//! audio from that specific process using CaptureTarget variants.
//! Tests gracefully skip when app capture is unsupported or audio
//! infrastructure is unavailable.

use std::time::{Duration, Instant};

use rsac::{ApplicationId, AudioCaptureBuilder, CaptureTarget};

use crate::helpers;

/// Test: Spawn audio player and capture by application PID.
///
/// `ApplicationId` is a cross-platform numeric PID string. Process-tree
/// behavior is covered separately in `process_tree_capture.rs`.
#[test]
fn test_app_capture_by_process_id() {
    require_app_capture!();

    // Generate test WAV
    let wav_path = helpers::generate_test_wav(5.0, 48000, 2);

    // Spawn audio player and get its PID
    let (child, pid) = match helpers::spawn_audio_player_get_pid(&wav_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[ci_audio] Could not spawn audio player: {e}");
            return;
        }
    };

    // Wait for player to start producing audio
    std::thread::sleep(Duration::from_millis(500));

    // Build capture targeting exactly the spawned process.
    let capture_result = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Application(ApplicationId(pid.to_string())))
        .sample_rate(48000)
        .channels(2)
        .build();

    let mut capture = match capture_result {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[ci_audio] Failed to build app capture (may not be supported in CI): {:?}",
                e
            );
            helpers::stop_player(child);
            return; // Graceful skip
        }
    };

    match capture.start() {
        Ok(()) => {}
        Err(e) => {
            eprintln!("[ci_audio] Failed to start app capture: {:?}", e);
            helpers::stop_player(child);
            return;
        }
    }

    let timeout = helpers::capture_timeout();
    let start = Instant::now();
    let mut got_audio = false;
    let mut total_samples = 0usize;
    let mut tone_present = false;
    let mut rms_ok = false;

    while start.elapsed() < timeout {
        match capture.read_buffer() {
            Ok(Some(buf)) => {
                total_samples += buf.data().len();
                if helpers::verify_non_silence(&buf, 0.001) {
                    got_audio = true;
                    rms_ok = helpers::verify_rms_energy(&buf, 0.01).1;
                    tone_present = helpers::verify_tone_present(&buf, 440.0);
                    break;
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                eprintln!("[ci_audio] Read error: {:?}", e);
                if e.is_fatal() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    let _ = capture.stop();
    helpers::stop_player(child);

    eprintln!(
        "[ci_audio] App capture by PID: total_samples={}, got_audio={}",
        total_samples, got_audio
    );

    if helpers::deterministic_audio_env() {
        // Deterministic Linux source: the spawned player streams a known
        // 440 Hz tone into the null sink, so app capture MUST observe it.
        assert!(
            got_audio,
            "deterministic source: app capture by PID {pid} received only silence"
        );
        assert!(
            rms_ok,
            "deterministic source: app capture RMS below 0.01 floor for PID {pid}"
        );
        assert!(
            tone_present,
            "deterministic source: 440 Hz tone not detected in app capture for PID {pid}"
        );
        eprintln!("[ci_audio] ✅ App capture received the 440 Hz tone from PID {pid}");
    } else if got_audio {
        eprintln!("[ci_audio] ✅ App capture received non-silent audio from PID {pid}");
    } else {
        eprintln!(
            "[ci_audio] ⚠ App capture did not receive non-silent audio (CI routing limitation)"
        );
    }

    // Clean up temp file
    let _ = std::fs::remove_file(&wav_path);
    if let Some(parent) = wav_path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

/// Linux regression: `Application(ApplicationId)` uses a numeric PID string.
///
/// Older tests treated the Linux `ApplicationId` as a PipeWire node ID. The
/// backend contract is now aligned with Windows/macOS: `ApplicationId` is the
/// application process ID, and the PipeWire backend resolves PID -> node
/// internally.
#[test]
#[cfg(target_os = "linux")]
fn test_app_capture_by_pid_string_linux() {
    require_app_capture!();

    let wav_path = helpers::generate_test_wav(5.0, 48000, 2);

    let (child, pid) = match helpers::spawn_audio_player_get_pid(&wav_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[ci_audio] Could not spawn audio player: {e}");
            return;
        }
    };

    // Wait for PipeWire to register the player's stream node.
    std::thread::sleep(Duration::from_millis(1000));

    let capture_result = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Application(ApplicationId(pid.to_string())))
        .sample_rate(48000)
        .channels(2)
        .build();

    let mut capture = match capture_result {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[ci_audio] Failed to build app capture for PID {pid}: {:?}",
                e
            );
            helpers::stop_player(child);
            return;
        }
    };

    if let Err(e) = capture.start() {
        eprintln!("[ci_audio] Failed to start app capture: {:?}", e);
        helpers::stop_player(child);
        return;
    }

    let timeout = helpers::capture_timeout();
    let start = Instant::now();
    let mut got_audio = false;
    let mut total_samples = 0usize;
    let mut tone_present = false;
    let mut rms_ok = false;

    while start.elapsed() < timeout {
        match capture.read_buffer() {
            Ok(Some(buf)) => {
                total_samples += buf.data().len();
                if helpers::verify_non_silence(&buf, 0.001) {
                    got_audio = true;
                    rms_ok = helpers::verify_rms_energy(&buf, 0.01).1;
                    tone_present = helpers::verify_tone_present(&buf, 440.0);
                    break;
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                eprintln!("[ci_audio] Read error: {:?}", e);
                if e.is_fatal() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    let _ = capture.stop();
    helpers::stop_player(child);

    eprintln!(
        "[ci_audio] Linux app capture by PID string: total_samples={}, got_audio={}",
        total_samples, got_audio
    );

    if helpers::deterministic_audio_env() {
        assert!(
            got_audio,
            "deterministic source: app capture via PID {pid} received only silence"
        );
        assert!(
            rms_ok,
            "deterministic source: app capture RMS below 0.01 floor for PID {pid}"
        );
        assert!(
            tone_present,
            "deterministic source: 440 Hz tone not detected via PID {pid}"
        );
        eprintln!("[ci_audio] ✅ App capture via PID {pid} received the 440 Hz tone");
    } else if got_audio {
        eprintln!("[ci_audio] ✅ App capture via PID {pid} received non-silent audio");
    } else {
        eprintln!("[ci_audio] ⚠ App capture via PID did not receive audio (CI routing)");
    }

    // Clean up temp file
    let _ = std::fs::remove_file(&wav_path);
    if let Some(parent) = wav_path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

/// Test: Verify that building capture with a nonexistent application ID
/// either errors gracefully or returns silence (does not panic/crash).
#[test]
fn test_app_capture_nonexistent_target() {
    require_app_capture!();

    // A PID string that cannot correspond to any real audio source.
    let bogus_id = "99999999";
    let result = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Application(ApplicationId(
            bogus_id.to_string(),
        )))
        .sample_rate(48000)
        .channels(2)
        .build();

    // Three legitimate outcomes, each with a SPECIFIC contract:
    //   1. build() errors → must be an application/backend rejection variant,
    //      NOT some unrelated error (e.g. InvalidParameter would mean the
    //      builder mis-validated a perfectly valid sample rate/channels).
    //   2. start() errors → same application/backend rejection variants
    //      (macOS parses the id as a PID and CoreAudioProcessTap::new fails).
    //   3. start() succeeds but yields silence → the PipeWire path, where a
    //      bogus TARGET_OBJECT routes no audio (asserted below).
    //
    // `is_expected_rejection` pins which variants count as a clean rejection
    // so an unrelated error type fails the test instead of passing silently.
    fn is_expected_rejection(e: &rsac::AudioError) -> bool {
        use rsac::AudioError::*;
        matches!(
            e,
            ApplicationNotFound { .. }
                | ApplicationCaptureFailed { .. }
                | BackendError { .. }
                | BackendInitializationFailed { .. }
                | StreamCreationFailed { .. }
                | StreamStartFailed { .. }
                | InternalError { .. }
                // In the dbus-less CI PipeWire VM, resolving the bogus app's
                // default device fails enumeration first — a legitimate
                // rejection of a nonexistent target.
                | DeviceEnumerationError { .. }
        )
    }

    match result {
        Err(e) => {
            assert!(
                is_expected_rejection(&e),
                "build() rejected nonexistent app '{bogus_id}' with an unexpected \
                 error variant (expected an Application*/Backend*/Stream* rejection): {e:?}"
            );
            eprintln!(
                "[ci_audio] ✅ Build correctly rejected nonexistent app ID: {:?}",
                e
            );
        }
        Ok(mut capture) => {
            // Start may succeed — in PipeWire, a nonexistent TARGET_OBJECT
            // just means no audio routes to us
            match capture.start() {
                Err(e) => {
                    assert!(
                        is_expected_rejection(&e),
                        "start() rejected nonexistent app '{bogus_id}' with an \
                         unexpected error variant: {e:?}"
                    );
                    eprintln!(
                        "[ci_audio] ✅ Start correctly rejected nonexistent app: {:?}",
                        e
                    );
                }
                Ok(()) => {
                    // Start may succeed even for a nonexistent target — in
                    // PipeWire a missing TARGET_OBJECT just routes nothing to
                    // us. If audio DOES arrive, it means we bound the wrong
                    // source, which is a real routing bug. Assert silence.
                    eprintln!(
                        "[ci_audio] Capture started with nonexistent target; verifying silence"
                    );
                    let start = Instant::now();
                    let mut produced_audio = false;
                    while start.elapsed() < Duration::from_millis(500) {
                        match capture.read_buffer() {
                            Ok(Some(buf)) => {
                                if helpers::verify_non_silence(&buf, 0.001) {
                                    produced_audio = true;
                                    break;
                                }
                            }
                            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                            Err(_) => break,
                        }
                    }
                    let _ = capture.stop();
                    assert!(
                        !produced_audio,
                        "nonexistent application target must not produce non-silent \
                         audio — capture bound the wrong source"
                    );
                    eprintln!(
                        "[ci_audio] ✅ Nonexistent target produced only silence (as expected)"
                    );
                }
            }
        }
    }
}
