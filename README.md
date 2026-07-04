# rsac — Rust Cross-Platform Audio Capture

A streaming-first audio capture library for Rust. Captures system audio, per-application audio, and process-tree audio on Windows (WASAPI), Linux (PipeWire), and macOS (CoreAudio Process Tap).

## Why rsac?

`cpal` and `portaudio-rs` expose device-level primitives — open an input, read f32 samples — but cannot capture a single application's audio on any platform without virtual-cable workarounds. rsac wraps WASAPI Process Loopback, CoreAudio Process Tap, and PipeWire node monitors behind one unified `AudioCaptureBuilder → AudioCapture` API, so per-app and per-process-tree capture work the same way on Windows, macOS, and Linux.

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

See [VISION.md](VISION.md) for the full in-scope / out-of-scope list.

## CI Status

[![CI](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/workflows/ci.yml)
[![Audio Tests](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/workflows/ci-audio-tests.yml/badge.svg?branch=master)](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/workflows/ci-audio-tests.yml)

The `ci.yml` badge covers lint + unit tests on **all three platforms** (one
workflow, per-OS jobs — Linux/Windows/macOS statuses live in the
[Actions tab](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/workflows/ci.yml)).

### Audio Integration Tests

| | System | Device | Process |
|---|---|---|---|
| **Linux** (PipeWire) | `linux-system` | `linux-device` | `linux-process` |
| **Windows** (VB-CABLE) | `windows-system` | `windows-device` | `windows-process` |
| **macOS** (BlackHole) | `macos-system` | `macos-device` | `macos-process` |

Each cell is a separate CI job visible in the [Actions tab](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/workflows/ci-audio-tests.yml). Linux is the primary platform; Windows and macOS process capture use `continue-on-error`.

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
- **Sink adapters** — `NullSink`, `ChannelSink`, `WavFileSink` (note: the `pipe_to()` driver is not yet implemented — drive sinks from your own read/subscribe loop, or use `drain_to()`)
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
let default = enumerator.get_default_device()?;
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
rsac = { version = "0.4", features = ["compose"] }
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
    match session.read_buffer() {
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

The binary is a thin demo over the library API (requires the `cli` feature —
`cargo install rsac --features cli`, or `cargo run --features cli -- <cmd>`
from a checkout):

```bash
# Show platform capabilities
rsac info

# List audio devices
rsac list

# Capture system audio (live level meter)
rsac capture

# Capture a specific app by name
rsac capture --app firefox

# Record to WAV file (output path is a positional argument)
rsac record recording.wav --duration 30
```

## Capture Mode Support

| Mode | Windows (WASAPI) | Linux (PipeWire) | macOS (CoreAudio) |
|---|---|---|---|
| System default | Yes | Yes | Yes |
| Application (PID) | Process loopback | pw-dump node | Process Tap (14.4+) |
| ApplicationByName | sysinfo PID resolve | pw-dump node serial | Process Tap (14.4+) |
| Process tree | Process loopback | PID node mapping | Process Tap (14.4+) |
| Device selection | Yes | Yes | Yes |

### macOS Enumeration Scope

On **macOS 14.4+**, `list_audio_sources()` / `list_audio_applications()` return apps that are **actually producing audio**: the implementation intersects the `NSWorkspace.runningApplications` list (for the localized name + bundle id) with the set of PIDs CoreAudio reports as live audio processes via `kAudioHardwarePropertyProcessObjectList`, filtering out the large mass of GUI apps that aren't currently playing audio. On **macOS &lt; 14.4** (where the audio-process-object API and Process Taps are unavailable) it transparently falls back to the full, unfiltered `NSWorkspace` list. This matches Windows (WASAPI session enumeration) and Linux (PipeWire stream nodes via the native in-process registry listener, with `pw-dump` only as a fallback), which also report only endpoints with an active audio session — so on supported OS versions all three platforms surface a "currently producing audio" set.

Device enumeration on macOS (`enumerate_devices()`) lists all CoreAudio output devices the process can see, which is comparable to the other platforms. What is *not* enumerable from rsac on macOS: the live per-process audio signal graph (which PIDs are routing to which device at this instant) — that information is not exposed outside Core Audio, and Process Tap attachment is the only way to observe per-app audio. Screen Recording permission (TCC) is required at capture time; `check_audio_capture_permission()` returns `NotDetermined` until the OS prompt has been answered, because macOS does not expose a reliable pre-flight query on supported versions.

## Installation

Add to `Cargo.toml`:

```toml
[dependencies]
rsac = { git = "https://github.com/Codeseys-Labs/rust-crossplat-audio-capture" }
```

### Cargo Features

| Feature | Default | Adds |
|---|---|---|
| `feat_windows` / `feat_linux` / `feat_macos` | ✅ | Platform backends |
| `compose` | — | Multi-source channel composition (`rubato` + `audioadapter-buffers`) |
| `cli` | — | The `rsac` demo binary's deps (`clap`, `color-eyre`, `ctrlc`, `env_logger`) — library consumers don't pay for them |
| `async-stream` | — | `futures_core::Stream` support (`atomic-waker`) |
| `sink-wav` | — | `WavFileSink` |
| `tracing` | — | Structured `tracing` events/spans (facade only) |

See [`docs/features.md`](docs/features.md) for the full matrix.

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

**macOS** — Xcode Command Line Tools. Screen Recording permission required. Process Tap requires macOS 14.4+.

## Documentation

- [`VISION.md`](VISION.md) — What rsac is, what it isn't, and how we verify the vision on every commit.
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — Three-layer architecture overview plus per-backend specifics (WASAPI / PipeWire / CoreAudio Process Tap).
- [`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md) — Toolchain pin, local gate (`fmt` + `clippy` + `doc`), test matrix, release procedure, PR checklist.
- [`docs/features.md`](docs/features.md) — Cargo feature matrix: which features are default, which platforms they enable, and what system packages each one needs.
- [`docs/troubleshooting.md`](docs/troubleshooting.md) — High-signal fixes for the most common build and runtime errors (PipeWire libs missing, Xcode CLT, TCC permission, WASAPI session contention, etc.).
- [`docs/architecture/`](docs/architecture/) — Detailed design documents for the core, bridge, and backend layers.
- [`docs/CI_AUDIO_TESTING.md`](docs/CI_AUDIO_TESTING.md) — How audio integration tests run in CI across all three platforms (6 of 9 cells REAL on every run; macOS gaps explained).
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

[AudioGraph](https://github.com/Codeseys-Labs/audio-graph) is a desktop app (Tauri v2) that captures live system audio, performs real-time speech recognition, speaker diarization, entity extraction, and builds a temporal knowledge graph. Included as a [git submodule](apps/audio-graph/).

## Running Tests

```bash
# Unit tests (no audio hardware needed)
cargo test --lib --no-default-features --features feat_linux

# CI audio integration tests (requires PipeWire + virtual sink)
cargo test --test ci_audio --no-default-features --features feat_linux -- --test-threads=1

# Docker-based testing
cd docker/linux && docker-compose run pipewire-test
```

## Contributing

See [`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md) for the toolchain pin,
local gate (`cargo fmt --all -- --check` + `cargo clippy --all-targets --all-features -- -D warnings` + `cargo doc --no-deps --all-features`),
test matrix, release procedure, and PR checklist. Architecture rules
and layering invariants live in [`AGENTS.md`](AGENTS.md).

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE),
at your option.
