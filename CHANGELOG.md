# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

In addition to the standard Keep-a-Changelog subsections, each release that
touches the C ABI carries a dedicated **`### C ABI changes`** subsection.
This records every change to the `rsac-ffi` exported `extern "C"` symbols or
the generated `rsac.h` header that affects binary compatibility — symbol
removals/renames, signature or `#[repr(C)]` layout changes, and changed
return/error-code semantics. Per the
[versioning & ABI contract](docs/RELEASE_PROCESS.md#versioning--abi-contract),
any such change is a **MAJOR** bump for the FFI surface; the subsection tells
consumers who pin the `.so`/`.dll`/`.dylib` exactly what to recompile against.
Releases with no ABI change omit the subsection (or state "No C ABI changes").

## [Unreleased]

Correctness-focused fixes from the 2026-05-29 deep-dive audit (waves 1–2),
closing the real-time-safety, callback-delivery, and error-classification
findings. Three Architecture Decision Records were recorded alongside the code:
[ADR-0001 (RT-allocation guarantee)](docs/designs/0001-rt-allocation-guarantee.md),
[ADR-0002 (callback delivery)](docs/designs/0002-callback-delivery.md), and
[ADR-0003 (terminal stream error)](docs/designs/0003-terminal-stream-error.md).

### Added

- `AudioError::StreamEnded { reason }` (ADR-0003) — a `Fatal`, `ErrorKind::Stream`
  variant emitted when a read is attempted on a stream that has reached a terminal
  state (`Stopped` / `Closed` / `Error`). Distinguishes a clean end-of-stream from
  the recoverable `StreamReadError`, so read loops that break on `is_fatal()`
  terminate instead of busy-waiting. `AudioError` now has **22** variants (was 21).
- Push-callback delivery is now wired (ADR-0002): a callback registered via
  `set_callback` is invoked from a dedicated non-RT pump thread spawned by
  `start()` (mirroring `subscribe()`). The FFI trampoline is wrapped in
  `catch_unwind` so a panicking C callback cannot unwind across the boundary.

### Changed

- **RT producer is now allocation-free in steady state** (ADR-0001). Fixed the
  `push_samples_or_drop` scratch-shrink bug (the scratch `Vec` could collapse to
  capacity 0 and then re-allocate on the audio callback thread) and sized the
  seed/scratch buffers from a named worst-case-period constant so recycled buffers
  converge to zero allocation. Documented the guarantee precisely: allocation-free
  in steady state, with bounded one-time growth during warm-up or when the period
  grows.
- `PlatformCapabilities::query()` is now gated on the `feat_*` features so it
  cannot claim support for a backend that was not compiled in.
- `recoverability()` now uses an exhaustive `match` (no `_` catch-all): adding a
  new `AudioError` variant forces a compile error until its recoverability is
  classified deliberately.

### Fixed

- `buffers_iter()` no longer terminates prematurely on a transient empty read,
  and no longer drops buffers still queued in the ring after `stop()` — the
  buffered tail is drained before the iterator ends.
- `ChannelSink` is now bounded (back-pressure instead of unbounded memory growth),
  and `WavFileSink` finalizes the WAV header on drop.
- Removed the unsound manual `Send`/`Sync` impls on `AudioBuffer` (it is auto-`Send`/
  `Sync` via its fields) and replaced ad-hoc `eprintln!` diagnostics with the `log`
  facade.
- Removed a reachable `panic!` in the `AudioCapture` `Debug` impl; calling `start()`
  on an already-stopped stream now returns an error instead of misbehaving.

### Deprecated

### Removed

### Security

### C ABI changes

No C ABI changes. (See the note at the top of this file for when this
subsection is required and what it must record.)

## [0.2.0] - 2026-04-18

Trait-level backpressure signaling, unified Linux backend errors, broader
`ci_audio` coverage (ApplicationByName + ApplicationByPID), and docs that
explain macOS enumeration scope. One breaking change on the device
enumerator (see Removed).

### Added

- `ci_audio` integration coverage for application-scoped capture:
  `application_by_name.rs` exercises `CaptureTarget::ApplicationByName`
  across backends, and `application_by_pid.rs` exercises
  `CaptureTarget::ApplicationByPID`, including a CI existence-check path
  so heterogeneous runners don't fail when the target app isn't present.
- README section describing macOS enumeration scope — clarifies that
  `list_audio_sources()` / `list_audio_applications()` return a superset
  (every GUI app) on macOS vs. the "currently producing audio" set that
  WASAPI session enumeration and PipeWire `pw-dump` yield on Windows and
  Linux, and that Process Tap attachment is the only way to observe the
  live per-process audio graph.

### Changed

- Relocated `is_under_backpressure()` from an inherent method on
  `BridgeStream<S>` to the `CapturingStream` trait, so every backend
  (WASAPI, PipeWire, CoreAudio) exposes backpressure signaling through
  the same dynamic-dispatch path used by `AudioCapture`. Callers should
  invoke it via `AudioCapture::is_under_backpressure()` or through the
  trait. (See Removed for the inherent-method deletion.)
- Linux device enumeration failures (cannot reach PipeWire via `pw-cli`
  or `pw-dump`) now return `AudioError::BackendError` with a descriptive
  message, matching the variants used by the Windows (WASAPI) and macOS
  (CoreAudio) backends. Callers pattern-matching for platform-specific
  recovery can now distinguish "backend busted" from "device really not
  there" on Linux. Cases where the backend is healthy but no matching
  device exists continue to return `AudioError::DeviceNotFound`.
- Strengthened `ci_audio` integration test assertions alongside the
  existing no-panic backbone: `test_stream_start_read_stop` now checks
  that returned buffers match the requested sample rate and channels and
  that `num_frames() * channels() == data().len()`;
  `test_capture_format_correct` asserts `overrun_count()` is monotonically
  non-decreasing across successive reads. The graceful-fallthrough
  pattern for heterogeneous CI hardware is preserved.
- Reorganized CI workflows into platform-specific files (`windows.yml`,
  `linux.yml`, `macos.yml`) plus a shared `code-quality.yml`, with
  reusable workflow components and a fixed PipeWire setup on Linux.
- `cargo fmt` baseline applied repo-wide; `clippy` cleanups across
  `src/core/introspection.rs` and backend code (including the Rust 1.95
  `collapsible_match` / `manual_checked_ops` lints) so the workspace
  builds cleanly on the pinned toolchain.

### Deprecated

- `CapturingStream::close()` is now a no-op default method. New code
  should rely on `stop()` plus `Drop` for stream teardown. The trait
  method is retained for one minor-version cycle to ease migration and
  will be removed in a future release.

### Removed

- **Breaking:** `CrossPlatformDeviceEnumerator::get_default_device()` no
  longer takes a `DeviceKind` argument — the parameter was silently
  ignored by every backend and has been dropped. Migration: remove the
  argument at the call site (e.g.
  `enumerator.get_default_device(DeviceKind::Output)` becomes
  `enumerator.get_default_device()`).
- **Breaking:** `BridgeStream::is_under_backpressure()` is no longer
  available as an inherent method; the trait-backed dispatch path via
  `CapturingStream::is_under_backpressure()` is now the only entry
  point. Migration: call through `AudioCapture::is_under_backpressure()`
  or bring `CapturingStream` into scope and invoke
  `stream.is_under_backpressure()` via the trait.

## [0.1.0]

Initial public pre-release: cross-platform audio capture with WASAPI
(Windows), PipeWire (Linux), and CoreAudio Process Tap (macOS 14.4+)
backends, ring-buffer data plane, and trait-based `CapturingStream` API.
