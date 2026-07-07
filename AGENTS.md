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

> **Source of truth = the code.** The documents under `docs/architecture/` are
> early *design* documents (the `*_DESIGN.md` / `ARCHITECTURE_OVERVIEW.md` set).
> The shipped implementation intentionally diverged from them in several places,
> so they are **historical/aspirational, not canonical or guaranteed-matching**.
> Each carries a banner listing its known divergences. When a design doc and the
> code disagree, **the code wins** — read the rustdoc, the modules under
> [`src/`](src/), and the ADRs in [`docs/designs/`](docs/designs/). For an
> accurate user-facing overview see [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md).

The architecture is described in detail across the original design documents,
plus a comprehensive reference analysis:

| Document | Purpose |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | **Accurate** user-facing architecture overview (start here) |
| [Architecture Overview](docs/architecture/ARCHITECTURE_OVERVIEW.md) | Master architecture *design* doc (historical) |
| [API Design](docs/architecture/API_DESIGN.md) | Original public API *design* (historical — see banner) |
| [Error & Capability Design](docs/architecture/ERROR_CAPABILITY_DESIGN.md) | Error taxonomy + platform capabilities *design* (historical) |
| [Backend Contract](docs/architecture/BACKEND_CONTRACT.md) | Internal backend traits + module architecture (design) |
| [ADRs](docs/designs/) | Architecture Decision Records (0001–0016, indexed in [docs/designs/README.md](docs/designs/README.md)) — accepted decisions |
| [Reference Analysis](reference/REFERENCE_ANALYSIS.md) | Analysis of 10 reference repos mapped to rsac's architecture |
| [Local Testing Guide](docs/LOCAL_TESTING_GUIDE.md) | How to test on physical macOS, Windows, and Linux machines |
| [macOS Version Compatibility](docs/MACOS_VERSION_COMPATIBILITY.md) | macOS API compatibility matrix, version-specific fallbacks, known issues |

### Key Architecture Points (Implemented)

- **Canonical API**: Builder/trait API flow — fully implemented:
  ```
  AudioCaptureBuilder → AudioCapture → CapturingStream
  ```
- **Streaming-first**: The core abstraction is `CapturingStream::read_chunk() → AudioBuffer`. File writing is implemented as a sink adapter ([`src/sink/`](src/sink/mod.rs)), not a primary concern.
- **Ring buffer bridge**: OS audio callbacks push data into an `rtrb` SPSC lock-free ring buffer; consumer threads pull data out via `BridgeStream`. The [`push_samples_or_drop()`](src/bridge/ring_buffer.rs:159) method provides allocation-free pushes on real-time callback threads via a **free-list return ring**: the consumer recycles drained `Vec<f32>` allocations back to the producer (the unavoidable allocation is performed on the non-RT consumer thread). Implemented in [`src/bridge/`](src/bridge/mod.rs).
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
- **Error model**: 23 categorized error variants (across 7 `ErrorKind` categories) with three-state recoverability (`Recoverable`, `TransientRetry`, `Fatal`). The `recoverability()` match is exhaustive (no catch-all) so a new variant must be classified or the crate won't compile. See [`src/core/error.rs`](src/core/error.rs).
- **Platform capabilities**: [`PlatformCapabilities`](src/core/capabilities.rs) struct for honest reporting of what each backend supports — never pretend a platform can do something it cannot. On macOS, capabilities are determined at runtime using [`get_macos_version()`](src/core/capabilities.rs:175) (sysctl-based, no subprocess) to detect Process Tap availability (requires macOS 14.4+).
- **Sink adapters**: [`AudioSink`](src/sink/traits.rs) trait with three implementations:
  - [`NullSink`](src/sink/null.rs) — discards data (testing/benchmarking)
  - [`ChannelSink`](src/sink/channel.rs) — sends buffers over `mpsc` channel
  - [`WavFileSink`](src/sink/wav.rs) — writes to WAV files (behind `sink-wav` feature)
