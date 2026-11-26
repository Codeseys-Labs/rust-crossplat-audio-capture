# rsac — Vision & Scope

> An application-aware desktop audio capture and session library for Rust,
> with experimental, parked mobile backends.

## One-line Positioning

**rsac** brings system, device, application and process-tree capture under
one API for Windows, macOS and Linux, without prescribing recording,
transcription, DSP or other downstream processing. Platform capabilities
and verification differ; see [desktop refinement](docs/DESKTOP_REFINEMENT.md).

## Problem We Solve

Most audio libraries on Rust (`cpal`, `portaudio-rs`) give you the
device-level primitive: "open this input, read f32 samples." That's
insufficient for modern apps that need:

1. **Per-application capture** — record Firefox's audio without
   recording Slack, or transcribe Zoom without a virtual-cable hack.
2. **Process-tree capture** — record a parent process AND all its
   spawned children (e.g. a browser and its renderer subprocesses).
3. **Simultaneous multi-source capture** — capture a microphone and
   system audio at the same time, to the same pipeline, without
   hacks like pactl / Loopback.
4. **Clean stream passthrough** — hand captured audio to downstream
   code (recording, in-flight transcription, real-time processing,
   forwarding to a cloud service) without the library dictating
   how that happens.

These capabilities exist natively in each OS, but the APIs are
wildly different:
- **Windows 10 21H1+**: WASAPI Process Loopback (`AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS`)
- **macOS 14.4+**: CoreAudio Process Tap (`CATapDescription`, private API, exposed via Objective-C)
- **Linux (PipeWire ≥ 0.3.44)**: PipeWire node-level monitor ports

rsac wraps all three behind a single `AudioCaptureBuilder → AudioCapture → CapturingStream`
pipeline.

## What's In Scope (v0.x)

### Capture sources (CaptureTarget variants)

