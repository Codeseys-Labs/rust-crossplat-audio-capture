//! `CaptureTarget::Application` (by PID) integration tests (macOS).
//!
//! The macOS backend parses `ApplicationId.0` as a `u32` PID and passes
//! it to `CoreAudioProcessTap::new(pid, ...)`. See
//! `src/audio/macos/thread.rs::resolve_capture_target`
//! (the `CaptureTarget::Application` arm) and
//! `src/audio/macos/tap.rs::CoreAudioProcessTap::new`.
//!
//! The Process Tap tests are `#[ignore]` by default because they exercise
//! CoreAudio Process Taps (macOS 14.4+) and need TCC permission for
//! audio capture. The non-numeric parse test is safe to run because it fails
//! before Process Tap creation. Developers can run the ignored tests manually
//! with:
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

/// Asserts the backend accepts a numeric PID of a running process and
/// exercises the full `CaptureTarget::Application` path at `start()`:
/// parse PID → `CoreAudioProcessTap::new` → aggregate device → bind source.
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
    let mut capture = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Application(ApplicationId(pid.to_string())))
        .sample_rate(48000)
        .channels(2)
        .build()
        .expect("Application(PID) build should validate config before start-time resolution");

    match capture.start() {
        Ok(()) => {
            eprintln!(
                "[ci_audio] ✅ Application(PID={}) started source: {:?}",
                pid,
                capture.config().target
            );
            let _ = capture.stop();
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
/// versions. What we *do* lock in: start-time resolution must surface an
/// error, not silently produce a stream and not panic.
#[test]
#[ignore = "requires macOS 14.4+ with audio capture permission"]
fn select_by_nonexistent_pid_returns_error() {
    if skip_if_unsupported() {
        return;
    }

    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Application(ApplicationId(
            NONEXISTENT_PID.to_string(),
        )))
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(AudioError::BackendError { .. }) | Err(AudioError::InternalError { .. }) => {
            // Acceptable if build() ever grows an eager Process Tap precheck.
            eprintln!(
                "[ci_audio] ✅ Nonexistent PID {} rejected at build() with expected error variant",
                NONEXISTENT_PID
            );
            return;
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
                "[ci_audio] ✅ Nonexistent PID {} rejected with ApplicationNotFound at build()",
                NONEXISTENT_PID
            );
            return;
        }
        Err(other) => panic!(
            "Unexpected build error type for nonexistent PID {}: {:?}",
            NONEXISTENT_PID, other
        ),
    };

    match capture.start() {
        Err(AudioError::BackendError { .. }) | Err(AudioError::InternalError { .. }) => {
            eprintln!(
                "[ci_audio] ✅ Nonexistent PID {} rejected at start() with expected error variant",
                NONEXISTENT_PID
            );
        }
        Err(AudioError::ApplicationNotFound { identifier }) => {
            assert!(
                identifier.contains(&NONEXISTENT_PID.to_string()),
                "ApplicationNotFound identifier should embed the PID, got: {}",
                identifier
            );
            eprintln!(
                "[ci_audio] ✅ Nonexistent PID {} rejected with ApplicationNotFound at start()",
                NONEXISTENT_PID
            );
        }
        Err(other) => panic!(
            "Unexpected start error type for nonexistent PID {}: {:?}",
            NONEXISTENT_PID, other
        ),
        Ok(()) => {
            let _ = capture.stop();
            panic!("Expected error for nonexistent PID {NONEXISTENT_PID}, but start() succeeded");
        }
    }
}

/// `ApplicationId.0` is a `String`; the backend parses it as `u32`. A
/// non-numeric string must produce `AudioError::ApplicationNotFound`
/// with the raw identifier embedded (per `resolve_capture_target`'s
/// `map_err`). This locks in the error *shape* so callers can rely on
/// `ApplicationNotFound` being the parse-failure signal.
///
/// Does not need TCC permission: parsing happens before any Process Tap call.
/// The current backend resolves the target at `start()`, while `build()` only
/// validates configuration and creates the backend handle.
#[test]
fn select_by_non_numeric_id_returns_application_not_found() {
    if skip_if_unsupported() {
        return;
    }

    let bogus = "not-a-pid";
    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Application(ApplicationId(bogus.to_string())))
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(AudioError::ApplicationNotFound { identifier }) => {
            // Acceptable if build() ever grows a PID precheck — still the
            // documented variant, embedding the raw ID.
            assert!(
                identifier.contains(bogus),
                "ApplicationNotFound identifier should embed the raw id, got: {}",
                identifier
            );
            eprintln!(
                "[ci_audio] ✅ Non-numeric id '{}' rejected with ApplicationNotFound at build()",
                bogus
            );
            return;
        }
        Err(other) => panic!(
            "build() for non-numeric id '{}' must succeed or return ApplicationNotFound; got: {:?}",
            bogus, other
        ),
    };

    match capture.start() {
        Err(AudioError::ApplicationNotFound { identifier }) => {
            assert!(
                identifier.contains(bogus),
                "ApplicationNotFound identifier should embed the raw id, got: {}",
                identifier
            );
            eprintln!(
                "[ci_audio] ✅ Non-numeric id '{}' rejected with ApplicationNotFound at start()",
                bogus
            );
        }
        Err(other) => panic!(
            "Expected ApplicationNotFound for non-numeric id '{}', got: {:?}",
            bogus, other
        ),
        Ok(()) => {
            let _ = capture.stop();
            panic!(
                "Expected ApplicationNotFound for non-numeric id '{bogus}', but start() succeeded"
            );
        }
    }
}

/// Edge case: passing the test harness's own PID must not panic or
/// hang, regardless of whether the backend permits self-tapping. This
/// guards the `CoreAudioProcessTap::new` code path against crashes
/// (dereferencing nil Objective-C objects, panicking on OSStatus
/// conversions, etc.) when the target process is the caller.
///
/// The test succeeds as long as `build()` and `start()` return — `Ok` or `Err`
/// are both acceptable. We only fail on a panic from inside the backend.
#[test]
#[ignore = "requires macOS 14.4+ with audio capture permission"]
fn select_by_self_pid_does_not_panic() {
    if skip_if_unsupported() {
        return;
    }

    let self_pid = std::process::id();
    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Application(ApplicationId(
            self_pid.to_string(),
        )))
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[ci_audio] ✅ Self-PID {} rejected cleanly at build() with: {:?}",
                self_pid, e
            );
            return;
        }
    };

    match capture.start() {
        Ok(()) => {
            eprintln!(
                "[ci_audio] ✅ Self-PID {} accepted (no self-tap rejection on this host)",
                self_pid
            );
            let _ = capture.stop();
        }
        Err(e) => eprintln!(
            "[ci_audio] ✅ Self-PID {} rejected cleanly at start() with: {:?}",
            self_pid, e
        ),
    }
}
