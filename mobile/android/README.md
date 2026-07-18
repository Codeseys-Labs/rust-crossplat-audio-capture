# rsac Android glue (`rsac.aar`)

> **Status: builds in CI** (compile-verified; no device runtime
> verification). The `mobile-android` CI job (rsac-1a6e) runs
> `gradle assembleRelease` (Gradle 8.11.1 / JDK 17 / android-35) and asserts
> the `.aar` artifact on every code PR — green since 2026-07-06. The Gradle
> wrapper remains deliberately absent (CI provisions Gradle). The JNI
> natives this glue calls ship inside `librsac.so`
> (`src/audio/android/jni.rs`, rsac-77f1), which CI builds into the AAR's
> jniLibs (rsac-0aa9). In a build without the native library, native calls
> throw `UnsatisfiedLinkError` — guard with
> `RsacProjection.isNativeAvailable()`.

First-party Kotlin glue for rsac's Android playback-capture backend
(ADR-0012 "batteries included"). Per the ownership boundary (ADR-0012 §4.2)
this module carries **no capture policy**: consent flow, foreground-service
plumbing, the Java `AudioRecord` read loop, and name/PID→UID lookup only.
Target resolution, error classification, and stream semantics live in Rust
(`src/audio/android/`).

## Contents

| File | Role |
|---|---|
| `RsacProjection.kt` | MediaProjection consent flow (ActivityResult API) → opaque native token for `AudioCaptureBuilder::with_android_projection` |
| `RsacCaptureService.kt` | Foreground service (`mediaProjection` type): notification plumbing, start/stop, stops registered bridges on destroy |
| `CaptureBridge.kt` | Dedicated Java capture thread: `AudioPlaybackCaptureConfiguration` + `AudioRecord` (`ENCODING_PCM_FLOAT`), blocking reads into one reused buffer → `nativePush` per period |
| `PackageResolver.kt` | `packageName → UID` (PackageManager) and `PID → UID` (`/proc/<pid>/status`) for the ADR-0013 mapping |
| `RsacDevices.kt` | Input-device enumeration (`AudioManager.getDevices`) as a flat `id␟type␟name` / `␞`-joined string — a **Rust → Java** lookup (regular `fun`, **not** an `external fun`, so absent from the JNI symbol-contract table below), mirroring `PackageResolver` (rsac-ad8a) |
| `src/main/AndroidManifest.xml` | Permissions + the `mediaProjection`-typed service (merged into the host app) |

## JNI symbol contract

**Lockstep with `src/audio/android/jni.rs`** — Rust registers these via
`RegisterNatives` in `JNI_OnLoad` (there are deliberately **no `Java_*`
name-resolved exports**; `JNI_OnLoad` is the `.so`'s single JNI entry
point, asserted by CI's llvm-nm step). Renaming a class, method, or
signature on either side breaks capture — the host-run `jni_lockstep`
tests in `src/audio/mod.rs` guard both sides on every `cargo test --lib`.

| Kotlin declaration (class) | Registered as | Direction |
|---|---|---|
| `@JvmStatic external fun nativeRetainProjection(projection: MediaProjection): Long` (`ai.codeseys.rsac.RsacProjection`) | `nativeRetainProjection` `(Landroid/media/projection/MediaProjection;)J` | Kotlin → Rust: wrap the consented `MediaProjection` in a JNI `GlobalRef`, return the opaque token |
| `@JvmStatic external fun nativePush(session: Long, buf: FloatArray, frames: Int, channels: Int, sampleRate: Int)` (`ai.codeseys.rsac.CaptureBridge`) | `nativePush` `(J[FIII)V` | Kotlin → Rust: per-period ingest into the capture's ring buffer (session-lifetime scratch, drop-don't-block) |
| `@JvmStatic external fun nativeSessionEnded(session: Long)` (`ai.codeseys.rsac.CaptureBridge`) | `nativeSessionEnded` `(J)V` | Kotlin → Rust: terminal handshake — the read thread exited; a still-registered session is treated as spontaneous death (ADR-0010) |

