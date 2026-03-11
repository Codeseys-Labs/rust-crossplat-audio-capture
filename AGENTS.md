# AGENTS.md — AI Agent & Contributor Guide

> **Definitive reference for AI agents and human contributors working on `rsac`.**
> Read this file before touching any code. It describes the project's identity, architecture, conventions, current state, and workflow expectations.

---

## 1. Project Identity

| Field | Value |
|---|---|
| **Name** | `rsac` (Rust Cross-Platform Audio Capture) |
| **Type** | Rust library with sample CLI/TUI demo applications |
| **Primary deliverable** | Library crate (`rsac`) |
| **Secondary deliverables** | Sample CLI and TUI demo applications |
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

### Key Architecture Points

- **Canonical API**: New builder/trait API flow:
  ```
  AudioCaptureBuilder → AudioCapture → CapturingStream
  ```
- **Streaming-first**: The core abstraction is `CapturingStream::read_chunk() → AudioBuffer`. File writing is implemented as a sink adapter, not a primary concern.
- **Ring buffer bridge**: OS audio callbacks push data into an `rtrb` SPSC lock-free ring buffer; consumer threads pull data out via `CapturingStream`.
- **`BridgeStream<S>`**: Universal `CapturingStream` implementation that eliminates per-platform duplication of the ring-buffer-to-consumer pattern.
- **`CaptureTarget` enum**: Unified target model covering all capture modes:
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
- **Platform capabilities**: `PlatformCapabilities` struct for honest reporting of what each backend supports — never pretend a platform can do something it cannot.
- **Module layering** (strict DAG — no reverse dependencies):
  ```
  core/ → bridge/ → backend/ → api/ → lib.rs
  ```

---

## 3. Current State

The codebase is **in transition** from an old backend-centric API to the new builder/trait API.

### What's old (to be removed)

- `AudioCaptureBackend` trait — dead code
- `AudioCaptureStream` trait — dead code
- `get_audio_backend()` function — dead code
- Standalone `ApplicationCapture` trait — to be merged into `CapturingStream`
- [`src/audio/core.rs`](src/audio/core.rs) — old API types, scheduled for removal

### Platform backend maturity

| Platform | Status |
|---|---|
| **macOS** | Most complete — validates the architecture end-to-end |
| **Windows** | Needs `create_stream()` wiring to the new API |
| **Linux** | Needs `CapturingStream` implementation |

### Other known gaps

- [`src/audio/discovery.rs`](src/audio/discovery.rs) contains mostly mock data and needs a full rewrite against real platform APIs.
- [`src/main.rs`](src/main.rs) needs to be rebuilt as a thin library consumer (not a standalone application).

---

## 4. Source Code Layout

```
src/
├── lib.rs                  # Public API exports
├── api.rs                  # AudioCaptureBuilder, AudioCapture
├── main.rs                 # CLI binary (to be rebuilt as thin library consumer)
├── core/                   # Core types, traits, errors
│   ├── mod.rs
│   ├── buffer.rs           # AudioBuffer
│   ├── config.rs           # AudioCaptureConfig, StreamConfig
│   ├── error.rs            # AudioError taxonomy (21 variants)
│   ├── interface.rs        # CapturingStream, AudioDevice, DeviceEnumerator traits
│   └── processing.rs       # Audio processing traits
├── audio/                  # Platform backends + cross-platform abstractions
│   ├── mod.rs              # Cross-platform dispatch
│   ├── application_capture.rs  # ApplicationCapture trait (to be merged)
│   ├── capture.rs          # Capture helpers
│   ├── core.rs             # Old API types (to be removed)
│   ├── discovery.rs        # App discovery (mostly mock, needs rewrite)
│   ├── windows/            # WASAPI backend
│   ├── linux/              # PipeWire backend
│   └── macos/              # CoreAudio + Process Tap backend
├── bin/                    # Binary targets (demo apps, test utilities)
└── utils/                  # Utility modules
```

### Supporting directories