- **Module layering** (strict DAG — no reverse dependencies):
  ```
  core/ → bridge/ → audio/ (backends) → api/ → compose/ (opt-in) → lib.rs
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
| **macOS** | CoreAudio | [`src/audio/macos/thread.rs`](src/audio/macos/thread.rs) | ✅ Wired via `MacosPlatformStream` — tested on macOS 26 Tahoe (full unit suite + integration tests) |

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

**Mobile (in progress — ADR-0012/0013, [`docs/MOBILE_BACKEND_DESIGN.md`](docs/MOBILE_BACKEND_DESIGN.md)):**
the mobile backends are implemented and **compile-checked only** (`aarch64-linux-android` /
`aarch64-apple-ios` check+clippy green; **no runtime verification on any device yet** —
do not claim "tested on Android/iOS"). First-party glue (`mobile/android` AAR Kotlin,
`mobile/ios` SwiftPM incl. the canonical broadcast-ring contract) **builds in CI**
(the `mobile-android` / `mobile-ios` ci.yml jobs: cross-target check+clippy, real
Gradle AAR + xcodebuild SwiftPM builds incl. `librsac.so` in jniLibs with its
`JNI_OnLoad` export asserted — compile-proof only).

| Capture Mode | Android (AAudio + AudioPlaybackCapture) | iOS (AVAudioEngine) |
|---|---|---|
| **Device — default mic** (`Device("default")`) | 🟡 compiled, unverified (rsac-20cd) | 🟡 compiled, unverified (rsac-9e02) |
| **System default** (= playback capture, ADR-0013) | 🟡 compiled, unverified (rsac-77f1: AAR Kotlin `AudioRecord` loop + JNI ingest; needs `with_android_projection` token + FGS, API 29+) | 🟡 compiled, unverified (rsac-b3aa: ReplayKit ring consumer; needs `with_ios_app_group` + embedded extension + user-started broadcast) |
| **Application / ByName / ProcessTree** | 🟡 compiled, unverified (rsac-77f1: UID filters; tree ≡ app — same requirements as SystemDefault) | ❌ permanent — no iOS API (never soften) |
| **Device selection (real device list)** | ⏳ rsac-ad8a (Java AudioManager via AAR) | ❌ session-routed, not free selection |

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
│   ├── error.rs            # AudioError (23 variants), ErrorKind, Recoverability,
│   │                       #   BackendContext
│   ├── interface.rs        # CapturingStream, AudioDevice, DeviceEnumerator traits
│   ├── introspection.rs    # Cross-platform source discovery, permission checks,
│   │                       #   CaptureTarget convenience constructors
│   └── processing.rs       # Audio processing traits
├── bridge/                 # Ring buffer bridge (data plane)
│   ├── mod.rs              # Re-exports + integration tests
│   ├── state.rs            # AtomicStreamState, StreamState enum
│   ├── ring_buffer.rs      # BridgeProducer, BridgeConsumer, BridgeShared, create_bridge()
│   ├── stream.rs           # BridgeStream<S>, PlatformStream trait
│   └── mock.rs             # Mock audio backend (440Hz sine wave, test-utils feature)
├── sink/                   # Sink adapters for audio data
│   ├── mod.rs              # Re-exports
│   ├── traits.rs           # AudioSink trait
│   ├── null.rs             # NullSink (discard)
│   ├── channel.rs          # ChannelSink (mpsc)
│   └── wav.rs              # WavFileSink (behind sink-wav feature)
├── compose/                # Multi-source channel composition (compose feature, ADR-0011)
│   ├── mod.rs              # Module docs + re-exports
│   ├── builder.rs          # CompositionBuilder, Group, GroupLayout, ChannelMap
│   ├── engine.rs           # Compositor thread: FIFOs, master-clock pacing, mixdown
│   ├── resample.rs         # rubato wrapper (per-source rate → session rate)
│   ├── stream.rs           # Composition handle + ComposedStreamView (CapturingStream)
│   └── tests.rs            # Engine-loop tests over scripted sources
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
│   ├── android/            # AAudio mic + AudioPlaybackCapture backend (cfg feat_android — compile-checked, unverified on-device)
│   │   ├── mod.rs          # AndroidDeviceEnumerator + AndroidAudioDevice
│   │   ├── aaudio.rs       # In-tree AAudio NDK FFI (no crate deps)
│   │   ├── jni.rs          # JNI boundary: JNI_OnLoad/RegisterNatives, session registry, natives (jni-sys)
│   │   ├── playback.rs     # AndroidPlaybackDevice/Stream — AAR Kotlin loop orchestration, ADR-0013 UID mapping
│   │   └── thread.rs       # AndroidPlatformStream + RT data callback → BridgeProducer
│   └── ios/                # AVAudioEngine backend (mic + ReplayKit consumer; cfg feat_ios — compile-checked, unverified on-device)
│       ├── mod.rs          # IosDeviceEnumerator + IosAudioDevice
│       ├── avaudio.rs      # objc2-avf-audio input-node tap → BridgeProducer
│       └── thread.rs       # IosPlatformStream
├── bin/                    # Binary targets (all require features; see docs/features.md)
│   ├── standardized_test.rs
│   ├── app_capture_test.rs
│   ├── pipewire_test.rs
│   ├── pipewire_diagnostics.rs
│   ├── wasapi_session_test.rs
│   └── smoke_alpine.rs
└── utils/                  # Utility modules
    ├── mod.rs
    └── test_utils.rs
```

### Supporting directories

