# ADR 0015 — macOS system-audio-capture permission preflight (private TCC SPI)

**Status:** Accepted
**Date:** 2026-07-06
**Scope:** `src/audio/macos/permission.rs` (new), `src/core/introspection.rs`
(`check_audio_capture_permission`), `Cargo.toml` (`macos-tcc-spi` feature)
**Verdict:** rsac MAY report the macOS Process-Tap TCC authorization status
(`kTCCServiceAudioCapture`) ahead of a capture attempt, but ONLY behind an
opt-in `macos-tcc-spi` Cargo feature, because the sole mechanism is a **private,
undocumented** TCC.framework SPI (`TCCAccessPreflight`). With the feature off,
`check_audio_capture_permission()` keeps its honest `NotDetermined` stub on macOS
and the public API is unchanged.

## 1. Context

macOS Process Taps (`AudioHardwareCreateProcessTap` / `CATapDescription`, macOS
14.4+) are gated by the **Audio Capture** TCC service (`kTCCServiceAudioCapture`)
— a distinct, stricter service from the microphone (`kTCCServiceMicrophone`,
which `AVCaptureDevice.authorizationStatus(for: .audio)` covers) and from Screen
Recording (`kTCCServiceScreenCapture`). It surfaces in System Settings under
**Privacy & Security → Screen & System Audio Recording**.

Three facts make this hard to handle honestly (all verified on macOS 26 Tahoe,
arm64, during the 2026-07-06 real-hardware verification of PR #35):

1. **There is no public API to query this permission before attempting a tap.**
   `AVCaptureDevice.authorizationStatus(.audio)` reports the *microphone*
   service, not `kTCCServiceAudioCapture`. The only working preflight is the
   private `TCCAccessPreflight("kTCCServiceAudioCapture", NULL)` SPI in
   `/System/Library/PrivateFrameworks/TCC.framework` — the approach the
   reference implementation [`insidegui/AudioCap`](../../reference/AudioCap)
   uses (`AudioCap/ProcessTap/AudioRecordingPermission.swift`, behind its own
   `ENABLE_TCC_SPI` build flag).

2. **Denial is silent.** With permission missing/denied, every setup call
   (`AudioHardwareCreateProcessTap`, `AudioHardwareCreateAggregateDevice`,
   `AudioDeviceCreateIOProcIDWithBlock`, `AudioDeviceStart`) returns `noErr`,
   then the IOProc delivers **zeroed buffers** (or no callbacks). Apple does not
   document this. So the *attempt-and-handle* path cannot distinguish "denied"
   from "genuinely silent audio" by return code — see [ADR-0016](0016-macos-process-tap-silent-zeros-guard.md)
   for the runtime side of the same problem.

3. **The `Info.plist` key is a hard prerequisite, and attribution matters.**
   Without `NSAudioCaptureUsageDescription` in the *responsible* (LaunchServices-
   attributed) app bundle, `tccd` refuses the request categorically — no prompt
   ever fires. A terminal-launched CLI makes the terminal the responsible
   process, so it is silently denied unless the terminal itself carries the key.

Meanwhile `check_audio_capture_permission()` already exists
([`src/core/introspection.rs`](../../src/core/introspection.rs)) and returns
`PermissionStatus::NotDetermined` unconditionally on macOS — a documented stub
whose own comment names this ADR's work as the intended fix.

## 2. Decision drivers

- **Honesty over convenience.** rsac's stated philosophy
  ([`src/core/capabilities.rs`](../../src/core/capabilities.rs) §doc,
  `AGENTS.md` §"never pretend a platform can do something it cannot") forbids
  reporting `Granted` when we cannot actually know. A stub that returns
  `NotDetermined` is honest; a preflight that returns a real answer is *more*
  useful — but only if it is real.
- **Private SPI is a liability.** `TCCAccessPreflight` is undocumented, may
  change or vanish in a future macOS, and its presence in a binary is a known
  App Store review risk. It must never be on the default build path.
- **The public API surface must not change** whether or not the feature is on.
  `check_audio_capture_permission() -> PermissionStatus` is the single entry
  point; the feature only changes the macOS *answer* from always-`NotDetermined`
  to a real preflight result.
- **No new dependency.** The shim uses `libc::dlopen`/`dlsym` (already a
  dependency) and `core-foundation` `CFString` (already a macOS dependency).

## 3. Decision

Add an opt-in `macos-tcc-spi` Cargo feature (default OFF). When enabled on
macOS, `check_audio_capture_permission()` dispatches to a new
`src/audio/macos/permission.rs` that:

- `dlopen`s `/System/Library/PrivateFrameworks/TCC.framework` (RTLD_LAZY),
- `dlsym`s `TCCAccessPreflight` (signature `(CFStringRef, CFDictionaryRef) ->
  c_long`),
- calls it with the `kTCCServiceAudioCapture` CFString and a null dictionary,
- maps the result: `0 → Granted`, `1 → Denied`, anything else (incl. `2`) →
  `NotDetermined`,
- and on ANY failure (framework missing, symbol absent, dlopen error) returns
  `NotDetermined` — never panics, never claims a definite answer it doesn't have.

With the feature OFF, the macOS arm keeps returning `NotDetermined` exactly as
today. Non-macOS platforms keep returning `NotRequired`.

We deliberately do **not** wire `TCCAccessRequest` (the prompt-trigger SPI) in
this ADR: triggering the consent prompt is a host-app UX concern (it needs a
foreground app with the `Info.plist` key), not a capture-library concern. The
library's job is to *report* status; the host app decides when to prompt.

## 4. Consequences

- Consumers that opt into `macos-tcc-spi` (typically a bundled GUI app that
  already ships `NSAudioCaptureUsageDescription`, e.g. the audio-graph Tauri
  app) get a real, prompt-free authorization check for onboarding UX.
- The default build, crates.io publish, and every CI leg stay free of the
  private SPI — no App Store risk for the common consumer, no undocumented
  symbol in the shipped `.rlib`.
- The preflight is **advisory**: because the TCC DB row can be stale relative to
  runtime enforcement (and because a terminal-launched or unbundled process is
  denied regardless of the DB), a `Granted` preflight is not a guarantee of
  non-silent capture. The runtime silent-zeros guard in
  [ADR-0016](0016-macos-process-tap-silent-zeros-guard.md) is the authoritative
  backstop; this preflight is the cheap, proactive UX signal.
- If a future macOS removes `TCCAccessPreflight`, the shim degrades to
  `NotDetermined` (its dlsym fails) rather than breaking the build — the feature
  becomes a no-op, and the runtime guard still catches denial.

## 5. Alternatives considered

- **Public API only (no SPI).** Rejected as the *only* option: there is no
  public preflight for `kTCCServiceAudioCapture`, so the stub would stay a stub
  forever and consumers get no proactive signal. Kept as the default (feature
  off) precisely because it's the honest floor.
- **Unconditionally link the SPI (no feature gate).** Rejected: forces the
  private-SPI liability onto every consumer and the crates.io artifact.
- **Trigger the prompt from the library (`TCCAccessRequest`).** Deferred: it is
  a host-app UX decision and requires a foreground bundle; out of scope for a
  capture library.

## 6. References

- [`reference/AudioCap/AudioCap/ProcessTap/AudioRecordingPermission.swift`](../../reference/AudioCap)
  — the canonical `TCCAccessPreflight`/`TCCAccessRequest` worked example.
- [ADR-0016](0016-macos-process-tap-silent-zeros-guard.md) — the runtime
  silent-zeros guard (the authoritative denial backstop).
- Seed `rsac-84b8`.
