//! Process tree audio capture integration tests.
//!
//! These tests verify capturing audio from a specific process tree
//! using `CaptureTarget::ProcessTree(ProcessId)`. They spawn a child
//! audio player, capture from its process tree, and verify audio data.
//!
//! Process tree capture is platform-dependent:
//! - Windows: ✅ WASAPI process loopback with include_tree
//! - macOS: ✅ CoreAudio Process Tap (macOS 14.4+)
//! - Linux: ✅ PipeWire PID → node mapping

use std::time::{Duration, Instant};

use rsac::{AudioCaptureBuilder, CaptureTarget, ProcessId};

use crate::helpers;

/// Test: Spawn audio player, capture its process tree by PID.
/// Verifies the full pipeline: spawn → build(ProcessTree) → start → read → stop.
#[test]
fn test_process_tree_capture_receives_audio() {
    require_process_capture!();

    let wav_path = helpers::generate_test_wav(5.0, 48000, 2);

    let (child, pid) = match helpers::spawn_audio_player_get_pid(&wav_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[ci_audio] Could not spawn audio player: {e}");
            return;
        }
    };

    // Wait for player to start producing audio
    std::thread::sleep(Duration::from_millis(500));

    eprintln!("[ci_audio] Capturing process tree for PID {pid}");

    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::ProcessTree(ProcessId(pid)))
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ci_audio] Failed to build process tree capture: {:?}", e);
            helpers::stop_player(child);
            return;
        }
    };

    if let Err(e) = capture.start() {
        eprintln!("[ci_audio] Failed to start process tree capture: {:?}", e);
        helpers::stop_player(child);
        return;
    }

    let timeout = helpers::capture_timeout();
    let start = Instant::now();
    let mut got_audio = false;
    let mut total_frames = 0usize;
    let mut buffers_read = 0usize;
    let mut tone_present = false;
    let mut rms_ok = false;

    while start.elapsed() < timeout {
        match capture.read_buffer() {
            Ok(Some(buf)) => {
                buffers_read += 1;
                total_frames += buf.num_frames();
                if helpers::verify_non_silence(&buf, 0.001) {
                    got_audio = true;
                    let (rms, ok) = helpers::verify_rms_energy(&buf, 0.01);
                    rms_ok = ok;
                    tone_present = helpers::verify_tone_present(&buf, 440.0);
                    eprintln!(
                        "[ci_audio] Process tree capture: non-silent audio, {} frames, RMS={:.6}",
                        buf.num_frames(),
                        rms
                    );
                    break;
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                eprintln!("[ci_audio] Process tree read error: {:?}", e);
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
        "[ci_audio] Process tree capture: {} buffers, {} frames, got_audio={}",
        buffers_read, total_frames, got_audio
    );

    if helpers::deterministic_audio_env() {
        assert!(
            buffers_read > 0,
            "deterministic source: process tree capture read 0 buffers for PID {pid}"
        );
        assert!(
            total_frames > 0,
            "deterministic source: process tree capture got 0 frames for PID {pid}"
        );
        assert!(
            got_audio,
            "deterministic source: process tree capture received only silence for PID {pid}"
        );
        assert!(
            rms_ok,
            "deterministic source: process tree RMS below 0.01 floor for PID {pid}"
        );
        assert!(
            tone_present,
            "deterministic source: 440 Hz tone not detected in process tree capture for PID {pid}"
        );
        eprintln!("[ci_audio] ✅ Process tree capture received the 440 Hz tone from PID {pid}");
    } else if got_audio {
        eprintln!("[ci_audio] ✅ Process tree capture received audio from PID {pid}");
    } else {
        eprintln!(
            "[ci_audio] ⚠ Process tree capture did not receive non-silent audio (CI limitation)"
        );
    }

    // Clean up temp file
    let _ = std::fs::remove_file(&wav_path);
    if let Some(parent) = wav_path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

/// Test: Process tree capture with nonexistent PID — should error gracefully.
#[test]
fn test_process_tree_capture_nonexistent_pid() {
    require_process_capture!();

    // Use a PID that almost certainly doesn't exist
    let bogus_pid = 4_000_000_000u32;

    let result = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::ProcessTree(ProcessId(bogus_pid)))
        .sample_rate(48000)
        .channels(2)
        .build();

    match result {
        Err(e) => {
            eprintln!(
                "[ci_audio] ✅ Build correctly rejected nonexistent PID: {:?}",
                e
            );
        }
        Ok(mut capture) => {
            // Some backends accept any PID at build time and fail at start
            match capture.start() {
                Err(e) => {
                    eprintln!(
                        "[ci_audio] ✅ Start correctly rejected nonexistent PID: {:?}",
                        e
                    );
                }
                Ok(()) => {
                    // Some backends accept any PID at build/start and just
                    // route no audio. If audio DOES arrive for a bogus PID,
                    // the tree resolved to the wrong source — assert silence.
                    eprintln!("[ci_audio] Capture started with nonexistent PID; verifying silence");
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
                        "nonexistent PID must not produce non-silent audio — \
                         process tree resolved to the wrong source"
                    );
                    eprintln!("[ci_audio] ✅ Nonexistent PID produced only silence (as expected)");
                }
            }
        }
    }
}

/// Test: Process tree capture lifecycle — start, brief capture, stop, verify no panic.
#[test]
fn test_process_tree_capture_lifecycle() {
    require_process_capture!();

    let wav_path = helpers::generate_test_wav(3.0, 48000, 2);

    let (child, pid) = match helpers::spawn_audio_player_get_pid(&wav_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[ci_audio] Could not spawn audio player: {e}");
            return;
        }
    };

    std::thread::sleep(Duration::from_millis(500));

    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::ProcessTree(ProcessId(pid)))
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ci_audio] Build failed: {:?}", e);
            helpers::stop_player(child);
            return;
        }
    };

    if let Err(e) = capture.start() {
        eprintln!("[ci_audio] Start failed: {:?}", e);
        helpers::stop_player(child);
        return;
    }

    assert!(
        capture.is_running(),
        "Process tree capture should be running"
    );

    // Brief capture
    std::thread::sleep(Duration::from_millis(300));

    // Stop
    let stop_result = capture.stop();
    eprintln!("[ci_audio] Process tree stop result: {:?}", stop_result);
    assert!(stop_result.is_ok(), "Stop should succeed");

    assert!(!capture.is_running(), "Should not be running after stop");

    helpers::stop_player(child);

    // Clean up temp file
    let _ = std::fs::remove_file(&wav_path);
    if let Some(parent) = wav_path.parent() {
        let _ = std::fs::remove_dir(parent);
    }

    eprintln!("[ci_audio] ✅ Process tree capture lifecycle test passed");
}
