# AGENTS.md — AI Agent & Contributor Guide

> **Definitive reference for AI agents and human contributors working on `rsac`.**
> Read this file before touching any code. It describes the project's identity, architecture, conventions, current state, and workflow expectations.

---

## 1. Project Identity

| Field | Value |
|---|---|
| **Name** | `rsac` (Rust Cross-Platform Audio Capture) |
| **Type** | Rust library with sample CLI demo application |
| **Primary deliverable** | Library crate (`rsac`) |
| **Secondary deliverables** | CLI demo app (`rsac` binary) + example programs |
| **Platforms** | Windows (WASAPI), Linux (PipeWire), macOS (CoreAudio Process Tap) |
| **Priority order** | **Correctness → UX → Breadth** |

### Core Capability

**Streaming-first audio capture.** The library pipes audio to downstream consumers for in-flight computation (analysis, transformation, forwarding) — not just file writing. Supported capture targets include system audio, per-application audio, per-process audio, and process-tree audio.

---

## 2. Architecture Overview

The architecture is documented in detail across four canonical documents, plus a comprehensive reference analysis:

| Document | Purpose |
|---|---|
| [Architecture Overview](docs/architecture/ARCHITECTURE_OVERVIEW.md) | Master architecture overview |
| [API Design](docs/architecture/API_DESIGN.md) | Canonical public API surface |
| [Error & Capability Design](docs/architecture/ERROR_CAPABILITY_DESIGN.md) | Error taxonomy + platform capabilities |
| [Backend Contract](docs/architecture/BACKEND_CONTRACT.md) | Internal backend traits + module architecture |
| [Reference Analysis](reference/REFERENCE_ANALYSIS.md) | Analysis of 10 reference repos mapped to rsac's architecture |

### Key Architecture Points (Implemented)

- **Canonical API**: Builder/trait API flow — fully implemented:
  ```
  AudioCaptureBuilder → AudioCapture → CapturingStream
  ```
- **Streaming-first**: The core abstraction is `CapturingStream::read_chunk() → AudioBuffer`. File writing is implemented as a sink adapter ([`src/sink/`](src/sink/mod.rs)), not a primary concern.
- **Ring buffer bridge**: OS audio callbacks push data into an `rtrb` SPSC lock-free ring buffer; consumer threads pull data out via `BridgeStream`. Implemented in [`src/bridge/`](src/bridge/mod.rs).
- **[`BridgeStream<S>`](src/bridge/stream.rs)**: Universal `CapturingStream` implementation used by **all three backends** (WASAPI, PipeWire, CoreAudio). Eliminates per-platform duplication of the ring-buffer-to-consumer pattern.
- **[`PlatformStream`](src/bridge/stream.rs:47) trait**: Internal backend contract. Each platform implements `stop_capture()` and `is_active()`. The rest (ring buffer, state, reads) is handled by `BridgeStream`.
- **[`AtomicStreamState`](src/bridge/state.rs)**: Lock-free state machine for stream lifecycle: `Created → Running → Stopping → Stopped → Closed`.
- **[`CaptureTarget`](src/core/config.rs) enum**: Unified target model covering all capture modes:
  ```rust
  enum CaptureTarget {
      SystemDefault,
      Device(DeviceId),
      Application(ApplicationId),
      ApplicationByName(String),
      ProcessTree(ProcessId),
  }
  ```
- **Error model**: 21 categorized error variants with three-state recoverability (`Recoverable`, `TransientRetry`, `Fatal`). See [`src/core/error.rs`](src/core/error.rs).
- **Platform capabilities**: [`PlatformCapabilities`](src/core/capabilities.rs) struct for honest reporting of what each backend supports — never pretend a platform can do something it cannot.
- **Sink adapters**: [`AudioSink`](src/sink/traits.rs) trait with three implementations:
  - [`NullSink`](src/sink/null.rs) — discards data (testing/benchmarking)
  - [`ChannelSink`](src/sink/channel.rs) — sends buffers over `mpsc` channel
  - [`WavFileSink`](src/sink/wav.rs) — writes to WAV files (behind `sink-wav` feature)