```
bindings/
├── rsac-ffi/               # C FFI layer (56 extern "C" functions, cdylib + staticlib)
├── rsac-python/            # Python bindings (PyO3 + maturin)
├── rsac-napi/              # Node.js/TypeScript bindings (napi-rs)
└── rsac-go/                # Go bindings (CGo over C FFI)
apps/
└── audio-graph/            # Tauri v2 desktop app (standalone checkout, git-ignored — audio-graph depends on rsac; no longer a submodule)
docs/
├── architecture/           # Original architecture *design* docs (historical; code is source of truth)
├── OBJC2_MIGRATION_PLAN.md # objc2 migration plan (completed)
├── CROSS_LANGUAGE_BINDINGS.md # Cross-language binding research + design
├── LOCAL_TESTING_GUIDE.md
└── MACOS_VERSION_COMPATIBILITY.md
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
  - `compose` — Multi-source channel composition (`src/compose/`; adds `rubato` + `audioadapter-buffers`) — ADR-0011
  - `cli` — Demo binaries' deps (`clap`, `color-eyre`, `ctrlc`, `env_logger`); NOT in defaults, so library consumers don't pull them
  - `macos-tcc-spi` — real `check_audio_capture_permission()` preflight via the private `TCCAccessPreflight` SPI, `dlopen`'d at runtime (ADR-0015); off by default so the published artifact carries no private-symbol usage

### Data & Types

- All audio data standardized to **`f32`** internally
- [`SampleFormat`](src/core/config.rs) enum: `I16`, `I24`, `I32`, `F32`
- [`AudioFormat`](src/core/config.rs) struct: `sample_rate`, `channels`, `sample_format`
- Error type: [`AudioError`](src/core/error.rs) (23 categorized variants)
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

### Local DevEx: mise + lefthook + the gate

The repo's local tooling was overhauled 2026-07-05 (seeds rsac-7e19 …
rsac-61ce). The pieces and how to use them:

**One-shot onboarding** (full walkthrough: [`docs/CONTRIBUTING.md`](docs/CONTRIBUTING.md) §1–2):

```bash
# Rust comes from rustup + rust-toolchain.toml (pinned 1.95.0) — automatic.
# Everything else (bun / node / go / python / lefthook) is pinned in mise.toml:
mise install        # install the pinned polyglot toolchain
mise run setup      # install the git hooks (lefthook install)
```

**The gate** — a faithful replica of ci.yml's `lint` job for the host OS.
Run it before pushing (the pre-push hook runs it anyway):

```bash
mise run gate        # or: bash scripts/gate.sh   (pwsh scripts/gate.ps1 on Windows)
#   fmt --check → clippy -D warnings (feat_<host>,compose,cli) → bare-build smoke
mise run gate:full   # + lib tests, doctests, docsrs cargo doc, module-DAG guard
mise run test        # just the CI test-job replica for the host OS
```

mise is a convenience, not a requirement — every task is a thin alias over
`scripts/gate.sh`, which runs directly under bash (Git bash on Windows).

**Git hooks** ([`lefthook.yml`](lefthook.yml), opt-in per clone, CI is the
backstop): pre-commit = rustfmt check (only when `.rs` staged); commit-msg =
rejects `Co-Authored-By:` trailers and tool bylines (§6 conventions,
enforced mechanically); pre-push = the gate. Escape hatch: `--no-verify`.

**Other DevEx surfaces:**

- **Devcontainer** ([`.devcontainer/`](.devcontainer/devcontainer.json)) —
  full Linux/PipeWire environment for Windows/macOS contributors working
  the `feat_linux` leg.
- **Editor**: [`.vscode/settings.json`](.vscode/settings.json) (checked in)
  enables the off-by-default features for rust-analyzer so `src/compose/`,
  `src/main.rs`, and gated examples get diagnostics. Non-VS-Code recipe in
  CONTRIBUTING § Editor setup.
- **Scripts**: [`scripts/README.md`](scripts/README.md) is the live-list —
  every script's purpose and caller. Anything not listed was deleted as rot.
- **Docs**: [`docs/README.md`](docs/README.md) is the index — every doc with
  a current/historical status.
- **Local audio testing**: on a machine with real audio, export
  `RSAC_CI_AUDIO_DETERMINISTIC=1` so capture tests hard-fail on silence
  instead of warning (all knobs:
  [`docs/CI_AUDIO_TESTING.md`](docs/CI_AUDIO_TESTING.md) §5).

Plain-cargo equivalents still work if you want no tooling at all:

```bash
cargo check                          # fast compile check
cargo test --lib                     # unit tests
cargo check --features feat_linux    # a specific platform feature
```

### Local testing on physical machines

See the [Local Testing Guide](docs/LOCAL_TESTING_GUIDE.md) for comprehensive instructions
on testing system capture, application capture, and process tree capture on macOS, Windows,
and Linux.

### CI infrastructure: Blacksmith runners

All CI runs on [Blacksmith](https://blacksmith.sh/) runners — a drop-in replacement for GitHub-hosted runners with 2x faster hardware and co-located caching. Workflows are in [`.github/workflows/`](.github/workflows/).

| Workflow | Purpose |
|---|---|
| [`ci.yml`](.github/workflows/ci.yml) | Lint, unit tests (3 platforms), MSRV, feature powerset, ARM64 cross-compile, binding runtime smokes (Python import + napi `node --test`). A `changes` gate job skips the whole compile matrix on docs-only PRs (skipped jobs count as passing for required checks); pushes/tags/dispatch always run everything. |
| [`ci-audio-tests.yml`](.github/workflows/ci-audio-tests.yml) | Audio integration tests (9 platform x tier jobs) |

**Runner labels:**

| Platform | Runner Label | Specs |
|---|---|---|
| **Linux** | `blacksmith-4vcpu-ubuntu-2404` | 4 vCPU, 16 GB RAM, Ubuntu 24.04 |
| **Windows (compile)** | `blacksmith-4vcpu-windows-2025` | 4 vCPU, 14 GB RAM, Windows Server 2025 (compile + unit tests only) |
| **Windows (audio)** | `windows-latest` (GitHub-hosted) | GitHub-hosted runner with full audio stack (VB-CABLE via `LABSN/sound-ci-helpers@v1`) |
| **macOS** | `blacksmith-6vcpu-macos-15` | 6 vCPU, 24 GB RAM, macOS 15 Sequoia, Apple Silicon M4 |

**Blacksmith audio device probe results** (run 2026-04-13):

| Platform | Virtual Audio Available? | Details |
|---|---|---|
| **Linux** | ✅ Working | PipeWire launched manually (not via systemd — Firecracker VMs lack D-Bus user session). Virtual null sink via `pactl load-module module-null-sink`. Requires `pulseaudio-utils` for `pactl`, and `XDG_RUNTIME_DIR` setup. |
| **Windows** | ❌ No audio stack | VB-CABLE setup exe installs without error, but Windows Audio services (`AudioSrv`, `AudioEndpointBuilder`) don't exist in the Firecracker microVM. No audio endpoints are created. **Option D probe confirmed**: `AudioSes.dll`, `AudioEndpointBuilder.dll`, `Audiodg.exe` are all absent from System32; audio registry keys don't exist; `Install-WindowsFeature Server-Media-Foundation` unavailable. The audio subsystem is not part of the Blacksmith Windows image at all. Windows audio integration tests run on GitHub-hosted `windows-latest` instead. |
| **macOS** | ✅ Working | BlackHole 2ch installs via `brew`, CoreAudio daemon running, virtual 48kHz stereo device as default I/O. Apple Silicon M4 hardware (not a VM). |

**SSH debugging:** Blacksmith supports SSH access to running jobs using your GitHub SSH keys. Enable in [Blacksmith Settings > Features](https://app.blacksmith.sh/settings?tab=features). Connection info appears in the "Setup runner" step of each job. Add a sleep step on failure to keep the VM alive for debugging.

### CI expectations & platform verification status

All three platforms are verified:

| Platform | Verification | Test Results |
|---|---|---|
| **Linux** | ✅ CI (PipeWire) | Full platform-independent unit suite passes |
| **Windows** | ✅ Real hardware | WASAPI capture tested with all capture modes |
| **macOS** | ✅ Real hardware (macOS 26 Tahoe) | Full unit suite + integration tests pass |

The library unit suite is **300+ tests** (the exact count varies by platform and
enabled features — e.g. the Windows `--lib` suite is larger than the
platform-independent Linux subset, so don't pin a brittle exact number) plus the
`ci_audio` integration suite (~40+ tests), all passing with 0 failures on the
verified platforms.

- Docker-based testing available for cross-platform validation (see `docker/`)
- macOS backend includes compatibility with macOS 14.4–15 (Sonoma/Sequoia) and macOS 26 (Tahoe) via 3-path API fallback. See [macOS Version Compatibility](docs/MACOS_VERSION_COMPATIBILITY.md).

### Architecture alignment

The **code is the source of truth.** The documents in `docs/architecture/` are
early design docs that the implementation has diverged from — treat them as
historical context, not a spec to conform to. Durable design decisions are
recorded as ADRs in [`docs/designs/`](docs/designs/); add a new ADR when you
make one. If you spot a design doc that contradicts the code, fix the doc (or
note the divergence in its banner) rather than changing the code to match it.

### Task management — seeds (`sd` CLI)

The backlog is **git-native**: issues live in [`.seeds/issues.jsonl`](.seeds/)
and are managed exclusively through the [`sd` CLI](https://github.com/jayminwest/seeds)
(the full command reference is the tool-managed block in
[`CLAUDE.md`](CLAUDE.md); run `sd prime` at session start for agent context).
The working loop:

```bash
sd ready                          # unblocked work (dependency-aware)
sd update <id> --status in_progress
sd close <id> --reason "…"        # close with the evidence (commit SHA, verification)
sd create --title "…" --type task --priority 2 --labels "…"
sd dep add <id> <depends-on>      # blockers; epics depend on their child seeds
sd sync                           # stage+commit .seeds/ before pushing
```

Conventions this repo layers on top: seed descriptions end with an
`(effort: … | verify: … | wave: N)` footer plus acceptance criteria; epics
(e.g. the mobile push: umbrella `rsac-0991` → `rsac-5823`/`rsac-57cb`/`rsac-71d2`)
carry resume-context in their descriptions and depend on their children, so
`sd ready` surfaces exactly the unblocked entry points. **Never hand-edit
`.seeds/issues.jsonl`** — the CLI maintains timestamps/`closedAt`/dependency
integrity (`sd doctor` catches drift).

### Git commit conventions

Commit messages **must not** contain a `Co-Authored-By:` trailer or any
tool-generated byline (e.g. "Generated with Claude Code"). Keep messages plain:
a concise summary line plus an optional body explaining the *why*. This applies to
all contributors and AI agents working in this repo. With the lefthook hooks
installed (`mise run setup`), the commit-msg hook
([`scripts/hooks/commit-msg.sh`](scripts/hooks/commit-msg.sh)) enforces this
mechanically.

### Code review dispositions — file an issue for anything not fixed in the PR

Every review comment (from a human, CodeRabbit, or an agent) must reach one of
two terminal states **before the PR merges**: *fixed in the PR*, or *captured in
a tracking GitHub issue*. ("Fixed in the PR" includes the case where the fix
already exists — see **already-addressed** below.) A review finding must
**never silently disappear** when its PR merges. Triage each comment into exactly
one disposition:

| Disposition | Action |
|---|---|
| **fix-now** | Fix it in the PR. Reply on the thread noting the fix. |
| **already-addressed** | A form of *fixed in the PR* — the fix already exists. Reply pointing at the current code that handles it (no new issue needed). |
| **valid-defer** | **Open a tracking issue** (label `deferred-review` + a domain label like `bug`/`tech-debt`/`ci`), then reply on the thread linking the issue (`📌 Tracked in #N`). |
| **invalid** / **wont-fix** | **Record the decision in an issue** (one consolidated "review dispositions" issue per PR is fine; label `invalid`/`wontfix`; close it as *not planned* — it is a searchable decision record, not open work) and reply on the thread with the rationale + link. |