| Variant | What it captures | Status |
|---|---|---|
| `SystemDefault` | Whole-system output (loopback of the default sink) | ✅ all 3 platforms |
| `Device(DeviceId)` | A specific input or loopback device | ✅ all 3 platforms |
| `Application(ApplicationId)` | One app by numeric PID string (resolved to each backend's native capture target) | ✅ all 3 platforms |
| `ApplicationByName(String)` | One app by exact name (case-insensitive) — convenience wrapper | ✅ all 3 platforms |
| `ProcessTree(ProcessId)` | A parent process AND all descendants (follows fork/exec) | ✅ all 3 platforms |

### The output contract

- `AudioCapture::read_buffer() -> AudioResult<Option<AudioBuffer>>` — the
  canonical pull-model interface (consumer asks; producer fills the ring).
  `Ok(None)` means "no data yet" (not end-of-stream); a terminal stream is
  signalled by an `Err` carrying a fatal `AudioError` (use
  `AudioError::is_fatal()` / `recoverability()` to decide retry-vs-stop).
  `AudioCapture::subscribe() -> mpsc::Receiver<AudioBuffer>` provides the
  push-based delivery mode on top of the same ring. Downstream consumers can:
  - Record to disk (write `AudioBuffer::data()` samples as WAV/FLAC via
    `hound`, `symphonia`, etc.)
  - Stream to a transcription service (Whisper, Deepgram, AssemblyAI,
    Gemini Live)
  - Run DSP in-flight (filtering, VAD, feature extraction)
  - Forward over WebSocket / gRPC
- **Ergonomic lifecycle** — `AudioCaptureBuilder::start() -> RunningCapture`
  returns an RAII guard that `Deref`s to the full `AudioCapture` surface and
  calls `stop()` on `Drop`, so "build, start, use, tear down" is one call. The
  `capture!` macro (`capture!(system, rate: 48000)`) is a one-line builder, and
  `rsac::prelude::*` re-exports the everyday surface (the macro,
  `RunningCapture`, `CaptureTarget`, `AudioBuffer`, errors, …) in a single
  import. String targets are first-class: `CaptureTarget` round-trips through
  `FromStr` / `TryFrom<&str>` / `Display`, and the builder exposes
  `target_str()` (fallible) plus `try_target_str()` (infallible best-effort).
- **Read-only level metering on `AudioBuffer`** — `rms()`, `peak()`,
  `rms_dbfs()`, `peak_dbfs()`, and the per-channel `channel_rms()` /
  `channel_peak()` are allocation-free and `#[inline]`, so they are safe to call
  on the audio callback thread. These are observability metadata, **not** DSP —
  they read the buffer and never mutate it.
- **Diagnostics** — `CapturingStream::is_under_backpressure() -> bool` (also
  exposed as `AudioCapture::is_under_backpressure()`) lets consumers throttle /
  switch downstreams when the ring buffer is filling. `AudioCapture::stream_stats()
  -> StreamStats` is a cheap point-in-time snapshot (buffers pushed / captured /
  dropped, uptime, running state, negotiated-format description) and
  `backpressure_report() -> BackpressureReport` adds a **windowed** drop-rate view
  (since 0.4.0, rsac-cfe4): `pushed` / `dropped` / `drop_rate` over a bounded recent
  window read from the producer's alloc-free sliding ring, an estimated `window`
  span, and the carried consecutive-drop flag — so a sustained 1-in-N loss the
  consecutive bool resets away is still surfaced. Both are `#[non_exhaustive]`.
- **Device-change watching** — `DeviceEnumerator::watch(on_event) ->
  DeviceWatcher` (reachable from the `CrossPlatformDeviceEnumerator` facade)
  delivers `DeviceEvent`s (add / remove / default-changed) on the backend's OS
  notification thread, never the RT audio thread. The returned `DeviceWatcher` is
  an RAII guard that unregisters the listener on `Drop`. **Per-platform divergence
  (intentional, documented):** Windows and macOS hand events off through a
  bounded channel + helper thread (drop-on-full backpressure), while Linux
  invokes the handler directly on the PipeWire loop thread — see
  [`docs/designs/`](docs/designs/) for the device-watch threading ADR.
- **Buffer timestamps** — `AudioBuffer::timestamp() -> Option<Duration>` is
  populated on **all three backends** with a *stream-position* stamp: frames
  offered so far ÷ delivered rate, computed with pure integer math on the RT
  push path (no clock syscall; ADR-0001 unchanged), and producer-side drops
  surface as *gaps* between consecutive timestamps. Composed buffers (the
  `compose` feature) carry the same stream-position semantics. Note this is
  stream position, not wall-clock time — periods where the OS delivers
  nothing do not advance it. An upgrade to per-backend device clocks (WASAPI
  `GetPosition`, PipeWire `pw_time`, CoreAudio `mSampleTime`) remains tracked
  (rsac-ec25).
- The default hot path is **alloc-free in steady state** (the producer reuses
  ring slots via a free-list return ring — see
  [`docs/designs/0001-rt-allocation-guarantee.md`](docs/designs/0001-rt-allocation-guarantee.md)),
  with one owned `AudioBuffer` materialized per delivered chunk on the non-RT
  consumer side. (An opt-in zero-copy `SampleRing` producer plane was prototyped
  and A/B-benchmarked behind a `bridge-zerocopy` feature but removed — it lost
  end-to-end in every measured environment; see
  [`docs/designs/0006-bridge-zerocopy-samplering.md`](docs/designs/0006-bridge-zerocopy-samplering.md).)

### Multi-source

- Multiple `AudioCapture` instances can run simultaneously in the
  same process.
- Each has its own isolated `BridgeStream` + ring buffer, so they
  cannot interfere.
- Example use case: one capture for `SystemDefault` (for recording),
  another for `Application(chrome)` (for transcription).
- With the opt-in `compose` feature, multiple sources can also be
  **composed into one multi-channel stream** — groups of sources mixed to
  mono/stereo channels or kept at their negotiated channel width, appended in
  declaration order (see "Channel composition" below and
  [ADR-0011](docs/designs/0011-compose-feature.md)).

### Cross-platform parity

- Same `AudioBuffer` shape on all 3 platforms (f32 interleaved,
  sample_rate + channels).
- Same error taxonomy (`AudioError` enum — 7 `ErrorKind` categories).
- Same feature flags (`feat_windows` / `feat_linux` / `feat_macos`)
  — but platform is also gated by `#[cfg(target_os = ...)]`, so
  cross-compilation behaves predictably.
- Same device-introspection surface — `AudioDevice::describe() -> DeviceInfo`
  and `AudioDevice::supported_formats() -> Vec<AudioFormat>` are implemented on
  all three backends (WASAPI, PipeWire, CoreAudio), so format discovery no
  longer differs by platform.
- **Bindings at parity** — the C/Go (`rsac-ffi` + cgo), Python (PyO3), and
  Node (napi) bindings expose the same surface: `stream_stats()`, format query,
  metering, string targets, and idiomatic context managers / RAII. Python ships
  a single `cp39-abi3` wheel (CPython stable ABI, 3.9+).

Android and iOS are experimental and parked, not desktop-parity promises.
Their features remain enabled by default; compile, emulator/simulator and
physical-device evidence are distinct. See
[AGENTS.md §3](AGENTS.md#3-current-state) and the
[mobile freeze checklist](docs/DESKTOP_REFINEMENT.md#mobile-freeze-and-unfreeze).

## What's Out of Scope (by design)

rsac is a **capture** library, not a **DSP** library. The following
are explicitly deferred to downstream crates:

| Out-of-scope concern | Use instead |
|---|---|
| Resampling (as a general service) | `samplerate`, `rubato`, `libsoxr-sys` — rsac uses `rubato` *internally* only for `compose`-feature rate alignment |
| Encoding (MP3, AAC, Opus) | `hound` (WAV), `symphonia` (decode), `opus` crate |
| Playback | `cpal`, `rodio` |
| Audio effects (compression, EQ, reverb) | `fundsp`, `camilladsp`, `dasp` |
| Voice-activity detection | `voice_activity_detector`, `webrtc-vad` |
| Acoustic echo cancellation | `speexdsp-sys`, platform-native libs |

### Channel composition (the `compose` feature) — a deliberate scope change

Earlier revisions of this document declared stream mixing out of scope.
That stance was **amended** by
[ADR-0011](docs/designs/0011-compose-feature.md) (2026-07-04): multi-source
**channel composition** is now in scope, behind the opt-in `compose` cargo
feature. A `CompositionBuilder` takes *groups* of `CaptureTarget`s; each group
either mixes down to Mono/Stereo (gain-weighted plain summation) or passes a
single source's negotiated channels through, and the groups append — in
declaration order — into one interleaved-f32 multi-channel stream speaking the
same `CapturingStream` contract as a single capture. Desktop sources currently
request two channels; passthrough does not promise the device's native width.
Sources at a different negotiated rate are resampled (via `rubato`) to the
session rate on a dedicated non-RT compositor thread.

Why the change: heterogeneous rates (Windows process loopback cannot
autoconvert) and cross-source alignment (app taps go silent; sources start at
different times) are problems only the capture layer sees clearly — every
downstream was going to re-solve them, badly. What did **not** change: rsac
still ships no effects, no limiter, no encoding, and no general-purpose DSP.
With the feature off, the dependency graph and API are exactly as before. For
one-off mixing of two homogeneous buffers, the 3-line downstream adder still
works:

```rust
let mixed: Vec<f32> = buf_a.data().iter().zip(buf_b.data()).map(|(a, b)| a + b).collect();
```

Live source gain/mute and group gain are already available on `Composition`.
For one-source rate alignment, use a single `Group` with `.keep_channels()` in
a `CompositionBuilder` at the desired session rate. This incurs a compositor
thread and quantum buffering; it is not a general resampling service.

For recording or encoding, implement `AudioSink` downstream and use `drain_to`;
codec adapters can live in companion crates. For a voice pipeline, compose
microphone and loopback as separate groups and pass them downstream for VAD,
echo cancellation and reference policy. Compose does not promise hardware-clock
alignment. See [companion-crate seams](docs/DESKTOP_REFINEMENT.md#companion-crate-seams).

## Recently Shipped (was on the roadmap, now in-scope)

The following were "roadmap" items in earlier revisions and have since landed;
they are documented above as part of the in-scope surface:

- **`CaptureTarget::FromStr` / `TryFrom<&str>` / `Display`** — round-trip
  string parsing for CLI-friendly and FFI-friendly targets.
- **`rsac::prelude`** — one-import module re-exporting the everyday surface.
- **`capture!` macro** and **`RunningCapture` RAII** — the one-line build path.
- **`AudioBuffer` level metering** (`rms`/`peak`/`*_dbfs`/`channel_*`).
- **Built-in sink driver — shipped.** `drain_to(sink)` pumps the bundled
  `AudioSink` adapters (`NullSink` / `ChannelSink` / `WavFileSink`) on a
  background thread from `AudioCapture`, `RunningCapture`, and `Composition`
  (`compose`) — no hand-rolled read loop. The earlier `pipe_to` name is retired
  (rsac-2135).
- **`stream_stats()` / `backpressure_report()`** diagnostics.
- **Compose live controls and bindings** — per-source gain/mute and group gain;
  Python, Node and Go expose composition alongside the C FFI.
- **Ring-slot configuration** — all five backends honor `buffer_size`, including
  the fixed macOS production path; it is a slot count, not frames.
- **`DeviceWatcher` + `watch()`** device-change notifications (all 3 platforms).
- **Cross-platform `supported_formats()` / `describe()`** — including Linux
  (PipeWire) native device + app enumeration.
- **abi3-py39** Python wheels — a single `cp39-abi3` wheel replaces the
  per-version matrix (adopted within the 0.2.0 line; see
  [`docs/designs/abi3-decision.md`](docs/designs/abi3-decision.md)).

## What's On the Roadmap (explicit backlog, not promises)

- **Alpine musl wheels** (rsac#19) — once PipeWire runtime linkage
  is validated on Alpine containers.
- **docs.rs rendering verification** (rsac#16) — one-shot post-publish
  check via `scripts/verify-docs-rs.sh`.
- **`hotplug-recovery`** — default-off desktop default-device recovery; see
  [ADR-0019](docs/designs/0019-hotplug-auto-recovery.md) for scope, lifecycle,
  defaults and verification gates.
- **Device clocks** — stream-position timestamps are shipped; validated
  hardware-clock metadata remains future work, without changing compose alignment.
- **Period-aware ring sizing** — `calculate_capacity_for_period` is implemented
  but unwired pending effective per-backend period negotiation before allocation.
  Explicit slot requests already work on Windows, Linux, macOS, Android and iOS
  ([ADR-0007](docs/designs/0007-capacity-period-sizing.md#status-addendum--2026-11-25)).

Each of these is tracked on `Codeseys-Labs/rust-crossplat-audio-capture` and/or
in [`docs/reviews/`](docs/reviews/).

## How We Verify the Vision

### Routine and audio lanes

CI is manually dispatched. The main [`ci.yml`](.github/workflows/ci.yml) runs
desktop lint, unit, MSRV and binding checks scoped by the branch's diff; it and
the expensive virtual-audio and mobile runtime workflows run only when someone
dispatches them, plus stable release tags. A PR without a dispatched run has no
CI evidence, and a skipped hardware-gated test is not runtime evidence.

CI is hybrid: ordinary desktop lanes use Blacksmith runners; Windows and macOS
audio integration use GitHub-hosted runners, which are free for a public
repository and where the suite is proven. macOS Process Tap tests additionally require TCC
permission. See [CI audio testing](docs/CI_AUDIO_TESTING.md) and the
[v0.5 release bar](docs/DESKTOP_REFINEMENT.md#v05-desktop-bar).

### Post-publish verification

- **`scripts/verify-docs-rs.sh`** — one-command HTTP probe of
  docs.rs rendering after `cargo publish`.
- `docs/RELEASE_PROCESS.md` — canonical release procedure, links
  all 3 registry workflows (crates.io / npm / PyPI).

## Design Principles

1. **One API surface** — `AudioCaptureBuilder` is the only public
   facade. No escape hatches into platform-specific types.
2. **Pull model by default** — consumer asks, producer fills the
   ring buffer. No callback threading quirks.
3. **Lock-free hot path** — `rtrb` SPSC ring buffer; OS audio
   thread never holds a user-visible lock.
4. **Error-first** — every fallible operation returns `AudioResult<T>`.
   No panics in library code.
5. **Platform-honest** — `PlatformCapabilities::query()` tells you
   what's actually supported before you build a capture, so user
   code can branch on "does this OS/OS-version actually have process
   taps?"

## References

- **README.md** — user-facing quickstart + install
- **CHANGELOG.md** — version history
- **docs/features.md** — Cargo feature flag matrix
- **docs/troubleshooting.md** — common platform issues
- **docs/architecture/** — per-backend design docs
- **docs/RELEASE_PROCESS.md** — release procedure
- **docs/reviews/rsac-architecture-audit.md** — most recent
  architecture audit (verdict: HEALTHY)

---

_Last revised: 2026-11-25_
