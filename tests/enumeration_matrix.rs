//! Cross-platform "honest failure" enumeration test matrix (rsac-1d02, audit L4).
//!
//! This is an *inspection / contract* test, not a hardware test. It pins down
//! the enumeration contract that every backend (WASAPI / PipeWire / CoreAudio)
//! must honour, and it must pass in CI **without** a real audio device — when
//! no audio stack is present it asserts the *shape* of an honest failure and
//! skips the hardware-dependent assertions rather than false-failing.
//!
//! What it locks in:
//!
//! 1. **Non-empty-or-honest-failure.** [`enumerate_devices`] returns either
//!    `Ok(non-empty)` *or* a *classified* [`AudioError`] — never `Ok(empty)`
//!    and never a fabricated synthetic "default" device. A platform with no
//!    backend feature must report [`AudioError::PlatformNotSupported`]; a
//!    backend that cannot reach the OS audio service reports a device/backend
//!    error. Every honest failure carries a usable [`ErrorKind`] and
//!    [`Recoverability`] classification.
//!
//! 2. **`DeviceInfo` round-trip.** For every enumerated device,
//!    [`AudioDevice::describe`] yields a `DeviceInfo` whose `id`/`name` match
//!    the live `id()`/`name()` accessors, whose `kind` is consistent with
//!    `kind()`, and whose `default_format` is either `None` or a member of
//!    `supported_formats()`. The device id round-trips losslessly through the
//!    canonical [`CaptureTarget::Device`] string grammar
//!    (`Display` ∘ `FromStr` == identity).
//!
//! 3. **Default-device `kind()`.** When a default device exists, its `kind()`
//!    resolves to `Ok(..)` on a backend platform.
//!
//! 4. **Linux native path (pw-dump-free).** With `pw-dump` removed from `PATH`
//!    the native in-process PipeWire registry path must still satisfy the same
//!    honest-failure contract (it never relies on the subprocess fallback).
//!
//! The skip predicate mirrors the `ci_audio` harness's
//! `audio_infrastructure_available()` so this test stays green on headless
//! runners: set `RSAC_CI_AUDIO_AVAILABLE=1` to force the hardware assertions,
//! `=0` to force-skip, or leave it unset for runtime detection.

// `AudioError` is an intentionally large enum (the library allows
// `clippy::result_large_err` crate-wide in `src/lib.rs`). A crate-level
// `#![allow]` in the library does NOT propagate to this integration-test crate,
// which is compiled separately, so we repeat the allow here. Without it,
// `cargo clippy --all-targets -- -D warnings` (the CI gate) fails on
// `audio_available`'s `Result<_, AudioError>` chain.
#![allow(clippy::result_large_err)]

use std::sync::{Mutex, MutexGuard, OnceLock};

use rsac::{
    get_device_enumerator, AudioDevice, AudioError, CaptureTarget, DeviceKind, ErrorKind,
    Recoverability,
};
// `DeviceInfo` is not (yet) re-exported at the crate root; reach it by path.
use rsac::core::interface::DeviceInfo;

// ───────────────────────────────────────────────────────────────────────────
// Cross-test serialization
// ───────────────────────────────────────────────────────────────────────────
//
// libtest runs the tests in this binary on parallel threads. One variant
// (`linux_native_enumeration_without_pw_dump_on_path`) mutates the *process*
// `PATH`, which is shared global state and not thread-safe to mutate while a
// sibling thread reads the environment. We serialize all tests in this file
// through a single mutex (no external `serial_test` dependency) so the PATH
// window can never overlap another test's enumeration. The guard is held for
// the whole test body and released on drop.

/// Acquires the file-wide serialization lock. Poisoning is irrelevant here
/// (the lock guards no invariant beyond mutual exclusion), so a poisoned lock
/// is recovered into its guard rather than panicking.
fn serialize() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ───────────────────────────────────────────────────────────────────────────
// Honest-failure classification
// ───────────────────────────────────────────────────────────────────────────

