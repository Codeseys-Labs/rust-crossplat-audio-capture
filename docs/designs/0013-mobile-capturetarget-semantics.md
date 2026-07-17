# ADR 0013 — Mobile `CaptureTarget` semantics: strict Android mapping, ReplayKit-backed iOS `SystemDefault`, explicit consent token

**Status:** Accepted
**Date:** 2026-07-04
**Scope:** future `src/audio/android/` + `src/audio/ios/` backends,
`src/core/config.rs` (`AndroidProjectionToken`, cfg-gated builder method),
`src/core/capabilities.rs` (`requires_user_consent`), `src/core/error.rs`
(consent/extension precondition variants), `mobile/ios/` mmap-ring transport.
Design elaborated in
[`docs/MOBILE_BACKEND_DESIGN.md`](../MOBILE_BACKEND_DESIGN.md).
**Verdict:** the existing `CaptureTarget` enum maps onto mobile primitives
without new variants. **Android:** `SystemDefault` = `AudioPlaybackCapture`
(all capturable playback), `Application`/`ApplicationByName` =
`addMatchingUid` (package → UID), `ProcessTree` = PID → UID (≡ application —
Android app processes share a UID), `Device` = AAudio input. **iOS:**
`SystemDefault` = ReplayKit Broadcast Upload Extension delivering audio to
the host over a **memory-mapped SPSC ring in the shared App Group
container**; `Device` = AVAudioEngine input; `Application*`/`ProcessTree` =
permanently `PlatformNotSupported`. The Android `MediaProjection` consent
token enters via an **explicit builder method with a `build()` preflight** —
no global state.

## 1. Context

`CaptureTarget` is rsac's unified target model. Mobile OSes expose capture
primitives that differ sharply from desktop loopback: Android gates playback
capture behind a user-consent `MediaProjection` and filters by **UID** (not
PID); iOS offers no third-party API for capturing another app at all, and
system-wide audio only through a user-initiated ReplayKit broadcast running in
a separate extension process (~50 MB cap). The project's honesty rule
(AGENTS.md §7) forbids pretending otherwise; the platform-capabilities system
exists precisely for this. The 2026-07-04 planning session resolved how each
variant behaves on each mobile OS, how the consent token flows, and how
extension audio reaches the host process.

## 2. Decision drivers

- **No API forks.** The same `CaptureTarget` program must express intent on
  all five platforms; per-platform enums would fragment every binding.
- **Honest capabilities.** iOS must never claim application capture; Android
  must surface the consent requirement before any device is touched.
- **Explicitness over magic.** Hidden global state (a process-global
  projection token) makes multi-capture semantics racy and untestable.
- **Bridge semantics reuse.** The iOS extension transport should look like a
  `BridgeProducer`/ring so the host side inherits proven terminal/overrun
  semantics (ADR-0003, ADR-0010) instead of inventing new ones.
- **Fail at `build()`, not mid-capture.** Missing consent/extension is a
  configuration error, detectable before any OS resource is acquired.

## 3. Considered options

### iOS `SystemDefault` behavior

**(a) Honestly limited:** `SystemDefault` returns `PlatformNotSupported`;
only mic (`Device`) works; ReplayKit documented but unwired.
✅ smallest surface, zero extension machinery; ❌ ships an iOS backend that
cannot capture system audio at all — the headline capability.

**(b) ReplayKit-wired (chosen):** `SystemDefault` consumes the broadcast
extension's stream. ✅ real system capture on iOS, the only route that exists;
➖ requires the consumer app to embed an extension target + App Group
entitlement, capture is user-initiated only, and the extension's ~50 MB cap
bounds buffering.

**(c) Mic fallback:** `SystemDefault` silently falls back to the microphone
so "something works". ❌ **rejected as dishonest** — it violates the
capability-honesty rule by delivering different audio than requested.

### Consent-token entry (Android)

**(a) Explicit builder token + preflight (chosen):** AAR helper runs the
consent Intent → opaque token (JNI `GlobalRef`, `int64_t` across FFI) →
`AudioCaptureBuilder::with_android_projection(...)`; `build()` fails
playback-capture targets without a token (`ConfigurationError`-class).
✅ explicit, testable, no global state; ➖ one cfg-gated builder method + one
FFI function per binding.