- **Module layering** (strict DAG — no reverse dependencies):
  ```
  core/ → bridge/ → audio/ (backends) → api/ → lib.rs
  ```

---

## 3. Current State

The architectural transformation is **complete**. Phases 0–4 are done. The old API has been fully removed and the new builder/trait API is the only API.

### What was removed (Phase 0 — Done ✅)

The following legacy types were deleted and no longer exist in the codebase:

- `AudioCaptureBackend` trait
- `AudioCaptureStream` trait
- `get_audio_backend()` function
- `PipeWireBackend`, `WasapiBackend`, `CoreAudioBackend` structs
- `AudioApplication`, `AudioStream`, `SampleType`, `StreamDataCallback`
- Duplicate `ProcessError`
- [`src/audio/core.rs`] — entire file removed
- 14 old types removed from [`src/lib.rs`](src/lib.rs) exports

### Platform backend maturity (Phase 3 — Done ✅)

All three backends are wired through `BridgeStream<S>`:

| Platform | Backend | Thread module | Status |
|---|---|---|---|
| **Windows** | WASAPI | [`src/audio/windows/thread.rs`](src/audio/windows/thread.rs) | ✅ Wired via `WindowsPlatformStream` |
| **Linux** | PipeWire | [`src/audio/linux/thread.rs`](src/audio/linux/thread.rs) | ✅ Wired via `LinuxPlatformStream` |
| **macOS** | CoreAudio | [`src/audio/macos/thread.rs`](src/audio/macos/thread.rs) | ✅ Wired via `MacosPlatformStream` |

### Demo apps (Phase 4 — Done ✅)

- [`src/main.rs`](src/main.rs) is a cross-platform CLI demo with `info`, `list`, `capture`, `record` subcommands — uses only the public library API, no `#[cfg(target_os)]`
- Three new examples: [`basic_capture.rs`](examples/basic_capture.rs), [`record_to_file.rs`](examples/record_to_file.rs), [`list_devices.rs`](examples/list_devices.rs)
- Four old-API binaries disabled in `Cargo.toml` (commented out): `firefox_capture_test`, `real_pipewire_test`, `dynamic_vlc_capture`, `audio_recorder_tui`

### Known remaining gaps

- [`src/audio/discovery.rs`](src/audio/discovery.rs) — still contains mostly mock data; needs a full rewrite against real platform APIs.
- [`src/audio/application_capture.rs`](src/audio/application_capture.rs) — deprecated standalone trait; superseded by `CaptureTarget` + builder but file still exists.
- Several old binaries in `src/bin/` still reference deprecated patterns (commented out in Cargo.toml but source files remain).

---

## 4. Source Code Layout

