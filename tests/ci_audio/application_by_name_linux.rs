//! `CaptureTarget::ApplicationByName` integration tests (Linux / PipeWire).
//!
//! The Linux backend resolves an application *name* to a node via
//! `app_name_matches(candidate, query)` (`src/audio/linux/thread.rs`):
//! case-insensitive exact match, OR path basename, OR `.exe`-stripped basename —
//! **never** arbitrary substring. It matches against two fields independently:
//! PipeWire `application.name` and `application.process.binary`. Resolution runs
//! at `start()` (not `build()`) and requires the matched node's media class to be
//! `Stream/Output/Audio`.
//!
//! Complements the macOS-only `application_by_name` module (cfg'd out here) and
//! the Windows `application_by_name_windows` module: before this module there was
//! zero integration coverage of the Linux name-resolution path.
//!
//! Blind-test hazard: `helpers::spawn_audio_player_get_pid` picks `pw-play` OR
//! `paplay` depending on `PULSE_SINK`, so the registered `application.name` is
//! NOT a fixed literal. Rather than hardcode `"pw-play"`, we resolve the actual
//! node name for the spawned PID via `helpers::find_pipewire_app_name_for_pid`
//! and feed *that* to `ApplicationByName` — this removes the ambiguity and
//! directly exercises the node-name contract. Because we bind the exact
//! app-name of our own PID's node, content can be HARD-asserted under the
//! deterministic gate (unlike the Windows module, whose first-match powershell
//! may not be the player).
#![cfg(target_os = "linux")]

use std::time::{Duration, Instant};

use rsac::{AudioCaptureBuilder, AudioError, CaptureTarget};

use crate::helpers;

/// A name that cannot plausibly match any real running process.
const MISSING_APP_NAME: &str = "ThisApplicationDefinitelyDoesNotExist_rsac_12345";

