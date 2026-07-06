//! `CaptureTarget::ApplicationByName` integration tests (macOS).
//!
//! The macOS backend enumerates running applications via
//! `NSWorkspace.runningApplications` and performs an **exact,
//! case-insensitive** match on the app's localized name (via
//! `app_name_matches` → `str::eq_ignore_ascii_case`). It deliberately does
//! **not** do substring matching — that historically diverged from the
//! Windows/Linux backends and could resolve `"Music"` to `"Apple Music"`
//! (audit L3). See `src/audio/macos/thread.rs::resolve_capture_target` /
//! `app_name_matches` and `src/audio/macos/coreaudio.rs::enumerate_audio_applications`.
//!
//! # When resolution happens: `start()`, not `build()`
//!
//! Name resolution (NSWorkspace enumeration → PID → Process Tap) runs on the
//! capture path inside `create_macos_capture` → `resolve_capture_target`,
//! which is reached at **`start()`** — NOT at `build()`. `build()` only
//! validates capability and resolves the default output device; it does not
//! look up the named application. Tests therefore assert name-resolution
//! outcomes against `start()`, not `build()`.
//!
//! These tests are `#[ignore]` by default because the happy-path ones
//! exercise CoreAudio Process Taps (macOS 14.4+) and need TCC permission for
//! audio capture. Developers can run them manually with:
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

/// Asserts the pipeline resolves an `ApplicationByName` target when the named
/// app is currently running, using the app's **exact** localized name. A
/// successful `start()` means the macOS backend found the PID via NSWorkspace
/// (exact, case-insensitive name match), created the Process Tap, and started
/// the AudioUnit — the full happy-path for the name-based selection code.
///
/// Resolution happens at `start()` (see the module doc), so `build()` is
/// expected to succeed and the source binding is verified after `start()`.
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

    // build() only validates capability + resolves the default output device;
    // the NSWorkspace name lookup + Process Tap happen at start().
    let mut capture = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::ApplicationByName(KNOWN_APP_NAME.to_string()))
        .sample_rate(48000)
        .channels(2)
        .build()
        .expect("build() should succeed (it does not resolve the app name)");

    match capture.start() {
        Ok(()) => {
            eprintln!(
                "[ci_audio] ✅ ApplicationByName('{}') resolved + started: {:?}",
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
                "Expected ApplicationByName('{}') to resolve at start(); got: {:?}",
                KNOWN_APP_NAME, e
            );
        }
    }
}

/// Asserts capture resolution returns `AudioError::ApplicationNotFound`
/// with the unknown name embedded in the error's `identifier` field when
/// no running app matches. This exercises the error path inside
/// `resolve_capture_target` — `enumerate_audio_applications()` succeeds
/// (possibly with zero apps) but the exact-match `.find()` returns `None`.
///
/// # Where the error surfaces
///
/// Name resolution happens on the capture path inside
/// `create_macos_capture` → `resolve_capture_target`, which runs at
/// `start()` — NOT at `build()` (build only validates capability and
/// resolves the default output device). So the `ApplicationNotFound`
/// must be asserted against `start()`, not `build()`.
///
/// # Gating
///
/// Runs on capable runners (not `#[ignore]`). This path short-circuits at
/// the `.find()` failure BEFORE any `CoreAudioProcessTap::new` call, so it
/// does NOT touch the `kTCCServiceAudioCapture` gate and cannot hang on the
/// Process Tap path. It only needs NSWorkspace, which returns an
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

/// The macOS backend matches names **case-insensitively** (via
/// `app_name_matches` → `str::eq_ignore_ascii_case`), so `"finder"`
/// (lowercase) must resolve to the same app as `"Finder"`. This test locks
/// in that contract — if a future change breaks case-insensitivity, it fails
/// and forces an explicit decision.
///
/// Resolution happens at `start()` (see the module doc), so this asserts
/// against `start()`, not `build()`.
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
        .expect("build() should succeed (it does not resolve the app name)");

    match capture.start() {
        Ok(()) => {
            eprintln!(
                "[ci_audio] ✅ Case-insensitive match: '{}' resolved to '{}'",
                lower, KNOWN_APP_NAME
            );
            let _ = capture.stop();
        }
        Err(e) => panic!(
            "Expected case-insensitive match for '{}' to resolve at start(); got: {:?}",
            lower, e
        ),
    }
}

/// Matching is **exact** (not substring): the macOS backend uses
/// `app_name_matches` (`str::eq_ignore_ascii_case`), NOT
/// `to_lowercase().contains(...)`. A unique *prefix* of a running app's name
/// must therefore **fail** to resolve, returning `AudioError::ApplicationNotFound`.
///
/// `"Find"` is a prefix of `"Finder"`; under the old (removed) substring
/// algorithm it would have resolved, but exact matching rejects it. This
/// test guards against a regression back to substring matching, mirroring
/// the `app_name_matches_rejects_substrings` unit test in
/// `src/audio/macos/thread.rs`.
///
/// # Gating
///
/// Not `#[ignore]`: like `select_by_missing_name_returns_error`, this
/// short-circuits at the exact-match `.find()` failure BEFORE any
/// `CoreAudioProcessTap::new` call, so it needs only NSWorkspace and does not
/// touch the TCC Process-Tap gate. (In the astronomically unlikely event a
/// running app is literally named `"Find"`, resolution would succeed at
/// start(); we tolerate that below rather than assert a false negative.)
#[test]
fn substring_prefix_does_not_resolve() {
    if skip_if_unsupported() {
        return;
    }

    // A prefix of "Finder". Exact matching must NOT resolve this to Finder.
    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::ApplicationByName("Find".to_string()))
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(AudioError::ApplicationNotFound { identifier }) => {
            assert!(
                identifier.contains("Find"),
                "error identifier should contain the queried name, got: {identifier}"
            );
            eprintln!("[ci_audio] ✅ prefix 'Find' rejected at build(): {identifier}");
            return;
        }
        Err(other) => panic!("build() must succeed or return ApplicationNotFound; got: {other:?}"),
    };

    match capture.start() {
        Err(AudioError::ApplicationNotFound { identifier }) => {
            assert!(
                identifier.contains("Find"),
                "error identifier should contain the queried name, got: {identifier}"
            );
            eprintln!(
                "[ci_audio] ✅ exact matching rejected prefix 'Find' (no substring match): {identifier}"
            );
        }
        Err(other) => panic!(
            "Expected ApplicationNotFound for prefix 'Find' under exact matching, got: {other:?}"
        ),
        Ok(()) => {
            // Only reachable if a running app is literally named "Find".
            let _ = capture.stop();
            eprintln!(
                "[ci_audio] ⚠ a running app is exactly named 'Find'; exact match resolved it \
                 (not a substring match — acceptable)"
            );
        }
    }
}
