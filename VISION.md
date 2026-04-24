# rsac — Vision & Scope

> A streaming-first, cross-platform audio capture library for Rust —
> from any source, into any downstream.

## One-line Positioning

**rsac** is the only Rust library that captures per-application and
per-process-tree audio, per-device input, and whole-system audio —
from a single unified API, across Windows, macOS, and Linux —
without forcing the consumer into a specific downstream (recording,
streaming transcription, DSP, or mixing).

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
| `Application(ApplicationId)` | One app by PID (Windows: process ID; macOS: PID → CATapDescription; Linux: PipeWire node) | ✅ all 3 platforms |
| `ApplicationByName(String)` | One app by name substring (case-insensitive) — convenience wrapper | ✅ macOS, partial Windows/Linux |
| `ProcessTree(ProcessId)` | A parent process AND all descendants (follows fork/exec) | ✅ all 3 platforms |

### The output contract

- `CapturingStream::subscribe() -> mpsc::Receiver<AudioBuffer>` — the
  canonical pull-model interface. Downstream consumers can:
  - Record to disk (write `AudioBuffer.data` as WAV/FLAC via `hound`,
    `symphonia`, etc.)
  - Stream to a transcription service (Whisper, Deepgram, AssemblyAI,
    Gemini Live)
  - Run DSP in-flight (filtering, VAD, feature extraction)
  - Forward over WebSocket / gRPC
- `BridgeStream<S>::is_under_backpressure() -> bool` — lets consumers
  throttle / switch downstreams when the ring buffer is filling.
- Zero-copy where the backend allows it (ring buffer → consumer
  without intermediate Vec).

### Multi-source

- Multiple `AudioCapture` instances can run simultaneously in the
  same process.
- Each has its own isolated `BridgeStream` + ring buffer, so they
  cannot interfere.
- Example use case: one capture for `SystemDefault` (for recording),
  another for `Application(chrome)` (for transcription).

### Cross-platform parity

- Same `AudioBuffer` shape on all 3 platforms (f32 interleaved,
  sample_rate + channels).
- Same error taxonomy (`AudioError` enum — 7 `ErrorKind` categories).
- Same feature flags (`feat_windows` / `feat_linux` / `feat_macos`)
  — but platform is also gated by `#[cfg(target_os = ...)]`, so
  cross-compilation behaves predictably.

## What's Out of Scope (by design)

rsac is a **capture** library, not a **DSP** library. The following
are explicitly deferred to downstream crates:

| Out-of-scope concern | Use instead |
|---|---|
| Stream mixing (combining 2+ captures into 1 output) | `rodio::Source::mix` or a custom `f32 + f32` adder on top of rsac's buffers |
| Resampling | `samplerate`, `rubato`, `libsoxr-sys` |
| Encoding (MP3, AAC, Opus) | `hound` (WAV), `symphonia` (decode), `opus` crate |
| Playback | `cpal`, `rodio` |
| Audio effects (compression, EQ, reverb) | `fundsp`, `camilladsp`, `dasp` |
| Voice-activity detection | `voice_activity_detector`, `webrtc-vad` |
| Acoustic echo cancellation | `speexdsp-sys`, platform-native libs |

### Why not own mixing?

Mixing requires downstream-specific decisions: (a) what sample-rate
to mix at (resampling cost), (b) per-source gain, (c) clipping /
limiter strategy, (d) real-time vs. buffered. These belong to the
application, not the capture layer. rsac exposes `AudioBuffer.data: Vec<f32>` —
if you want to mix two captures, it's 3 lines:
```rust
let mixed: Vec<f32> = buf_a.data.iter().zip(&buf_b.data).map(|(a, b)| a + b).collect();
```

If a downstream crate like `rsac-mixer` emerges, we'll link it from
docs — but it won't be in the core.

## What's On the Roadmap (explicit backlog, not promises)

- **Alpine musl wheels** (rsac#19) — once PipeWire runtime linkage
  is validated on Alpine containers.
- **docs.rs rendering verification** (rsac#16) — one-shot post-publish
  check via `scripts/verify-docs-rs.sh`.
- **abi3-py39** for Python bindings (deferred per rsac#18 decision,
  adopted post-v0.2.0) — shrinks the PyPI wheel matrix from 15 → 3
  jobs.
- **`CaptureTarget::FromStr`** for CLI-friendly string parsing.
- **`rsac::prelude`** module for one-line imports.
- **Linux `supported_formats()` query** — PipeWire exposes this,
  we just haven't wired it through.

Each of these has a GitHub issue on `Codeseys-Labs/rust-crossplat-audio-capture`.

## How We Verify the Vision

### Unit tests (on every commit)

- **Default matrix** (`.github/workflows/ci.yml`): 3 platforms × (lint
  + unit tests + bindings check + downstream audio-graph build).
  All 298+ library tests + 22 ci_audio integration tests
  (subscribe, process_tree, ApplicationByName, ApplicationByPID)
  gated to `#[ignore]` or `require_audio!()` so CI doesn't need real
  audio hardware.

### Integration tests with real audio (gated)

- **`.github/workflows/ci-audio-tests.yml`** (846 lines): 9-job
  matrix (3 platforms × 3 modes: system / device / process) with
  virtual audio sources (PipeWire dummy sink on Linux, VB-CABLE on
  Windows, loopback via BlackHole or platform-native on macOS).
- Runs on `workflow_dispatch` + tagged releases (not every push —
  slow, requires audio runtime).

### Runner-specific

- **Blacksmith 4vcpu/6vcpu runners** (Linux, Windows, macOS) are
  preferred over GitHub-hosted for speed + audio subsystem support.
- `.github/workflows/blacksmith-audio-probe.yml` — diagnostic that
  confirms audio devices are visible on Blacksmith hosts.

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

_Last revised: 2026-04-24_
