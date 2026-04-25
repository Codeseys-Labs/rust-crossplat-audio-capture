# rsac Architecture

This document is the user-facing architecture overview. For fine-grained
design rationale (error taxonomy, backend contract, phased rollout) see
[`docs/architecture/`](architecture/); for the scope manifesto see
[`VISION.md`](../VISION.md).

## 1. The three-layer split

rsac is organised as a strict layering DAG with no reverse dependencies:

```
core/  →  bridge/  →  audio/ (platform backends)  →  api/
                                                       ↘
                                                        sink/
```

| Layer | Crate path | Responsibility |
|---|---|---|
| **core** | [`rsac::core`](../src/core/mod.rs) | Platform-agnostic types and traits: [`AudioBuffer`](../src/core/buffer.rs), [`CaptureTarget`](../src/core/config.rs), [`AudioError`](../src/core/error.rs), [`PlatformCapabilities`](../src/core/capabilities.rs), [`CapturingStream`](../src/core/interface.rs), runtime introspection helpers. |
| **bridge** | [`rsac::bridge`](../src/bridge/mod.rs) | Lock-free SPSC ring-buffer bridge (via [`rtrb`](https://crates.io/crates/rtrb)) that every backend uses to hand audio off to the consumer thread. Also owns the [`StreamState`](../src/bridge/state.rs) state machine and the `BridgeStream<S>` adapter. |
| **audio** | [`rsac::audio`](../src/audio/mod.rs) | Per-OS backends: WASAPI on Windows, PipeWire on Linux, CoreAudio Process Tap on macOS. Each implements the internal `PlatformStream` trait and plugs into `BridgeStream<S>`. |
| **api** | [`rsac::api`](../src/api.rs) | Public facade: `AudioCaptureBuilder` → `AudioCapture`. This is the only entry point library consumers need. |
| **sink** | [`rsac::sink`](../src/sink/mod.rs) | Optional downstream adapters: `NullSink`, `ChannelSink`, `WavFileSink` (behind `sink-wav`). |

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
- **Pull model.** The consumer chooses when to read: blocking
  `read_chunk()`, non-blocking `try_read_chunk()`, push-based
  `subscribe()` (returns `mpsc::Receiver<AudioBuffer>`), or (behind
  `async-stream`) `AsyncAudioStream` implementing `futures_core::Stream`.
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
- Reading a buffer after `Stopped` yields `AudioError::StreamError`.

## 3. Capture-target resolution

[`CaptureTarget`](../src/core/config.rs) is the unified enum:

| Variant | What it captures | Resolver |
|---|---|---|
| `SystemDefault` | System output (loopback of default sink) | OS default output enumerator |
| `Device(DeviceId)` | One specific input or loopback device | Device enumerator (per-backend) |
| `Application(ApplicationId)` | One application session | Backend-native session ID |
| `ApplicationByName(String)` | First app whose name substring-matches (case-insensitive) | `sysinfo` PID lookup (Win/macOS) or `pw-dump` node serial (Linux) |
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
  19043 (21H1) and newer. Older builds skip early via
  `PlatformCapabilities`.
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
  `application.process.id` or `application.name`, and attach to that node.
- `ProcessTree(pid)` walks the tree with `sysinfo` and maps each PID to a
  PipeWire node. See the helpers in `tests/ci_audio/helpers.rs` for the
  exact `pw-dump` parsing logic used in CI.
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

## 5. Error model

Every fallible operation returns `AudioResult<T>` (alias for
`Result<T, AudioError>`). `AudioError` variants are tagged with:

- [`ErrorKind`](../src/core/error.rs) — one of
  `Configuration`, `Device`, `Stream`, `Backend`, `Application`,
  `Platform`, `Internal`.
- [`Recoverability`](../src/core/error.rs) — `Recoverable`,
  `TransientRetry`, `Fatal`, or `UserError`.
- Optional [`BackendContext`](../src/core/error.rs) — a structured
  wrapper for OS-level error codes and the name of the failing operation.

Call `AudioError::kind()`, `AudioError::recoverability()`, and
`AudioError::backend_context()` to drive retry / fallback logic. The full
taxonomy lives in
[`docs/architecture/ERROR_CAPABILITY_DESIGN.md`](architecture/ERROR_CAPABILITY_DESIGN.md).

## 6. Thread safety contract

- All public types (`AudioCapture`, `AudioBuffer`, `PlatformCapabilities`,
  sinks) are `Send + Sync`.
- The data plane (OS callback thread ↔ consumer thread) is lock-free.
- The control plane (`start`, `stop`, state transitions) goes through
  `Arc<Mutex<S>>` on the platform stream plus the atomic state machine.
- Multiple `AudioCapture` instances per process are supported and do not
  share ring buffers.

## 7. Where to go from here

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
