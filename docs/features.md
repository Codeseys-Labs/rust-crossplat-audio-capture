# rsac Cargo Feature Matrix

This document enumerates every Cargo feature exposed by `rsac`, what it enables, and what host dependencies it requires. Features are declared in [`Cargo.toml`](../Cargo.toml).

## Summary Table

| Feature | In `default`? | Platforms | Enables | Requires |
|---|---|---|---|---|
| `feat_windows` | yes | Windows | WASAPI backend: system + loopback + per-process + process-tree capture, WASAPI device enumeration, session enumeration | Windows 10+ host. No extra system packages (WASAPI ships with the OS). Rust target `x86_64-pc-windows-msvc` or `x86_64-pc-windows-gnu`. |
| `feat_linux` | yes | Linux | PipeWire backend: system + per-app + process-tree capture via monitor streams, PipeWire device enumeration, `pw-dump` node resolution | `libpipewire-0.3-dev`, `libspa-0.2-dev`, `pkg-config`, `clang`/`libclang-dev`, `llvm-dev`. Runtime: PipeWire 0.3.44+ daemon active. |
| `feat_macos` | yes | macOS | CoreAudio backend: system capture, per-process + process-tree capture via Process Tap, aggregate device construction, `NSWorkspace` application enumeration | Xcode Command Line Tools. Process Tap requires **macOS 14.4+**. Audio Capture (`kTCCServiceAudioCapture`) TCC permission required at capture time — distinct from Screen Recording. |
| `default` | — | all | Meta-feature: `["feat_windows", "feat_linux", "feat_macos"]` | Each enabled backend's requirements above. |
| `async-stream` | no | all | `AudioCapture::audio_data_stream()` returning a `futures_core::Stream<Item = AudioResult<AudioBuffer>>`; also required by `examples/async_capture.rs` | Pulls in `atomic-waker` dep. Consumer needs an async runtime (Tokio, smol, etc.). |
| `sink-wav` | no | all | `WavFileSink` adapter (writes captured audio to a WAV file through the `AudioSink` trait) | `hound` is always a hard dependency, so no extra install — the gate is API-surface only. |
| `tracing` | **no** | all | Routes the internal `rsac_event!` / `rsac_span!` instrumentation macros to the [`tracing`](https://docs.rs/tracing) facade and makes `rsac::install_default_tracing()` available. With the feature **off**, the same macros expand to `log::` calls (behavior-identical), so logging works either way — this flag only changes the *backend*. | Pulls in the `tracing` facade crate only (no `tracing-subscriber`); the consumer installs their own subscriber. |
| `bridge-zerocopy` | **no** | all | Compiles the opt-in sample-domain SPSC ring (`SampleRing` producer/consumer) that writes interleaved `f32` straight into the ring via `rtrb`'s `write_chunk_uninit` + `CopyToUninit`, avoiding the per-buffer `Vec`/`AudioBuffer` allocation. **Currently A/B-benchmarked only** — see the note below. | None extra (uses the existing `rtrb` dep). |
| `test-utils` | no | all | Re-exports test helpers used by integration tests and external binding crates | None. Used internally by `tests/` and the `bindings/rsac-*` workspace members. |

## Platform-feature semantics

The three `feat_*` flags are a two-way gate: code inside a platform backend is compiled **only** when both `target_os` matches and the feature is on. See `src/audio/mod.rs` for the `cfg(all(target_os = "…", feature = "feat_…"))` guards.

Consequences:

- Building on Linux with `--no-default-features --features feat_windows` compiles nothing from `src/audio/windows/` (the `target_os` check fails), so `get_device_enumerator()` will return `AudioError::PlatformNotSupported`.
- Cross-compiling to a target without the matching feature enabled produces the same error.
- On the correct host, disabling all three feature flags yields a library that compiles but cannot enumerate or capture — `get_device_enumerator()` always errors. This is only useful for doc/test builds.

## Data-plane and observability features

### `bridge-zerocopy` — opt-in sample-domain ring (benchmark-only today)

`bridge-zerocopy` compiles a second, parallel data plane: `SampleRingProducer` /
`SampleRingConsumer` (in `src/bridge/ring_buffer.rs`, all gated behind
`#[cfg(feature = "bridge-zerocopy")]`). Instead of allocating one `AudioBuffer`
(a `Vec<f32>`) per callback, the producer copies the interleaved `f32` samples
directly into the ring's uninitialised slots with `rtrb`'s `write_chunk_uninit`
+ `CopyToUninit`, with no per-buffer allocation.

**Honest status: implemented and tested, but not wired into any backend.** No
code in `src/audio/` constructs a `SampleRing` — the WASAPI, PipeWire, and
CoreAudio capture threads all push into the default `AudioBuffer` ring. The
zero-copy plane is exercised only by the A/B comparison in `benches/bridge.rs`.
Enabling the feature therefore compiles the extra types but does **not** change
the runtime path of a real capture. The default path is *allocation-free in
steady state* (see ADR-0001 and [`PERFORMANCE.md`](PERFORMANCE.md)); the
literal *zero-copy* promise is delivered only by this not-yet-wired plane.

### `tracing` — structured instrumentation backend switch

rsac instruments its non-RT control paths with two internal macros,
`rsac_event!` and `rsac_span!` (defined in `src/trace.rs`). They are a
dual-backend shim:

- **Feature off (default):** the macros expand to `log::` calls. No extra
  dependency beyond `log`, which is always present.
- **Feature on:** the macros emit `tracing` events/spans, and
  `rsac::install_default_tracing()` becomes available. rsac depends on the
  `tracing` *facade* only — it deliberately does **not** pull in
  `tracing-subscriber`; the consumer installs whatever subscriber/filter they
  want (the built-in default uses `NoSubscriber`).

Either way, these macros are for control-plane events (build, start, stop,
device-watch) and must never be invoked on the RT audio-callback thread.

## Recommended invocations

```bash
# Typical dev build — all backends, current platform does the work
cargo build

# Linux-only build (CI-style, skip Windows/macOS backends you can't link anyway)
cargo build --no-default-features --features feat_linux

# Unit tests, no hardware, on Linux
cargo test --lib --no-default-features --features feat_linux

# Enable async Stream API for Tokio consumers
cargo build --features async-stream

# Structured tracing instead of `log`
cargo build --features tracing

# A/B-benchmark the opt-in zero-copy sample ring
cargo bench --bench bridge --features bridge-zerocopy

# Full feature surface (async + WAV sink + tracing)
cargo build --features "async-stream sink-wav tracing"
```

## Binding feature-resolution convention (canonical)

This is the **one** pattern every `rsac` language binding
(`rsac-ffi`, `rsac-napi`, `rsac-python`, and any future binding) follows
to select the host audio backend. New bindings copy this verbatim; it is
the single source of truth referenced by all four manifests.

**Rule:** a binding depends on `rsac` with `default-features = false` and
selects exactly the one backend matching the build target via
`[target.'cfg(...)'.dependencies.rsac]` blocks. This guarantees a Linux
build never compiles (or links) the Windows/CoreAudio backends or pulls
their OS-only system crates, and vice-versa — important because the
`feat_*` flags are a two-way gate (see "Platform-feature semantics"
above), so a wrong-OS backend would be dead code that still bloats the
dependency graph.

```toml
# Canonical per-target backend selection for a binding's Cargo.toml.
# Each table enables exactly the backend that matches the host platform;
# default-features = false keeps the other two backends (and their
# system deps) out of the build.

[target.'cfg(windows)'.dependencies.rsac]
path = "../.."           # or a published `version = "X.Y.Z"`
default-features = false
features = ["feat_windows"]

[target.'cfg(target_os = "linux")'.dependencies.rsac]
path = "../.."
default-features = false
features = ["feat_linux"]

[target.'cfg(target_os = "macos")'.dependencies.rsac]
path = "../.."
default-features = false
features = ["feat_macos"]
```

Bindings that re-export passthrough features (e.g. `sink-wav`,
`async-stream`) declare a mirroring `[features]` entry, exactly as
`rsac-ffi` does:

```toml
[features]
default = []
sink-wav = ["rsac/sink-wav"]
# (feat_* are selected per-target above, not listed here)
```

### All-platform / docs.rs builds

There is **no separate `all-backends` feature** and none is needed: the
crate's existing `default = ["feat_windows", "feat_linux", "feat_macos"]`
meta-feature is the all-backends opt-in. Because `feat_*` is two-way
gated on `target_os`, turning all three on still compiles only the host
backend on any single runner — so a docs.rs-style `--all-features`
(or `default`) build is safe everywhere and is what
`[package.metadata.docs.rs]` (`all-features = true`) relies on. To force
all three backends *on* from a binding (e.g. a deliberate
`cargo doc --all-features` of the binding crate), depend on `rsac`
without `default-features = false`, or add a binding-local
`all-backends = ["rsac/feat_windows", "rsac/feat_linux", "rsac/feat_macos"]`
feature.

### Per-binding status

| Binding | Conforms? | Notes |
|---|---|---|
| `rsac-python` | yes | Uses the `[target.'cfg(...)'.dependencies.rsac]` blocks above with `default-features = false`. Reference implementation. |
| `rsac-ffi` | yes (variant) | Mirrors `rsac`'s `feat_*` through its own `[features]` table and depends on `rsac` with `default-features = false`; consumers pass `--features feat_<os>`. Equivalent end state — only the host backend compiles. |
| `rsac-napi` | migrating | Historically depended on `rsac` with implicit defaults (all three backends, bloating non-host builds). Should adopt the per-target blocks above so a Linux/Windows/macOS build pulls only its backend. |

> The manifest edits that bring `rsac-napi` (and align `rsac-ffi`) onto
> this pattern live with the crate-owning change; this document is the
> convention those manifests point at.

## Binaries / examples gated by features

Some binaries require a specific feature to build (see `Cargo.toml`):

- `pipewire_diagnostics` — `feat_linux`
- `wasapi_session_test` — `feat_windows`
- `examples/async_capture.rs` — `async-stream`

All other `[[bin]]` and `[[example]]` entries compile under the default feature set.

## What is *not* behind a feature flag

The following are always compiled and have no opt-out:

- Core types (`AudioBuffer`, `CaptureTarget`, `AudioError`, `PlatformCapabilities`, `StreamConfig`)
- `BridgeStream<S>` lock-free ring-buffer bridge (`rtrb`)
- `NullSink`, `ChannelSink`
- Sample-rate / channel validation in `AudioCaptureBuilder`
- `hound` WAV dependency (only the `WavFileSink` type is gated, not the crate)
- `sysinfo`, on Windows and macOS, for PID resolution by process name

## Version note

This matrix reflects `rsac` at the 0.2.0 release line. Future provider-architecture work may add feature flags for cloud-backed capture providers — those will be listed here as they land.

`rsac` and its bindings bump in lockstep on every semver tag, and any
change to the `rsac-ffi` C ABI is a MAJOR bump for the FFI surface. See
the [versioning & ABI contract](RELEASE_PROCESS.md#versioning--abi-contract)
in the release process for the full policy, the CHANGELOG `### C ABI changes`
convention, and the `rsac-go` tag shape.
