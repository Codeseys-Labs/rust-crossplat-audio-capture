# Consuming rsac from Downstream Projects

> The one-stop map of **every public surface rsac ships** and how a downstream
> project consumes it. This guide is an index — each section gives you the
> working install/link recipe and deep-links to the authoritative doc instead
> of duplicating it. For framework-specific integration (Tauri, Dioxus,
> Electron, Deno, Bun, Flutter, mobile) see
> [`FRAMEWORK_COMPATIBILITY.md`](FRAMEWORK_COMPATIBILITY.md).

## Which surface do I want?

| You are building… | Use | Section |
|---|---|---|
| A Rust application/library (incl. Tauri or Dioxus desktop) | the `rsac` crate | [Rust crate](#rust-crate) |
| A C / C++ / anything-with-C-interop program | `rsac-ffi` (cdylib/staticlib + `rsac.h`) | [C FFI](#c-ffi) |
| A Python tool or pipeline | the `rsac` Python package (PyO3) | [Python](#python) |
| A Node.js / Bun / Electron (main-process) / Deno 2 app | `@rsac/audio` (napi) | [Node.js / Bun](#nodejs--bun) |
| A Go program | `rsac-go` (CGo over the C FFI) | [Go](#go) |
| A Flutter / .NET / Qt / other-runtime app | `rsac-ffi` via your runtime's C interop | [C FFI](#c-ffi) + [`FRAMEWORK_COMPATIBILITY.md`](FRAMEWORK_COMPATIBILITY.md) |
| A quick recording/monitoring tool with no code | the `rsac` CLI demo | [CLI demo](#cli-demo--examples) |

All bindings sit on the same core and meet the same capture contract — the
parity matrix and per-binding error-delivery semantics are specified in
[`CROSS_LANGUAGE_BINDINGS.md`](CROSS_LANGUAGE_BINDINGS.md#shipped-binding-parity-020).

## Publish status (read this first)

**rsac is not yet published to crates.io / PyPI / npm** — the release
automation exists but the first publish is pending. Until then:

- **Rust**: the git dependency is the working recipe (below).
- **Python / Node**: build from source (`maturin develop`, `napi build`) per
  the binding READMEs; `pip install rsac` / `npm install @rsac/audio` are
  forward-looking.
- **Go**: consume by path — the module is not tagged yet
  ([`bindings/rsac-go/README.md`](../bindings/rsac-go/README.md)).

Current version: **0.4.0**, MSRV **1.87**, license MIT OR Apache-2.0.
Supported platforms: Windows 10 2004+ (WASAPI), Linux (PipeWire 0.3.44+),
macOS (CoreAudio; Process Tap features need 14.4+). Per-platform system
dependencies: [README § Platform Dependencies](../README.md#platform-dependencies).

## Rust crate

```toml
[dependencies]
rsac = { git = "https://github.com/Codeseys-Labs/rust-crossplat-audio-capture" }
```

Ninety-second tour (full reference: [`docs/API.md`](API.md)):

```rust
use rsac::prelude::*;

let mut capture = AudioCaptureBuilder::new()
    .with_target(CaptureTarget::SystemDefault) // or .target_str("name:Firefox")?
    .sample_rate(48000)
    .channels(2)
    .build()?;
capture.start()?;
while let Some(buf) = capture.read_buffer()? {
    println!("{} frames, {:.1} dBFS", buf.num_frames(), buf.rms_dbfs());
}
```

### Choosing consumption surfaces

rsac deliberately offers several delivery models — pick per use case
([`API.md § Consuming audio`](API.md#consuming-audio)):

| Surface | Shape | When |
|---|---|---|
| `read_buffer()` / `read_chunk_nonblocking()` / `read_chunk_blocking()` | pull | you own the loop |
| `buffers_iter()` | blocking iterator | simple linear consumers |
| `subscribe()` / `subscribe_with_errors()` | `mpsc` channel push | decoupled worker threads; the `_with_errors` variant also delivers recoverable errors and the terminal cause |
| `set_callback()` | callback push | thin integrations; set before `start()` |
| `audio_data_stream()` | async `Stream` (`async-stream` feature) | tokio/async consumers |
| `RunningCapture::drain_to(sink)` | sink drain | "just write it somewhere" — pairs with the sink adapters |

Error handling is contract-driven: **recoverable errors never end a stream;
only the fatal terminal does** (ADR-0003/0010). Branch on
`AudioError::is_recoverable()` / `kind()` / `user_message()` (which gives
UI-ready summary + remedy text) rather than matching variants — the enum is
`#[non_exhaustive]`.

### Cargo features a consumer picks

Canonical matrix with host-dependency notes: [`features.md`](features.md).

| Feature | One-liner |
|---|---|
| `feat_windows` / `feat_linux` / `feat_macos` (default: all three) | platform backends; two-way gated with `target_os`, so cross-platform consumers can just use defaults |
| `async-stream` | `audio_data_stream()` async `Stream` |
| `compose` | multi-source channel composition (`CompositionBuilder` → one interleaved multi-channel stream; [ADR-0011](designs/0011-compose-feature.md), [`API.md § Composing`](API.md#composing-multiple-sources)) |
| `sink-wav` | `WavFileSink` adapter |
| `tracing` | route internal events to the `tracing` facade |
| `cli` | the demo binary's deps — never needed by library consumers |
| `test-utils` | mock-backend helpers for your tests |
| `bridge-zerocopy` | benchmark-only alternative data plane — **not** a consumer perf switch |

### Discovery, capabilities, and diagnostics

- **Sources**: `list_audio_sources()`, `list_audio_applications()`,
  `AudioSource::to_capture_target()` — cross-platform, no `#[cfg]` needed
  ([`API.md § Source discovery`](API.md#source-discovery-and-diagnostics)).
- **Devices**: `get_device_enumerator()` → `enumerate_devices()`,
  `default_device()` (canonical spelling; `get_default_device()` is a
  deprecated alias), `watch(handler)` for hot-plug/default-change events.
- **Capabilities**: `PlatformCapabilities::query()` — honest per-platform
  support flags (incl. `requires_user_consent` for the planned mobile
  backends). Check capability first; handle permission and per-target
  resolution errors at `build()`/`start()`.
- **Permission**: `check_audio_capture_permission()` (macOS TCC et al).
- **Runtime health**: `stream_stats()` (lifetime counters),
  `backpressure_report()` (windowed drop rate), `overrun_count()` — all cheap,
  lock-free reads ([`PERFORMANCE.md`](PERFORMANCE.md#backpressure-and-diagnostics)).

### Sinks and ecosystem interop

`AudioSink` adapters: `NullSink`, `ChannelSink`, `WavFileSink` (`sink-wav`).
Drive them from your own loop or `RunningCapture::drain_to()` (there is no
`pipe_to()` driver). Bridging `AudioBuffer` into dasp / cpal / rodio / hound /
symphonia: copy-paste recipes in [`INTEROP.md`](INTEROP.md).

## CLI demo & examples

```bash
cargo run --features cli -- info       # platform capabilities
cargo run --features cli -- list       # devices & applications
cargo run --features cli -- capture --app Firefox
cargo run --features cli -- record out.wav --duration 10
```

Examples (`cargo run --example <name> --features <req>`): `basic_capture`,
`record_to_file`, `verify_audio` (all `cli`), `list_devices` (no features),
`async_capture` (`async-stream`), `composed_capture` (`compose`).

## C FFI

Crate: [`bindings/rsac-ffi`](../bindings/rsac-ffi/README.md). Artifacts:
`cdylib` + `staticlib`; the consumer-facing header is the curated
[`include/rsac.h`](../bindings/rsac-ffi/include/rsac.h) (56 functions —
opaque handles, error codes + thread-local `rsac_error_message()`,
`catch_unwind` on every boundary, "Rust allocates, Rust frees").

Build with the backend for your OS (`default = []`):

```bash
cargo build -p rsac-ffi --release --features feat_linux   # or feat_windows / feat_macos
```

Per-OS link lines (full details + smoke test:
[README § Linking](../bindings/rsac-ffi/README.md#linking)):

- **Linux**: `-lrsac_ffi -lpipewire-0.3 -lspa-0.2 -lpthread -ldl -lm`
- **macOS**: `-lrsac_ffi -framework CoreAudio -framework AudioToolbox -framework CoreFoundation -framework Security -framework SystemConfiguration`
- **Windows (MSVC)**: `rsac_ffi.lib ole32.lib oleaut32.lib winmm.lib ksuser.lib uuid.lib`

Compose over C needs `--features compose` + `-DRSAC_FEATURE_COMPOSE`.

## Python

Package/import name **`rsac`** (PyO3 + maturin, one `cp39-abi3` wheel per
platform, Python ≥ 3.9). From source:
`pip install maturin && maturin develop --release` in `bindings/rsac-python`.

```python
import rsac
with rsac.AudioCapture(target=rsac.CaptureTarget.parse("name:Firefox")) as cap:
    for buf in cap:                      # ends cleanly on the fatal terminal
        print(buf.rms_dbfs())
```

Full tour (targets, stats, async context manager, exception hierarchy):
[`bindings/rsac-python/README.md`](../bindings/rsac-python/README.md).

## Node.js / Bun

Package **`@rsac/audio`** (napi-rs, Node ≥ 18; Bun-first build tooling; works
in Electron's main process; expected to work in Deno 2 via `npm:` — see
[`FRAMEWORK_COMPATIBILITY.md`](FRAMEWORK_COMPATIBILITY.md#deno-2)). From
source: `bun install && bun run build` in `bindings/rsac-napi`.

```ts
import { AudioCapture } from '@rsac/audio';
const capture = new AudioCapture({ sampleRate: 48000, channels: 2 });
capture.onData((chunk) => console.log(chunk.numFrames, chunk.rmsDbfs));
capture.start();
```

`u64` counters cross as `BigInt`; samples as `Float32Array`. Full tour:
[`bindings/rsac-napi/README.md`](../bindings/rsac-napi/README.md).

## Go

Module `github.com/Codeseys-Labs/rsac-go` (Go ≥ 1.22), CGo over the C FFI —
**consume by path until it is tagged**. `make build` compiles the staticlib
and the Go package; Windows requires the `x86_64-pc-windows-gnu` Rust target
+ MinGW (MSVC `.lib` won't link under cgo).

```go
capture, _ := rsac.NewCaptureBuilder().WithTargetString("name:Firefox").Build()
defer capture.Close()
capture.Start()
for buf := range capture.Stream(ctx) { fmt.Println(buf.RMSDbfs()) }
```

Full tour: [`bindings/rsac-go/README.md`](../bindings/rsac-go/README.md).

## Mobile (Android / iOS) — compiled surface growing, nothing device-verified

The mobile backends are code-complete: `feat_android`/`feat_ios` compile
AAudio / AVAudioEngine **microphone** backends
(`CaptureTarget::Device(DeviceId("default"))`), the iOS **`SystemDefault`
broadcast path** (ReplayKit ring consumer; configure
`AudioCaptureBuilder::with_ios_app_group(…)` and embed the `mobile/ios`
RsacBroadcastKit extension), and Android **playback capture** — all four
tiers of what `SystemDefault`/`Application*`/`ProcessTree` mean on Android
per ADR-0013, via the AAR's Kotlin loop + JNI ingest (needs API 29+, a
`with_android_projection` consent token, and the AAR's foreground service).
First-party glue ships in `mobile/{android,ios}/` and builds in CI,
including `librsac.so` packaged into the AAR. Honesty status:
**compile-checked cross-targets only — no runtime verification on any
device; do not ship mobile capture claims.** Per-app capture on iOS is
permanently unavailable. The consent surface: capabilities report
`requires_user_consent`, Android builds expose
`AudioCaptureBuilder::with_android_projection(AndroidProjectionToken)` (C FFI:
`rsac_builder_set_android_projection`), and consent-gated targets without
their artifact fail `build()` with `AudioError::UserConsentRequired`. Full
design + status: [`MOBILE_BACKEND_DESIGN.md`](MOBILE_BACKEND_DESIGN.md).

## When something goes wrong

- Build/runtime fixes by symptom: [`troubleshooting.md`](troubleshooting.md)
- Verifying capture on a physical machine: [`LOCAL_TESTING_GUIDE.md`](LOCAL_TESTING_GUIDE.md)
- Behavior questions (data flow, threading contract, per-backend specifics):
  [`ARCHITECTURE.md`](ARCHITECTURE.md)
- RT/latency concerns and ring sizing: [`PERFORMANCE.md`](PERFORMANCE.md)