Token lifetime: **one token = one projection session**; release
(`DeleteGlobalRef` + `MediaProjection.stop()`) is Rust's job, tied to the
owning capture's drop. There is no process-global token registry.

## Native library

`librsac.so` (the rsac cdylib built with `cargo ndk`, per-ABI under
`src/main/jniLibs/<abi>/`) is packaged by the CI job — not present in-tree.
Load name: `System.loadLibrary("rsac")`.

## Integration recipe (non-Tauri hosts: Dioxus / Flutter / native Kotlin)

In-tree consumption works today; Maven/GitHub Packages distribution is a
follow-up seed.

1. **Depend on the module** — `settings.gradle.kts` of the host app:

   ```kotlin
   include(":rsac")
   project(":rsac").projectDir = file("path/to/rsac/mobile/android")
   ```

   then `implementation(project(":rsac"))` — or drop a built `rsac.aar` into
   `app/libs` and use `implementation(files("libs/rsac.aar"))`.

2. **Runtime permission** — request `RECORD_AUDIO` (required even for
   playback capture) before building a capture.

3. **Run the consent flow** from a `ComponentActivity`. `request` starts the
   `mediaProjection` foreground service for you, on the consent-success path
   — **do not** call `RsacCaptureService.start()` yourself beforehand: on
   API 34+ starting a `mediaProjection`-typed FGS before consent exists throws
   `SecurityException` (rsac-cabf).

   ```kotlin
   RsacProjection.request(activity, object : RsacProjection.Callback {
       override fun onToken(token: Long) { /* hand `token` to Rust */ }
       override fun onDenied(reason: String) { /* surface to the user */ }
   })
   ```

4. **Hand the token to Rust.**
   - **Dioxus / direct Rust:** pass the `Long` across your JNI/FFI boundary
     and call `AudioCaptureBuilder::with_android_projection(AndroidProjectionToken(token))`.
   - **Flutter / C consumers:** pass it as `int64_t` through rsac-ffi:
     `rsac_builder_set_android_projection(builder, token)`.
   - From here Rust owns everything: it constructs/drives `CaptureBridge`
     over JNI (rsac-77f1), audio flows through the normal
     `BridgeStream` → `read_chunk()` pipeline.

5. **Stop:** drop the capture on the Rust side (releases the token and stops
   the bridge), then `RsacCaptureService.stop(context)`.

Mic-only capture (`CaptureTarget::Device("default")`) needs none of the
projection machinery — only the `RECORD_AUDIO` grant (pure-NDK AAudio path).

## Host-app obligations (documented, not solvable here)

- **Play Store media-projection declaration:** declaring the
  `mediaProjection` foreground-service type triggers a policy declaration at
  app submission. Budget for it.
- **Package visibility (API 30+):** `PackageResolver.uidForPackage` is
  subject to visibility filtering — add a `<queries>` element for packages
  you target by name.
- **What's never capturable:** apps targeting pre-API-29 or setting
  `android:allowAudioPlaybackCapture="false"`, and
  `USAGE_VOICE_COMMUNICATION` audio — the OS silently omits them; rsac's
  streams simply won't contain those apps.

## CI-VERIFY ledger (for rsac-1a6e)

Every uncertain API detail is marked with a `// CI-VERIFY:` comment at the
use site:

| Location | Question |
|---|---|
| `build.gradle.kts` | `kotlin { compilerOptions { } }` DSL vs `kotlinOptions` fallback under the resolved KGP |
| `RsacProjection.kt` | Does `getMediaProjection()` itself throw on API 34+ without the running FGS, or only capture start? |
| `RsacProjection.kt` (`NATIVE_LIBRARY_NAME`) | cdylib artifact name from cargo-ndk must be `librsac.so` |
| `CaptureBridge.kt` (`stop()`) | `AudioRecord.stop()` reliably unblocks `READ_BLOCKING` within the join timeout on-device |
| `RsacDevices.kt` (`inputDevices`) | On-device `AudioManager.getDevices(GET_DEVICES_INPUTS)` output shape: real `getId()` values are positive, `getProductName()` labels contain no `␞`/`␟`, and the default-input ordering assumption holds (rsac-e6d3) |
