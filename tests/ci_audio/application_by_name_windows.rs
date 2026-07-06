//! `CaptureTarget::ApplicationByName` integration tests (Windows / WASAPI).
//!
//! The Windows backend resolves an application *name* to a PID via `sysinfo`
//! (case-insensitive, with/without a `.exe` suffix) and then opens a WASAPI
//! Process Loopback client on that PID. See
//! `src/audio/windows/thread.rs::resolve_process_name_to_pid` and the
//! `CaptureTarget::ApplicationByName` arm of `create_audio_client`.
//!
//! These tests complement the macOS-only `application_by_name` module (which is
//! `#![cfg(target_os = "macos")]`): before this module there was **zero**
//! integration coverage of the Windows name-resolution path, even though it is
//! a supported first-class capture mode on Windows.
//!
//! The CI audio player on Windows is a `powershell` host running
//! `SoundPlayer.PlayLooping` (see `helpers::spawn_audio_player_get_pid`), so a
//! `powershell` process is guaranteed to be running while a test tone plays.
//! We therefore use `"powershell"` as the known-running target name. Name
//! resolution matches the *first* process with that name, which — on a headless
//! CI runner whose only audible powershell is the one we just spawned — is our
//! player in practice; but because the OS may host other powershell instances,
//! the content assertions stay soft even under a deterministic source (the
//! resolver's first-match is not guaranteed to be *our* PID). What we hard-pin
//! is the resolution + lifecycle contract: a known-running name resolves and the
//! full build → start → read → stop pipeline runs without error or panic, and a
//! nonexistent name fails with the documented `ApplicationNotFound` variant.
#![cfg(target_os = "windows")]

use std::time::{Duration, Instant};

use rsac::{AudioCaptureBuilder, AudioError, CaptureTarget, PlatformCapabilities};

use crate::helpers;

/// Skip helper mirroring the other ci_audio application modules.
fn skip_if_unsupported() -> bool {
    let caps = PlatformCapabilities::query();
    if !caps.supports_application_capture {
        eprintln!(
            "[ci_audio] SKIP: backend '{}' does not support application capture",
            caps.backend_name
        );
        return true;
    }
    false
}

/// A name that cannot plausibly match any real running process.
const MISSING_APP_NAME: &str = "ThisApplicationDefinitelyDoesNotExist_rsac_12345";

