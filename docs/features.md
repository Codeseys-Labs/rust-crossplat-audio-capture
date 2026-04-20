# rsac Cargo Feature Matrix

This document enumerates every Cargo feature exposed by `rsac`, what it enables, and what host dependencies it requires. Features are declared in [`Cargo.toml`](../Cargo.toml).

## Summary Table

| Feature | In `default`? | Platforms | Enables | Requires |
|---|---|---|---|---|
| `feat_windows` | yes | Windows | WASAPI backend: system + loopback + per-process + process-tree capture, WASAPI device enumeration, session enumeration | Windows 10+ host. No extra system packages (WASAPI ships with the OS). Rust target `x86_64-pc-windows-msvc` or `x86_64-pc-windows-gnu`. |
| `feat_linux` | yes | Linux | PipeWire backend: system + per-app + process-tree capture via monitor streams, PipeWire device enumeration, `pw-dump` node resolution | `libpipewire-0.3-dev`, `libspa-0.2-dev`, `pkg-config`, `clang`/`libclang-dev`, `llvm-dev`. Runtime: PipeWire 0.3.44+ daemon active. |
| `feat_macos` | yes | macOS | CoreAudio backend: system capture, per-process + process-tree capture via Process Tap, aggregate device construction, `NSWorkspace` application enumeration | Xcode Command Line Tools. Process Tap requires **macOS 14.4+**. Screen Recording (TCC) permission required at capture time. |
| `default` | — | all | Meta-feature: `["feat_windows", "feat_linux", "feat_macos"]` | Each enabled backend's requirements above. |
| `async-stream` | no | all | `AudioCapture::audio_data_stream()` returning a `futures_core::Stream<Item = AudioResult<AudioBuffer>>`; also required by `examples/async_capture.rs` | Pulls in `atomic-waker` dep. Consumer needs an async runtime (Tokio, smol, etc.). |
| `sink-wav` | no | all | `WavFileSink` adapter (writes captured audio to a WAV file through the `AudioSink` trait) | `hound` is always a hard dependency, so no extra install — the gate is API-surface only. |
| `test-utils` | no | all | Re-exports test helpers used by integration tests and external binding crates | None. Used internally by `tests/` and the `bindings/rsac-*` workspace members. |

## Platform-feature semantics

The three `feat_*` flags are a two-way gate: code inside a platform backend is compiled **only** when both `target_os` matches and the feature is on. See `src/audio/mod.rs` for the `cfg(all(target_os = "…", feature = "feat_…"))` guards.

Consequences:

- Building on Linux with `--no-default-features --features feat_windows` compiles nothing from `src/audio/windows/` (the `target_os` check fails), so `get_device_enumerator()` will return `AudioError::PlatformNotSupported`.
- Cross-compiling to a target without the matching feature enabled produces the same error.
- On the correct host, disabling all three feature flags yields a library that compiles but cannot enumerate or capture — `get_device_enumerator()` always errors. This is only useful for doc/test builds.

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

# Full feature surface (async + WAV sink)
cargo build --features "async-stream sink-wav"
```

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