```
docs/architecture/          # Canonical architecture documents (source of truth)
examples/                   # Example programs
tests/                      # Integration tests
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

### Data & Types

- All audio data standardized to **`f32`** internally
- Error type: [`AudioError`](src/core/error.rs) (21 categorized variants)
- Result type: `AudioResult<T> = Result<T, AudioError>`

### Patterns

- **Builder pattern** for capture configuration (`AudioCaptureBuilder`)
- **Interior mutability** (`Mutex`, `Arc`) inside `AudioCapture` for `&self` methods
- **Lock-free ring buffers** (`rtrb`) for bridging OS callback threads to consumer threads
- **Trait-based abstraction** — platform backends implement internal traits; consumers use `CapturingStream`

### Naming

- Public API types live in [`src/api.rs`](src/api.rs) and [`src/core/`](src/core/mod.rs)
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
| Use the old API (`AudioCaptureBackend`, `get_audio_backend()`) | Deprecated — will be removed |
| Use the standalone `ApplicationCapture` trait for new code | Use `CaptureTarget` with the builder instead |
| Make `CapturingStream` depend on file I/O | File writing is a sink adapter, not a core concern |
| Pretend a platform supports a feature it doesn't | Use explicit capability errors via `PlatformCapabilities` |
| Hold locks in real-time audio callback threads | Use lock-free ring buffers (`rtrb`) |
| Add new `AudioError` variants without categorizing them | Every variant must have an `ErrorKind` and recoverability classification |
| Silently diverge from architecture docs | Propose changes explicitly if the design needs updating |

---

## 8. Implementation Phases (Roadmap)

| Phase | Focus | Status |
|---|---|---|
| **Phase 0** | Repo alignment & legacy API deprecation | In progress |
| **Phase 1** | Core API contract freeze (new types, traits, errors) | In progress |
| **Phase 2** | Streaming/data-plane & sink adapters (ring buffer bridge, `BridgeStream`) | Planned |
| **Phase 3** | Platform backends — macOS first (validates architecture), then Windows, then Linux | Planned |
| **Phase 4** | Rebuild demo CLI/TUI as thin library consumers | Planned |
| **Phase 5** | Breadth expansion (more formats, richer async, advanced features) | Future |

---

## 9. Key Dependencies

| Crate | Purpose |
|---|---|
| `rtrb` | Lock-free SPSC ring buffer for audio data bridge |
| `hound` | WAV file writing (for sink adapter) |
| `futures-core` | Async `Stream` trait (optional, behind `async` feature) |
| `atomic-waker` | Async notification from ring buffer (optional) |

### Platform-specific

| Platform | Dependencies |
|---|---|
| **Windows** | `wasapi` crate |
| **Linux** | `pipewire` / `libpipewire-sys` crates |
| **macOS** | CoreAudio frameworks (system, linked via `build.rs`) |

---

## 10. Quick Reference: Core Types

```rust
// Builder → configured capture → active stream
AudioCaptureBuilder::new()
    .with_target(CaptureTarget::SystemDefault)
    .with_config(StreamConfig { sample_rate: 48000, channels: 2, .. })
    .build()?                    // → AudioCapture
    .start()?                    // → CapturingStream

// Reading audio (streaming-first)
let chunk: AudioBuffer = stream.read_chunk()?;

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
```

---

## 11. For AI Agents Specifically

1. **Read the architecture docs first.** The four documents in `docs/architecture/` are the source of truth. Code may lag behind them during transition.
2. **Check current state before implementing.** The old API and new API coexist. Verify which types/traits are canonical before writing code.
3. **Scope changes tightly.** Prefer small, focused changes that move one thing forward over sweeping refactors.
4. **Report back clearly.** When completing a task, summarize what changed, what was discovered, and what remains.
5. **Respect the module DAG.** `core/` knows nothing about `audio/`. `audio/` knows nothing about `api/`. Violations break the architecture.
6. **Test on the target platform.** If you're implementing a Windows backend change, validate with `cargo check --features feat_windows` at minimum.
7. **When in doubt, ask.** If a design decision isn't covered by the architecture docs, surface it rather than guessing.
