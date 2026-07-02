//! `CaptureTarget::ApplicationByName` integration tests (macOS).
//!
//! The macOS backend enumerates running applications via
//! `NSWorkspace.runningApplications` and performs an exact case-insensitive
//! match on the app's localized name. See
//! `src/audio/macos/thread.rs::resolve_capture_target` and
//! `src/audio/macos/coreaudio.rs::enumerate_audio_applications`.
//!
//! The happy-path tests are `#[ignore]` by default because they exercise
//! CoreAudio Process Taps (macOS 14.4+) and need TCC permission for
//! audio capture. Error-path tests are safe to run because they fail before
//! Process Tap creation. Developers can run the ignored tests manually with:
//!
//! ```text
//! cargo test --test ci_audio application_by_name \
//!     --features feat_macos -- --ignored --test-threads=1
//! ```
//!
//! On other platforms the module is empty by design.
#![cfg(target_os = "macos")]

use rsac::{AudioCaptureBuilder, AudioError, CaptureTarget, PlatformCapabilities};

/// Finder is part of every macOS user session and appears in
/// `NSWorkspace.runningApplications`. It's the most stable target for
/// a "known-running" application check.
const KNOWN_APP_NAME: &str = "Finder";

/// A string that cannot plausibly match any real running app.
const MISSING_APP_NAME: &str = "ThisApplicationDefinitelyDoesNotExist_12345";

/// A substring of `Finder` that should not match under the exact-match contract.
const FINDER_SUBSTRING: &str = "Find";

/// Skip helper — mirrors the pattern used by other ci_audio modules
/// but inlined because `ApplicationByName` only exists on macOS and
/// we don't want to require the cross-platform `require_app_capture!`
/// macro (which pulls in `require_audio!`).
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

/// Asserts an `ApplicationByName` target starts when the named app is currently
/// running. Target resolution happens at `start()` on macOS; `build()` only
/// validates configuration and prepares the backend handle.
///
/// Ignored by default: requires macOS 14.4+, TCC audio permission, and
/// for Finder to be running (always true in a normal user session, but
/// not guaranteed in a headless CI sandbox).
#[test]
#[ignore = "requires macOS 14.4+ with audio capture permission and Finder running"]
fn select_by_exact_name_binds_source() {
    if skip_if_unsupported() {
        return;
    }

    let mut capture = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::ApplicationByName(KNOWN_APP_NAME.to_string()))
        .sample_rate(48000)
        .channels(2)
        .build()
        .expect("ApplicationByName build should validate config before start-time resolution");

    match capture.start() {
        Ok(()) => {
            eprintln!(
                "[ci_audio] ✅ ApplicationByName('{}') started source: {:?}",
                KNOWN_APP_NAME,
                capture.config().target
            );
            let _ = capture.stop();
        }
        Err(e) => {
            // In a sandboxed/headless CI Finder may not be listed, or
            // Process Tap creation may fail due to TCC. Surface the
            // error so developers see why the test skipped locally.
            panic!(
                "Expected ApplicationByName('{}') to start; got: {:?}",
                KNOWN_APP_NAME, e
            );
        }
    }
}

