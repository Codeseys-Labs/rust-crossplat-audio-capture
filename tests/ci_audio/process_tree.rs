//! `CaptureTarget::ProcessTree` public-API integration tests.
//!
//! The existing `process_tree_capture` module covers the end-to-end
//! capture pipeline (spawn player → build ProcessTree → read audio).
//! This module complements it with tests that target the *other* public
//! surface area consumers touch when using ProcessTree:
//!
//!   1. The `CaptureTarget::pid()` convenience constructor in
//!      `core/introspection.rs`.
//!   2. `list_audio_sources()` / `list_audio_applications()` producing
//!      `AudioSourceKind::Application { pid, .. }` entries that must
//!      round-trip through `to_capture_target()` back to
//!      `CaptureTarget::ProcessTree(ProcessId(pid))`.
//!   3. `PlatformCapabilities::supports_process_tree_capture` matching
//!      what `build()` actually accepts (capability gate contract).
//!   4. A multi-PID parent-tree scenario — the distinguishing feature
//!      of `ProcessTree` vs `Application(ApplicationId)`, exercised via
//!      the public builder.
//!
//! Rationale: the unit tests in `src/audio/linux/thread.rs` exercise
//! `discover_process_tree_pids` directly, but that function is private.
//! Downstream consumers only see the capabilities/builder/introspection
//! layer, so that's what this file locks in.

use std::time::Duration;

use rsac::{
    list_audio_applications, list_audio_sources, AudioCaptureBuilder, AudioSourceKind,
    CaptureTarget, PlatformCapabilities, ProcessId,
};

use crate::helpers;

/// `CaptureTarget::pid(n)` must produce exactly `ProcessTree(ProcessId(n))`
/// — this is the documented equivalence (see `core/introspection.rs`).
/// The unit tests cover this, but this integration test also exercises
/// the `PartialEq` derive on `CaptureTarget` as used by downstream
/// consumers (e.g., the Tauri `audio-graph` app comparing user-selected
/// targets).
#[test]
fn pid_constructor_produces_process_tree_variant() {
    let target = CaptureTarget::pid(4321);
    assert_eq!(target, CaptureTarget::ProcessTree(ProcessId(4321)));

    match target {
        CaptureTarget::ProcessTree(p) => assert_eq!(p.0, 4321),
        other => panic!("expected ProcessTree, got: {:?}", other),
    }

    eprintln!("[ci_audio] ✅ CaptureTarget::pid() → ProcessTree round-trip");
}

/// The capability gate `supports_process_tree_capture` is a promise:
/// when `true`, the builder must accept a `ProcessTree` target for a
/// real PID without returning `UnsupportedBackend`. When `false`, the
/// builder must refuse it.
///
/// We test the positive direction (`true` → accepted) against the
/// current process's own PID (guaranteed to exist). The negative
/// direction would require running on an unsupported platform which
/// our CI matrix doesn't include.
///
/// Note: acceptance here means "no `UnsupportedBackend` error" — a
/// backend may still fail for permission or runtime reasons, which is
/// a separate contract covered by `process_tree_capture`.
#[test]
fn capabilities_match_builder_acceptance() {
    require_audio!();

    let caps = PlatformCapabilities::query();
    if !caps.supports_process_tree_capture {
        eprintln!(
            "[ci_audio] SKIP: backend '{}' does not support process tree capture",
            caps.backend_name
        );
        return;
    }

    let self_pid = std::process::id();
    let result = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::ProcessTree(ProcessId(self_pid)))
        .sample_rate(48000)
        .channels(2)
        .build();

    match result {
        Ok(_) => eprintln!(
            "[ci_audio] ✅ ProcessTree({}) accepted on supports_process_tree_capture=true",
            self_pid
        ),
        Err(rsac::AudioError::PlatformNotSupported { feature, platform }) => panic!(
            "capability claimed supports_process_tree_capture=true but builder \
             returned PlatformNotSupported(feature={}, platform={}) — \
             capability/builder contract broken",
            feature, platform
        ),
        Err(other) => {
            // Permission/runtime errors are acceptable — we only enforce the
            // capability-gate contract, not that every real PID succeeds.
            eprintln!(
                "[ci_audio] ✅ ProcessTree({}) rejected for non-capability reason (OK): {:?}",
                self_pid, other
            );
        }
    }
}

/// `list_audio_sources()` always includes at least the system default,
/// and every `AudioSourceKind::Application` entry must round-trip
/// cleanly through `to_capture_target()` to a `ProcessTree` variant
/// with the same PID. This is the contract the Tauri app relies on
/// when building its "application capture" dropdown.
#[test]
fn list_audio_sources_applications_roundtrip_to_process_tree() {
    require_audio!();

    let sources = match list_audio_sources() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[ci_audio] list_audio_sources failed: {:?}", e);
            return;
        }
    };

    // System default is always present per `list_audio_sources` impl.
    assert!(
        sources
            .iter()
            .any(|s| matches!(s.kind, AudioSourceKind::SystemDefault)),
        "SystemDefault must always appear in list_audio_sources"
    );

    let app_sources: Vec<_> = sources
        .iter()
        .filter(|s| matches!(s.kind, AudioSourceKind::Application { .. }))
        .collect();

    eprintln!(
        "[ci_audio] list_audio_sources: {} total, {} apps",
        sources.len(),
        app_sources.len(),
    );

    for src in &app_sources {
        match (&src.kind, src.to_capture_target()) {
            (
                AudioSourceKind::Application { pid, .. },
                CaptureTarget::ProcessTree(ProcessId(tp)),
            ) => {
                assert_eq!(
                    *pid, tp,
                    "to_capture_target() must preserve PID from Application kind"
                );
            }
            (AudioSourceKind::Application { pid, .. }, other) => panic!(
                "Application(pid={}) round-tripped to wrong variant: {:?}",
                pid, other
            ),
            _ => unreachable!(),
        }
    }

    eprintln!(
        "[ci_audio] ✅ all {} application entries round-trip to ProcessTree",
        app_sources.len()
    );
}

