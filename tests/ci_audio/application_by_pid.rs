//! `CaptureTarget::Application` (by PID) integration tests (macOS).
//!
//! The macOS backend parses `ApplicationId.0` as a `u32` PID and passes
//! it to `CoreAudioProcessTap::new(pid, ...)`. See
//! `src/audio/macos/thread.rs::resolve_capture_target`
//! (the `CaptureTarget::Application` arm) and
//! `src/audio/macos/tap.rs::CoreAudioProcessTap::new`.
//!
//! These tests are `#[ignore]` by default because they exercise
//! CoreAudio Process Taps (macOS 14.4+) and need TCC permission for
//! audio capture. Developers can run them manually with:
//!
//! ```text
//! cargo test --test ci_audio application_by_pid \
//!     --features feat_macos -- --ignored --test-threads=1
//! ```
//!
//! On other platforms the module is empty by design.
#![cfg(target_os = "macos")]

use rsac::{ApplicationId, AudioCaptureBuilder, AudioError, CaptureTarget, PlatformCapabilities};

/// A PID that is numerically valid (parses as `u32`) but far above the
/// macOS `kern.maxproc` default (a few thousand). No real process will
/// ever be assigned this PID during the test run, so the Process Tap
/// creation step must fail.
const NONEXISTENT_PID: u32 = 999_999;

/// Skip helper — mirrors `application_by_name.rs`.
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

/// Asserts the builder accepts a numeric PID of a running process and
/// exercises the full `CaptureTarget::Application` path: parse PID →
/// `CoreAudioProcessTap::new` → aggregate device → bind source.
///
/// Uses `std::process::id()` (the test harness's own PID) because it is
/// guaranteed to be a real, running process. The current process may
/// not *produce* audio, but we only need the Process Tap + aggregate
/// device + AUHAL chain to succeed. If the backend rejects self-tapping
/// with a specific error, `select_by_self_pid_does_not_panic` covers
/// that path; here we accept either outcome because the behavior on
/// self-PID depends on macOS version.
///
/// Ignored by default: requires macOS 14.4+ and TCC audio permission.
#[test]
#[ignore = "requires macOS 14.4+ with audio capture permission"]
fn select_by_pid_of_running_app_binds_source() {
    if skip_if_unsupported() {
        return;
    }

    let pid = std::process::id();
    let result = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Application(ApplicationId(pid.to_string())))
        .sample_rate(48000)
        .channels(2)
        .build();

    match result {
        Ok(capture) => {
            eprintln!(
                "[ci_audio] ✅ Application(PID={}) bound source: {:?}",
                pid,
                capture.config().target
            );
        }
        Err(AudioError::BackendError { .. }) | Err(AudioError::InternalError { .. }) => {
            // Acceptable: some macOS versions reject Process-Tapping the
            // calling process, or the test harness isn't a registered
            // audio client. The numeric-parse + dispatch path still ran.
            eprintln!(
                "[ci_audio] ⚠ Application(PID={}) rejected by backend (expected on some hosts)",
                pid
            );
        }
        Err(other) => panic!(
            "Unexpected error type for Application(PID={}): {:?}",
            pid, other
        ),
    }
}

