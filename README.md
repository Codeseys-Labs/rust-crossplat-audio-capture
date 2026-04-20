# rsac — Rust Cross-Platform Audio Capture

A streaming-first audio capture library for Rust. Captures system audio, per-application audio, and process-tree audio on Windows (WASAPI), Linux (PipeWire), and macOS (CoreAudio Process Tap).

## CI Status

### Unit Tests

| Platform | Status |
|----------|--------|
| Linux | ![Linux](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/workflows/ci.yml/badge.svg?branch=master) |
| Windows | ![Windows](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/workflows/ci.yml/badge.svg?branch=master) |
| macOS | ![macOS](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/workflows/ci.yml/badge.svg?branch=master) |

### Audio Integration Tests

![Audio Tests](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/actions/workflows/ci-audio-tests.yml/badge.svg?branch=master)

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
- **Lock-free ring buffers** (`rtrb` SPSC) bridging OS callback threads to consumer threads
- **Push-based subscription** (`subscribe()` returns `mpsc::Receiver<AudioBuffer>`)
- **Overflow monitoring** (`overrun_count()` tracks dropped buffers)
- **Backpressure signaling** (`is_under_backpressure()` on the `CapturingStream` trait — returns `true` when sustained consecutive frame drops indicate the consumer cannot keep up; use to throttle, warn, or switch providers)
- **Sink adapters** — `NullSink`, `ChannelSink`, `WavFileSink`
- **Platform capability reporting** — `PlatformCapabilities::query()` for honest feature detection

## Quick Start

```rust
use rsac::{AudioCaptureBuilder, CaptureTarget};

let mut capture = AudioCaptureBuilder::new()
    .with_target(CaptureTarget::SystemDefault)
    .sample_rate(48000)
    .channels(2)
    .build()?;

capture.start()?;

// Streaming-first: read audio chunks in a loop
loop {
    if let Some(buffer) = capture.read_buffer()? {
        let samples: &[f32] = buffer.data();
        let frames = buffer.num_frames();
        // process audio...
    }
}

capture.stop()?;
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

## CLI Demo

The binary is a thin demo over the library API:

```bash
# Show platform capabilities
rsac info

# List audio devices
rsac list

# Capture system audio (live level meter)
rsac capture

# Capture a specific app by name
rsac capture --app firefox

# Record to WAV file
rsac record --duration 30 --output recording.wav
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

On macOS, enumeration returns a superset of what is actually capturable — the audio graph is opaque until a Process Tap is installed. `list_audio_sources()` / `list_audio_applications()` use `NSWorkspace.runningApplications`, which reports every running app with a GUI activation policy, *not* only apps currently producing audio. Callers cannot distinguish "silent" from "playing" before attempting capture; most apps in the returned list will have no audio output at the moment of enumeration. By contrast, Windows (WASAPI session enumeration) and Linux (PipeWire stream nodes via `pw-dump`) report only endpoints with an active audio session, so those lists are closer to a true "currently producing audio" set.

Device enumeration on macOS (`enumerate_devices()`) lists all CoreAudio output devices the process can see, which is comparable to the other platforms. What is *not* enumerable from rsac on macOS: the live per-process audio signal graph (which PIDs are routing to which device at this instant) — that information is not exposed outside Core Audio, and Process Tap attachment is the only way to observe per-app audio. Screen Recording permission (TCC) is required at capture time; `check_audio_capture_permission()` returns `NotDetermined` until the OS prompt has been answered, because macOS does not expose a reliable pre-flight query on supported versions.

## Installation

Add to `Cargo.toml`:

```toml
[dependencies]
rsac = { git = "https://github.com/Codeseys-Labs/rust-crossplat-audio-capture" }
```

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

- [`docs/features.md`](docs/features.md) — Cargo feature matrix: which features are default, which platforms they enable, and what system packages each one needs.
- [`docs/troubleshooting.md`](docs/troubleshooting.md) — High-signal fixes for the most common build and runtime errors (PipeWire libs missing, Xcode CLT, TCC permission, WASAPI session contention, etc.).
- [`docs/architecture/`](docs/architecture/) — Full design documents for the core, bridge, and backend layers.
- [`docs/CI_AUDIO_TESTING.md`](docs/CI_AUDIO_TESTING.md) — How audio integration tests run in CI across all three platforms.

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

1. Fork & create a feature branch
2. Read [`AGENTS.md`](AGENTS.md) for architecture rules and conventions
3. Run `cargo fmt --all && cargo clippy` before submitting
4. CI runs lint, unit tests (3 platforms), and audio integration tests

## License

MIT — see [LICENSE](LICENSE) for details.