/// End-to-end: spawn the CI audio player, resolve its real PipeWire
/// `application.name`, then capture by that name via
/// `CaptureTarget::ApplicationByName` and assert the 440 Hz tone arrives.
#[test]
fn select_by_name_of_running_player_binds_and_reads() {
    require_app_capture!();

    let wav_path = helpers::generate_test_wav(5.0, 48000, 2);

    let (child, pid) = match helpers::spawn_audio_player_get_pid(&wav_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[ci_audio] Could not spawn audio player: {e}");
            let _ = std::fs::remove_file(&wav_path);
            return;
        }
    };

    // Resolve the exact app-name of our PID's node, polling with a bounded
    // deadline: node registration is asynchronous, and a single early pw-dump
    // (or one nameless node — see the helper's multi-node note) must not
    // masquerade as an environment gap.
    let lookup_deadline = Instant::now() + Duration::from_secs(5);
    let app_name = loop {
        if let Some(name) = helpers::find_pipewire_app_name_for_pid(pid) {
            break Some(name);
        }
        if Instant::now() >= lookup_deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let Some(app_name) = app_name else {
        helpers::stop_player(child);
        let _ = std::fs::remove_file(&wav_path);
        if helpers::deterministic_audio_env() {
            // Deterministic leg: the routed player MUST register a node — a
            // missing name after 5s is a real regression, not a routing gap.
            panic!(
                "deterministic source: no PipeWire node/app-name for PID {pid} within 5s — \
                 the player's node never registered or carries no usable name"
            );
        }
        eprintln!(
            "[ci_audio] no PipeWire node/app-name for PID {pid}; skipping (CI routing limitation)"
        );
        return;
    };
    eprintln!("[ci_audio] Resolved PID {pid} to application.name='{app_name}'");

    let capture_result = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::ApplicationByName(app_name.clone()))
        .sample_rate(48000)
        .channels(2)
        .build();

    let mut capture = match capture_result {
        Ok(c) => c,
        Err(e) => {
            if helpers::deterministic_audio_env() {
                helpers::stop_player(child);
                let _ = std::fs::remove_file(&wav_path);
                panic!(
                    "deterministic source: ApplicationByName('{app_name}') build failed — \
                     capture must build under RSAC_CI_AUDIO_DETERMINISTIC=1: {e:?}"
                );
            }
            eprintln!("[ci_audio] Failed to build ApplicationByName('{app_name}') capture: {e:?}");
            helpers::stop_player(child);
            let _ = std::fs::remove_file(&wav_path);
            return;
        }
    };

    if let Err(e) = capture.start() {
        // Resolution happens here; a deterministic source must resolve + start.
        if helpers::deterministic_audio_env() {
            helpers::stop_player(child);
            let _ = std::fs::remove_file(&wav_path);
            panic!(
                "deterministic source: ApplicationByName('{app_name}') start failed — \
                 capture must start under RSAC_CI_AUDIO_DETERMINISTIC=1: {e:?}"
            );
        }
        eprintln!("[ci_audio] Failed to start ApplicationByName('{app_name}') capture: {e:?}");
        helpers::stop_player(child);
        let _ = std::fs::remove_file(&wav_path);
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
                helpers::assert_buffer_format(&buf, 48000, 2);
                if helpers::verify_non_silence(&buf, 0.001) {
                    got_audio = true;
                    rms_ok = helpers::verify_rms_energy(&buf, 0.01).1;
                    tone_present = helpers::verify_tone_present(&buf, 440.0);
                    break;
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                eprintln!("[ci_audio] Read error: {e:?}");
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
        "[ci_audio] ApplicationByName('{app_name}'): total_samples={total_samples}, got_audio={got_audio}"
    );

    if helpers::deterministic_audio_env() {
        // Deterministic Linux source: we resolved the exact app-name of OUR
        // player's node, so its 440 Hz tone MUST route to this capture.
        assert!(
            got_audio,
            "deterministic source: ApplicationByName('{app_name}') received only silence"
        );
        assert!(
            rms_ok,
            "deterministic source: ApplicationByName('{app_name}') RMS below 0.01 floor"
        );
        assert!(
            tone_present,
            "440 Hz tone not detected via ApplicationByName('{app_name}')"
        );
        eprintln!("[ci_audio] ✅ ApplicationByName('{app_name}') received the 440 Hz tone");
    } else if got_audio {
        eprintln!("[ci_audio] ✅ ApplicationByName('{app_name}') received non-silent audio");
    } else {
        eprintln!(
            "[ci_audio] ⚠ ApplicationByName('{app_name}') did not receive audio (CI routing limitation)"
        );
    }

    let _ = std::fs::remove_file(&wav_path);
    if let Some(parent) = wav_path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

/// A name that matches no running process must be rejected — it must never bind
/// and stream audio. On Linux the no-match `ByAppName` path returns
/// `AudioError::ApplicationNotFound` (with the requested name in `identifier`)
/// when resolution reaches the pw-dump matcher; but on the dbus-less Firecracker
/// runner where `pw-dump` is unreachable, resolution can surface a
/// `BackendError` / `DeviceEnumerationError` instead. We therefore pin the
/// *contract* (a documented rejection variant, and if it is `ApplicationNotFound`
/// the identifier embeds the name), tolerating the environment-gap variants as a
/// clean rejection rather than false-RED-ing on CI routing. Resolution runs at
/// `start()`; `build()` may surface it if a precheck is added later.
#[test]
fn select_by_missing_name_returns_error() {
    require_app_capture!();

    // The set of variants that count as a clean rejection of a missing name.
    // ApplicationNotFound is the documented no-match signal; the Backend/Device
    // variants cover the dbus-less runner where pw-dump/native enumeration is
    // unreachable (mirrors app_capture.rs::is_expected_rejection).
    fn assert_expected_rejection(e: &AudioError) {
        use rsac::AudioError::*;
        if let ApplicationNotFound { identifier } = e {
            assert!(
                identifier.contains(MISSING_APP_NAME),
                "ApplicationNotFound identifier should embed the missing name, got: {identifier}"
            );
            eprintln!("[ci_audio] ✅ Got ApplicationNotFound for missing name: {identifier}");
            return;
        }
        assert!(
            matches!(
                e,
                BackendError { .. }
                    | BackendInitializationFailed { .. }
                    | DeviceEnumerationError { .. }
                    | StreamCreationFailed { .. }
                    | StreamStartFailed { .. }
            ),
            "missing app name '{MISSING_APP_NAME}' must be rejected with ApplicationNotFound \
             (or an environment-gap Backend*/Stream*/DeviceEnumeration variant on the \
             dbus-less runner); got an unexpected variant: {e:?}"
        );
        eprintln!("[ci_audio] ✅ Missing name rejected with an environment-gap variant: {e:?}");
    }

    let build = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::ApplicationByName(
            MISSING_APP_NAME.to_string(),
        ))
        .sample_rate(48000)
        .channels(2)
        .build();

    let mut capture = match build {
        Ok(c) => c,
        Err(e) => {
            assert_expected_rejection(&e);
            return;
        }
    };

    match capture.start() {
        Err(e) => assert_expected_rejection(&e),
        Ok(()) => {
            // On the deterministic leg the resolver is reachable, so a missing
            // name MUST be rejected at start() (resolution runs there) — a
            // silent successful start would mean the documented
            // ApplicationNotFound contract is broken.
            if helpers::deterministic_audio_env() {
                let _ = capture.stop();
                panic!(
                    "deterministic source: start() succeeded for missing app name \
                     '{MISSING_APP_NAME}' — resolution must reject it with \
                     ApplicationNotFound"
                );
            }
            // Non-deterministic hosts only: PipeWire may start with an
            // unresolved target and simply route no audio. The missing name
            // must then yield only silence — non-silent audio would mean we
            // bound the wrong source.
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
                "missing app name '{MISSING_APP_NAME}' must not produce non-silent audio — \
                 capture bound the wrong source"
            );
            eprintln!("[ci_audio] ✅ Missing name started but produced only silence (as expected)");
        }
    }
}
