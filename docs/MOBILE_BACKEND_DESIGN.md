# Mobile Backend Design — Android & iOS

> **Status: fully implemented (compile-checked only, no on-device runtime
> proof yet).** Implemented behind `feat_android`/`feat_ios`: the AAudio /
> AVAudioEngine `Device("default")` microphone slices (rsac-20cd /
> rsac-9e02), the iOS `SystemDefault` ReplayKit ring consumer (rsac-b3aa,
> `src/audio/ios/broadcast.rs`, App Group id via
> `AudioCaptureBuilder::with_ios_app_group`), and Android playback capture —
> all four `AudioPlaybackCapture` tiers via the AAR's Kotlin loop + JNI
> ingest (rsac-77f1, `src/audio/android/{jni,playback}.rs`, consent token
> via `with_android_projection`) — cross-target check + clippy green, **no
> runtime verification on any device yet** (seeds rsac-e6d3 / rsac-97c8).
> First-party glue lives in `mobile/{android,ios}/` and **builds in CI**,
> including `librsac.so` packaged into the AAR's jniLibs with its
> `JNI_OnLoad` export asserted (rsac-0aa9/rsac-77f1).
> **Where implementation and this doc diverge, the code (and
> `mobile/ios/Sources/RsacBroadcastKit/RingLayout.swift` for the ring
> contract) wins** — known divergences: `RsacProjection.request` is
> callback-async (ActivityResult), not the synchronous `request(activity):
> Long` sketched below; the ring's canonical field layout lives in
> RingLayout.swift v1; the JNI ingest uses a **registry-id session** (not
> the raw pointer sketched below — `CaptureBridge.stop()`'s bounded join
> cannot prove push quiescence, see `src/audio/android/jni.rs`) and a
> `nativeSessionEnded` terminal handshake the sketch below omits. The
> durable decisions are ADRs:
> [ADR-0012](designs/0012-mobile-platform-strategy.md) (platform strategy &
> packaging) and [ADR-0013](designs/0013-mobile-capturetarget-semantics.md)
> (CaptureTarget semantics). Framework-facing guidance lives in
> [`FRAMEWORK_COMPATIBILITY.md`](FRAMEWORK_COMPATIBILITY.md).
>
> Per AGENTS.md: when this doc and future code disagree, **the code wins** —
> update this doc or supersede the ADRs.

## Goals and non-goals

**Goals**

- Android backend: system capture, per-app capture, process-tree capture (via
  `AudioPlaybackCapture`), mic capture (via AAudio) — all delivered through the
  existing `BridgeStream<S>` / `PlatformStream` architecture, capabilities
  honestly reported.
- iOS backend: mic capture (AVAudioEngine) and system capture via a ReplayKit
  Broadcast Upload Extension; everything else honestly `false`.
- Batteries included: rsac ships the Kotlin AAR (`mobile/android/`) and Swift
  package (`mobile/ios/`) so Tauri, Dioxus, Flutter, and native apps all share
  one consent-flow implementation (ADR-0012).
- The desktop API is unchanged; mobile adds cfg-gated surface only.

**Non-goals (permanent or out of scope)**

- **iOS per-app / process-tree capture: impossible for third-party apps.**
  Apple provides no API. This is a permanent ❌, never to be softened in docs
  or capabilities.
- Capturing Android apps that target pre-Android-10 or set
  `android:allowAudioPlaybackCapture="false"` — the OS excludes them; rsac
  will deliver silence-free streams that simply omit those apps, and the docs
  must say so.
- Voice-call capture on Android (`USAGE_VOICE_COMMUNICATION` is never
  capturable by third parties).
- Emulating desktop loopback semantics where the OS has none.

## Architecture recap (what mobile plugs into)

Every rsac backend implements the internal
[`PlatformStream`](../src/bridge/stream.rs) trait (`stop_capture()`,
`is_active()`) and pushes audio into a
[`BridgeProducer`](../src/bridge/ring_buffer.rs) (lock-free SPSC ring,
`push_samples_or_drop()` for allocation-free producer pushes); consumers read
via `BridgeStream<S>`. The module DAG is
`core → bridge → audio → api`. Mobile backends are two new leaf modules:

```
src/audio/android/    # cfg(target_os = "android"), feature feat_android
├── mod.rs
├── aaudio.rs         # AAudio mic capture (pure NDK, no Java)
├── playback.rs       # AudioPlaybackCapture orchestration (JNI → AAR service)
├── jni.rs            # JNI ingest surface: Java capture loop → BridgeProducer
└── thread.rs         # AndroidPlatformStream (PlatformStream impl)

src/audio/ios/        # cfg(target_os = "ios"), feature feat_ios
├── mod.rs
├── avaudio.rs        # AVAudioEngine input-node mic capture (objc2)
├── broadcast.rs      # App Group mmap-ring consumer for the ReplayKit path
└── thread.rs         # IosPlatformStream (PlatformStream impl)

mobile/android/       # Gradle project → rsac.aar (Kotlin) — NOT a Cargo crate
mobile/ios/           # SwiftPM package + broadcast-extension template
```

`mobile/` joins the crates.io `exclude` list (like `/apps` and `/bindings`).
Feature naming follows the existing convention: `feat_android`, `feat_ios`,
double-gated with `target_os` exactly like the desktop backends
(src/audio/mod.rs), so non-mobile builds are unaffected and the honest-stub
fallback (`PlatformNotSupported`) keeps working for OSes with no backend.

## Android

### Two data paths

| Path | API | Language | Targets served |
|---|---|---|---|
| Mic | **AAudio** (NDK, API 26+) | pure Rust over NDK FFI | `Device(DeviceId)` |
| Playback capture | **`AudioRecord` + `AudioPlaybackCaptureConfiguration`** (API 29+) | **Java/Kotlin only** — no NDK equivalent exists | `SystemDefault`, `Application`, `ApplicationByName`, `ProcessTree` |

The AAudio path is conventional: an `AAudioStreamBuilder` input stream whose
data callback pushes into `BridgeProducer::push_samples_or_drop()` — same shape
as the desktop backends. `reference/cpal`'s Android host is a working example
of AAudio stream setup from Rust.

The playback-capture path is the structurally unusual one: **the read loop must
live in Java**, because `AudioPlaybackCaptureConfiguration` can only be
attached to a Java `AudioRecord`. The AAR owns a dedicated Java thread that
loops `audioRecord.read(floatBuf, ...)` and calls **one** JNI-registered native
function per buffer:

```
// registered from Rust via JNI_OnLoad / RegisterNatives
fn Java_ai_codeseys_rsac_CaptureBridge_nativePush(
    env: JNIEnv, _cls: JClass,
    session: jlong,          // opaque pointer to the per-capture ingest state
    buf: JFloatArray, frames: jint, channels: jint, sample_rate: jint,
)
```

`nativePush` copies out of the Java array (`GetFloatArrayRegion` into a
reused scratch buffer) and calls `push_samples_or_drop()`. Ring-full ⇒ drop and
count (`overrun_count`), never block the Java thread.

### RT-safety statement (ADR-0001 restated for JNI)

The Java capture thread is **not** an OS real-time audio callback thread —
`AudioRecord.read` is a buffered blocking read. ADR-0001's allocation
prohibition therefore applies in adapted form:

- The **Rust side of `nativePush` must not allocate per-call** — scratch
  buffers are allocated once at session start; the free-list return ring
  applies as on desktop.
- The **Java side allocates its read buffer once** and reuses it; no per-read
  garbage (GC pauses on the capture thread become drops, which the overrun
  counters make visible).
- The AAudio mic callback **is** RT-adjacent (AAudio may use a real-time
  thread): full ADR-0001 rules apply, identical to desktop.

### Consent-token flow (`with_android_projection`)