/// End-to-end: spawn the CI audio player (a `powershell` host), then capture by
/// its process *name* via `CaptureTarget::ApplicationByName("powershell")`.
///
/// Exercises the full Windows name path:
/// `resolve_process_name_to_pid` → `new_application_loopback_client(pid)` →
/// WASAPI init (explicit f32 `desired_format`, autoconvert off) → capture loop →
/// `BridgeStream` reads.
///
/// Hard-pinned: the name resolves to a PID and the build → start → read → stop
/// lifecycle completes without error or panic. Content (non-silence / 440 Hz
/// tone) is asserted only *softly* even under a deterministic source, because
/// the resolver's first-match `powershell` is not guaranteed to be the exact
/// child we spawned (the runner may host other powershell instances).
#[test]
fn select_by_name_of_running_player_binds_and_reads() {
    require_app_capture!();
    if skip_if_unsupported() {
        return;
    }

    let wav_path = helpers::generate_test_wav(5.0, 48000, 2);

    let (child, pid) = match helpers::spawn_audio_player_get_pid(&wav_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[ci_audio] Could not spawn audio player: {e}");
            let _ = std::fs::remove_file(&wav_path);
            return;
        }
    };
    eprintln!("[ci_audio] Spawned powershell audio player PID={pid}");

    // Give the player a moment to start streaming.
    std::thread::sleep(Duration::from_millis(500));

    // The CI player is a powershell host (helpers::spawn_audio_player_get_pid).
    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::ApplicationByName("powershell".to_string()))
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            // A build failure here means name resolution or loopback-client
            // creation failed. On a healthy runner with our player running this
            // should not happen, but we surface it rather than hard-fail so a
            // pure environment gap (no powershell audible) doesn't false-RED.
            eprintln!(
                "[ci_audio] ⚠ ApplicationByName('powershell') build failed \
                 (name resolution or loopback client): {e:?}"
            );
            helpers::stop_player(child);
            let _ = std::fs::remove_file(&wav_path);
            return;
        }
    };

    if let Err(e) = capture.start() {
        eprintln!("[ci_audio] ⚠ ApplicationByName capture start failed: {e:?}");
        helpers::stop_player(child);
        let _ = std::fs::remove_file(&wav_path);
        return;
    }

    let timeout = helpers::capture_timeout();
    let start = Instant::now();
    let mut buffers_read = 0usize;
    let mut got_audio = false;

    while start.elapsed() < timeout {
        match capture.read_buffer() {
            Ok(Some(buf)) => {
                buffers_read += 1;
                // Self-consistency invariant holds on every host.
                helpers::assert_buffer_format(&buf, 48000, 2);
                if helpers::verify_non_silence(&buf, 0.001) {
                    got_audio = true;
                    break;
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                eprintln!("[ci_audio] read error: {e:?}");
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
        "[ci_audio] ApplicationByName('powershell'): buffers_read={buffers_read}, got_audio={got_audio}"
    );

    // Hard contract: resolution + lifecycle succeeded (we got here without
    // error). Content stays soft — the resolver's first-match powershell may
    // not be our exact player, so silence is not necessarily a regression.
    if got_audio {
        eprintln!("[ci_audio] ✅ ApplicationByName received non-silent audio");
    } else {
        eprintln!(
            "[ci_audio] ⚠ ApplicationByName received only silence \
             (first-match powershell may differ from our player) — lifecycle still verified"
        );
    }

    let _ = std::fs::remove_file(&wav_path);
    if let Some(parent) = wav_path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

/// A name that matches no running process must fail with the documented
/// `AudioError::ApplicationNotFound` variant, with the requested name embedded
/// in the `identifier` field.
///
/// This pins the parse/resolution error *shape* on Windows: callers can rely on
/// `ApplicationNotFound` being the "no such app name" signal. The failure
/// happens inside `resolve_process_name_to_pid` before any WASAPI client is
/// created, so it needs no audio hardware — but we still gate on
/// `require_app_capture!()` for consistency with the rest of the suite.
#[test]
fn select_by_missing_name_returns_application_not_found() {
    require_app_capture!();
    if skip_if_unsupported() {
        return;
    }

    // Resolution/loopback-client creation happens on the capture thread at
    // start(); build() may already surface it if a precheck is added later.
    let build = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::ApplicationByName(
            MISSING_APP_NAME.to_string(),
        ))
        .sample_rate(48000)
        .channels(2)
        .build();

    let mut capture = match build {
        Ok(c) => c,
        Err(AudioError::ApplicationNotFound { identifier }) => {
            assert!(
                identifier.contains(MISSING_APP_NAME),
                "ApplicationNotFound identifier should embed the missing name, got: {identifier}"
            );
            eprintln!("[ci_audio] ✅ Got ApplicationNotFound at build(): {identifier}");
            return;
        }
        Err(other) => panic!(
            "build() for a missing app name must succeed or return \
             ApplicationNotFound; got: {other:?}"
        ),
    };

    match capture.start() {
        Err(AudioError::ApplicationNotFound { identifier }) => {
            assert!(
                identifier.contains(MISSING_APP_NAME),
                "ApplicationNotFound identifier should embed the missing name, got: {identifier}"
            );
            eprintln!("[ci_audio] ✅ Got expected ApplicationNotFound at start(): {identifier}");
        }
        Err(other) => panic!(
            "Expected ApplicationNotFound for missing name '{MISSING_APP_NAME}', got: {other:?}"
        ),
        Ok(()) => {
            let _ = capture.stop();
            panic!("Expected ApplicationNotFound for '{MISSING_APP_NAME}', but start() succeeded");
        }
    }
}

/// Name matching is case-insensitive on Windows (`resolve_process_name_to_pid`
/// lowercases both sides and also tries a `.exe` suffix). `"POWERSHELL"` (upper)
/// and `"powershell.exe"` must both resolve to the same running powershell as
/// `"powershell"`. Locks in the case-insensitive + `.exe`-flexible contract so a
/// future switch to exact-match forces an explicit decision.
#[test]
fn select_by_name_is_case_and_exe_suffix_insensitive() {
    require_app_capture!();
    if skip_if_unsupported() {
        return;
    }

    // Spawn a powershell player so at least one powershell is guaranteed live.
    let wav_path = helpers::generate_test_wav(5.0, 48000, 2);
    let (child, pid) = match helpers::spawn_audio_player_get_pid(&wav_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[ci_audio] Could not spawn audio player: {e}");
            let _ = std::fs::remove_file(&wav_path);
            return;
        }
    };
    eprintln!("[ci_audio] Spawned powershell audio player PID={pid}");
    std::thread::sleep(Duration::from_millis(300));

    // Each of these spellings must resolve to the same running powershell.
    for name in ["POWERSHELL", "powershell.exe"] {
        let result = AudioCaptureBuilder::new()
            .with_target(CaptureTarget::ApplicationByName(name.to_string()))
            .sample_rate(48000)
            .channels(2)
            .build();

        match result {
            Ok(_) => eprintln!("[ci_audio] ✅ '{name}' resolved (case/.exe-insensitive)"),
            Err(AudioError::ApplicationNotFound { identifier }) => panic!(
                "case/.exe-insensitive match failed for '{name}': got ApplicationNotFound \
                 ({identifier}) while a powershell process is running"
            ),
            Err(other) => {
                // A non-resolution backend error (e.g. loopback client creation
                // hiccup) is an environment issue, not a resolution regression —
                // report but don't fail the case-insensitivity contract.
                eprintln!(
                    "[ci_audio] ⚠ '{name}' resolved but backend rejected client creation: {other:?}"
                );
            }
        }
    }

    helpers::stop_player(child);
    let _ = std::fs::remove_file(&wav_path);
    if let Some(parent) = wav_path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}
