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

### Added

- **Windowed backpressure report (`backpressure_report()`).** A bounded,
  recent-window view of producer drop activity, complementing the lifetime
  counters in `stream_stats()`. Unlike the consecutive-drop
  `is_under_backpressure` flag (which resets on any successful push), the
  windowed `drop_rate` surfaces a *sustained* 1-in-N loss pattern. Backed by an
  alloc-free, lock-free fixed ring of `(pushed, dropped)` slots the producer
  advances on every push path (rsac-cfe4). Exposed across the whole surface:
  - Rust: `AudioCapture::backpressure_report() -> BackpressureReport`
    (`window`, `pushed`, `dropped`, `drop_rate`, `is_under_backpressure`).
  - C FFI: `rsac_capture_backpressure_report()` filling `RsacBackpressureReport`.
  - Go: `capture.BackpressureReport()`; Python: `capture.backpressure_report()`;
    Node: `capture.backpressureReport()`.
- **macOS spontaneous device-death detection (ADR-0010).** A
  `kAudioDevicePropertyDeviceIsAlive` listener on the captured device drives the
  bridge to a terminal `Error` state when a device/tap dies *without* a
  `stop()`/`Drop` (e.g. the interface is unplugged), so a parked blocking reader
  observes a fatal `StreamEnded` instead of hanging indefinitely (rsac-ead3).

### Changed

- `AudioCapture::backpressure_report()` now reports a *windowed* view with a
  populated `window` span (estimated from the buffer size and negotiated sample
  rate), instead of lifetime counters with a zero `window`. The public
  `BackpressureReport` shape is unchanged.

### Fixed

- `cargo doc --all-features` is clean again: cross-module/cfg-divergent doc
  references now use plain backticks rather than intra-doc method-path links
  that require the target type to be in scope.
