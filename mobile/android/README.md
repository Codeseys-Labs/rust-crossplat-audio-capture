# rsac Android glue (`rsac.aar`)

> **⚠️ Source-complete; not yet built in CI.** These sources have never been
> compiled — no Android SDK/Gradle existed on the authoring machine.
> **rsac-1a6e** adds the Gradle CI job (and the Gradle wrapper, deliberately
> absent here) and trues up every version pin marked *"expected; CI trues
> up"*. The JNI symbols this glue calls ship with **rsac-77f1**
> (`src/audio/android/jni.rs`); until then `librsac.so` is absent and native
> calls throw `UnsatisfiedLinkError` — guard with
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
| `src/main/AndroidManifest.xml` | Permissions + the `mediaProjection`-typed service (merged into the host app) |

## JNI symbol contract

**Lockstep with `src/audio/android/jni.rs` when rsac-77f1 lands** — Rust
registers these via `RegisterNatives` in `JNI_OnLoad`; renaming a class,
method, or signature on either side breaks capture. A CI drift guard is part
of rsac-1a6e.

| Kotlin declaration (class) | JNI symbol | Direction |
|---|---|---|
| `@JvmStatic external fun nativeRetainProjection(projection: MediaProjection): Long` (`ai.codeseys.rsac.RsacProjection`) | `Java_ai_codeseys_rsac_RsacProjection_nativeRetainProjection` | Kotlin → Rust: wrap the consented `MediaProjection` in a JNI `GlobalRef`, return the opaque token |
| `@JvmStatic external fun nativePush(session: Long, buf: FloatArray, frames: Int, channels: Int, sampleRate: Int)` (`ai.codeseys.rsac.CaptureBridge`) | `Java_ai_codeseys_rsac_CaptureBridge_nativePush` | Kotlin → Rust: per-period ingest into `BridgeProducer::push_samples_or_drop()` |

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

3. **Start the foreground service** (mandatory ordering on API 34+):

   ```kotlin
   RsacCaptureService.start(context)
   ```

4. **Run the consent flow** from a `ComponentActivity`:

   ```kotlin
   RsacProjection.request(activity, object : RsacProjection.Callback {
       override fun onToken(token: Long) { /* hand `token` to Rust */ }
       override fun onDenied(reason: String) { /* surface to the user */ }
   })
   ```

5. **Hand the token to Rust.**
   - **Dioxus / direct Rust:** pass the `Long` across your JNI/FFI boundary
     and call `AudioCaptureBuilder::with_android_projection(AndroidProjectionToken(token))`.
   - **Flutter / C consumers:** pass it as `int64_t` through rsac-ffi:
     `rsac_builder_set_android_projection(builder, token)`.
   - From here Rust owns everything: it constructs/drives `CaptureBridge`
     over JNI (rsac-77f1), audio flows through the normal
     `BridgeStream` → `read_chunk()` pipeline.

6. **Stop:** drop the capture on the Rust side (releases the token and stops
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
