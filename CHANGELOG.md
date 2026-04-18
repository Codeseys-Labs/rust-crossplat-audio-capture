# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

### Changed

### Deprecated

### Removed

### Fixed

### Security

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
