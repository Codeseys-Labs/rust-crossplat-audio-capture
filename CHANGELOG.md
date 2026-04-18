# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

### Changed

- Reorganized CI workflows:
  - Split into platform-specific files:
    - windows.yml for Windows audio tests
    - linux.yml for Linux audio tests (PipeWire/PulseAudio)
    - macos.yml for macOS audio tests
    - code-quality.yml for shared checks
  - Improved maintainability with modular structure
  - Added reusable workflow components
  - Fixed PipeWire setup in Linux workflow
- Linux device enumeration failures (cannot reach PipeWire via `pw-cli` or
  `pw-dump`) now return `AudioError::BackendError` with a descriptive
  message, matching the variants used by the Windows (WASAPI) and macOS
  (CoreAudio) backends. Callers pattern-matching for platform-specific
  recovery can now distinguish "backend busted" from "device really not
  there" on Linux. Cases where the backend is healthy but no matching
  device exists continue to return `AudioError::DeviceNotFound`.
- Relocated `is_under_backpressure()` from an inherent method on
  `BridgeStream<S>` to the `CapturingStream` trait, so every backend
  (WASAPI, PipeWire, CoreAudio) exposes backpressure signaling through
  the same dynamic-dispatch path used by `AudioCapture`. The inherent
  `BridgeStream::is_under_backpressure()` has been removed (see the
  Removed section below). Callers should invoke it via
  `AudioCapture::is_under_backpressure()` or through the trait.
- Strengthened `ci_audio` integration test assertions alongside the
  existing no-panic backbone: `test_stream_start_read_stop` now checks
  that returned buffers match the requested sample rate and channels and
  that `num_frames() * channels() == data().len()`;
  `test_capture_format_correct` asserts `overrun_count()` is monotonically
  non-decreasing across successive reads. The graceful-fallthrough
  pattern for heterogeneous CI hardware is preserved.

### Deprecated

- `CapturingStream::close()` is now a no-op default method. New code
  should rely on `stop()` plus `Drop` for stream teardown. The trait
  method is retained for one minor-version cycle to ease migration and
  will be removed in a future release.

### Removed (Breaking)

- `CrossPlatformDeviceEnumerator::get_default_device()` no longer takes
  a `DeviceKind` argument — the parameter was silently ignored by every
  backend and has been dropped. Migration: remove the argument at the
  call site (e.g. `enumerator.get_default_device(DeviceKind::Output)`
  becomes `enumerator.get_default_device()`).
- `BridgeStream::is_under_backpressure()` is no longer available as an
  inherent method; the trait-backed dispatch path via
  `CapturingStream::is_under_backpressure()` is now the only entry
  point. Migration: call through `AudioCapture::is_under_backpressure()`
  or bring `CapturingStream` into scope and invoke
  `stream.is_under_backpressure()` via the trait.

### Technical Debt
