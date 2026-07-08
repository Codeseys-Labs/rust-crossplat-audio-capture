# rsac Architecture

This document is the user-facing architecture overview. For fine-grained
design rationale (error taxonomy, backend contract, phased rollout) see
[`docs/architecture/`](architecture/); for the scope manifesto see
[`VISION.md`](../VISION.md).

## 1. The layering split

rsac is organised as a layering DAG. The **intended invariant** is that
dependencies only ever point *down* this chain — no layer reaches back up into a
layer above it:

```
core/  →  bridge/  →  audio/ (platform backends)  →  api/  →  compose/ (opt-in)
                                                       ↘
                                                        sink/
```

| Layer | Crate path | Responsibility |
|---|---|---|
| **core** | [`rsac::core`](../src/core/mod.rs) | Platform-agnostic types and traits: [`AudioBuffer`](../src/core/buffer.rs), [`CaptureTarget`](../src/core/config.rs), [`AudioError`](../src/core/error.rs), [`PlatformCapabilities`](../src/core/capabilities.rs), [`CapturingStream`](../src/core/interface.rs), the [`DeviceEnumerator`](../src/core/interface.rs)/[`DeviceWatcher`](../src/core/interface.rs) surface, runtime introspection helpers. |
| **bridge** | [`rsac::bridge`](../src/bridge/mod.rs) | Lock-free SPSC ring-buffer bridge (via [`rtrb`](https://crates.io/crates/rtrb)) that every backend uses to hand audio off to the consumer thread. Also owns the [`StreamState`](../src/bridge/state.rs) state machine and the `BridgeStream<S>` adapter. |
| **audio** | [`rsac::audio`](../src/audio/mod.rs) | Per-OS backends: WASAPI on Windows, PipeWire on Linux, CoreAudio Process Tap on macOS. Each implements the internal `PlatformStream` trait and plugs into `BridgeStream<S>`. Also hosts the per-OS `DeviceEnumerator` implementations and native device/application enumeration. |
| **api** | [`rsac::api`](../src/api.rs) | Public facade: `AudioCaptureBuilder` → `AudioCapture`, whose lifecycle is driven by its `start()`/`stop()` methods (plus the `RunningCapture` RAII guard returned by the builder's own `AudioCaptureBuilder::start()` convenience method, which builds, starts, and stops on `Drop`). This is the primary entry point library consumers need. |
| **sink** | [`rsac::sink`](../src/sink/mod.rs) | Optional downstream adapters: `NullSink`, `ChannelSink`, `WavFileSink` (behind `sink-wav`). |
| **compose** | [`rsac::compose`](../src/compose/mod.rs) | Opt-in (`compose` feature, [ADR-0011](designs/0011-compose-feature.md)) multi-source channel composition: `CompositionBuilder` → `Composition`. Owns N inner `AudioCapture`s, aligns them on a dedicated compositor thread (master-clock pacing, silence-pad/trim, `rubato` resampling to the session rate), mixes groups to Mono/Stereo or passes native channels through, and delivers one interleaved multi-channel stream through the same `BridgeStream` ring + `CapturingStream` contract as a single capture. Top of the DAG: it consumes `api`, `bridge`, and `core`; nothing may import it. |

The DAG ordering is also asserted in the crate root docs ([`src/lib.rs`](../src/lib.rs))
and enforced by convention in [`AGENTS.md`](../AGENTS.md).

> **Known deviation (tracked).** The DAG invariant is *not* fully clean in the
> shipped code: [`core/introspection.rs`](../src/core/introspection.rs) reaches
> **up** into the `audio` layer to implement source/application discovery. Its
> `list_audio_sources()` calls `crate::audio::get_device_enumerator()`, and the
> per-OS `list_audio_applications_into()` arms call
> `crate::audio::macos::enumerate_audio_applications()`,
> `crate::audio::windows::enumerate_application_audio_sessions()`, and
> `crate::audio::linux::enumerate_audio_applications()`. This is a `core → audio`
> reverse edge that breaks the "core depends on nothing internal" rule. It is a
> real finding from the 2026-05-30 architecture critique, not a clean seam: the
> module's own "Separation of Concerns" doc-comment relabels core/bridge/audio as
> one lump to make the edge look conformant, which should not be taken at face
> value. The accepted fix is to move the discovery functions into the `audio`/`api`
> layer (re-exporting at the same `lib.rs` paths so the public surface is
> unchanged) and to add a CI guard for reverse edges. Until that lands, treat
> introspection as the documented exception to the DAG.

Every public type above has a rustdoc entry. See the crate-level docs on
[docs.rs/rsac](https://docs.rs/rsac) for the live index.

## 2. Data flow

### Capture pipeline

```
┌────────────────────┐   OS real-time thread    ┌───────────────┐
│ OS audio callback  │ ────── f32 samples ────▶ │ BridgeProducer│
│ (WASAPI / PipeWire │                          │   (lock-free) │
│  / CoreAudio)      │                          └───────┬───────┘
└────────────────────┘                                  │ push
                                                        ▼
                                                  ┌────────────┐
                                                  │ rtrb SPSC  │
                                                  │ ring buffer│
                                                  └─────┬──────┘
                                                        │ pop
                           consumer thread              ▼
┌────────────────────┐   AudioBuffer        ┌──────────────────┐
│ user code /        │ ◀───────────────────│ BridgeConsumer    │
│ AudioCapture /     │                      │ ↓                 │
│ subscribe() /      │                      │ BridgeStream<S>   │
│ AsyncAudioStream   │                      │ (CapturingStream) │
└────────────────────┘                      └──────────────────┘
```

Key properties:

- **Lock-free hot path.** The OS audio thread never holds a user-visible
  lock. Samples are pushed into `rtrb::Producer` with no allocation
  (see [`BridgeProducer::push_samples_or_drop`](../src/bridge/ring_buffer.rs)
  — a reusable scratch `Vec<f32>` absorbs back-pressure cycles without
  allocating on the RT thread).
- **Bounded backlog.** If the consumer falls behind, `push_samples_or_drop`
  drops the buffer and bumps both a total `buffers_dropped` counter and a
  `consecutive_drops` counter. When consecutive drops exceed the
  `backpressure_threshold` (10 by default, ≈100 ms at typical rates),
  `CapturingStream::is_under_backpressure()` returns `true`. Consumers use
  this signal to throttle, warn, or switch provider.
- **Pull model.** The consumer chooses when to read, via the public
  `AudioCapture` surface: non-blocking `read_buffer()`, blocking
  `read_buffer_blocking()`, the `buffers_iter()` iterator, push-based
  `subscribe()` (returns `mpsc::Receiver<AudioBuffer>`), or (behind
  `async-stream`) `audio_data_stream()` → `AsyncAudioStream` implementing
  `futures_core::Stream`. (`read_chunk()`/`try_read_chunk()` are the
  `pub(crate)` `CapturingStream` primitives these wrap, not public API.)
- **Multiple captures per process.** Each `AudioCapture` owns its own
  ring-buffer bridge, so parallel captures (e.g., SystemDefault plus a
  per-app capture for a different app) do not interfere.

### State machine

`BridgeStream<S>` drives a lock-free state machine stored in
[`AtomicStreamState`](../src/bridge/state.rs):

```
Created ──▶ Running ──▶ Stopping ──▶ Stopped ──▶ Closed
                    │             ▲
                    ▼             │
                   Error ─────────┘
```

- Transitions are compare-and-swap on a single `AtomicU8`.
- The producer signals "done" by moving `Running → Stopping`. The consumer
  is still permitted to drain buffers in `Stopping`.
- `stop()` is idempotent: calling it twice is not an error.
- Reading a buffer after the stream reaches a terminal state
  (`Stopped` / `Closed` / `Error`) yields `AudioError::StreamEnded`
  (Fatal — see [ADR-0003](designs/0003-terminal-stream-error.md)), so read loops
  that break on `is_fatal()` terminate cleanly. Genuinely transient read hiccups
  surface as the recoverable `AudioError::StreamReadError`.

## 3. Capture-target resolution

[`CaptureTarget`](../src/core/config.rs) is the unified enum:

| Variant | What it captures | Resolver |
|---|---|---|
| `SystemDefault` | System output (loopback of default sink) | OS default output enumerator |
| `Device(DeviceId)` | One specific input or loopback device | Device enumerator (per-backend) |
| `Application(ApplicationId)` | One application process | Numeric PID string, resolved by the backend |
| `ApplicationByName(String)` | First app whose name matches exactly (case-insensitive) | Exact process/app-name lookup, then backend PID resolution |
| `ProcessTree(ProcessId)` | A parent process and its descendants | Platform-native process loopback / Process Tap with tree bit |

The same enum compiles on every platform. When a variant is not supported
on the current OS, `AudioCaptureBuilder::build()` returns
`AudioError::PlatformNotSupported` rather than panicking. Callers that
want to gate features should call
[`PlatformCapabilities::query()`](../src/core/capabilities.rs) first.

## 4. Per-platform backend specifics

### Windows — WASAPI

- Event-driven shared-mode capture on a dedicated COM MTA thread
  (see [`src/audio/windows/thread.rs`](../src/audio/windows/thread.rs)).
- COM initialization is wrapped in a `ComInitializer` RAII guard so
  `CoUninitialize` runs even if the capture panics.
- System/device capture uses standard WASAPI loopback on the selected
  render endpoint.
- Per-app / process-tree capture uses Process Loopback
  (`AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS`), available on Windows 10 build
  19043 (21H1) and newer. (`PlatformCapabilities::query()` reports
  `supports_application_capture = true` unconditionally on Windows — there
  is no runtime build-number gate today; on an older build the Process
  Loopback `IAudioClient::Initialize` call fails and surfaces as an
  `AudioError` at capture time rather than a pre-flight capability denial.)
- Application resolution by name goes through `sysinfo` to find a PID,
  then WASAPI Process Loopback with that PID; for `ProcessTree` the
  `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE` flag is set.

### Linux — PipeWire

- PipeWire's `Rc<_>` types (`MainLoop`, `Context`, `Core`, `Stream`) are
  `!Send`. rsac dedicates a single `PipeWireThread`
  ([`src/audio/linux/thread.rs`](../src/audio/linux/thread.rs)) to own
  them, and user threads communicate with it over a command/response
  channel.
- `SystemDefault` attaches a monitor stream to the default sink node.
- `Device(DeviceId)` targets a sink node by its `object.serial`.
- `Application` / `ApplicationByName` shell out to `pw-dump`, match on
  `application.process.id` for PID strings or exact case-insensitive
  `application.name` / `application.process.binary`, and attach to that node.
- `ProcessTree(pid)` walks the tree with `sysinfo` and maps each PID to a
  PipeWire node using the same `pw-dump` metadata resolver.
- Build-time requires `libpipewire-0.3-dev`, `libspa-0.2-dev`,
  `pkg-config`, `clang`/`libclang-dev`, `llvm-dev`. Runtime requires
  PipeWire 0.3.44+ with the user-session daemon running.

### macOS — CoreAudio Process Tap

- AudioUnit render callback on the CoreAudio real-time thread; data is
  pushed into the shared ring buffer and exposed through
  `BridgeStream<MacosPlatformStream>` (see
  [`src/audio/macos/thread.rs`](../src/audio/macos/thread.rs)).
- Per-application and process-tree capture use the **Process Tap** API
  (`CATapDescription` + aggregate device), available on **macOS 14.4+**.
  Older macOS versions are honestly reported by `PlatformCapabilities`.
- `SystemDefault` also uses `AudioHardwareCreateProcessTap` with a
  system-wide `CATapDescription` — the *same* TCC gate as per-app
  capture. Screen Recording permission is not sufficient; the runtime
  requires `kTCCServiceAudioCapture`.
- Application resolution uses `NSWorkspace.runningApplications`. Unlike
  Windows/Linux, this returns a superset of what is actually producing
  audio (README § "macOS Enumeration Scope" has the full caveat).
- macOS version detection uses `sysctl` (no subprocess, no user-shell
  dependency); see `get_macos_version` in
  [`src/core/capabilities.rs`](../src/core/capabilities.rs).

## 5. Device-change notifications (`watch()`)

rsac exposes a device hot-plug / default-change subscription. The user-facing
entry point is `CrossPlatformDeviceEnumerator::watch(on_event)` (obtained from
[`get_device_enumerator()`](../src/audio/mod.rs)); it dispatches to the active
backend's [`DeviceEnumerator::watch`](../src/core/interface.rs) implementation.
On success it returns a [`DeviceWatcher`](../src/core/interface.rs) RAII guard.
The handler is invoked once per [`DeviceEvent`](../src/core/interface.rs)
(`DeviceAdded` / `DeviceRemoved` / `DefaultChanged` / `StateChanged`).

`watch()` is a **provided** trait method that defaults to
`AudioError::PlatformNotSupported`. Backends whose
[`PlatformCapabilities::supports_device_change_notifications`](../src/core/capabilities.rs)
is `false` inherit that default; the three OS backends each override it with a
real OS listener. The trait contract guarantees only that `on_event` runs on the
backend's **OS notification thread — never the real-time audio callback thread**,
so the handler may allocate and lock. Dropping the `DeviceWatcher` unregisters the
OS listener and joins the notify thread, after which the handler is guaranteed not
to run again (a take-once `Option<Box<dyn FnOnce() + Send>>` teardown whose `Drop`
is best-effort and never panics).

### Per-platform threading divergence (tracked)

The *delivery* threading model differs across platforms — a real, behavioural
divergence that the trait doc does **not** fully unify. Document it so consumers
can branch honestly. This is recorded in **ADR-0004** (device-watch threading
model); see [`docs/designs/`](designs/).

| Platform | OS listener | Hand-off | User handler runs on | Backpressure |
|---|---|---|---|---|
| **Windows** | `IMMNotificationClient` via `RegisterEndpointNotificationCallback` | bounded `sync_channel(64)` + dedicated `rsac-wasapi-device-watch` helper thread | the helper thread (never the COM notification thread) | `try_send`; on a full channel the COM callback **drops** the event rather than block COM |
| **macOS** | `AudioObjectAddPropertyListener` (3 system-object selectors) | bounded `sync_channel(64)` + dedicated helper thread | the helper thread (never the CoreAudio property-listener thread) | bounded; the listener proc **drops** the event on a full or disconnected channel |
| **Linux** | PipeWire registry `global`/`global_remove` + `default`-metadata `property` listeners | **none** — no channel, no helper thread | **directly on the PipeWire loop thread** (a dedicated `rsac-pw-watch` thread that owns the `!Send` `MainLoop`/`Context`/`Core`/`Registry`) | none — the loop thread runs the closure inline |

Why Linux differs: PipeWire's `MainLoop`/`Context`/`Core`/`Registry` are `Rc`
(`!Send`) and must all live on one thread, so the persistent watch loop thread
*is* the natural delivery thread; invoking the user closure inline (guarded by an
`Rc<RefCell<…>>`) avoids a second hop. The Windows/macOS hand-off exists because
their OS notification threads must not run arbitrary user code (COM re-entrancy /
CoreAudio listener constraints). The trait contract ("OS notification thread,
never the RT thread") holds on all three, but the **channel bound (64), the
drop-on-full event-loss policy, and the extra thread hop** are present on
Windows/macOS and absent on Linux. ADR-0004 records the considered options
(direct-invoke vs helper-thread hand-off; bounded vs unbounded; drop policy) and
the per-platform decision.

## 6. Error model

Every fallible operation returns `AudioResult<T>` (alias for
`Result<T, AudioError>`). `AudioError` variants are tagged with:

- [`ErrorKind`](../src/core/error.rs) — one of
  `Configuration`, `Device`, `Stream`, `Backend`, `Application`,
  `Platform`, `Internal`.
- [`Recoverability`](../src/core/error.rs) — one of `Recoverable`,
  `TransientRetry`, or `Fatal` (three states).
- Optional [`BackendContext`](../src/core/error.rs) — a structured
  wrapper for OS-level error codes and the name of the failing operation,
  carried inside the variants that wrap a backend failure.

Call `AudioError::kind()`, `AudioError::recoverability()`, and the
`is_recoverable()` / `is_fatal()` helpers to drive retry / fallback logic. The
full taxonomy lives in
[`docs/architecture/ERROR_CAPABILITY_DESIGN.md`](architecture/ERROR_CAPABILITY_DESIGN.md).

## 7. Thread safety contract

- All public types (`AudioCapture`, `AudioBuffer`, `PlatformCapabilities`,
  sinks) are `Send + Sync`.
- The data plane (OS callback thread ↔ consumer thread) is lock-free.
- The control plane (`start`, `stop`, state transitions) goes through
  `Arc<Mutex<S>>` on the platform stream plus the atomic state machine.
- Device-change notifications run on the backend's OS notification thread
  (see [§5](#5-device-change-notifications-watch)), never the RT audio thread.
- Multiple `AudioCapture` instances per process are supported and do not
  share ring buffers.

## 8. Where to go from here

| If you want to… | Read |
|---|---|
| Learn scope and non-goals | [`VISION.md`](../VISION.md) |
| See the Cargo feature matrix | [`docs/features.md`](features.md) |
| Understand the error enum in depth | [`docs/architecture/ERROR_CAPABILITY_DESIGN.md`](architecture/ERROR_CAPABILITY_DESIGN.md) |
| Understand the backend contract | [`docs/architecture/BACKEND_CONTRACT.md`](architecture/BACKEND_CONTRACT.md) |
| See the full API surface | [`docs/architecture/API_DESIGN.md`](architecture/API_DESIGN.md) or [docs.rs/rsac](https://docs.rs/rsac) |
| Build and test locally | [`docs/LOCAL_TESTING_GUIDE.md`](LOCAL_TESTING_GUIDE.md) |
| Debug CI audio test behaviour | [`docs/CI_AUDIO_TESTING.md`](CI_AUDIO_TESTING.md) |
| Contribute code | [`docs/CONTRIBUTING.md`](CONTRIBUTING.md) |
| Cut a release | [`docs/RELEASE_PROCESS.md`](RELEASE_PROCESS.md) |