Rationale: a deferred or rejected finding that lives only inside a merged PR's
review thread is effectively lost — future contributors can't discover it, and a
real bug (e.g. a deferred use-after-free) can rot unnoticed. Issues are the
durable, searchable backlog; PR threads are ephemeral.

Reply to the originating comment in every case so the reviewer (and the bot) can
see the outcome and resolve the thread.

### Stacked pull requests — split big changes along the DAG

A change that layers along the module DAG (`core → bridge → audio → api → lib`)
and would otherwise become a large, hard-to-review PR should ship as a **stack of
small PRs**, one layer per PR, merged **bottom-up**. We do this **gh-native**
(plain `git` + `gh`, no Graphite/spr/ghstack). Each child PR's base is its *parent
branch* (not `master`), so each layer's diff stays small and independently
reviewable — which also keeps CodeRabbit under its size limit (the 167-file PR #27
auto-paused its review; a stack would have kept each layer reviewable).

The one hazard is our **squash-merge** culture: after the bottom PR squash-merges,
recover each child with `git rebase --onto origin/master <old-parent> <child>` —
**never** a plain rebase (that re-replays the already-merged commits). Always pass
`--delete-branch` to `gh pr merge` (the repo has `deleteBranchOnMerge=false`, so
that flag is what retargets the child), and `--force-with-lease`, never `--force`.