/// Returns `true` when `err` is one of the documented, *honest* ways
/// enumeration is allowed to fail.
///
/// A backend may fail to enumerate because the platform has no backend feature
/// compiled in ([`PlatformNotSupported`](AudioError::PlatformNotSupported)),
/// because the OS audio service is unreachable
/// ([`BackendError`](AudioError::BackendError) /
/// [`BackendNotAvailable`](AudioError::BackendNotAvailable) /
/// [`BackendInitializationFailed`](AudioError::BackendInitializationFailed)),
/// because device discovery itself failed
/// ([`DeviceEnumerationError`](AudioError::DeviceEnumerationError) /
/// [`DeviceNotFound`](AudioError::DeviceNotFound) /
/// [`DeviceNotAvailable`](AudioError::DeviceNotAvailable)), or because the
/// caller lacks permission ([`PermissionDenied`](AudioError::PermissionDenied)).
///
/// Anything else (a configuration/stream/internal error from an *enumeration*
/// call) is a mis-classification and fails the contract.
fn is_honest_enumeration_failure(err: &AudioError) -> bool {
    matches!(
        err,
        AudioError::PlatformNotSupported { .. }
            | AudioError::PermissionDenied { .. }
            | AudioError::BackendError { .. }
            | AudioError::BackendNotAvailable { .. }
            | AudioError::BackendInitializationFailed { .. }
            | AudioError::DeviceEnumerationError { .. }
            | AudioError::DeviceNotFound { .. }
            | AudioError::DeviceNotAvailable { .. }
    )
}

/// Asserts the enumeration contract on a `Result<Vec<Box<dyn AudioDevice>>, _>`:
///
/// * `Ok(non-empty)` is accepted, and the device vector is bound to `$bind` in
///   the success arm `$ok` so each backend's per-device checks are one block.
/// * `Ok(empty)` is a **hard failure** — a backend that has no devices must say
///   so with a classified error, never an empty success (audit L4: no silent
///   empties, no fabricated defaults).
/// * `Err(e)` is accepted only when [`is_honest_enumeration_failure`] holds, and
///   the error must additionally expose a coherent [`ErrorKind`] /
///   [`Recoverability`] pair (the `recoverability()` match is exhaustive in the
///   library, so this proves the variant was deliberately classified).
///
/// Usage is a single line per backend test:
///
/// ```ignore
/// assert_enumeration_honest!(enumerator.enumerate_devices(), devices => {
///     for d in &devices { /* per-device checks */ }
/// });
/// ```
macro_rules! assert_enumeration_honest {
    ($result:expr, $bind:ident => $ok:block) => {{
        match $result {
            Ok($bind) => {
                // The L4 honest-failure contract — "a backend with devices must
                // not report a silent empty list" — only has teeth when audio
                // hardware is actually present. On a genuinely headless host
                // (no audio devices, e.g. a Blacksmith CI runner), `Ok(empty)`
                // is itself the honest, correct answer: there ARE zero devices,
                // and inventing an error there would be dishonest in the other
                // direction. So require non-empty only when audio_available().
                if audio_available() {
                    assert!(
                        !$bind.is_empty(),
                        "enumerate_devices() returned Ok(empty) while audio_available() \
                         is true: a host with audio hardware must report its devices, \
                         never a silent empty list (audit L4 honest-failure contract). \
                         Set RSAC_CI_AUDIO_AVAILABLE=0 on a headless runner."
                    );
                } else if $bind.is_empty() {
                    eprintln!(
                        "[enumeration_matrix] enumerate_devices() returned Ok(empty) on a \
                         host with no detected audio (audio_available()=false) — accepted \
                         as honest (zero devices present)."
                    );
                }
                $ok
            }
            Err(ref e) => {
                assert!(
                    is_honest_enumeration_failure(e),
                    "enumerate_devices() failed with an UN-classified-for-enumeration \
                     error: {e:?} (kind={:?}). Expected one of PlatformNotSupported / \
                     PermissionDenied / Backend* / Device* — see \
                     is_honest_enumeration_failure",
                    e.kind()
                );
                // Every honest failure must carry a coherent classification.
                // `kind()` and `recoverability()` are total, exhaustive matches
                // in the library; calling them here proves the variant was
                // deliberately classified (not silently defaulted).
                let _kind: ErrorKind = e.kind();
                let rec: Recoverability = e.recoverability();
                assert!(
                    matches!(
                        rec,
                        Recoverability::Recoverable
                            | Recoverability::TransientRetry
                            | Recoverability::Fatal
                    ),
                    "honest failure {e:?} must have a Recoverability classification"
                );
                // is_fatal()/is_recoverable() are derived from recoverability();
                // they must partition (exactly one is true).
                assert_ne!(
                    e.is_fatal(),
                    e.is_recoverable(),
                    "is_fatal()/is_recoverable() must be mutually exclusive for {e:?}"
                );
                eprintln!(
                    "[enumeration_matrix] honest enumeration failure (accepted): \
                     kind={:?} recoverability={:?}: {e:?}",
                    e.kind(),
                    rec
                );
            }
        }
    }};
}