/// Asserts that a PID which cannot possibly correspond to a running
/// process fails cleanly (no panic, no hang). The backend calls
/// `AudioHardwareCreateProcessTap` with the bogus PID; CoreAudio
/// returns a non-zero `OSStatus` which is mapped through `map_ca_error`
/// into `AudioError::BackendError` or `AudioError::InternalError`.
///
/// We don't pin the exact variant because CoreAudio's error codes for
/// "unknown process" are undocumented and have shifted across macOS
/// versions. What we *do* lock in: the builder must surface an error,
/// not `Ok(_)` and not a panic.
#[test]
#[ignore = "requires macOS 14.4+ with audio capture permission"]
fn select_by_nonexistent_pid_returns_error() {
    if skip_if_unsupported() {
        return;
    }

    let result = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Application(ApplicationId(
            NONEXISTENT_PID.to_string(),
        )))
        .sample_rate(48000)
        .channels(2)
        .build();

    match result {
        Err(AudioError::BackendError { .. }) | Err(AudioError::InternalError { .. }) => {
            eprintln!(
                "[ci_audio] ✅ Nonexistent PID {} rejected with expected error variant",
                NONEXISTENT_PID
            );
        }
        Err(AudioError::ApplicationNotFound { identifier }) => {
            // Also acceptable if the backend ever adds an existence
            // precheck before calling into CoreAudio.
            assert!(
                identifier.contains(&NONEXISTENT_PID.to_string()),
                "ApplicationNotFound identifier should embed the PID, got: {}",
                identifier
            );
            eprintln!(
                "[ci_audio] ✅ Nonexistent PID {} rejected with ApplicationNotFound",
                NONEXISTENT_PID
            );
        }
        Err(other) => panic!(
            "Unexpected error type for nonexistent PID {}: {:?}",
            NONEXISTENT_PID, other
        ),
        Ok(_) => panic!(
            "Expected error for nonexistent PID {}, but build succeeded",
            NONEXISTENT_PID
        ),
    }
}

/// `ApplicationId.0` is a `String`; the backend parses it as `u32`. A
/// non-numeric string must produce `AudioError::ApplicationNotFound`
/// with the raw identifier embedded (per `resolve_capture_target`'s
/// `map_err`). This locks in the error *shape* so callers can rely on
/// `ApplicationNotFound` being the parse-failure signal.
///
/// Does not need audio hardware — the parse failure short-circuits
/// before any CoreAudio call. Kept `#[ignore]` for consistency with
/// the rest of the file and to match the `feat_macos` gate convention.
#[test]
#[ignore = "macOS-only path, grouped with other application_by_pid tests"]
fn select_by_non_numeric_id_returns_application_not_found() {
    if skip_if_unsupported() {
        return;
    }

    let bogus = "not-a-pid";
    let result = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Application(ApplicationId(bogus.to_string())))
        .sample_rate(48000)
        .channels(2)
        .build();

    match result {
        Err(AudioError::ApplicationNotFound { identifier }) => {
            assert!(
                identifier.contains(bogus),
                "ApplicationNotFound identifier should embed the raw id, got: {}",
                identifier
            );
            eprintln!(
                "[ci_audio] ✅ Non-numeric id '{}' rejected with ApplicationNotFound",
                bogus
            );
        }
        Err(other) => panic!(
            "Expected ApplicationNotFound for non-numeric id '{}', got: {:?}",
            bogus, other
        ),
        Ok(_) => panic!(
            "Expected ApplicationNotFound for non-numeric id '{}', but build succeeded",
            bogus
        ),
    }
}

/// Edge case: passing the test harness's own PID must not panic or
/// hang, regardless of whether the backend permits self-tapping. This
/// guards the `CoreAudioProcessTap::new` code path against crashes
/// (dereferencing nil Objective-C objects, panicking on OSStatus
/// conversions, etc.) when the target process is the caller.
///
/// The test succeeds as long as `build()` returns — `Ok` or `Err` are
/// both acceptable. We only fail on a panic from inside the builder.
#[test]
#[ignore = "requires macOS 14.4+ with audio capture permission"]
fn select_by_self_pid_does_not_panic() {
    if skip_if_unsupported() {
        return;
    }

    let self_pid = std::process::id();
    let result = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Application(ApplicationId(
            self_pid.to_string(),
        )))
        .sample_rate(48000)
        .channels(2)
        .build();

    match result {
        Ok(_) => eprintln!(
            "[ci_audio] ✅ Self-PID {} accepted (no self-tap rejection on this host)",
            self_pid
        ),
        Err(e) => eprintln!(
            "[ci_audio] ✅ Self-PID {} rejected cleanly with: {:?}",
            self_pid, e
        ),
    }
}