Full playbook (when to stack vs parallel PRs, exact commands, pitfalls):
[`docs/STACKED_PRS.md`](docs/STACKED_PRS.md).

### Release-stacking SDLC — decomposing a fat branch into a reviewed release

> First executed for 0.4.1 (decision record seed `rsac-79bc`, epic `rsac-4844`,
> PRs #36+). **This is the house SDLC for any body of work that outgrew one
> PR** (CodeRabbit caps review at 150 changed files). Improve this section as
> retros land — it is a living process, not a frozen one.

**The pattern** (integration branch + just-in-time layers):

1. **Freeze** the source ("fat") branch: no new work lands there except
   explicitly-owned in-flight workstreams; record the frozen SHA.
2. **Create `release/X.Y.Z` from `master`** — the integration branch every
   layer PR targets. Before the first layer PR, commit on it:
   - `release/**` added to `pull_request` **and** `push` triggers in
     `ci.yml` + `ci-audio-tests.yml` (otherwise layer PRs run **zero CI** and
     the failure is silent — it happened);
   - `.coderabbit.yaml` with `reviews.base_branches: ["release/.*"]`
     (otherwise CodeRabbit refuses the base; the file is read from PR *head*
     branches, so every layer cut from the release branch inherits it).
3. **Cut layers just-in-time**, one domain per PR, bottom-up along the module
   DAG: *library → bindings → CI/DevEx → docs/seeds → platform tails*. Each
   layer: new `stack/<ver>-<n>-<name>` branch **from the current release
   branch** in a **separate `git worktree`** (never switch a shared checkout),
   `git checkout <frozen-sha> -- <layer paths>`, apply the documented
   hand-adjustments, validate locally (the gate + the layer's own suite),
   push, open the PR against `release/X.Y.Z`.
4. **Review every layer**: full CI matrix + CodeRabbit (nudge with
   `@coderabbitai full review` if it doesn't auto-start) + any other reviewer.
   Triage per §6 dispositions with one addition — the **identity-preserving
   policy**: while the stack is in flight, valid findings become seeds worked
   *after* the stack completes; nothing lands in a layer that the frozen
   source didn't carry (otherwise "functionally identical to the source"
   becomes unverifiable). Post the disposition table on the PR.
5. **Squash-merge bottom-up** (`gh pr merge --squash --delete-branch`), close
   the layer's seed with evidence, then cut the next layer from the updated
   release branch. Just-in-time cutting avoids the squash-rebase recovery
   hazard of true parent-based stacks entirely.
6. **Finish**: diff the release branch against the frozen SHA and account for
   every delta (expected: the trigger/CodeRabbit commits, documented manifest
   adjustments, lockfile resolution noise); run the full local gate; bump the
   version (`scripts/bump-version.sh`); open `release/X.Y.Z → master` — its
   content is already reviewed layer-by-layer.

**Hard-won rules (retro log — append, don't delete):**

- *rustfmt resolves `mod` declarations regardless of `cfg`*, so the library
  crate ships as **one** layer — feature modules (`compose`) and
  target-gated backends cannot be split out of a layer that contains
  `lib.rs`/`audio/mod.rs`.
- *cfg-independent `include_str!` contract tests* (e.g. `jni_lockstep`) drag
  their pinned foreign sources into the same layer as the tests.
- Shared files (`Cargo.toml`, `Cargo.lock`, `ci.yml`) need **documented
  per-layer hand-adjustments** (workspace members, `[[example]]` entries,
  stripped-then-restored CI jobs); write the adjustment into the layer seed
  *and* the manifest comment.
- A **regenerated lockfile silently reverts advisory fixes** — after cutting
  a layer, re-check `cargo deny`-relevant pins against the source branch
  (crossbeam-epoch lesson).
- Building a sibling crate locally can **regenerate committed artifacts**
  (cbindgen headers) with toolchain-version noise — restore them before
  committing.
- **`git checkout <sha> -- <paths>` cannot express deletions.** Files deleted
  on the frozen source but present on master silently survive the layer.
  Enumerate them per layer with
  `git diff --diff-filter=D --name-only master..<frozen-sha> -- <paths>` and
  `git rm` them explicitly (0.4.1 lesson: 4 deletions from 3 different
  layers had to be swept into L5).
- **Rename detection hides deletions from `--diff-filter=D`.** A file moved
  (e.g. into `docs/history/`) is classified `R`, not `D`, so the deletion
  sweep misses the old path and the release branch ends up with BOTH copies.
  Use `--no-renames` (or filter `DR` and take each rename's source path) in
  the sweep (0.4.1 lesson: 10 docs shadow-copies survived to the final
  identity diff before this was caught).
- **One invalid pathspec aborts the whole multi-path checkout** — the valid
  paths are silently skipped too. Verify with `git status` file-counts after
  every checkout, and split uncertain paths into their own command.
- Multi-session hygiene: **never `git add -A` in a shared checkout**, always
  stage explicit paths; layer work happens in disposable worktrees;
  `--force-with-lease` only, and only on branches you own.

---

## 7. What NOT to Do

| ❌ Don't | Why |
|---|---|
| Add backend-specific logic to demo apps | Demo apps must go through the library API only |
| Reference any old API types (`AudioCaptureBackend`, `get_audio_backend()`, etc.) | They have been deleted — they no longer exist |
| Reference the deleted `ApplicationCapture` trait | It was removed — use `CaptureTarget` with the builder instead |
| Make `CapturingStream` depend on file I/O | File writing is a sink adapter, not a core concern |
| Pretend a platform supports a feature it doesn't | Use explicit capability errors via `PlatformCapabilities` |
| Hold locks or allocate on real-time audio callback threads | Use lock-free ring buffers (`rtrb`) via `BridgeProducer`; use [`push_samples_or_drop()`](src/bridge/ring_buffer.rs:159) for alloc-free RT callbacks (free-list return ring) |
| Add new `AudioError` variants without categorizing them | Every variant must have an `ErrorKind` and recoverability classification |
| Bypass `BridgeStream<S>` for new backends | All backends must use `BridgeStream` + `PlatformStream` trait |
| Treat `docs/architecture/*_DESIGN.md` as the spec | They are historical design docs; the **code is the source of truth**. Record durable decisions as ADRs in `docs/designs/` |
| Import from `src/audio/core.rs` | File was deleted in Phase 0 |
| Add `Co-Authored-By:` trailers or tool bylines to commit messages | This repo requires plain commit messages — no co-author trailers, no "Generated with …" lines (see §6 Git commit conventions; the lefthook commit-msg hook rejects them) |
| Let a review comment be deferred/rejected with no tracking issue | Every non-fixed review finding must be captured in a GitHub issue before the PR merges, or it silently disappears (see §6 Code review dispositions) |
| Hand-edit `.seeds/issues.jsonl` | The backlog is CLI-managed — `sd create`/`update`/`close`/`dep` keep timestamps, `closedAt`, and dependency integrity; hand edits trip `sd doctor` (see §6 Task management) |
| Push without running the gate | `mise run gate` (or `bash scripts/gate.sh`) is the ci.yml lint-job replica — skipping it just moves the failure into CI (see §6 Local DevEx) |

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
  - Full unit suite + integration tests passing
  - System capture, application capture, process tree capture all verified
  - macOS 26 compatibility: 3-path API fallback in [`create_process_tap_description()`](src/audio/macos/tap.rs:774)
    - Path 1: `initStereoMixdownOfProcesses:` with AudioObjectIDs (macOS 26+)
    - Path 2: `setProcesses:exclusive:` with PIDs (macOS 14.4–15)
    - Path 3: Separate `setProcesses:` + `setExclusive:` (macOS 26 fallback)
  - `respondsToSelector:` guards for removed selectors (`setPrivateTap:`, `setProcesses:exclusive:`)
  - PID→AudioObjectID translation via `kAudioHardwarePropertyTranslatePIDToProcessObject` (`'id2p'`)
  - Aggregate device UID uses tap UUID for collision prevention
  - CStr null pointer checks added to prevent UB in tap UUID handling
  - [`push_samples_or_drop()`](src/bridge/ring_buffer.rs:159) — allocation-free RT callback method backed by a free-list return ring (consumer recycles drained `Vec<f32>`s to the producer)
  - Runtime macOS version detection via sysctl in [`PlatformCapabilities::macos()`](src/core/capabilities.rs:113)
  - Comprehensive docs: [macOS Version Compatibility](docs/MACOS_VERSION_COMPATIBILITY.md), [macOS 26 Process Tap Fix](docs/MACOS26_PROCESS_TAP_FIX.md)

**Recently completed:**
- ✅ **Multi-source channel composition (`compose` feature, ADR-0011)** — `CompositionBuilder`/`Composition` in `src/compose/`: groups of `CaptureTarget`s mixed to Mono/Stereo (per-source gain) or kept as native channels, appended into one interleaved multi-channel `CapturingStream`; rubato resampling to the session rate; master-clock pacing with silence-pad/trim stats. 30+ unit tests (scripted-source engine harness) + `compose::` ci_audio integration module + `examples/composed_capture.rs`.
- ✅ **`cli` feature** — clap/color-eyre/ctrlc/env_logger no longer unconditional; demo bins/examples declare `required-features = ["cli"]`; library consumers' dep tree is lean.
- ✅ **CI hardening** — `msrv` (1.87) job, `feature-powerset` (cargo-hack, depth 2), `cargo-semver-checks` release gate, stale audio-probe workflow deleted, ARM64 grep gates replaced with exit-code-authoritative checks.
- ✅ **`#![warn(missing_docs)]`** enforced; rustdoc gaps filled (ErrorKind variants, AudioError fields, platform enumerator items).
- ✅ **`cocoa`/`objc` → `objc2` migration** — Phase 1 (coreaudio.rs, 12 sites) + Phase 2 (tap.rs, ~65 sites) complete. `cocoa` and `objc` crates fully removed from dependencies. See §9.1.
- ✅ **Cross-language bindings** — C FFI (`bindings/rsac-ffi/`, 56 functions), Python (`bindings/rsac-python/`, PyO3), Node.js/TS (`bindings/rsac-napi/`, napi-rs), Go (`bindings/rsac-go/`, CGo). All compile.
- ✅ **Cross-platform introspection module** — `src/core/introspection.rs`: `list_audio_sources()`, `list_audio_applications()`, `CaptureTarget::app()`/`pid()`/`device()` convenience constructors, `check_audio_capture_permission()`.
- ✅ **Mock audio backend** — `src/bridge/mock.rs`: synthetic 440Hz sine wave through real BridgeStream pipeline, 6 unit tests.
- ✅ **Audio-graph migrated** to use `rsac::list_audio_sources()` — replaced ~120 lines of per-platform `#[cfg]` code.

**Remaining:**
- **Mobile — the playback-capture tiers are code-complete** (what `SystemDefault` means on mobile, ADR-0013): Android `AudioPlaybackCapture` + JNI ingest landed (rsac-77f1: all four tiers via the AAR Kotlin loop, `src/audio/android/{jni,playback}.rs`; `librsac.so` packaging rsac-0aa9) and iOS `SystemDefault` is compiled (rsac-b3aa: ReplayKit ring consumer mirroring the canonical `mobile/ios` RingLayout v1 contract) — both pending runtime proof
- **Mobile — runtime verification** (the honest gap: everything mobile is compile-proof only): Android emulator leg rsac-e6d3, iOS simulator/device leg rsac-97c8 — the AGENTS mobile matrix cells stay "compiled, unverified" until these are green
- **Mobile — delivery**: real Android device enumeration (rsac-ad8a), AAR Maven + SwiftPM distribution (rsac-05b6), `tauri-plugin-rsac` (rsac-f21c) + the audio-graph decision (rsac-0ac9, ADR-0014) — the rsac-ffi mobile-triple cross-checks landed (rsac-7a18)
- **Binding capability parity, FFI leg** — additive C ABI accessors (device-change notifications, sample-format list, rate range/whitelist) so Go reaches parity (rsac-a9af, re-scoped)
- Additional sink adapters
- Performance benchmarking and optimization — benches ship in-tree (`benches/`) but no CI job executes them; ADR-0006's `bridge-zerocopy` promote-or-remove decision is blocked on that A/B data
- macOS 15 (Sequoia) testing on real hardware (expected to work via Path 2, untested)
- **Linux `ApplicationByName` happy-path integration test** — Windows has `application_by_name_windows`; the Linux happy path (pinned `pw-dump` node name) is still absent and macOS's is `#[ignore]`d behind TCC
- **Harden non-silence assertions** — Linux capture tests still use soft warnings; flipping `RSAC_CI_AUDIO_DETERMINISTIC=1` needs the deterministic PipeWire routing evidence (seeds rsac-6efb / rsac-b106)
- **First crates.io publish** — the crate is not yet on crates.io (README's `version = "0.4"` snippet and the docs.rs links are forward-looking until then); release automation exists, needs `CARGO_REGISTRY_TOKEN` + a tag
- **Compose follow-ups** — Python/Node/Go bindings exposure (C FFI shipped; rsac-fba7), live per-source gain/mute (rsac-5a2d), v2 layouts (rsac-7c93)
- **Blacksmith Windows audio support** — request Blacksmith add audio subsystem to Windows Server images (see §6 runner labels)

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
| `rubato` | FFT resampling for compose-feature rate alignment (optional, behind `compose`) |
| `audioadapter-buffers` | Interleaved-slice adapters consumed by rubato v3 (optional, behind `compose`) |

### Platform-specific

| Platform | Dependencies |
|---|---|
| **Windows** | `wasapi`, `windows`, `windows-core`, `widestring`, `sysinfo` |
| **Linux** | `pipewire`, `libspa`, `libspa-sys` |
| **macOS** | `coreaudio-rs`, `coreaudio-sys`, `objc2`, `objc2-foundation`, `objc2-app-kit`, `core-foundation`, `core-foundation-sys`, `sysinfo` |

### 9.1 `cocoa`/`objc` → `objc2` Migration (COMPLETE ✅)

The `cocoa` (0.26.1) and `objc` (0.2.7) crates were **deprecated** and have been fully replaced by `objc2` (v0.6). Both crates are removed from `Cargo.toml`. See [`docs/OBJC2_MIGRATION_PLAN.md`](docs/OBJC2_MIGRATION_PLAN.md) for the full migration plan.

**Phase 1** (coreaudio.rs): 12 callsites migrated to typed `objc2-app-kit` APIs (`NSWorkspace::sharedWorkspace()`, `.runningApplications()`, etc.). 50% code reduction, all `unsafe` blocks removed.

**Phase 2** (tap.rs): ~65 callsites migrated. `CATapDescription` remains raw `objc2::msg_send!` (private class not in any framework crate). Foundation classes (`NSUUID`, `NSArray`, `NSNumber`, `NSString`, `NSAutoreleasePool`) migrated to typed `objc2-foundation` APIs. `YES`/`NO` → `true`/`false`, `id` → `*mut AnyObject`, `Class::get()` → `AnyClass::get(c"...")`.

**Key fix discovered during testing:** `setMuteBehavior:` selector expects `i64` (ObjC type code `'q'`), not `i32`. The old `objc` crate silently accepted the wrong type; `objc2` validates argument types at runtime and caught this bug.

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
let default = enumerator.get_default_device()?;

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
producer.push_samples_or_drop(data, channels, sample_rate); // alloc-free RT-safe (free-list ring)
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

1. **The architecture is implemented.** Phases 0–4 are complete. **The code is the source of truth** — the four design documents in `docs/architecture/` are historical and have known divergences from the shipped code (each has a banner; see §2). Do not treat the codebase as "in transition" — the new API is the only API. When in doubt, read the rustdoc and `src/`, not the design docs.
2. **The old API is gone.** Do not reference `AudioCaptureBackend`, `AudioCaptureStream`, `get_audio_backend()`, `src/audio/core.rs`, or any of the 14 removed types. They do not exist.
3. **All backends use `BridgeStream<S>`.** If adding a new backend, implement the `PlatformStream` trait and wrap with `BridgeStream`. Do not create a custom `CapturingStream` implementation.
4. **Scope changes tightly.** Prefer small, focused changes that move one thing forward over sweeping refactors.
5. **Report back clearly.** When completing a task, summarize what changed, what was discovered, and what remains.
6. **Respect the module DAG.** `core/` knows nothing about `bridge/`. `bridge/` knows nothing about `audio/`. `audio/` knows nothing about `api/`. Violations break the architecture.
7. **Test on the target platform.** If you're implementing a Windows backend change, validate with `cargo check --features feat_windows` at minimum.
8. **Phase 5 is the frontier.** New work should focus on breadth expansion: async streams, better device enumeration, additional sinks, performance optimization.
9. **When in doubt, ask.** If a design decision isn't covered by the architecture docs, surface it rather than guessing.
10. **macOS is tested and working.** All capture modes verified on macOS 26 Tahoe. The 3-path fallback in [`tap.rs`](src/audio/macos/tap.rs) handles API differences across macOS 14.4–26. See [macOS Version Compatibility](docs/MACOS_VERSION_COMPATIBILITY.md) for the full compatibility matrix.
