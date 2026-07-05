# ADR 0012 — Batteries-included mobile platform strategy: backends in-crate, Kotlin AAR + Swift package owned by rsac

**Status:** Accepted
**Date:** 2026-07-04
**Scope:** future `src/audio/android/` + `src/audio/ios/` backend modules,
new top-level `mobile/android/` (Gradle → `rsac.aar`) and `mobile/ios/`
(SwiftPM package + broadcast-extension template), `Cargo.toml`
(`feat_android`/`feat_ios` features, `mobile/` in the crates.io `exclude`
list). Design elaborated in
[`docs/MOBILE_BACKEND_DESIGN.md`](../MOBILE_BACKEND_DESIGN.md).
**Verdict:** rsac itself owns the full mobile story: the Rust backends
(Android `AudioPlaybackCapture`+AAudio, iOS AVAudioEngine+ReplayKit) live in
`src/audio/{android,ios}/` behind the standard `BridgeStream<S>` /
`PlatformStream` contract, **and** rsac ships the unavoidable Kotlin/Swift
glue as first-party artifacts: a Gradle AAR under `mobile/android/` (consent
Intent helper, media-projection foreground service, Java `AudioRecord`
capture loop, JNI symbols) and a SwiftPM package under `mobile/ios/`
(`AVAudioSession` helpers, Broadcast Upload Extension template). Every
framework — Tauri, Dioxus, Flutter, native Kotlin/Swift — consumes the same
core and the same glue.

## 1. Context

rsac is desktop-3 today (WASAPI / PipeWire / CoreAudio). The 2026-07-04
planning session committed a full mobile push (Android + iOS). Mobile capture
cannot be pure Rust:

- Android's `AudioPlaybackCapture` is a **Java-only** API (no NDK
  equivalent): the capture read loop must live in Kotlin/Java, and the
  `MediaProjection` consent dialog can only be launched from an Activity.
- iOS system capture requires a ReplayKit **Broadcast Upload Extension** — an
  Xcode target written in Swift that the consumer app embeds.

So *somewhere* there must be Kotlin and Swift code. The question this ADR
answers is **where it lives and who owns it**. The repo already compiles as an
honest stub on non-desktop targets (`PlatformCapabilities::unsupported()`,
`PlatformNotSupported`), so the architecture is mobile-safe; only the
backends and glue are missing.

## 2. Decision drivers

- **Framework-agnosticism.** Tauri v2, Dioxus, Flutter, React Native and
  native apps must all be able to consume mobile capture; Tauri is not the
  only citizen (Dioxus has no plugin system at all).
- **Architecture rule:** all backends implement `PlatformStream` and go
  through `BridgeStream<S>` *in rsac* (AGENTS.md §7 "do not bypass") — a
  backend living in an external plugin crate would violate this or force
  bridge internals public.
- **One consent-flow implementation.** MediaProjection consent + foreground
  service plumbing is subtle (service types, notification, token lifetime);
  making every downstream re-implement it guarantees divergence and bugs.
- **Version lockstep.** Kotlin/Swift glue is tightly coupled to the JNI/FFI
  symbols of the exact rsac version; separate repos make lockstep manual.
- **CI cost is real** — Gradle + Xcode enter this repo's CI.

## 3. Considered options

### Option A — Batteries-included in-repo: backends in `src/audio/`, AAR + SwiftPM in `mobile/` (chosen)

- ✅ Single repo, single version, JNI/FFI symbols and glue can never drift
  apart; CI builds them together.
- ✅ Every framework gets the same tested consent flow; `tauri-plugin-rsac`
  (ADR-0014) becomes a thin wrapper instead of an owner.
- ✅ Backends keep `pub(crate)` access to bridge internals (ring, state,
  terminal signal) like every desktop backend.
- ➖ Gradle + Xcode toolchains join CI (AAR assemble, SwiftPM build).
- ➖ The repo gains non-Rust source trees to maintain.

### Option B — Glue and backends live in `tauri-plugin-rsac` only

- ✅ Least work for the immediate Tauri consumer (audio-graph).
- ❌ Locks mobile to Tauri: Dioxus/Flutter/native get nothing.
- ❌ Violates the backend contract — the plugin either bypasses
  `BridgeStream<S>` or rsac must expose bridge internals publicly.
- ❌ Couples backend release cadence to the plugin's.

### Option C — Backends in-crate, but Kotlin/Swift in separate repos (`rsac-android`, `rsac-swift`)

- ✅ Keeps this repo's CI free of Gradle/Xcode; idiomatic distribution
  (Maven/SwiftPM) from dedicated repos.
- ❌ JNI symbol ↔ Kotlin glue lockstep across repos is a manual, error-prone
  process (the existing version-lockstep CI gate can't reach them).
- ❌ Three repos to review/release for one logical change; submodule friction
  already observed with `apps/audio-graph`.

A sub-option of A — placing the glue under `bindings/rsac-android` — was
rejected on taxonomy: `bindings/` holds *alternative API surfaces* over the
same core; the AAR/SwiftPM glue is *required backend infrastructure* without
which the Rust API cannot function on that OS. Conflating them would misstate
what the directory promises.

## 4. Decision

**Option A.** Sub-decisions:

1. **Layout:** `mobile/android/` (Gradle project producing `rsac.aar`) and
   `mobile/ios/` (SwiftPM package incl. broadcast-extension template) at the
   repo top level; both added to the crates.io `exclude` list alongside
   `/apps` and `/bindings`. Rust backends at `src/audio/android/` and
   `src/audio/ios/`, double-gated (`target_os` + `feat_android`/`feat_ios`)
   exactly like the desktop backends — the module DAG is unchanged.
2. **Ownership boundary:** the AAR/SwiftPM glue contains *no capture policy*
   — it is consent flow, service plumbing, the Java read loop, and JNI/ring
   transport. All target resolution, error classification, and stream
   semantics stay in Rust.
3. **Distribution:** AAR to Maven (GitHub Packages first), SwiftPM via git
   tag — release automation is a follow-up seed; in-tree consumption works
   from day one.

## 5. Consequences

- Tauri, Dioxus, Flutter, and native apps share one consent-flow
  implementation and one backend; `tauri-plugin-rsac` (ADR-0014) shrinks to a
  thin adapter.
- **Negative:** this repo's CI grows Gradle and Xcode jobs (AAR assemble,
  SwiftPM build, `cargo ndk`/`aarch64-apple-ios` checks) — slower CI, more
  toolchain surface to keep green.
- **Negative:** Kotlin/Swift reviewers are now required for changes under
  `mobile/` — a new competency the project must staff.
- The feature matrix grows (`feat_android`, `feat_ios` join the powerset job).
- `PlatformCapabilities` gains `requires_user_consent` (additive; desktop
  reports `false`).
- Neutral: docs.rs target list may grow mobile targets later; not required
  for this decision.
