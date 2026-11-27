# rsac — Rust Cross-Platform Audio Capture

A streaming-first audio capture library for Rust. Captures system audio, per-application audio, and process-tree audio on Windows (WASAPI), Linux (PipeWire), and macOS (CoreAudio Process Tap).

## Why rsac?

`cpal` and `portaudio-rs` expose device-level primitives — open an input, read f32 samples — but cannot capture a single application's audio on any platform without virtual-cable workarounds. rsac wraps WASAPI Process Loopback, CoreAudio Process Tap, and PipeWire node monitors behind one `AudioCaptureBuilder → AudioCapture` API. [Capture scope and test coverage vary by backend](#capture-mode-support).

| Capability | rsac | cpal | portaudio-rs |
|---|---|---|---|
| System-output capture (loopback) | ✅ | ✅ | ✅ |
| Per-device input capture | ✅ | ✅ | ✅ |
| Per-app / per-PID capture | ✅ | ❌ | ❌ |
| Per-process-tree capture (app + children) | ✅ | ❌ | ❌ |
| Multi-source simultaneous | ✅ (one process, multiple `AudioCapture` instances) | ⚠️ (manual per-stream) | ⚠️ |
| Backpressure signaling | ✅ (`is_under_backpressure()`) | ❌ | ❌ |
| Cross-platform consistency | ✅ | ✅ (mature) | ⚠️ |

### What rsac is NOT

rsac is a capture library, not a DSP or playback library. For downstream concerns, reach for:

- **Mixing (general-purpose)** → `rodio::Source::mix`, or a 3-line `f32 + f32` adder over rsac's `AudioBuffer.data()` — but see the opt-in [`compose` feature](#composing-multiple-sources-the-compose-feature) for capture-side multi-source channel composition ([ADR-0011](docs/designs/0011-compose-feature.md))
- **Resampling (general-purpose)** → `rubato` / `samplerate` (rsac uses `rubato` internally only to rate-align composed sources)
- **Encoding** → `hound` (WAV) / `symphonia` / `opus`
- **Playback** → `cpal` / `rodio`

See [VISION.md](VISION.md) for the full in-scope / out-of-scope list, and
[docs/INTEROP.md](docs/INTEROP.md) for copy-paste recipes bridging rsac buffers
into `dasp`, `cpal`/`rodio`, `hound`, and encoder pipelines.

## CI Status

