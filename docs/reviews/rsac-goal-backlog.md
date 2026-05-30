# rsac Executable Goal Backlog

Capture-only mandate: every task is additive/non-breaking, touches no DSP/mixing/resampling/encoding, and never allocates/locks/blocks on the OS audio callback thread. DAG honored: `core/ → bridge/ → audio/ → api/ → lib.rs`.

After cross-epic dedup, 38 seed tasks remain. Duplicates merged: CaptureTarget FromStr/Display (Ergonomics-H1 ≡ Bindings-H1), AudioBuffer metering (Ergonomics-H2 ≡ Observability-H2 ≡ Bindings-H2 core half), stream_stats/format/StreamStats (Ergonomics ≡ Observability cluster ≡ Bindings-H3), windowed BackpressureReport (Performance ≡ Observability-M6; Ergonomics-M6 deferred-windowing variant dropped), AudioError::user_message (Ergonomics-M5 ≡ Observability-M5), prelude (Ergonomics ≡ Bindings), tracing (Ergonomics-L1 ≡ Observability-L1).

---

## Epic: Observability — make a running capture knowable & debuggable

### P0
- **Surface bridge counters + producing-state on the CapturingStream trait** — default-0 accessors `buffers_captured/buffers_pushed/buffers_dropped`; BridgeStream overrides read BridgeShared atomics Relaxed. Prerequisite for stream_stats + windowed backpressure.
- **Add start_instant uptime tracking to AudioCapture** — single Instant store on the control path; set once on first real start(), cleared in stop()/Drop.
- **Add AudioCapture::format() exposing the negotiated delivery format** — zero-cost metadata peek; the bridge already publishes negotiated_format.
- **Implement AudioCapture::stream_stats() + complete the StreamStats struct (H3)** — `#[non_exhaustive]`, add buffers_captured/dropped/pushed + uptime + dropped_ratio(); honors the broken doc promise at introspection.rs:326.

### P1
- **Add zero-alloc AudioBuffer metering (rms/peak/dBFS, channel-strided, NaN-safe) (H2)** — single source of truth; deletes cli.rs rms_level, pipewire_test calculate_rms, rsac-python hand-rolled rms/peak.
- **Add RT-safe windowed drop-rate tracking + BackpressureReport (M6)** — fixed inline `[AtomicU64; N]` window, Relaxed adds on push; read-side BackpressureReport; legacy bool unchanged.
- **Add AudioError::user_message() -> UserFacingError with remedy hints (M5, exhaustive)** — no `_` arm; carries summary/remedy/recoverability/kind.

### P2
- **Add per-buffer stream-relative timestamps (push_samples_or_drop_at)** — alloc-free delegating overload so observers can measure latency/jitter.

### P3
- **Add optional `tracing` feature with lifecycle spans/events (L1)** — `rsac_event!`/`rsac_span!` falls back to log:: when off; capture_id correlation; never on the RT path.
- **Criterion benchmark + observability snapshot for stats/backpressure read path** — guards that read-path stays cheap and the RT producer stays alloc-free with windowed counters on.

---

## Epic: Ergonomics & Helpers — trivial-to-use, feature-rich

### P1
- **Add rsac::prelude module with explicit re-exports** — `use rsac::prelude::*;`; feature-gated identically to lib.rs.
- **Implement CaptureTarget FromStr + TryFrom<&str> + Display round-trip (H1)** — exhaustive mini-grammar; the upstream symbol every binding + builder.target_str delegates to.

### P2
- **Add AudioCaptureBuilder::target_str() string-target setter** — feeds a raw string straight into the builder.
- **Add one-call AudioCaptureBuilder::start() -> RunningCapture RAII guard** — Deref/DerefMut + Drop-stop; no ManuallyDrop.
- **Add AudioCaptureBuilder::preflight() capability/format check before device resolution** — single-sources the cheap validations build() already runs.

### P3
- **Expose PlatformCapabilities::SUPPORTED_SAMPLE_RATES as a public const** — removes the private literal duplicated in build().
- **Add a `capture!` declarative macro for one-line builder construction** — flattens the builder chain; public-API only.

---

## Epic: Performance — zero-copy data plane, measured alloc-free RT, capacity tuning

### P0
- **Add criterion benchmark harness + benches/ scaffolding for the bridge data plane** — push throughput / round-trip latency / capacity sweep; the baseline all later perf tasks compare against.
- **Add allocation-counting test harness asserting push_samples_or_drop is alloc-free in steady state** — turns ADR-0001 prose into an enforced gate (tests/rt_alloc.rs, single-threaded).

### P1
- **Eliminate the consumer-side double-copy in BridgeConsumer::pop** — move the ring buffer to the user; recycle a spare off-RT-thread Vec.
- **Add a sample-domain SPSC ring with rtrb 0.3.4 write_chunk_uninit (feature `bridge-zerocopy`, default off)** — truly zero-copy producer; bump rtrb 0.3.3→0.3.4 (additive).

### P2
- **Add synchronous per-call overflow reporting to the producer push API** — `push_samples_reporting -> PushOutcome{pushed,dropped_this_call}`, additive, no alloc/lock.
- **Derive ring capacity from the negotiated device callback period** — `calculate_capacity_for_period`; wire WASAPI/PipeWire/CoreAudio; fall back to 64.
- **Cache-pad BridgeShared diagnostic atomics to remove false-sharing tail latency** — separate producer/consumer counters onto distinct cache lines.
- **Add a panic guard at the OS callback boundary in the producer push methods** — catch_unwind, log once, increment drop, transition to Error; no alloc on happy path.

