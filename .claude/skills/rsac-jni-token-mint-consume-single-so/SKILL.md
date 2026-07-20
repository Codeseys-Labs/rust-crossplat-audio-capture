---
name: rsac-jni-token-mint-consume-single-so
description: |
  Rust-on-Android trap: two .so files that each statically link the same Rust
  library contain DISJOINT copies of its statics — an opaque handle (JNI
  GlobalRef wrapped as a jlong token, registry entry, channel, atomic) minted
  through one .so is not safely consumable through the other. Use when:
  (1) adding a second cdylib that `pub use`s rsac (or any shared Rust rlib),
  (2) a JNI token/handle "mysteriously" fails validation or dereferences a
  dead registry when consumed, (3) deciding lib names for test-only Android
  cdylibs, (4) reviewing System.loadLibrary call sites across test classes.
  In rsac specifically: RsacProjection hard-codes System.loadLibrary("rsac"),
  so any driver consuming an AndroidProjectionToken must live in THE SAME
  librsac.so that registered nativeRetainProjection.
author: Claude Code
version: 1.0.0
date: 2026-07-20
---

# JNI tokens must mint and consume inside one .so (single rsac copy)

## Problem

rsac's Android consent flow mints an opaque projection token in Kotlin
(`RsacProjection` → `nativeRetainProjection`, registered by rsac's
`JNI_OnLoad`) and consumes it in Rust (`AndroidProjectionToken::from_raw` →
`with_android_projection`). If the mint path and the consume path resolve
into *different* .so files that each statically link rsac, the token's
backing state (deletion latch, any registry the token participates in) lives
in one copy's statics while the consumer reads the other copy's — silent
misbehavior, not a linker error.

Extra hazard: **both** .so files re-export rsac's `#[no_mangle] JNI_OnLoad`,
and `RegisterNatives` is last-loaded-wins per Java class. If both libs load
in one process, whichever loaded last owns `RsacProjection`'s natives.

## Context / Trigger Conditions

- Adding any new cdylib crate with `rsac = { path = ... }` + `pub use rsac;`
  (the JNI_OnLoad keep-alive trick from `mobile/android-native`).
- A test needs to drive an API that consumes a Kotlin-minted native handle.
- Reviewing: multiple `System.loadLibrary` calls across androidTest classes.

## Solution (the wave-10 playback-tier design, review-verified)

1. **Name the test cdylib's `[lib] name = "rsac"` on purpose** so
   `System.loadLibrary("rsac")` (hard-coded in the shipped `RsacProjection`)
   loads the ONE .so that both registers the natives (mint) and exposes the
   driver export (consume). See `mobile/androidtest-native/Cargo.toml`.
2. The name **collides** with `mobile/android-native`'s output. Safe because
   they are never co-compiled: every Android build in CI uses explicit `-p`
   into distinct `-o` dirs, and no `cargo … --workspace` compile exists in
   any workflow/script (verified by grep + an empirical `cargo build
   --workspace`, which anyway only WARNS on the artifact-name collision).
   The root `rsac` crate is a pure rlib (no crate-type), so it never emits
   `librsac.*` itself.
3. **Keep load sets disjoint per test class**: the mic tier loads only
   `rsac_ffi` + shim; the playback tier loads only `rsac`. Kotlin
   `object`/companion initializers are LAZY (first access, not JUnit class
   enumeration), so scanning the APK's test classes does not load the other
   tier's libs. Sequential (non-orchestrated, non-parallel) execution keeps
   last-wins RegisterNatives deterministic even when both eventually load.

## Verification

- `cargo metadata` exits 0 with three `rsac`-named targets (root rlib +
  two cdylibs).
- Reviewer trace: no transitive class-init in the playback test touches
  `NativeCaptureDriver`; either test order leaves librsac.so's natives
  current during mint+consume.

## Notes

- If a future change enables the Android test orchestrator (fresh process
  per test) that is *safer*; in-process parallel test execution would race
  last-wins RegisterNatives and must not be enabled without revisiting this.
- If any CI job ever adds an Android-target `--workspace` build, the two
  cdylibs' output filenames collide in one invocation — split with `-p`.
- Generalizes beyond JNI: any two cdylibs linking one Rust lib have disjoint
  `static`s, `OnceLock`s, and registries. Handles must not cross.