[![CI](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/workflows/ci.yml)
[![Audio Tests](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/workflows/ci-audio-tests.yml/badge.svg?branch=master)](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/workflows/ci-audio-tests.yml)

The `ci.yml` badge covers lint + unit tests on **all three platforms** (one
workflow, per-OS jobs — Linux/Windows/macOS statuses live in the
[Actions tab](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/workflows/ci.yml)).

### Audio Integration Tests

See [Capture Mode Support](#capture-mode-support) for runtime evidence by target.
The [CI audio testing guide](docs/CI_AUDIO_TESTING.md) explains the runner setup
and which tests can skip. A green job is not itself proof of capture: hosted
macOS skips live Process Tap tests because it cannot grant System Audio Recording permission.

## Features

- **Streaming-first** — audio data is delivered via `AudioBuffer` chunks for in-flight processing, not just file writing
- **System-wide capture** on all three platforms
- **Per-application capture** by PID or name (WASAPI process loopback, PipeWire node mapping, CoreAudio Process Tap)
- **Process-tree capture** for child process hierarchies
- **One-line ergonomics** — the `capture!` macro and `rsac::prelude::*`; `AudioCaptureBuilder::start()` returns a `RunningCapture` RAII guard (`Deref`s to `AudioCapture`, stops on `Drop`)
- **String targets** — `CaptureTarget` round-trips through `FromStr` / `TryFrom<&str>` / `Display`; the builder takes `target_str()` / `try_target_str()`
- **RT-safe level metering** — `AudioBuffer::rms()` / `peak()` / `rms_dbfs()` / `peak_dbfs()` and per-channel `channel_rms()` / `channel_peak()` (allocation-free, callback-thread safe)
- **Lock-free ring buffers** (`rtrb` SPSC) bridging OS callback threads to consumer threads; alloc-free on the producer hot path in steady state
- **Push-based subscription** (`subscribe()` returns `mpsc::Receiver<AudioBuffer>`)
- **Stream diagnostics** — `stream_stats()` (`StreamStats`: buffers pushed/captured/dropped, uptime, negotiated format) and `backpressure_report()` (`BackpressureReport`)
- **Device-change watching** — `DeviceEnumerator::watch()` returns a `DeviceWatcher` RAII guard delivering `DeviceEvent`s off the RT thread (Windows/macOS via a bounded helper-thread channel; Linux directly on the PipeWire loop thread)
- **Device introspection** — `AudioDevice::describe()` → `DeviceInfo` and `supported_formats()` on all three backends
- **Overflow monitoring** (`overrun_count()` tracks dropped buffers)
- **Backpressure signaling** (`is_under_backpressure()` on the `CapturingStream` trait — returns `true` when sustained consecutive frame drops indicate the consumer cannot keep up; use to throttle, warn, or switch providers)
- **Sink adapters** — `NullSink`, `ChannelSink`, `WavFileSink` — drive them with `drain_to(sink)`, the built-in background-thread driver on `AudioCapture`, `RunningCapture`, and (with `compose`) `Composition`; or feed them from your own read/subscribe loop.
- **Multi-source channel composition** (opt-in `compose` feature) — group captures into one multi-channel stream: per-group Mono/Stereo mixdown with per-source gain, or native-channel passthrough; transparent `rubato` resampling to the session rate; master-clock alignment with silence-padding/trim counters ([ADR-0011](docs/designs/0011-compose-feature.md))
- **Platform capability reporting** — `PlatformCapabilities::query()` for honest feature detection
- **Language bindings at parity** — C/Go, Python (PyO3, single `cp39-abi3` wheel), and Node (napi), all exposing metering, `stream_stats`, format query, string targets, and idiomatic context managers / RAII

## Quick Start

```rust
use rsac::{AudioCaptureBuilder, CaptureTarget};
use std::time::Duration;

let mut capture = AudioCaptureBuilder::new()
    .with_target(CaptureTarget::SystemDefault)
    .sample_rate(48000)
    .channels(2)
    .build()?;

capture.start()?;

// Streaming-first: read audio chunks in a loop.
//
// read_buffer() returns AudioResult<Option<AudioBuffer>>:
//   Ok(Some(buf)) — a chunk is ready
//   Ok(None)      — no data *yet* (do NOT treat as end-of-stream; back off briefly)
//   Err(e)        — break only if e.is_fatal(); recoverable errors are transient
loop {
    match capture.read_buffer() {
        Ok(Some(buffer)) => {
            let samples: &[f32] = buffer.data();
            let frames = buffer.num_frames();
            // RT-safe metering — no hand-rolled RMS needed:
            let level_dbfs = buffer.rms_dbfs();
            let _ = (samples, frames, level_dbfs); // process audio...
        }
        Ok(None) => {
            // Ring is momentarily empty — avoid busy-spinning.
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(e) if e.is_fatal() => {
            eprintln!("capture ended: {e}");
            break;
        }
        Err(e) => {
            // Recoverable (e.g. a transient read hiccup) — log and keep going.
            eprintln!("transient read error: {e}");
        }
    }
}

capture.stop()?;
```

> The `?` operator on `read_buffer()` is a footgun in a capture loop: it would
> terminate the whole function on a *recoverable* error. Match on the result and
> branch on `AudioError::is_fatal()` instead, as above.

### One-liner with the prelude + `capture!` macro

```rust
use rsac::prelude::*;

# fn main() -> Result<(), Box<dyn std::error::Error>> {
// `start()` returns a RunningCapture RAII guard: it Derefs to AudioCapture and
// calls stop() on Drop, so there is nothing to tear down by hand.
let mut running = capture!(system, rate: 48000, channels: 2).start()?;

if let Ok(Some(buffer)) = running.read_buffer() {
    println!("level: {:.1} dBFS", buffer.rms_dbfs());
}
# Ok(())
# } // `running` drops here → capture stops automatically
```

### Application Capture

```rust
use rsac::{AudioCaptureBuilder, CaptureTarget};

let capture = AudioCaptureBuilder::new()
    .with_target(CaptureTarget::ApplicationByName("firefox".into()))
    .build()?;
```

### Device Enumeration

```rust
use rsac::{get_device_enumerator, DeviceKind};

let enumerator = get_device_enumerator()?;
let devices = enumerator.enumerate_devices()?;
let default = enumerator.default_device()?;
```

### Device-Change Notifications

```rust
use rsac::{get_device_enumerator, DeviceEvent};

let enumerator = get_device_enumerator()?;

// `on_event` runs on the OS notification thread (never the RT audio thread),
// so it may allocate and lock. Hold the returned guard alive for as long as you
// want events; dropping it unregisters the listener.
let _watcher = enumerator.watch(Box::new(|event: DeviceEvent| match event {
    DeviceEvent::DeviceAdded { name, .. } => println!("added: {name}"),
    DeviceEvent::DeviceRemoved { id } => println!("removed: {id:?}"),
    DeviceEvent::DefaultChanged { .. } => println!("default device changed"),
    _ => {}
}))?;
```

> **Platform divergence (by design):** on Windows and macOS the handler runs on a
> dedicated helper thread fed by a bounded channel (events drop if it overflows);
> on Linux it runs directly on the PipeWire loop thread. Backends that have not
> wired an OS listener return `AudioError::PlatformNotSupported`, matching their
> `PlatformCapabilities::supports_device_change_notifications` flag.

### Composing Multiple Sources (the `compose` feature)

Capture several sources **simultaneously and composed into one multi-channel
stream** ([ADR-0011](docs/designs/0011-compose-feature.md)). Sources are
declared in *groups*: a group either mixes its sources down to one (Mono) or
two (Stereo) channels — with per-source gain — or passes a single source's
native channels through (`keep_channels()`). Groups append in declaration
order; the `ChannelMap` tells you which output channel belongs to which group.

```toml
rsac = { git = "https://github.com/Codeseys-Labs/rust-crossplat-audio-capture", features = ["compose"] }
```

```rust
use rsac::compose::{CompositionBuilder, Group, GroupLayout};
use rsac::CaptureTarget;

let mut session = CompositionBuilder::new()
    .sample_rate(48_000) // session rate; mismatched sources are resampled
    .group(
        Group::new("voice") // → 1 composed channel
            .source(CaptureTarget::ApplicationByName("discord".into()))
            .source_with_gain(CaptureTarget::ApplicationByName("zoom".into()), 0.8)
            .mixdown(GroupLayout::Mono),
    )
    .group(
        Group::new("system") // → the endpoint's native channels
            .source(CaptureTarget::SystemDefault)
            .keep_channels(),
    )
    .build()?;

session.start()?;
let map = session.channel_map().unwrap(); // e.g. 3 channels: voice, sysL, sysR
loop {
    match session.read_chunk_nonblocking() {
        Ok(Some(buffer)) => { /* interleaved f32, map.channels() wide */ }
        Ok(None) => std::thread::sleep(std::time::Duration::from_millis(1)), // no data *yet*
        Err(e) if e.is_fatal() => break, // composition ended and drained
        Err(e) => eprintln!("transient read error: {e}"), // retryable
    }
}
session.stop()?;
```

What the compositor handles for you:

- **Heterogeneous sample rates** — e.g. Windows process loopback cannot
  autoconvert, so an app source may deliver 44.1 kHz while system capture
  delivers 48 kHz; mismatched sources are resampled (via `rubato`) on the
  dedicated compositor thread (never an OS callback thread).
- **Alignment** — a master-clock source (the first system/device source) paces
  the output; sources that run behind are silence-padded, sources drifting
  ahead are bounded-trimmed, and a wall-clock fallback keeps the session alive
  if the master stalls. Per-source `padded_frames` / `trimmed_frames` /
  `resampling` counters are exposed via `session.stats()`.
- **Delivery** — the composed stream speaks the same `CapturingStream`
  contract as a single capture: `drain_to(WavFileSink)` records a
  multi-channel WAV, and terminal semantics (ADR-0003) apply.

The composition **owns** its inner captures — don't read the same sources
through other handles while it runs. Mixdown is plain gain-weighted summation
(no auto-limiter); enable `.clamp_output(true)` if you need hard `[-1, 1]`
bounds. See `examples/composed_capture.rs` for a runnable demo.

## CLI Demo

The binary is a thin demo over the library API. Until the first crates.io
publish, run it from a checkout with the `cli` feature:

```bash
# Show platform capabilities
cargo run --features cli -- info

# List audio devices
cargo run --features cli -- list

# Capture system audio (live level meter)
cargo run --features cli -- capture

# Capture a specific app by name
cargo run --features cli -- capture --app firefox

# Record to WAV file (output path is a positional argument)
cargo run --features cli -- record recording.wav --duration 30
```

### Examples

Runnable programs live in [`examples/`](examples/):

- `basic_capture` — pull loop with a live level readout (`--features cli`)
- `record_to_file` — record to WAV with `WavFileSink`, owning the write loop (read → write → check) so a failed or shortened recording is reported as such; the WAV header comes from the first delivered buffer (`--features "cli sink-wav"`)
- `probe_activity` — a bounded activity-probe recipe: one capture session, a wall-clock window over the read loop, peak/RMS, exit code 0/1/2/3. A recipe over the public API, not a library API; see [docs/API.md](docs/API.md#activity-probe-recipe) for its limits (`--features cli`)
- `list_devices`, `async_capture` (`async-stream`), `composed_capture` (`compose`), `verify_audio` (`cli`)

## Capture Mode Support

**0.5.0** · 🟢 Supported and tested · 🟡 Supported, not runtime-verified for the stated scope · 🔴 Not supported.
For capture, **tested** means a run delivered audio on the named OS or test environment.
Compile checks, device enumeration, and skipped tests do not count as capture evidence.
The matching Cargo backend must be enabled. Call `PlatformCapabilities::query()` to
check the current build and host; capability is not permission.

### Desktop

| Target | Windows · WASAPI | Linux · PipeWire | macOS · CoreAudio |
|---|---|---|---|
| Default output (`SystemDefault`) | 🟢 CI · endpoint loopback | 🟢 CI · default sink monitor | 🟢 Attended macOS 26 · Process Tap |
| Application by PID (`Application`) | 🟢 CI · PID and descendants | 🟢 CI · matching stream node | 🟢 Attended macOS 26 · Process Tap |
| Application by name (`ApplicationByName`) | 🟢 CI · name → PID and descendants | 🟢 CI · matching stream node | 🟢 Attended macOS 26 · Process Tap |
| Process tree (`ProcessTree`) | 🟢 CI · PID and descendants | 🟢 CI · selects a node from the tree, not a mix of every descendant | 🟢 Attended macOS 26 · Process Tap |
| Selected device (`Device`) | 🟢 CI · render loopback | 🟢 CI · selected PipeWire node | 🟢 CI · input-capable device (BlackHole); 🔴 output-only devices |

- [0.5.0 desktop audio evidence](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/runs/45914624405): Windows and Linux exercise live system, device, and process targets. Hosted macOS verifies `Device` on macOS 15; its Process Tap tests skip.
- The [attended macOS 26 record](AGENTS.md#phase-5-progress) and [Process Tap results](docs/MACOS26_PROCESS_TAP_FIX.md#test-results) predate 0.5.0. Process Tap needs **System Audio Recording** permission. On macOS **14.4–15** it is 🟡 supported without live capture evidence; below 14.4 it is 🔴 unsupported. Input-device capture does not use Process Tap.
- Windows application and tree targets require WASAPI Process Loopback (build 20348+). Linux requires PipeWire. See [platform requirements](docs/system_requirements.md) for setup.

### Mobile (experimental)

| Target | Android · AAudio / AudioPlaybackCapture | iOS · AVAudioEngine / ReplayKit |
|---|---|---|
| Default microphone (`Device("default")`) | 🟢 Instrumented emulator · shipped C ABI | 🟢 Simulator · Rust API and app-hosted C ABI |
| System playback (`SystemDefault`) | 🟢 Emulator · test app's eligible playback | 🟡 ReplayKit broadcast · compile-checked, no frame evidence |
| Application (`Application`) | 🟢 Emulator · own UID; 🟡 other apps | 🔴 No supported per-app audio API |
| Application by name (`ApplicationByName`) | 🟢 Emulator · own package → UID; 🟡 other apps | 🔴 No supported per-app audio API |
| Process tree (`ProcessTree`) | 🟢 Emulator · own PID → UID; 🟡 other apps. UID filtering, not a descendant walk. | 🔴 No supported process-tree audio API |
| Select an input by device ID (`Device(id)`) | 🟡 AAR-backed selection; emulator enumerates IDs but does not capture by ID | 🔴 Input is routed by the audio session |

Android playback needs API 29+, MediaProjection consent, the foreground
service, and audio that the source app allows to be captured. iOS playback
needs an App Group, an embedded broadcast extension, and a user-started
broadcast. Green mobile cells apply only to the
[Android emulator](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/runs/45914627247)
or [iOS simulator](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/runs/45914630577)
run; **no mobile path is verified on a physical device**. No other OS has a capture backend.

### macOS Enumeration Scope

On **macOS 14.4+**, `list_audio_sources()` / `list_audio_applications()` return apps that are **actually producing audio**: the implementation intersects the `NSWorkspace.runningApplications` list (for the localized name + bundle id) with the set of PIDs CoreAudio reports as live audio processes via `kAudioHardwarePropertyProcessObjectList`, filtering out the large mass of GUI apps that aren't currently playing audio. On **macOS &lt; 14.4** (where the audio-process-object API and Process Taps are unavailable) it transparently falls back to the full, unfiltered `NSWorkspace` list. This matches Windows (WASAPI session enumeration) and Linux (PipeWire stream nodes via the native in-process registry listener, with `pw-dump` only as a fallback), which also report only endpoints with an active audio session — so on supported OS versions all three platforms surface a "currently producing audio" set.

`enumerate_devices()` is not a guarantee that each device can be captured: use an input-capable device for direct capture. Process Tap requires **System Audio Recording**, not Screen Recording; direct AUHAL input uses the Microphone permission. `check_audio_capture_permission()` returns `NotDetermined` by default on macOS; the optional `macos-tcc-spi` feature enables a best-effort preflight.

## Installation

Add to `Cargo.toml`:

```toml
[dependencies]
rsac = { git = "https://github.com/Codeseys-Labs/rust-crossplat-audio-capture" }
```

### Cargo Features

The default feature set enables the five backend flags (`feat_windows`,
`feat_linux`, `feat_macos`, `feat_android`, `feat_ios`); only the one matching
the target OS takes effect. The features below are **off by default**.
Test evidence is scoped: a unit test of a shared library feature does not
imply live capture on every backend.

| Optional feature | Support | Test evidence |
|---|---|---|
| `hotplug-recovery` | Windows and Linux, `SystemDefault` only | 🟢 [Windows workstation default switch](docs/evidence/windows-default-switch.md), Windows loss/return and Linux routing/outage in [audio CI](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/runs/45914624405). 🔴 macOS in 0.5.0, mobile, and non-default targets. |
| `compose` | Shared multi-source channel composition | 🟢 Live capture on Windows/Linux and desktop engine unit tests. 🟡 Live composition on macOS and mobile. |
| `async-stream` | Shared `futures_core::Stream` reader | 🟢 Async delivery and terminal behavior in [Linux feature-combination tests](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/runs/45914621283); not an OS capture test. |
| `sink-wav` | Shared `WavFileSink` adapter | 🟢 WAV unit tests and Linux live first-buffer format test in the audio matrix. |
| `cli` | Demo binary and examples | 🟢 `rsac list` exercised on Windows; Linux/macOS builds checked. No mobile CLI runtime verification. |
| `tracing` | Shared structured events and spans; defaults to `log` without the flag | 🟢 Subscriber installation unit test on Windows. |
| `macos-tcc-spi` | macOS-only, private SPI for permission preflight | 🟢 macOS CI smoke test calls preflight. 🟡 Grant/deny accuracy is not established by that smoke test. |
| `test-utils` | Shared synthetic capture and test helpers | 🟢 Linux feature-combination tests; no device needed. |

See [Cargo features and dependencies](docs/features.md) for the complete
feature reference. Period-derived ring sizing and hardware clock metadata
are [not shipped in 0.5.0](docs/DESKTOP_REFINEMENT.md#d4--ring-sizing-and-clocks).

### Platform Dependencies

**Linux** — PipeWire dev libraries:
```bash
# Debian/Ubuntu
sudo apt install libpipewire-0.3-dev libspa-0.2-dev pkg-config libclang-dev

# Fedora
sudo dnf install pipewire-devel pkg-config clang-devel

# Arch
sudo pacman -S pipewire pkgconf clang
```

**Windows** — Rust toolchain only (WASAPI is built-in).

**macOS** — Xcode Command Line Tools. Process Tap requires macOS 14.4+ and System Audio Recording permission; direct input uses the Microphone permission.

## Documentation

Full index (every doc, with status): [`docs/README.md`](docs/README.md).

- [`VISION.md`](VISION.md) — What rsac is, what it isn't, and how we verify the vision on every commit.
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — Three-layer architecture overview plus per-backend specifics (WASAPI / PipeWire / CoreAudio Process Tap).
- [`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md) — Toolchain pin, local gate (`fmt` + `clippy` + `doc`), test matrix, release procedure, PR checklist.
- [`docs/features.md`](docs/features.md) — Cargo feature matrix: which features are default, which platforms they enable, and what system packages each one needs.
- [`docs/troubleshooting.md`](docs/troubleshooting.md) — High-signal fixes for the most common build and runtime errors (PipeWire libs missing, Xcode CLT, TCC permission, WASAPI session contention, etc.).
- [`docs/CONSUMING_RSAC.md`](docs/CONSUMING_RSAC.md) — How downstream projects consume any rsac surface: Rust crate, C FFI, Python, Node/Bun, Go, CLI — with the working install/link recipe for each.
- [`docs/FRAMEWORK_COMPATIBILITY.md`](docs/FRAMEWORK_COMPATIBILITY.md) — Using rsac from Tauri v2, Dioxus, Electron, Deno, Bun, Flutter, and more — per-framework status (verified / expected / blocked) and integration recipes.
- [`docs/MOBILE_BACKEND_DESIGN.md`](docs/MOBILE_BACKEND_DESIGN.md) — Android (`AudioPlaybackCapture` + AAudio) and iOS (AVAudioEngine + ReplayKit) backend design (experimental/parked: emulator/simulator-verified, physical devices pending; [ADR-0012](docs/designs/0012-mobile-platform-strategy.md), [ADR-0013](docs/designs/0013-mobile-capturetarget-semantics.md), [`docs/DESKTOP_REFINEMENT.md`](docs/DESKTOP_REFINEMENT.md)).
- [`docs/architecture/`](docs/architecture/) — Detailed design documents for the core, bridge, and backend layers.
- [`docs/CI_AUDIO_TESTING.md`](docs/CI_AUDIO_TESTING.md) — How audio integration tests run in CI across all three platforms (7 of 9 cells capture real audio when the suite is dispatched; macOS gaps explained).
- [`docs/RELEASE_PROCESS.md`](docs/RELEASE_PROCESS.md) — End-to-end procedure for cutting a new `rsac` release: pre-release checks, version bump, tag, `cargo publish`, verification, and rollback.

## Architecture

```
core/ → bridge/ → audio/ (backends) → api/ → lib.rs
```

- **`core/`** — `AudioBuffer`, `CaptureTarget`, `AudioError`, `PlatformCapabilities`, traits
- **`bridge/`** — `BridgeStream<S>`, lock-free ring buffer, `AtomicStreamState`
- **`audio/`** — Platform backends (WASAPI, PipeWire, CoreAudio), each implementing `PlatformStream`
- **`api/`** — `AudioCaptureBuilder` → `AudioCapture` (public entry points)
- **`sink/`** — `AudioSink` trait + `NullSink`, `ChannelSink`, `WavFileSink`

## Applications Built on rsac

### AudioGraph

[AudioGraph](https://github.com/Codeseys-Labs/audio-graph) is a desktop app (Tauri v2) that captures live system audio, performs real-time speech recognition, speaker diarization, entity extraction, and builds a temporal knowledge graph. It is consumed as a standalone checkout, not a tracked submodule in this repository.

## Running Tests

```bash
# Unit tests (no audio hardware needed)
cargo test --lib --no-default-features --features feat_linux

# CI audio integration tests (requires PipeWire + virtual sink)
cargo test --test ci_audio --no-default-features --features feat_linux -- --test-threads=1

# Legacy Docker matrix is retired; use CI/audio docs for maintained paths.
```

## Contributing

See [`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md) for the toolchain pin,
local gate (`cargo fmt --all -- --check` + `cargo clippy --all-targets --all-features -- -D warnings` + `cargo doc --no-deps --all-features`),
test matrix, release procedure, and PR checklist. Architecture rules
and layering invariants live in [`AGENTS.md`](AGENTS.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE),
at your option.