---

## Epic: Platform-Enumeration — native PipeWire, device-change events, richer metadata, format parity, dep bumps

### P1
- **Bump wasapi 0.22→0.23 + adopt safe WAVEFORMATEXTENSIBLE blob parsing** — parse_from_blob_bytes (read_unaligned), rust-version=1.76; eliminates unaligned-read UB.
- **Add AudioDevice::kind() fallible default + uniform DeviceKind across all 3 backends** — additive provided method; Windows delegates to IMMEndpoint, Linux maps is_input/output, macOS probes scope.
- **Add DeviceInfo snapshot + AudioDevice::describe() + additive AudioSourceKind::Device kind field** — `#[non_exhaustive]` DeviceInfo; Option<DeviceKind> on the source.
- **Linux: native in-process PipeWire registry enumeration (H4 part 1 — devices + default)** — kills pw-cli/pw-dump subprocess fragility; Rc<RefCell> Fn closures + core.sync roundtrip; keep subprocess fallback gated.
- **Linux: native in-process PipeWire application enumeration replacing pw-dump (H4 part 2)** — registry Node globals via the crate::audio facade; PID-dedup preserved.

### P2
- **macOS: enumerate all devices with multi-format probing + richer metadata** — kAudioDevicePropertyStreams + AvailableVirtualFormats; supported_formats parity with Windows.
- **macOS: filter enumerate_audio_applications to processes actually producing audio** — kAudioHardwarePropertyProcessObjectList ∩ NSWorkspace; 14.4+ with fallback.
- **Linux: wire supported_formats() via PipeWire node enum_params (PR-5)** — advisory SPA_PARAM_EnumFormat discovery; never overrides connect-time negotiation; empty→vec![].
- **Add supports_device_change_notifications capability flag + honest macOS sub-14.4 fields** — explain_unsupported() with SCK as assessed-not-implemented.
- **Add DeviceEvent + DeviceWatcher types and DeviceEnumerator::watch() default (M10 surface)** — `#[non_exhaustive]` enum; provided trait default = PlatformNotSupported; inherent CrossPlatform dispatch.
- **Cross-platform enumeration non-empty / honest-failure test matrix + DeviceInfo round-trip** — assert_enumeration_honest! macro; Linux variant runs with pw-dump off PATH.

### P3
- **Windows: implement watch() via IMMNotificationClient (M10 Windows arm)** — bounded-channel hand-off to a non-COM helper thread; Drop unregisters + joins.
- **macOS: implement watch() via AudioObjectPropertyListener (M10 macOS arm)** — device-list + default listeners; diff snapshots; Drop removes every listener.
- **Linux: implement watch() via persistent PipeWire registry/metadata listener (M10 Linux arm)** — new persistent loop thread; flips Linux supports_device_change_notifications=true.
- **Bump transitive enumeration deps + remove serde_json from the native Linux path** — gated on H4; retire pw-dump JSON parse.

---

## Epic: Bindings & Packaging — complete, ergonomic, publishable C/Python/Node/Go

### P0
- **Fix rsac-python feature hygiene: target-conditional backends instead of hardcoded feat_macos** — build is broken off-macOS today.

### P1
- **Define target-conditional feature presets shared across rsac-ffi/rsac-napi/rsac-python** — each binding compiles exactly one host backend; documented convention.
- **Make rsac-ffi publishable to crates.io** — remove publish=false, add metadata, version-pin rsac dep, gate release.yml step after rsac.
- **Eliminate f64 expansion in NAPI AudioChunk: expose interleaved f32 via Float32Array** — zero-copy hot path.
- **Fix #30: replace unaligned f32→u8 reinterpret in Python to_bytes() with bytemuck::cast_slice** — removes the unsafe block.
- **Fix #28: harden Go callback path against use-after-free of the C buffer pointer** — clear-callback at C layer precedes cgo.Handle Delete; `go test -race` churn test.
- **Add FFI stats accessors: rsac_capture_stream_stats() + rsac_capture_format()** — repr(C) structs, catch()-guarded, regenerate rsac_generated.h + hand-mirror rsac.h.

### P2
- **Fix #29: SHA-pin all third-party GitHub Actions in ci.yml** — match release.yml convention.
- **Surface stream_stats()/format() in Python, Node, and Go bindings** — propagate the facade beyond overrun_count.
- **Add string-target convenience to all bindings via CaptureTarget::FromStr** — one string entry point per binding.
- **Add AudioBuffer metering exposure through bindings (Python/Node/Go)** — call core metering; delete rsac-python hand-rolled rms/peak.
- **Establish a versioning + ABI-change contract across rsac and the four bindings** — lockstep bump, ABI-major rule for rsac-ffi, rsac-go tagging.
- **Build PyPI wheels for rsac-python across the platform matrix (cibuildwheel/maturin, abi3-py39)** — manylinux/Windows/macOS + sdist via Trusted Publishing.

### P3
- **Add rsac::prelude exposure in bindings** — route binding import blocks through the prelude (depends on prelude task).
- **Add a cross-compilation build matrix + build instructions for rsac-go's static FFI lib** — CI builds librsac_ffi.a per platform; no committed binaries.
- **Add Python async context-manager cleanup + finalizer safety net** — __aenter__/__aexit__ + __del__ stop().