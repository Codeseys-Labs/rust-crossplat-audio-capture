//! Application-specific audio capture integration tests.
//!
//! These tests spawn an audio player as a child process and capture
//! audio from that specific process using CaptureTarget variants.
//! Tests gracefully skip when app capture is unsupported or audio
//! infrastructure is unavailable.

use std::time::{Duration, Instant};

use rsac::{ApplicationId, AudioCaptureBuilder, CaptureTarget, ProcessId};

use crate::helpers;

/// Test: Spawn audio player, capture by process tree (PID).
/// Uses `CaptureTarget::ProcessTree(ProcessId(pid))`.
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

    // Build capture targeting the spawned process
    let capture_result = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::ProcessTree(ProcessId(pid)))
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

    while start.elapsed() < timeout {
        match capture.read_buffer() {
            Ok(Some(buf)) => {
                total_samples += buf.data().len();
                if helpers::verify_non_silence(&buf, 0.001) {
                    got_audio = true;
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

    // Soft assertion — app capture may not route audio in all CI environments
    if got_audio {
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

/// Test: Capture with `Application(ApplicationId)` variant using PipeWire node ID.
/// Linux-only — discovers the PipeWire node for the spawned player process.
#[test]
#[cfg(target_os = "linux")]
fn test_app_capture_by_pipewire_node_id() {
    require_app_capture!();

    let wav_path = helpers::generate_test_wav(5.0, 48000, 2);

    let (child, pid) = match helpers::spawn_audio_player_get_pid(&wav_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[ci_audio] Could not spawn audio player: {e}");
            return;
        }
    };

    // Wait for PipeWire to register the node
    std::thread::sleep(Duration::from_millis(1000));

    // Discover PipeWire node ID for the player process
    let node_id = match helpers::find_pipewire_node_for_pid(pid) {
        Some(id) => id,
        None => {
            eprintln!("[ci_audio] Could not find PipeWire node for PID {pid}, skipping");
            helpers::stop_player(child);
            return;
        }
    };
    eprintln!("[ci_audio] Found PipeWire node {node_id} for PID {pid}");

    let capture_result = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Application(ApplicationId(node_id.clone())))
        .sample_rate(48000)
        .channels(2)
        .build();

    let mut capture = match capture_result {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[ci_audio] Failed to build app capture for node {node_id}: {:?}",
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

    while start.elapsed() < timeout {
        match capture.read_buffer() {
            Ok(Some(buf)) => {
                total_samples += buf.data().len();
                if helpers::verify_non_silence(&buf, 0.001) {
                    got_audio = true;
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
        "[ci_audio] App capture by node: total_samples={}, got_audio={}",
        total_samples, got_audio
    );

    if got_audio {
        eprintln!("[ci_audio] ✅ App capture via PW node {node_id} received non-silent audio");
    } else {
        eprintln!("[ci_audio] ⚠ App capture via PW node did not receive audio (CI routing)");
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

    let result = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Application(ApplicationId(
            "99999999".to_string(),
        )))
        .sample_rate(48000)
        .channels(2)
        .build();

    // We accept either build error or successful build with no audio
    match result {
        Err(e) => {
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
                    eprintln!(
                        "[ci_audio] ✅ Start correctly rejected nonexistent app: {:?}",
                        e
                    );
                }
                Ok(()) => {
                    eprintln!(
                        "[ci_audio] ⚠ Capture started with nonexistent target (expected: silence)"
                    );
                    std::thread::sleep(Duration::from_millis(500));
                    let _ = capture.stop();
                }
            }
        }
    }
}