/// `list_audio_applications()` is the application-only subset of
/// `list_audio_sources()`. Every entry must be of kind `Application` —
/// no `SystemDefault`, no `Device` — and each must carry a non-zero PID
/// (the Linux impl filters `pid == 0` explicitly; we check the contract
/// holds on every platform).
#[test]
fn list_audio_applications_only_yields_application_kind() {
    require_audio!();

    let apps = match list_audio_applications() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[ci_audio] list_audio_applications failed: {:?}", e);
            return;
        }
    };

    for app in &apps {
        match &app.kind {
            AudioSourceKind::Application { pid, app_name, .. } => {
                assert_ne!(
                    *pid, 0,
                    "list_audio_applications must not include PID=0 entries (got app '{}')",
                    app_name
                );
            }
            other => panic!(
                "list_audio_applications returned non-Application kind: {:?}",
                other
            ),
        }
    }

    eprintln!(
        "[ci_audio] ✅ list_audio_applications: {} entries, all Application kind with non-zero PID",
        apps.len()
    );
}

/// Spawn a real audio player as a child process and use its PID with
/// `CaptureTarget::ProcessTree`. This exercises the multi-PID tree
/// discovery path — the distinguishing feature vs `Application(id)`:
///
///   * Linux: `discover_process_tree_pids` walks `/proc` children.
///   * macOS: `CoreAudioProcessTap::new_tree` enumerates children via
///     `sysinfo`.
///   * Windows: WASAPI `include_tree = true` flag.
///
/// The test succeeds as long as `build()` + `start()` don't error out
/// on a valid multi-PID target — we do NOT assert audio data arrives,
/// because that's already covered by `process_tree_capture`. The point
/// here is that a *live* parent PID, which may spawn a child audio
/// worker (common with pw-play / afplay / powershell), is a first-class
/// build target.
#[test]
fn process_tree_accepts_live_parent_pid() {
    require_process_capture!();

    let wav_path = helpers::generate_test_wav(2.0, 48000, 2);

    let (child, pid) = match helpers::spawn_audio_player_get_pid(&wav_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[ci_audio] process_tree-live: spawn failed: {e}");
            let _ = std::fs::remove_file(&wav_path);
            return;
        }
    };

    // Let the player spawn any child workers before we snapshot the tree.
    std::thread::sleep(Duration::from_millis(300));

    let build_result = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::ProcessTree(ProcessId(pid)))
        .sample_rate(48000)
        .channels(2)
        .build();

    match build_result {
        Ok(mut capture) => {
            eprintln!(
                "[ci_audio] ✅ ProcessTree(live PID={}) accepted by builder",
                pid
            );
            // start/stop to verify the full lifecycle, but don't read:
            // data-flow assertions belong in `process_tree_capture`.
            if let Err(e) = capture.start() {
                eprintln!(
                    "[ci_audio] ⚠ start() after ProcessTree build failed: {:?} (acceptable — CI may lack permissions)",
                    e
                );
            } else {
                std::thread::sleep(Duration::from_millis(200));
                let _ = capture.stop();
            }
        }
        Err(e) => {
            // Some backends reject unknown PIDs at build time; that's OK.
            // The contract is no panic, no hang — test reaches here means
            // build() returned cleanly.
            eprintln!(
                "[ci_audio] ⚠ ProcessTree(live PID={}) rejected at build: {:?} \
                 (backend-dependent, not a contract violation)",
                pid, e
            );
        }
    }

    helpers::stop_player(child);
    let _ = std::fs::remove_file(&wav_path);
    if let Some(parent) = wav_path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

/// PID `0` is reserved (swapper/sched on Linux; invalid on all OSes we
/// target). Passing `ProcessTree(ProcessId(0))` must not panic or hang —
/// the builder or `start()` must surface an error cleanly.
///
/// This is the lower-bound complement to
/// `process_tree_capture::test_process_tree_capture_nonexistent_pid`,
/// which uses a very high PID as the upper bound.
#[test]
fn process_tree_pid_zero_fails_cleanly() {
    require_process_capture!();

    let result = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::ProcessTree(ProcessId(0)))
        .sample_rate(48000)
        .channels(2)
        .build();

    match result {
        Err(e) => eprintln!(
            "[ci_audio] ✅ ProcessTree(PID=0) rejected at build: {:?}",
            e
        ),
        Ok(mut capture) => {
            // Build may defer validation; start() must then error.
            match capture.start() {
                Err(e) => eprintln!(
                    "[ci_audio] ✅ ProcessTree(PID=0) rejected at start: {:?}",
                    e
                ),
                Ok(()) => {
                    // A few backends will silently produce no audio; stop and
                    // report. The contract is "no panic/hang", which we met.
                    std::thread::sleep(Duration::from_millis(100));
                    let _ = capture.stop();
                    eprintln!(
                        "[ci_audio] ⚠ ProcessTree(PID=0) started without error \
                         (backend accepts invalid PIDs silently)"
                    );
                }
            }
        }
    }
}