`MediaProjection` requires a user-consent dialog and cannot be conjured from
Rust. The flow (decided in ADR-0012/ADR-0013 discussion, "explicit builder
token + preflight"):

1. The app calls the AAR helper
   (`RsacProjection.request(activity) : Long` — launches the consent
   `Intent`, wraps the resulting `MediaProjection` in a JNI `GlobalRef`, and
   returns an **opaque token** — a `jlong` handle, pointer-sized across FFI).
2. The token crosses into Rust (directly in Tauri/Dioxus; through rsac-ffi as
   an `int64_t` for Flutter/C consumers).
3. `#[cfg(target_os = "android")] AudioCaptureBuilder::with_android_projection(AndroidProjectionToken)`
   stores it.
4. **Preflight in `build()`:** a playback-capture target (`SystemDefault`,
   `Application*`, `ProcessTree`) without a token fails immediately with a
   `ConfigurationError`-class `AudioError` (new variant, categorized per the
   error-model rule — exhaustive `recoverability()` match forces
   classification). `Device` (mic) targets need no token.
5. Token lifetime: released (JNI `DeleteGlobalRef` + `MediaProjection.stop()`)
   when the owning capture is dropped; the AAR documents that one token = one
   projection session.

There is deliberately **no process-global token registry** — explicit
configuration, no hidden state, and multi-capture semantics stay well-defined.

### CaptureTarget mapping (normative — ADR-0013)

| `CaptureTarget` | Android meaning |
|---|---|
| `SystemDefault` | `AudioPlaybackCapture` with usage filters (`USAGE_MEDIA`, `USAGE_GAME`, `USAGE_UNKNOWN`), no UID filter — "all capturable playback" |
| `Application(ApplicationId)` | `addMatchingUid(uid)`; `ApplicationId` carries the app UID |
| `ApplicationByName(String)` | package name → UID via `PackageManager` (AAR helper), then as above |
| `ProcessTree(ProcessId)` | PID → UID (`/proc/<pid>/status` `Uid:` field), then `addMatchingUid`. **On Android all processes of an app share one UID, so tree ≡ app** — documented equivalence, not a limitation |
| `Device(DeviceId)` | AAudio input device (mic/USB/BT source from `AudioManager.getDevices`) |

### Manifest & service requirements (AAR-provided)

- `RECORD_AUDIO` runtime permission (required even for playback capture).
- A foreground service with
  `android:foregroundServiceType="mediaProjection"` +
  `FOREGROUND_SERVICE_MEDIA_PROJECTION` permission — capture must run inside
  it; the AAR ships the service and the notification plumbing.
- Play Store: declaring the media-projection foreground-service type triggers
  a policy declaration at submission. Document, don't solve.

### `PlatformCapabilities` on Android

```text
supports_system_capture:        true   (API 29+, consent required)
supports_application_capture:   true   (API 29+, consent required)
supports_process_tree_capture:  true   (≡ application; PID→UID)
supports_device_capture:        true   (AAudio mic)
requires_user_consent:          true   (NEW field — see below)
backend_name:                   "android-playback-capture/aaudio"
```

On API < 29, the playback-capture flags are `false` at runtime (SDK version
check — same runtime-gating pattern as macOS 14.4 Process Tap detection in
`PlatformCapabilities::macos()`).

## iOS

### Mic path (`Device`)

`AVAudioEngine.inputNode` tap (objc2 / objc2-avf-audio bindings, consistent
with the macOS backend's objc2 migration) → converter to interleaved f32 →
`push_samples_or_drop()`. Requires `NSMicrophoneUsageDescription` and an
`AVAudioSession` category of `.playAndRecord`/`.record`; the SwiftPM package
provides session-configuration helpers.

### System path (`SystemDefault`) — ReplayKit Broadcast Upload Extension

The only third-party route to system-wide audio on iOS. Shape:

- The consumer app embeds a **Broadcast Upload Extension** target generated
  from rsac's template in `mobile/ios/`. The user starts the broadcast
  (control-center picker / `RPSystemBroadcastPickerView`); iOS delivers
  `CMSampleBuffer`s (`.audioApp`, `.audioMic`) to the extension's
  `RPBroadcastSampleHandler`.
- **Transport (decided): a memory-mapped SPSC ring in the shared App Group
  container.** Extension = producer, host app = consumer. Fixed-size file
  (`mmap`), header with atomic read/write cursors + format fields + heartbeat,
  frames stored as interleaved f32 — deliberately mirroring the rtrb bridge
  semantics so the host side is a thin `PlatformStream` that drains the mmap
  ring into a normal `BridgeProducer`.
- **Signaling:** Darwin notifications (`CFNotificationCenterGetDarwinNotifyCenter`)
  for started/stopped/liveness; a heartbeat field in the ring header guards
  against a killed extension (missed heartbeats ⇒ terminal).
- **Memory budget:** the extension is hard-capped (~50 MB). The ring is sized
  in the low single-digit MB (seconds of 48 kHz stereo f32); ring-full ⇒ drop
  + overrun count, never block the sample handler.
- **Terminal semantics:** the user can stop the broadcast at any time; the
  host-side stream must end with the fatal terminal per ADR-0003/ADR-0010
  (broadcast-stopped ⇒ producer terminal signal ⇒ `StreamEnded`).
- **Costs to document loudly:** user-initiated only (no programmatic start),
  captures *everything* (no per-app filter), extension target + App Group
  entitlement required in the consumer app, App Store review scrutiny.

### CaptureTarget mapping (normative — ADR-0013)

| `CaptureTarget` | iOS meaning |
|---|---|
| `SystemDefault` | ReplayKit broadcast path (host-side mmap-ring consumer). Errors with actionable guidance if the App Group/extension is absent |
| `Device(DeviceId)` | AVAudioEngine input (mic/BT/USB via `AVAudioSession` routes) |
| `Application` / `ApplicationByName` / `ProcessTree` | **`PlatformNotSupported` — permanently.** Capabilities report `false` |

### `PlatformCapabilities` on iOS

```text
supports_system_capture:        true   (via broadcast extension; consent + extension required)
supports_application_capture:   false  (permanent — Apple provides no API)
supports_process_tree_capture:  false  (permanent)
supports_device_capture:        true   (AVAudioEngine mic)
requires_user_consent:          true
backend_name:                   "ios-avaudioengine/replaykit"
```

## Core-crate additions (shared)

- **`PlatformCapabilities.requires_user_consent: bool`** — `false` on all
  three desktop backends, `true` on Android/iOS. Additive field; desktop
  behavior unchanged. (Richer per-target consent detail was considered and
  deferred — one honest bool now, refine when real consumers need more.)
- **New `AudioError` variant(s)** for missing-consent/missing-extension
  preconditions — categorized (`ErrorKind`) and classified in the exhaustive
  `recoverability()` match, per the error-model rule.
- **`AndroidProjectionToken`** newtype (cfg-gated) + builder method; FFI adds
  `rsac_builder_set_android_projection(builder, int64_t)`.

## CI strategy

Staged, honest about what each stage proves:

1. **Stage 1 — DONE (2026-07-06):** the `mobile-android` / `mobile-ios` ci.yml
   jobs run the cross-target check+clippy matrix for both mobile targets, a
   real Gradle `assembleRelease` of the AAR (artifact-asserted), and
   `xcodebuild` of both SwiftPM products. These prove *compilation*, not
   capture. (First run caught two real bugs — the build.rs host-cfg dispatch
   and a Swift `close()` shadowing — which is exactly the point of the stage.)
2. **Stage 2 — seeded:** runtime verification. `rsac-0aa9` packages
   `librsac.so` into the AAR (cargo-ndk + jniLibs — prerequisite for any
   on-device run and for rsac-77f1's JNI symbols); `rsac-e6d3` brings up an
   API 29+ Android emulator leg (mic frames delivered, dormant `cfg(android)`
   tests executed; MediaProjection consent automation via `uiautomator` once
   rsac-77f1 lands); `rsac-97c8` does the iOS twin (simulator mic first,
   physical device as stretch, broadcast-extension end-to-end once rsac-b3aa
   lands). **Do not claim "tested on Android/iOS" until these are green** —
   same discipline as the desktop verification table in AGENTS.md §6.
3. **Stage 3 — seeded (delivery):** real Android device enumeration/selection
   (`rsac-ad8a`), glue distribution — Maven AAR + SwiftPM guidance
   (`rsac-05b6`), and rsac-ffi mobile-triple cross-checks for Flutter/C
   consumers (`rsac-7a18`).

## Risks & open questions (tracked, not hidden)

| Risk | Mitigation / status |
|---|---|
| JNI ingest adds a copy (Java array → Rust scratch) | Accepted: `AudioRecord.read` is already a buffered non-RT path; overrun counters expose any sustained cost. `GetPrimitiveArrayCritical` is a measured-later optimization |
| GC pauses on the Java capture thread | Manifest as drops, visible via `overrun_count`/`backpressure_report` — same observability story as desktop |
| ReplayKit extension killed / broadcast stopped mid-stream | Heartbeat + Darwin notifications ⇒ producer terminal signal (ADR-0010); stream ends with fatal terminal (ADR-0003) |
| 50 MB extension cap | Ring sized ≪ cap; drop-not-block policy |
| Store review (Play media-projection declaration; App Store broadcast scrutiny) | Documented consumer obligation; templates keep the surface minimal |
| `bun --compile`-class packaging issues don't apply here, but Flutter/RN mobile consumers need rsac-ffi built per mobile triple | rsac-napi per-target migration + FFI mobile-triple builds are seeded prerequisites |
| Android `ApplicationId` currently means "PID or platform app id" on desktop; on Android it must carry a UID | ADR-0013 fixes the meaning per-platform; introspection helpers (`list_audio_applications`) return UID-bearing ids on Android |

## Decision record

| Decision | Where |
|---|---|
| Batteries-included packaging (rsac owns AAR + SwiftPM), `mobile/` layout, backends in-crate | [ADR-0012](designs/0012-mobile-platform-strategy.md) |
| CaptureTarget mobile mapping, ReplayKit-as-SystemDefault, mmap-ring transport, explicit builder token + preflight | [ADR-0013](designs/0013-mobile-capturetarget-semantics.md) |
| Tauri integration model (direct dep on desktop; plugin as mobile vehicle) — *proposed* | [ADR-0014](designs/0014-tauri-integration-model.md) |