**(b) Process-global registration:** `rsac::android::set_media_projection()`.
✅ builder unchanged; ❌ hidden global state, racy for concurrent captures,
untestable preflight.

**(c) Kotlin-owned capture:** the AAR owns the whole lifecycle and Rust only
ingests samples. ❌ breaks the unified `CaptureTarget` story — Rust could not
express playback-capture targets on Android.

### Extension → host transport (iOS)

**(a) mmap'd SPSC ring in the App Group container (chosen):** fixed-size
mapped file, atomic cursors + heartbeat header, interleaved f32 — mirrors the
rtrb bridge; host side drains it into a normal `BridgeProducer`.
✅ lowest latency, allocation-free inside the extension budget, drop-not-block
matches rsac's overrun model; ➖ hand-rolled cross-process ring must be
audited (two processes, no shared Rust type system guarantees).

**(b) Unix socket in the App Group container:** ✅ simpler framing; ❌
per-buffer syscall/copy + connection lifecycle inside the 50 MB extension.

**(c) Defer the choice to implementation:** ❌ rejected — the transport shapes
the SwiftPM template and the host `PlatformStream`; deferring blocks both.

## 4. Decision

The verdict table, normatively:

| `CaptureTarget` | Android | iOS |
|---|---|---|
| `SystemDefault` | `AudioPlaybackCapture`, usage filters (`MEDIA`/`GAME`/`UNKNOWN`), no UID filter; consent token required | ReplayKit broadcast → App Group mmap ring; extension + App Group required; user-initiated |
| `Application(id)` | `addMatchingUid(uid)`; id carries the UID; consent required | `PlatformNotSupported` (permanent) |
| `ApplicationByName(s)` | package name → UID (`PackageManager`), then as above | `PlatformNotSupported` (permanent) |
| `ProcessTree(pid)` | PID → UID (`/proc/<pid>/status`), then `addMatchingUid` — **≡ application** (shared UID), documented | `PlatformNotSupported` (permanent) |
| `Device(id)` | AAudio input device | AVAudioEngine input |

Plus: (1) consent enters via option (a) — explicit builder token + `build()`
preflight; (2) iOS transport is option (a) — mmap SPSC ring; the Rust host's
start/stop/liveness contract is **heartbeat-poll-only** (bounded publish-word
polling + heartbeat-miss ⇒ producer terminal signal, ADR-0010 ⇒ fatal
terminal, ADR-0003) — the extension additionally posts Darwin notifications,
but they are an optional Swift-side signal the Rust consumer does not observe
(rsac-7e0a); (3) capabilities gain
`requires_user_consent: bool` (`true` on both mobile OSes, `false` on
desktop); (4) apps that are uncapturable by OS policy (pre-Android-10
targets, `allowAudioPlaybackCapture=false`, `USAGE_VOICE_COMMUNICATION`) are
simply absent from the mix — documented, not worked around.

## 5. Consequences

- One `CaptureTarget` program expresses intent on all five platforms; every
  binding keeps its existing target grammar (`system`, `app:<id>`,
  `name:<n>`, `tree:<pid>`, `device:<id>`) unchanged on mobile.
- **Negative:** iOS `SystemDefault` carries real integration burden for
  consumers — an extension target, App Group entitlement, user-initiated
  start, and capture-everything (no per-app filter). The docs must present
  this loudly; consumers who only need mic pay none of it.
- **Negative:** `ApplicationId` becomes platform-relative (PID-ish on
  desktop, UID on Android) — introspection helpers must return the right kind
  per platform, and the rustdoc must state it, or users will pass PIDs where
  UIDs are needed.
- **Negative:** the cross-process mmap ring is a second, hand-rolled ring
  implementation (rtrb cannot span processes) — it needs its own correctness
  tests and an explicit unsafe audit.
- Android tree≡app equivalence means `ProcessTree` adds no power over
  `Application` on Android — accepted and documented rather than simulated.
- New `AudioError` precondition variants join the exhaustive
  `recoverability()` match (compile-time forcing function keeps them
  classified).