- CI: the `softprops/action-gh-release` SHA is now unified across `release.yml`
  and `release-tag.yml` (#33); `subscribe()` integration coverage hard-asserts
  the 440 Hz tone content under a deterministic source (#34).

### C ABI changes

- **Added** (backward compatible — new symbol, no removals or layout changes):
  `rsac_capture_backpressure_report(const RsacCapture*, RsacBackpressureReport*)`
  and the `RsacBackpressureReport` value type (`window_secs`, `pushed`,
  `dropped`, `drop_rate`, `is_under_backpressure`). Existing symbols are
  unchanged, so consumers pinning the `.so`/`.dll`/`.dylib` need not recompile;
  this is a **MINOR** bump for the FFI surface.

## [0.3.0] - 2026-05-30

Two threads of work landed since 0.2.0. First, correctness-focused fixes from the
2026-05-29 deep-dive audit (waves 1–2) closed the real-time-safety,
callback-delivery, and error-classification findings. Second, a six-wave feature
program built out the capture-API surface — buffer metering, stream stats,
device-change watching, native PipeWire/CoreAudio enumeration, the `capture!`
macro and `rsac::prelude`, Python `abi3` wheels, and cross-platform Go CI — while
holding the capture-only scope (no DSP/mixing/resampling/encoding/playback) and the
RT-safety guarantee. Nine Architecture Decision Records now back these decisions:
[ADR-0001 (RT-allocation guarantee)](docs/designs/0001-rt-allocation-guarantee.md),
[ADR-0002 (callback delivery)](docs/designs/0002-callback-delivery.md),
[ADR-0003 (terminal stream error)](docs/designs/0003-terminal-stream-error.md),
ADR-0004 through ADR-0008 (device-watch threading & RAII teardown, the
[bridge-zerocopy `SampleRing`](docs/designs/0006-bridge-zerocopy-samplering.md)
data plane, period-derived ring sizing & `buffer_size` semantics, and the
CachePadded false-sharing mitigation),
and [ADR-0009 (tracing/log instrumentation shim)](docs/designs/0009-tracing-log-shim.md).
(See `docs/designs/` for ADR-0004–0008; sequential numbers are coordinated across
the parallel ADR set.)

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
- **`AudioBuffer` level metering** — zero-allocation, RT-safe, `#[inline]` read-only
  observability metadata (not signal processing): `rms()`, `peak()`, `rms_dbfs()`,
  `peak_dbfs()`, plus the channel-strided `channel_rms()` / `channel_peak()`
  variants. NaN-safe; computed on demand from the buffer's existing samples.
- **`StreamStats` and `BackpressureReport`** (`#[non_exhaustive]`) observability
  snapshots, surfaced via `AudioCapture::stream_stats()`. `StreamStats` carries
  buffers captured/dropped/pushed, uptime, and a guarded `dropped_ratio`;
  `BackpressureReport` is assembled from inline atomics with zero-division guards
  and honestly documents its current lifetime-window limitation. Bridge counters
  (`buffers_captured` / `buffers_dropped` / `is_producing`) are exposed as default
  methods on the `CapturingStream` trait so every backend reports through the same
  path. A criterion bench (`benches/observability.rs`) proves the read path is
  cheap, non-locking, and alloc-free.
- **`CaptureTarget` string round-tripping**: `FromStr`, `TryFrom<&str>`, and
  `Display`, round-tripping the canonical forms `system` / `device:<id>` /
  `app:<pid>` / `name:<n>` / `tree:<pid>` (case-insensitive schemes; colon-split
  preserves device ids like `hw:0,0`).
- **Builder ergonomics & RAII** on `AudioCaptureBuilder`: `target_str(&str)` (parse
  a canonical target string — the CLI/config counterpart to `with_target`);
  `preflight()` (validate capabilities, the supported-sample-rate whitelist, and the
  channel range before device resolution); and `start() -> RunningCapture`, an RAII
  guard that `Deref`/`DerefMut`s to `AudioCapture` and stops the stream on `Drop`
  (idempotent; `into_inner()` escapes the guard without stopping).
- **`capture!` declarative macro** for one-line builder construction
  (`capture!(system)`, `capture!(app: pid)`,
  `capture!(device: id, rate: 48000, channels: 2)`, `target_str: "…"`).
- **`rsac::prelude`** module re-exporting the common surface — including `capture!`,
  `RunningCapture`, and `DeviceInfo` — so `use rsac::prelude::*;` is a one-import
  setup.
- **`DeviceInfo` + `AudioDevice::describe()`**: a `#[non_exhaustive]` device
  descriptor and an infallible `describe()` default method composed from the
  existing accessors; additive `Option<DeviceKind>` on `AudioSourceKind::Device`.
- **Device hot-plug / default-change watching (M10)**: `DeviceEvent`
  (`#[non_exhaustive]`; `DeviceAdded` / `DeviceRemoved` / `DefaultChanged` /
  `StateChanged`), the `DeviceWatcher` RAII guard (runs its backend teardown exactly
  once on `Drop`), `DeviceEventHandler`, and `DeviceEnumerator::watch()` (a provided
  trait method defaulting to `PlatformNotSupported`). Per-OS arms now back it:
  Windows registers an `IMMNotificationClient`, macOS an
  `AudioObjectAddPropertyListener`, and Linux a persistent PipeWire registry/metadata
  listener. The handler runs on the OS notification thread, never the RT audio
  callback thread (per-platform delivery model recorded in the device-watch ADR;
  `supports_device_change_notifications` reports honestly per backend).
- **Native, subprocess-free platform enumeration**: Linux enumerates devices and
  audio-active applications via an in-process PipeWire registry (replacing
  `pw-cli`/`pw-dump`, with the subprocess fallback retained) and populates
  `supported_formats()` from the node's `EnumFormat` params; macOS enumerates output
  devices with multi-format probing and filters application enumeration to processes
  actually emitting audio (macOS 14.4+, graceful pre-14.4 fallback).
- **Optional `tracing` instrumentation** (default off; ADR-0009): the `rsac_event!`
  and `rsac_span!` macros emit `tracing` events/spans with `--features tracing` and
  fall back to the always-present `log::` facade when off (a span degrades to an
  event). The feature pulls in only the `tracing` facade (no `tracing-subscriber`);
  `install_default_tracing()` is a best-effort, idempotent convenience for
  binaries/examples. Control-plane only — these macros are prohibited on the RT
  audio callback / sample-push path.
- **Bridge data plane**: `calculate_capacity_for_period(period_frames, channels)`, a
  pure function deriving ring capacity from the negotiated device callback period
  (backends adopt it later — see the ring-sizing ADR); an opt-in `bridge-zerocopy`
  feature providing a sample-domain SPSC `SampleRing` written via `rtrb` 0.3.4
  `write_chunk_uninit` + `CopyToUninit` (default off; A/B'd in `benches/bridge.rs` —
  see the bridge-zerocopy ADR for status and promotion criteria); and a criterion
  bench harness (`benches/bridge.rs`) for producer throughput and push→pop latency.
- **Cross-language binding parity** with the Rust ground-truth surface — Python
  (PyO3), Node (napi-rs), and Go (cgo) all gained `stream_stats()`/`format()`, buffer
  metering (`rms`/`peak`/`rms_dbfs`/`peak_dbfs` and channel variants),
  target-from-string, and context-manager / RAII ergonomics. Python ships a single
  CPython `abi3` (`abi3-py39`) wheel per platform covering 3.9–3.13; napi carries u64
  counters as `BigInt` and f32 samples as `Float32Array`; Go copies borrowed C
  buffers into Go memory before dispatch.
- `tests/rt_alloc.rs` (`CountingAllocator` harness proving `push_samples_or_drop` is
  alloc-free in steady state, ADR-0001) and `tests/enumeration_matrix.rs`
  (cross-platform "honest failure" enumeration + `DeviceInfo` round-trip contract,
  device-free in headless CI).

### Changed

- **BREAKING (SemVer): four public enums are now `#[non_exhaustive]`** —
  `AudioError`, `CaptureTarget`, `AudioSourceKind`, and `PermissionStatus`. They
  are expected to grow, so downstream `match` expressions on them must now carry a
  trailing wildcard (`_ =>`) arm; adding a variant in a future minor release will
  no longer be a breaking change. The deliberately **closed** enums —
  `SampleFormat`, `DeviceKind`, `ErrorKind`, and `Recoverability` — are documented
  as stable, exhaustively-matchable sets that will not grow. In-crate exhaustive
  matches (e.g. `AudioError::recoverability()` / `user_message()`) are unaffected
  and intentionally remain exhaustive to keep forcing classification of every
  variant.
- **Terminal-read signaling now crosses the C FFI** (fixes a Wave-B/Wave-C
  interaction): `rsac_capture_read` / `rsac_capture_try_read` now read via the
  terminal-observable path so a stopped stream surfaces the fatal
  `RSAC_ERROR_STREAM_FAILED` (from `StreamEnded`) once drained, instead of a
  recoverable `RSAC_ERROR_STREAM_READ`. Binding pumps (Go/Node) that branch on
  recoverability now end cleanly on stop instead of spinning.
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
- **CachePadded diagnostic atomics** (ADR-0007): the producer- and consumer-written
  diagnostic counters are wrapped in a `#[repr(align(64))]` `CachePadded<T>` newtype
  to kill false sharing on the RT push path. Transparent `Deref` keeps every call
  site unchanged; the `rt_alloc` gate confirms no new allocation.
- `PlatformCapabilities::SUPPORTED_SAMPLE_RATES` is now a public const (the single
  source of truth the builder `preflight` whitelist references).
- Bumped `wasapi` 0.22 → 0.23 (safe `WAVEFORMATEXTENSIBLE` blob parse,
  `get_device`/`get_device_format`), establishing a `rust-version` floor of 1.87.

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
- Eliminated the consumer-side double-copy in the bridge `pop` path.
- Windows device-watch teardown pins its own `Arc<ComInitializer>` clone so the MTA
  apartment outlives the watcher even when the borrowed enumerator is dropped right
  after `watch()` returns.

### Deprecated

### Removed

### Security

### C ABI changes

**Additive only — no removals, renames, or layout changes to existing symbols.**
The `rsac-ffi` surface gained, in support of the binding-parity work above:

- `rsac_capture_stream_stats(capture, out: *mut RsacStreamStats) -> rsac_error_t`
  and `rsac_capture_format(capture, out: *mut RsacAudioFormat) -> rsac_error_t` —
  out-param accessors filling the new `#[repr(C)]` `RsacStreamStats` /
  `RsacAudioFormat` structs (both null-checked and `catch_unwind`-wrapped).
- `AudioBuffer` metering accessors over `RsacAudioBuffer`:
  `rsac_audio_buffer_rms`, `rsac_audio_buffer_peak`, `rsac_audio_buffer_rms_dbfs`,
  and `rsac_audio_buffer_peak_dbfs` (each returns `f32`; null-safe — the linear
  `rms`/`peak` accessors return `0.0` on a null buffer, while the `*_dbfs`
  accessors return `f32::NEG_INFINITY` (silence) on null, matching their
  silence-floor semantics).
- `rsac_builder_set_target_str(builder, spec: *const c_char) -> rsac_error_t` —
  set the capture target from a canonical target string.

The curated `rsac.h` and the vendored Go header were synced with these additions.
Because the changes only add symbols and `#[repr(C)]` types (no existing symbol
removed, renamed, re-signed, or re-laid-out), pinned `.so`/`.dll`/`.dylib`
consumers remain binary-compatible; consumers that want the new accessors recompile
against the updated `rsac.h`.

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