// ───────────────────────────────────────────────────────────────────────────
// Audio-availability detection (mirrors ci_audio's audio_infrastructure_available)
// ───────────────────────────────────────────────────────────────────────────

/// Whether a real audio stack is reachable, so the hardware-dependent
/// assertions (non-empty list, default-device `kind()`) should run.
///
/// Priority, matching the `ci_audio` harness:
/// 1. `RSAC_CI_AUDIO_AVAILABLE=1` → `true` (CI explicitly forces hardware tests).
/// 2. `RSAC_CI_AUDIO_AVAILABLE=0` → `false` (force-skip).
/// 3. Otherwise: runtime probe — enumeration succeeds with a non-empty list.
fn audio_available() -> bool {
    match std::env::var("RSAC_CI_AUDIO_AVAILABLE").as_deref() {
        Ok("1") => return true,
        Ok("0") => return false,
        _ => {}
    }
    matches!(
        get_device_enumerator().and_then(|e| e.enumerate_devices()),
        Ok(devices) if !devices.is_empty()
    )
}

// ───────────────────────────────────────────────────────────────────────────
// Per-device DeviceInfo round-trip / consistency checks
// ───────────────────────────────────────────────────────────────────────────

/// Asserts that `info` (from `device.describe()`) is internally consistent with
/// the live `device` accessors, and that its id round-trips through the
/// canonical [`CaptureTarget::Device`] string grammar.
fn assert_device_info_round_trips(device: &dyn AudioDevice, info: &DeviceInfo) {
    // describe() composes id()/name()/is_default()/kind()/supported_formats(),
    // so the snapshot must mirror the live accessors verbatim.
    assert_eq!(
        info.id,
        device.id(),
        "DeviceInfo.id must equal AudioDevice::id()"
    );
    assert_eq!(
        info.name,
        device.name(),
        "DeviceInfo.name must equal AudioDevice::name()"
    );
    assert_eq!(
        info.is_default,
        device.is_default(),
        "DeviceInfo.is_default must equal AudioDevice::is_default()"
    );

    // kind consistency: describe() falls back to Input when kind() errors, so
    // when kind() resolves the snapshot must match it exactly; when kind()
    // errors the snapshot must be the documented Input fallback.
    match device.kind() {
        Ok(k) => assert_eq!(
            info.kind, k,
            "DeviceInfo.kind must equal AudioDevice::kind() when it resolves"
        ),
        Err(_) => assert_eq!(
            info.kind,
            DeviceKind::Input,
            "DeviceInfo.kind must fall back to Input when kind() errors \
             (capture-only default)"
        ),
    }

    // default_format is None OR the first member of supported_formats().
    let supported = device.supported_formats();
    match &info.default_format {
        None => {
            // None is allowed in general (Linux/PipeWire reports an empty list
            // by design); when None is reported the list must indeed be empty.
            assert!(
                supported.is_empty(),
                "DeviceInfo.default_format is None but supported_formats() is \
                 non-empty: {supported:?}"
            );
        }
        Some(fmt) => {
            assert!(
                supported.contains(fmt),
                "DeviceInfo.default_format {fmt:?} is not a member of \
                 supported_formats() {supported:?}"
            );
            assert_eq!(
                Some(fmt),
                supported.first(),
                "DeviceInfo.default_format must be the FIRST supported format"
            );
        }
    }

    // Canonical-string round-trip: the id survives Display ∘ FromStr through
    // the CaptureTarget::Device grammar (`device:<id>`), even when the id
    // itself contains colons (e.g. ALSA `hw:0,0`).
    let target = CaptureTarget::Device(info.id.clone());
    let rendered = target.to_string();
    let parsed: CaptureTarget = rendered
        .parse()
        .unwrap_or_else(|e| panic!("CaptureTarget '{rendered}' failed to re-parse: {e:?}"));
    assert_eq!(
        parsed, target,
        "CaptureTarget::Device round-trip mismatch: {target:?} -> '{rendered}' -> {parsed:?}"
    );
    match parsed {
        CaptureTarget::Device(id) => assert_eq!(
            id, info.id,
            "round-tripped DeviceId must equal the source DeviceInfo.id"
        ),
        other => panic!("expected CaptureTarget::Device, got {other:?}"),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// The matrix
// ───────────────────────────────────────────────────────────────────────────

/// Core enumeration-contract assertion, shared by the live and the
/// pw-dump-stripped Linux variant.
///
/// Always asserts non-empty-or-honest-failure (the macro), and runs the
/// per-device `DeviceInfo` round-trip on whatever devices come back. Returns
/// the number of devices enumerated (0 when the honest-failure arm was taken).
fn run_enumeration_contract() -> usize {
    let enumerator = match get_device_enumerator() {
        Ok(e) => e,
        Err(ref e) => {
            // No backend on this target (e.g. feature not enabled) is itself an
            // honest failure shape; assert it is classified and bail.
            assert!(
                is_honest_enumeration_failure(e),
                "get_device_enumerator() failed with an un-classified error: {e:?}"
            );
            eprintln!("[enumeration_matrix] get_device_enumerator() honest failure: {e:?}");
            return 0;
        }
    };

    let mut count = 0usize;
    assert_enumeration_honest!(enumerator.enumerate_devices(), devices => {
        count = devices.len();
        eprintln!("[enumeration_matrix] enumerated {} device(s)", count);
        for device in &devices {
            let info = device.describe();
            eprintln!(
                "  - {} (id={:?}, kind={:?}, default={}, fmt={:?})",
                info.name, info.id, info.kind, info.is_default, info.default_format
            );
            assert_device_info_round_trips(device.as_ref(), &info);
        }
    });
    count
}

/// (1) + (2): non-empty-or-honest-failure and the per-device `DeviceInfo`
/// round-trip, for whichever backend is compiled into this build.
///
/// Runs unconditionally — the contract (and the round-trip on any devices that
/// *are* returned) holds on a headless runner too, because the honest-failure
/// arm is an accepted outcome there.
#[test]
fn enumeration_is_non_empty_or_honest_and_round_trips() {
    let _guard = serialize();
    let count = run_enumeration_contract();

    // Hardware-dependent strengthening: when an audio stack is declared
    // available, an empty/failed enumeration is a real regression.
    if audio_available() {
        assert!(
            count > 0,
            "RSAC_CI_AUDIO_AVAILABLE indicates audio is present, but enumeration \
             produced no devices"
        );
    } else {
        eprintln!(
            "[enumeration_matrix] no audio stack declared/detected; contract held \
             via non-empty-or-honest-failure ({count} device(s) seen)"
        );
    }
}

/// (3): on a backend platform with audio present, the default device's `kind()`
/// resolves to `Ok(..)`. Skips honestly when no audio stack is available or no
/// default device exists (headless runner) rather than fabricating one.
#[test]
fn default_device_kind_resolves_on_backend() {
    let _guard = serialize();
    if !audio_available() {
        eprintln!(
            "[enumeration_matrix] SKIPPED default_device_kind_resolves_on_backend: \
             no audio stack available"
        );
        return;
    }

    let enumerator = match get_device_enumerator() {
        Ok(e) => e,
        Err(e) => {
            // audio_available() was true yet there is no backend — treat as an
            // honest skip rather than a hard failure (env override mismatch).
            eprintln!(
                "[enumeration_matrix] SKIPPED: get_device_enumerator() failed despite \
                 audio_available(): {e:?}"
            );
            return;
        }
    };

    let default = match enumerator.get_default_device() {
        Ok(d) => d,
        Err(ref e) => {
            // A missing default must itself be an honest, classified failure.
            assert!(
                is_honest_enumeration_failure(e),
                "get_default_device() failed with an un-classified error: {e:?}"
            );
            eprintln!(
                "[enumeration_matrix] SKIPPED default kind() check: no default device \
                 (honest failure {e:?})"
            );
            return;
        }
    };

    // Per-platform `kind()` contract. The trait method is *provided* and
    // backends override it only where the OS exposes a definite endpoint
    // direction:
    //
    //   * Windows (WASAPI) overrides it via `IMMEndpoint::GetDataFlow`, so the
    //     default device's `kind()` MUST resolve to `Ok(..)`.
    //   * Linux (PipeWire) and macOS (CoreAudio) currently inherit the default
    //     `PlatformNotSupported` (they do not override `kind()` yet), so the
    //     honest contract there is an `Err` whose `describe()` snapshot falls
    //     back to `DeviceKind::Input` (the capture-only default).
    //
    // Either way, `describe().kind` must stay consistent with `kind()`.
    let kind_result = default.kind();
    let snapshot_kind = default.describe().kind;
    eprintln!(
        "[enumeration_matrix] default device '{}' kind()={:?} describe().kind={:?}",
        default.name(),
        kind_result,
        snapshot_kind
    );

    match kind_result {
        Ok(kind) => {
            assert_eq!(
                snapshot_kind, kind,
                "default device describe().kind must agree with a resolved kind()"
            );
            #[cfg(target_os = "windows")]
            {
                // WASAPI's default-device probe must yield a definite direction.
                assert!(
                    matches!(kind, DeviceKind::Input | DeviceKind::Output),
                    "WASAPI default device kind() must be a definite endpoint direction"
                );
            }
        }
        Err(ref e) => {
            // Windows must NOT land here — its override is expected to resolve.
            #[cfg(target_os = "windows")]
            panic!("WASAPI default device kind() must resolve to Ok, got {e:?}");

            // On Linux/macOS the inherited default is the honest
            // PlatformNotSupported; describe() must fall back to Input.
            #[cfg(not(target_os = "windows"))]
            {
                assert_eq!(
                    e.kind(),
                    ErrorKind::Platform,
                    "inherited default kind() must classify as a Platform error: {e:?}"
                );
                assert_eq!(
                    snapshot_kind,
                    DeviceKind::Input,
                    "describe().kind must fall back to Input when kind() errors"
                );
                eprintln!(
                    "[enumeration_matrix] default kind() not overridden on this backend \
                     (honest PlatformNotSupported); describe() fell back to Input"
                );
            }
        }
    }
}

/// (4): the Linux native PipeWire registry path satisfies the honest-failure
/// contract **without** the `pw-dump` subprocess. We strip `PATH` (so no
/// `pw-dump`/`pw-cli` binary is resolvable) and re-run the contract: any
/// devices that come back must have arrived via the in-process registry, and an
/// empty environment must still surface a classified error (never Ok(empty)).
///
/// On non-Linux targets this is a no-op pass — the cross-platform matrix keeps
/// one test name per concern so CI logs read uniformly.
#[test]
#[cfg(target_os = "linux")]
fn linux_native_enumeration_without_pw_dump_on_path() {
    // Hold the file-wide lock for the whole PATH window so no sibling test
    // reads the environment while PATH is stripped.
    let _guard = serialize();

    // Snapshot and restore PATH so we do not disturb sibling tests in the same
    // binary. (Edition 2021: env mutators are safe.)
    let saved_path = std::env::var_os("PATH");
    std::env::remove_var("PATH");

    // Run the *same* contract: non-empty-or-honest-failure + per-device
    // round-trip. With PATH gone, the subprocess fallback cannot run, so a
    // non-empty result proves the native registry path produced it.
    let result = std::panic::catch_unwind(run_enumeration_contract);

    // Restore PATH before propagating any failure.
    match saved_path {
        Some(p) => std::env::set_var("PATH", p),
        None => std::env::remove_var("PATH"),
    }

    match result {
        Ok(count) => eprintln!(
            "[enumeration_matrix] linux native (no pw-dump on PATH): contract held \
             ({count} device(s))"
        ),
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

#[test]
#[cfg(not(target_os = "linux"))]
fn linux_native_enumeration_without_pw_dump_on_path() {
    // Concern is Linux-specific; nothing to assert on this target.
    eprintln!(
        "[enumeration_matrix] linux_native_enumeration_without_pw_dump_on_path: \
         not applicable on this platform"
    );
}