/// Asserts capture resolution returns `AudioError::ApplicationNotFound`
/// with the unknown name embedded in the error's `identifier` field when
/// no running app matches. This exercises the error path inside
/// `resolve_capture_target` — `enumerate_audio_applications()` succeeds
/// (possibly with zero apps) but the `.find()` returns `None`.
///
/// # Where the error surfaces
///
/// Name resolution happens on the capture thread inside
/// `create_macos_capture` → `resolve_capture_target`, which runs at
/// `start()` — NOT at `build()` (build only validates capability and
/// resolves the default output device). So the `ApplicationNotFound`
/// must be asserted against `start()`, not `build()`.
///
/// # Gating
///
/// Runs on capable runners (was `#[ignore]`). This path short-circuits at
/// the `.find()` failure BEFORE any `CoreAudioProcessTap::new` call, so it
/// does NOT touch the `kTCCServiceAudioCapture` gate and cannot hang on the
/// 10-minute Process Tap path. It only needs NSWorkspace, which returns an
/// (possibly empty) list in any session. We still skip on backends without
/// application-capture capability via `skip_if_unsupported()`.
#[test]
fn select_by_missing_name_returns_error() {
    if skip_if_unsupported() {
        return;
    }

    // build() succeeds (resolves the default output device); the name lookup
    // and its ApplicationNotFound error happen at start().
    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::ApplicationByName(
            MISSING_APP_NAME.to_string(),
        ))
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(AudioError::ApplicationNotFound { identifier }) => {
            // Acceptable if build() ever grows a name precheck — still the
            // documented variant, embedding the missing name.
            assert!(
                identifier.contains(MISSING_APP_NAME),
                "error identifier should contain the missing name, got: {identifier}"
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
                "error identifier should contain the missing name, got: {identifier}"
            );
            eprintln!("[ci_audio] ✅ Got expected ApplicationNotFound at start(): {identifier}");
        }
        Err(other) => panic!("Expected ApplicationNotFound for missing app, got: {other:?}"),
        Ok(()) => {
            let _ = capture.stop();
            panic!("Expected ApplicationNotFound for '{MISSING_APP_NAME}', but start() succeeded");
        }
    }
}

/// The macOS backend matches exact names case-insensitively, so `"finder"`
/// must resolve to the same app as `"Finder"`.
///
/// Ignored by default for the same reasons as
/// `select_by_exact_name_binds_source`.
#[test]
#[ignore = "requires macOS 14.4+ with audio capture permission and Finder running"]
fn case_insensitive_match() {
    if skip_if_unsupported() {
        return;
    }

    let lower = KNOWN_APP_NAME.to_lowercase();
    let mut capture = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::ApplicationByName(lower.clone()))
        .sample_rate(48000)
        .channels(2)
        .build()
        .expect("ApplicationByName build should validate config before start-time resolution");

    match capture.start() {
        Ok(_) => {
            eprintln!(
                "[ci_audio] ✅ Case-insensitive match: '{}' resolved to '{}'",
                lower, KNOWN_APP_NAME
            );
            let _ = capture.stop();
        }
        Err(e) => panic!(
            "Expected case-insensitive match for '{}' to succeed; got: {:?}",
            lower, e
        ),
    }
}

/// Substring matching is intentionally rejected. `ApplicationByName` is exact
/// case-insensitive across platforms; a unique prefix of a known app name must
/// not resolve.
#[test]
fn substring_match_returns_error() {
    if skip_if_unsupported() {
        return;
    }

    let capture = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::ApplicationByName(
            FINDER_SUBSTRING.to_string(),
        ))
        .sample_rate(48000)
        .channels(2)
        .build();

    let mut capture = match capture {
        Ok(c) => c,
        Err(AudioError::ApplicationNotFound { identifier }) => {
            assert!(
                identifier.contains(FINDER_SUBSTRING),
                "error identifier should contain the missing substring, got: {identifier}"
            );
            eprintln!("[ci_audio] ✅ Substring rejected at build(): {identifier}");
            return;
        }
        Err(other) => panic!(
            "build() for substring query must succeed or return ApplicationNotFound; got: {other:?}"
        ),
    };

    match capture.start() {
        Err(AudioError::ApplicationNotFound { identifier }) => {
            assert!(
                identifier.contains(FINDER_SUBSTRING),
                "error identifier should contain the missing substring, got: {identifier}"
            );
            eprintln!("[ci_audio] ✅ Substring rejected at start(): {identifier}");
        }
        Err(other) => panic!("Expected ApplicationNotFound for substring query, got: {other:?}"),
        Ok(()) => {
            let _ = capture.stop();
            panic!("Substring query '{FINDER_SUBSTRING}' unexpectedly resolved");
        }
    }
}