```
src/
├── lib.rs                  # Public API exports
├── api.rs                  # AudioCaptureBuilder, AudioCapture
├── main.rs                 # CLI demo (info, list, capture, record subcommands)
├── core/                   # Core types, traits, errors
│   ├── mod.rs
│   ├── buffer.rs           # AudioBuffer (18+ methods)
│   ├── capabilities.rs     # PlatformCapabilities
│   ├── config.rs           # CaptureTarget, StreamConfig, AudioFormat, SampleFormat,
│   │                       #   DeviceId, ApplicationId, ProcessId newtypes
│   ├── error.rs            # AudioError (21 variants), ErrorKind, Recoverability,
│   │                       #   BackendContext
│   ├── interface.rs        # CapturingStream, AudioDevice, DeviceEnumerator traits
│   └── processing.rs       # Audio processing traits
├── bridge/                 # Ring buffer bridge (data plane)
│   ├── mod.rs              # Re-exports + integration tests
│   ├── state.rs            # AtomicStreamState, StreamState enum
│   ├── ring_buffer.rs      # BridgeProducer, BridgeConsumer, BridgeShared, create_bridge()
│   └── stream.rs           # BridgeStream<S>, PlatformStream trait
├── sink/                   # Sink adapters for audio data
│   ├── mod.rs              # Re-exports
│   ├── traits.rs           # AudioSink trait
│   ├── null.rs             # NullSink (discard)
│   ├── channel.rs          # ChannelSink (mpsc)
│   └── wav.rs              # WavFileSink (behind sink-wav feature)
├── audio/                  # Platform backends
│   ├── mod.rs              # Cross-platform dispatch
│   ├── application_capture.rs  # (deprecated — superseded by CaptureTarget)
│   ├── capture.rs          # Capture helpers
│   ├── discovery.rs        # App discovery (mostly mock — needs rewrite)
│   ├── windows/            # WASAPI backend
│   │   ├── mod.rs
│   │   ├── wasapi.rs       # WASAPI capture implementation
│   │   └── thread.rs       # WindowsPlatformStream + WASAPI capture thread
│   ├── linux/              # PipeWire backend
│   │   ├── mod.rs
│   │   ├── pipewire.rs     # PipeWire capture implementation
│   │   └── thread.rs       # LinuxPlatformStream + PipeWire dedicated thread
│   └── macos/              # CoreAudio + Process Tap backend
│       ├── mod.rs
│       ├── coreaudio.rs    # CoreAudio capture (uses BridgeProducer, no old VecDeque)
│       ├── tap.rs          # Process Tap FFI
│       └── thread.rs       # MacosPlatformStream + CoreAudio callback → BridgeProducer
├── bin/                    # Binary targets (some deprecated)
│   ├── standardized_test.rs
│   ├── run_tests.rs
│   ├── test_report_generator.rs
│   ├── app_capture_test.rs
│   ├── pipewire_test.rs
│   ├── pipewire_diagnostics.rs
│   └── (deprecated: firefox_capture_test, real_pipewire_test, etc.)
└── utils/                  # Utility modules
    ├── mod.rs
    └── test_utils.rs
```

### Supporting directories

```
docs/architecture/          # Canonical architecture documents (source of truth)
examples/                   # Example programs (basic_capture, record_to_file, list_devices, etc.)
tests/                      # Integration tests
reference/                  # Reference repos + analysis (REFERENCE_ANALYSIS.md)
scripts/                    # Build/test/CI helper scripts
docker/                     # Docker-based cross-platform testing
.github/workflows/          # CI workflows
```

---

## 5. Key Conventions

### Language & Tooling

- **Rust edition 2021**
- Platform-specific code gated behind `#[cfg(target_os = "...")]` and Cargo features:
  - `feat_windows` — Windows/WASAPI backend
  - `feat_linux` — Linux/PipeWire backend
  - `feat_macos` — macOS/CoreAudio backend
  - `async-stream` — Async `Stream` support (adds `atomic-waker`)
  - `sink-wav` — `WavFileSink` adapter
  - `test-utils` — Test utility exports

### Data & Types

- All audio data standardized to **`f32`** internally
- [`SampleFormat`](src/core/config.rs) enum: `I16`, `I24`, `I32`, `F32`
- [`AudioFormat`](src/core/config.rs) struct: `sample_rate`, `channels`, `sample_format`
- Error type: [`AudioError`](src/core/error.rs) (21 categorized variants)
- Result type: `AudioResult<T> = Result<T, AudioError>`

### Patterns

- **Builder pattern** for capture configuration ([`AudioCaptureBuilder`](src/api.rs))
- **Interior mutability** (`Mutex`, `Arc`) inside `AudioCapture` for `&self` methods
- **Lock-free ring buffers** (`rtrb`) for bridging OS callback threads to consumer threads
- **[`PlatformStream`](src/bridge/stream.rs:47) trait** — internal contract for platform-specific stop/active-check; wrapped by `BridgeStream<S>`
- **[`BridgeStream<S>`](src/bridge/stream.rs:83)** — universal `CapturingStream` implementation; all backends use this
- **[`AtomicStreamState`](src/bridge/state.rs)** — lock-free state machine for lifecycle: `Created → Running → Stopping → Stopped → Closed`
- **Sink adapters** — [`AudioSink`](src/sink/traits.rs) trait decouples data consumption from the capture pipeline

