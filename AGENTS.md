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
| **GitHub org** | [Codeseys-Labs](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture) |
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
| [Local Testing Guide](docs/LOCAL_TESTING_GUIDE.md) | How to test on physical macOS, Windows, and Linux machines |
| [macOS Version Compatibility](docs/MACOS_VERSION_COMPATIBILITY.md) | macOS API compatibility matrix, version-specific fallbacks, known issues |

### Key Architecture Points (Implemented)

- **Canonical API**: Builder/trait API flow — fully implemented:
  ```
  AudioCaptureBuilder → AudioCapture → CapturingStream
  ```
- **Streaming-first**: The core abstraction is `CapturingStream::read_chunk() → AudioBuffer`. File writing is implemented as a sink adapter ([`src/sink/`](src/sink/mod.rs)), not a primary concern.
- **Ring buffer bridge**: OS audio callbacks push data into an `rtrb` SPSC lock-free ring buffer; consumer threads pull data out via `BridgeStream`. The [`push_samples_or_drop()`](src/bridge/ring_buffer.rs:140) method provides zero-allocation pushes on real-time callback threads via an internal scratch buffer. Implemented in [`src/bridge/`](src/bridge/mod.rs).
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
- **Platform capabilities**: [`PlatformCapabilities`](src/core/capabilities.rs) struct for honest reporting of what each backend supports — never pretend a platform can do something it cannot. On macOS, capabilities are determined at runtime using [`get_macos_version()`](src/core/capabilities.rs:175) (sysctl-based, no subprocess) to detect Process Tap availability (requires macOS 14.4+).
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

The architectural transformation is **complete**. Phases 0–4 are done. The old API has been fully removed and the new builder/trait API is the only API. **All 10 gap closures (G1–G10) are done** — every capture level (system, application, process tree) is implemented on all three platforms. **All 3 platforms are verified on real hardware:** Windows ✅, macOS ✅ (macOS 26 Tahoe), Linux ✅ (CI).

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
| **macOS** | CoreAudio | [`src/audio/macos/thread.rs`](src/audio/macos/thread.rs) | ✅ Wired via `MacosPlatformStream` — tested on macOS 26 Tahoe (289 unit tests + 12 integration tests) |

### Demo apps (Phase 4 — Done ✅)

- [`src/main.rs`](src/main.rs) is a cross-platform CLI demo with `info`, `list`, `capture`, `record` subcommands — uses only the public library API, no `#[cfg(target_os)]`
- Three new examples: [`basic_capture.rs`](examples/basic_capture.rs), [`record_to_file.rs`](examples/record_to_file.rs), [`list_devices.rs`](examples/list_devices.rs)
- Four old-API binaries disabled in `Cargo.toml` (commented out): `firefox_capture_test`, `real_pipewire_test`, `dynamic_vlc_capture`, `audio_recorder_tui`

### Gap closures (G1–G10 — All Done ✅)

All ten identified gaps have been closed:

| Gap | Description | Status |
|---|---|---|
| **G1** | Windows WASAPI application capture (`ApplicationByName` via `sysinfo` PID resolution) | ✅ Done |
| **G2** | Windows WASAPI process tree capture (`ProcessTree` via process loopback) | ✅ Done |
| **G3** | Linux PipeWire application capture (`ApplicationByName` via `pw-dump` node resolution) | ✅ Done |
| **G4** | Linux PipeWire process tree capture (`ProcessTree` via PID → PipeWire node mapping) | ✅ Done |
| **G5** | macOS CoreAudio application capture (`ApplicationByName` via Process Tap) | ✅ Done |
| **G6** | macOS CoreAudio process tree capture (`ProcessTree` via Process Tap) | ✅ Done |
| **G7** | `subscribe()` method on `AudioCapture` — push-based channel delivery | ✅ Done |
| **G8** | `overrun_count()` on `AudioCapture` and `CapturingStream` — ring buffer overflow monitoring | ✅ Done |
| **G9** | Full `PlatformCapabilities` reporting with `supports_process_tree_capture` field | ✅ Done |
| **G10** | Device enumeration via real platform APIs (PipeWire, WASAPI, CoreAudio) | ✅ Done |

### Capture mode support matrix

| Capture Mode | Windows (WASAPI) | Linux (PipeWire) | macOS (CoreAudio) |
|---|---|---|---|
| **System default** | ✅ | ✅ | ✅ |
| **Application (by PID)** | ✅ process loopback | ✅ pw-dump node | ✅ Process Tap |
| **ApplicationByName** | ✅ sysinfo → PID | ✅ pw-dump → node serial | ✅ Process Tap |
| **ProcessTree** | ✅ process loopback | ✅ PID → PipeWire node | ✅ Process Tap |
| **Device selection** | ✅ | ✅ | ✅ |

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
│   ├── capture.rs          # Capture helpers
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

### Local testing on physical machines

See the [Local Testing Guide](docs/LOCAL_TESTING_GUIDE.md) for comprehensive instructions
on testing system capture, application capture, and process tree capture on macOS, Windows,
and Linux.

### CI infrastructure: Blacksmith runners

All CI runs on [Blacksmith](https://blacksmith.sh/) runners — a drop-in replacement for GitHub-hosted runners with 2x faster hardware and co-located caching. Workflows are in [`.github/workflows/`](.github/workflows/).

| Workflow | Purpose |
|---|---|
| [`ci.yml`](.github/workflows/ci.yml) | Lint, unit tests (3 platforms), ARM64 cross-compile |
| [`ci-audio-tests.yml`](.github/workflows/ci-audio-tests.yml) | Audio integration tests (9 platform x tier jobs) |
| [`blacksmith-audio-probe.yml`](.github/workflows/blacksmith-audio-probe.yml) | One-shot diagnostic: probe audio device availability on Blacksmith runners (workflow_dispatch only) |

**Runner labels:**

| Platform | Runner Label | Specs |
|---|---|---|
| **Linux** | `blacksmith-4vcpu-ubuntu-2404` | 4 vCPU, 16 GB RAM, Ubuntu 24.04 |
| **Windows** | `blacksmith-4vcpu-windows-2025` | 4 vCPU, 14 GB RAM, Windows Server 2025 (public beta) |
| **macOS** | `blacksmith-6vcpu-macos-15` | 6 vCPU, 24 GB RAM, macOS 15 Sequoia, Apple Silicon M4 |

**Blacksmith audio device probe results** (run 2026-04-13):

| Platform | Virtual Audio Available? | Details |
|---|---|---|
| **Linux** | 🟡 Needs fix | PipeWire packages install fine but `systemctl --user` services don't start in Firecracker microVMs (no D-Bus user session). Fix: launch `pipewire` + `wireplumber` manually, install `pulseaudio-utils` for `pactl`. |
| **Windows** | 🟡 Needs fix | `AlekseyMartynov/action-vbcable-win@v1` tag fails to resolve on Blacksmith Windows runner. Fix: pin to a valid tag or install VB-CABLE manually via PowerShell. |
| **macOS** | ✅ Working | BlackHole 2ch installs via `brew`, CoreAudio daemon running, virtual 48kHz stereo device as default I/O. Apple Silicon M4 hardware (not a VM). |

**SSH debugging:** Blacksmith supports SSH access to running jobs using your GitHub SSH keys. Enable in [Blacksmith Settings > Features](https://app.blacksmith.sh/settings?tab=features). Connection info appears in the "Setup runner" step of each job. Add a sleep step on failure to keep the VM alive for debugging.

### CI expectations & platform verification status

All three platforms are verified:

| Platform | Verification | Test Results |
|---|---|---|
| **Linux** | ✅ CI (PipeWire) | 258 platform-independent unit tests pass |
| **Windows** | ✅ Real hardware | WASAPI capture tested with all capture modes |
| **macOS** | ✅ Real hardware (macOS 26 Tahoe) | 289 unit tests + 12 integration tests pass |

- Docker-based testing available for cross-platform validation (see `docker/`)
- macOS backend includes compatibility with macOS 14.4–15 (Sonoma/Sequoia) and macOS 26 (Tahoe) via 3-path API fallback. See [macOS Version Compatibility](docs/MACOS_VERSION_COMPATIBILITY.md).

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
| Reference the deleted `ApplicationCapture` trait | It was removed — use `CaptureTarget` with the builder instead |
| Make `CapturingStream` depend on file I/O | File writing is a sink adapter, not a core concern |
| Pretend a platform supports a feature it doesn't | Use explicit capability errors via `PlatformCapabilities` |
| Hold locks or allocate on real-time audio callback threads | Use lock-free ring buffers (`rtrb`) via `BridgeProducer`; use [`push_samples_or_drop()`](src/bridge/ring_buffer.rs:140) for zero-alloc RT callbacks |
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
| **Phase 5** | Breadth expansion (more formats, richer async, advanced features) | 🟡 In Progress |

### Phase 5 progress

**Completed:**
- ✅ **All 10 gap closures (G1–G10) done** — see §3 for the full table
- ✅ Windows WASAPI: application capture (`ApplicationByName` via `sysinfo` PID resolution) + process tree capture (process loopback)
- ✅ Linux PipeWire: application capture (`ApplicationByName` via `pw-dump` node resolution) + process tree capture (PID → PipeWire node mapping)
- ✅ macOS CoreAudio: application capture + process tree capture (both via Process Tap)
- ✅ [`subscribe()`](src/api.rs:463) method on `AudioCapture` — push-based `mpsc` channel delivery
- ✅ [`overrun_count()`](src/api.rs:514) on `AudioCapture` and [`CapturingStream`](src/core/interface.rs:122) — ring buffer overflow monitoring
- ✅ Full [`PlatformCapabilities`](src/core/capabilities.rs) reporting with `supports_process_tree_capture` field
- ✅ Device enumeration rewritten against real platform APIs (PipeWire `pw-cli`/`pw-dump` on Linux, WASAPI on Windows, CoreAudio on macOS) — mock data removed
- ✅ `get_device_enumerator()` and `DeviceKind` exposed in public API ([`src/lib.rs`](src/lib.rs))
- ✅ `cmd_list()` CLI command now enumerates actual devices via the library API
- ✅ [`list_devices.rs`](examples/list_devices.rs) example updated to use real enumeration
- ✅ Compiler warnings cleaned up to zero
- ✅ Application capture integration tests added (`tests/ci_audio/app_capture.rs`):
  - `test_app_capture_by_process_id` — spawns audio player, captures by PID
  - `test_app_capture_by_pipewire_node_id` — Linux PipeWire node discovery + capture
  - `test_app_capture_nonexistent_target` — graceful error handling
- ✅ Test helpers for app capture: `require_app_capture!()`, `spawn_audio_player_get_pid()`, `find_pipewire_node_for_pid()`
- ✅ Platform-specific capability unit tests fixed for cross-platform CI (5 tests with `#[cfg]` guards + Windows/macOS variants)
- ✅ **macOS fully tested on real hardware (macOS 26 Tahoe)**:
  - 289 unit tests + 12 integration tests passing
  - System capture, application capture, process tree capture all verified
  - macOS 26 compatibility: 3-path API fallback in [`create_process_tap_description()`](src/audio/macos/tap.rs:774)
    - Path 1: `initStereoMixdownOfProcesses:` with AudioObjectIDs (macOS 26+)
    - Path 2: `setProcesses:exclusive:` with PIDs (macOS 14.4–15)
    - Path 3: Separate `setProcesses:` + `setExclusive:` (macOS 26 fallback)
  - `respondsToSelector:` guards for removed selectors (`setPrivateTap:`, `setProcesses:exclusive:`)
  - PID→AudioObjectID translation via `kAudioHardwarePropertyTranslatePIDToProcessObject` (`'id2p'`)
  - Aggregate device UID uses tap UUID for collision prevention
  - CStr null pointer checks added to prevent UB in tap UUID handling
  - [`push_samples_or_drop()`](src/bridge/ring_buffer.rs:140) — zero-allocation RT callback method with scratch buffer recycling
  - Runtime macOS version detection via sysctl in [`PlatformCapabilities::macos()`](src/core/capabilities.rs:113)
  - Comprehensive docs: [macOS Version Compatibility](docs/MACOS_VERSION_COMPATIBILITY.md), [macOS 26 Process Tap Fix](docs/MACOS26_PROCESS_TAP_FIX.md)

**Remaining:**
- Async stream support (behind `async-stream` feature, foundation in place via `atomic-waker`)
- Additional sink adapters
- Performance benchmarking and optimization
- macOS 15 (Sequoia) testing on real hardware (expected to work via Path 2, untested)
- Complete device enumeration on macOS (currently returns only default device)

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

// Push-based subscription (G7)
let rx = capture.subscribe()?;  // mpsc::Receiver<AudioBuffer>
std::thread::spawn(move || {
    while let Ok(buf) = rx.recv() {
        println!("Got {} frames", buf.num_frames());
    }
});

// Ring buffer overflow monitoring (G8)
let dropped = capture.overrun_count();
if dropped > 0 {
    eprintln!("Warning: {} buffers dropped (consumer too slow)", dropped);
}

// Device enumeration
let enumerator = rsac::get_device_enumerator()?;
let devices = enumerator.enumerate_devices()?;
let default = enumerator.get_default_device(DeviceKind::Output)?;

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
producer.push_samples_or_drop(data, channels, sample_rate); // zero-alloc RT-safe
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
10. **macOS is tested and working.** All capture modes verified on macOS 26 Tahoe. The 3-path fallback in [`tap.rs`](src/audio/macos/tap.rs) handles API differences across macOS 14.4–26. See [macOS Version Compatibility](docs/MACOS_VERSION_COMPATIBILITY.md) for the full compatibility matrix.