### Naming

- Public API types live in [`src/api.rs`](src/api.rs) and [`src/core/`](src/core/mod.rs)
- Bridge types live in [`src/bridge/`](src/bridge/mod.rs)
- Sink adapters live in [`src/sink/`](src/sink/mod.rs)
- Platform backends live in `src/audio/{platform}/`
- Binary targets live in `src/bin/`

---

## 6. Development Workflow

### Quick validation

```bash
# Fast compilation check (Linux is primary dev environment)
cargo check

# Run tests
cargo test

# Check a specific platform feature
cargo check --features feat_linux

# Run library tests only
cargo test --lib
```

### CI expectations

- **Linux + Windows** are primary CI platforms
- **macOS** is treated as incomplete (expected failures in some areas)
- Docker-based testing available for cross-platform validation (see `docker/`)

### Architecture alignment

All implementation decisions must align with the canonical documents in `docs/architecture/`. If you believe a design doc needs updating, propose the change explicitly — do not silently diverge.

### Task management

This project uses [Task Master](https://github.com/task-master-ai/task-master-ai) for task-driven development. See [`.roo/rules/taskmaster.md`](.roo/rules/taskmaster.md) and [`.roo/rules/dev_workflow.md`](.roo/rules/dev_workflow.md) for details.

---

## 7. What NOT to Do

| ❌ Don't | Why |
|---|---|
| Add backend-specific logic to demo apps | Demo apps must go through the library API only |
| Reference any old API types (`AudioCaptureBackend`, `get_audio_backend()`, etc.) | They have been deleted — they no longer exist |
| Use the standalone `ApplicationCapture` trait for new code | Use `CaptureTarget` with the builder instead |
| Make `CapturingStream` depend on file I/O | File writing is a sink adapter, not a core concern |
| Pretend a platform supports a feature it doesn't | Use explicit capability errors via `PlatformCapabilities` |
| Hold locks in real-time audio callback threads | Use lock-free ring buffers (`rtrb`) via `BridgeProducer` |
| Add new `AudioError` variants without categorizing them | Every variant must have an `ErrorKind` and recoverability classification |
| Bypass `BridgeStream<S>` for new backends | All backends must use `BridgeStream` + `PlatformStream` trait |
| Silently diverge from architecture docs | Propose changes explicitly if the design needs updating |
| Import from `src/audio/core.rs` | File was deleted in Phase 0 |

---

## 8. Implementation Phases (Roadmap)

| Phase | Focus | Status |
|---|---|---|
| **Phase 0** | Repo alignment & legacy API removal | ✅ Done |
| **Phase 1** | Core API contract freeze (new types, traits, errors) | ✅ Done |
| **Phase 2** | Streaming/data-plane & sink adapters (`BridgeStream`, ring buffer, sinks) | ✅ Done |
| **Phase 3** | Platform backends — all 3 wired through `BridgeStream` | ✅ Done |
| **Phase 4** | Rebuild demo CLI as thin library consumer + examples | ✅ Done |
| **Phase 5** | Breadth expansion (more formats, richer async, advanced features) | Future |

### Phase 5 potential work

- Async stream support (behind `async-stream` feature, foundation in place via `atomic-waker`)
- Richer device enumeration (replace mock data in `discovery.rs`)
- Additional sink adapters
- Advanced capture modes per platform
- Performance benchmarking and optimization

---

## 9. Key Dependencies

| Crate | Purpose |
|---|---|
| `rtrb` | Lock-free SPSC ring buffer for audio data bridge |
| `hound` | WAV file writing (for `WavFileSink` and CLI `record` command) |
| `clap` | CLI argument parsing (with derive) |
| `color-eyre` | Error reporting for CLI |
| `thiserror` | Error derive macros for `AudioError` |
| `log` | Logging facade |
| `futures-core` | Async `Stream` trait (optional, behind `async-stream` feature) |
| `atomic-waker` | Async notification from ring buffer (optional, behind `async-stream` feature) |

### Platform-specific

| Platform | Dependencies |
|---|---|
| **Windows** | `wasapi`, `windows`, `windows-core`, `widestring`, `sysinfo` |
| **Linux** | `pipewire`, `libspa`, `libspa-sys` |
| **macOS** | `coreaudio-rs`, `coreaudio-sys`, `objc2-core-audio`, `objc2-core-audio-types`, `objc2-core-foundation`, `core-foundation`, `core-foundation-sys`, `cocoa`, `objc` |

---

## 10. Quick Reference: Core Types

```rust
// Builder → configured capture → active stream
let mut capture = AudioCaptureBuilder::new()
    .with_target(CaptureTarget::SystemDefault)
    .sample_rate(48000)
    .channels(2)
    .build()?;                   // → AudioCapture

capture.start()?;

// Reading audio (streaming-first)
let buffer: AudioBuffer = capture.read_buffer()?.unwrap();
let data: &[f32] = buffer.data();
let frames: usize = buffer.num_frames();

// Stop capture
capture.stop()?;

// Error handling
match result {
    Err(AudioError::DeviceNotFound { .. }) => { /* ... */ }
    Err(e) if e.is_recoverable() => { /* retry logic */ }
    Err(e) => { /* fatal, bail */ }
    Ok(v) => { /* use v */ }
}

// Platform capability check
let caps = PlatformCapabilities::query();
if caps.supports_application_capture {
    // safe to use CaptureTarget::Application(..)
}

// Sink adapters
use rsac::{NullSink, ChannelSink};
use rsac::sink::AudioSink;

let mut sink = NullSink::new();
sink.write(&buffer)?;

let (mut tx, rx) = ChannelSink::new();
tx.write(&buffer)?;
let received = rx.try_recv()?;
```

### Internal Types (for backend implementors)

```rust
// Bridge: producer side (OS callback thread)
let (mut producer, consumer) = create_bridge(capacity, format);
producer.push(audio_buffer)?;       // or push_or_drop for non-blocking
producer.signal_done();              // when capture ends

// Bridge: consumer side (wrapped by BridgeStream)
let stream = BridgeStream::new(consumer, platform_stream, format, timeout);
let chunk = stream.read_chunk()?;    // blocking
let chunk = stream.try_read_chunk()?; // non-blocking

// PlatformStream trait (implement per backend)
impl PlatformStream for MyPlatformStream {
    fn stop_capture(&self) -> AudioResult<()> { /* ... */ }
    fn is_active(&self) -> bool { /* ... */ }
}
```

---

## 11. For AI Agents Specifically

1. **The architecture is implemented.** Phases 0–4 are complete. The four documents in `docs/architecture/` are the source of truth, and the code now *matches* them. Do not treat the codebase as "in transition" — the new API is the only API.
2. **The old API is gone.** Do not reference `AudioCaptureBackend`, `AudioCaptureStream`, `get_audio_backend()`, `src/audio/core.rs`, or any of the 14 removed types. They do not exist.
3. **All backends use `BridgeStream<S>`.** If adding a new backend, implement the `PlatformStream` trait and wrap with `BridgeStream`. Do not create a custom `CapturingStream` implementation.
4. **Scope changes tightly.** Prefer small, focused changes that move one thing forward over sweeping refactors.
5. **Report back clearly.** When completing a task, summarize what changed, what was discovered, and what remains.
6. **Respect the module DAG.** `core/` knows nothing about `bridge/`. `bridge/` knows nothing about `audio/`. `audio/` knows nothing about `api/`. Violations break the architecture.
7. **Test on the target platform.** If you're implementing a Windows backend change, validate with `cargo check --features feat_windows` at minimum.
8. **Phase 5 is the frontier.** New work should focus on breadth expansion: async streams, better device enumeration, additional sinks, performance optimization.
9. **When in doubt, ask.** If a design decision isn't covered by the architecture docs, surface it rather than guessing.
