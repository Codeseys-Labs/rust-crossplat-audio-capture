# API Documentation

> **Scope:** a task-oriented tour of the **shipped** public API of `rsac`. Every
> type and signature here is grounded against the code under
> [`src/`](../src/). For the full design rationale and the platform-mapping
> tables see [`docs/architecture/API_DESIGN.md`](architecture/API_DESIGN.md); for
> the authoritative per-item docs run `cargo doc --open` (or browse
> [docs.rs/rsac](https://docs.rs/rsac)).
>
> `rsac` is **capture-first** — it captures system, per-application, and
> process-tree audio on Windows (WASAPI), Linux (PipeWire), and macOS (CoreAudio
> Process Tap, 14.4+). It does **no** general-purpose DSP, encoding, playback,
> VAD, or AEC; those remain non-goals (see [`VISION.md`](../VISION.md)). The
> one deliberate scope amendment is the opt-in **`compose` feature**
> ([ADR-0011](designs/0011-compose-feature.md)): multi-source *channel
> composition* — per-group Mono/Stereo mixdown or native-channel passthrough,
> with internal `rubato` resampling to align source rates — see
> [Composing multiple sources](#composing-multiple-sources-compose-feature)
> below.

## At a glance

```rust
use rsac::prelude::*;

fn main() -> AudioResult<()> {
    let mut capture = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48_000)
        .channels(2)
        .build()?;

    capture.start()?;
    loop {
        match capture.read_buffer()? {
            Some(buffer) => {
                let _frames = buffer.num_frames();
                let _level = buffer.rms_dbfs();   // RT-safe level metering
                // … process `buffer.data()` …
            }
            None => std::thread::sleep(std::time::Duration::from_millis(1)),
        }
        # break;
    }
    capture.stop()?;
    Ok(())
}
```

Or the one-liner via the `capture!` macro and the build-and-start RAII guard:

```rust
use rsac::prelude::*;
use rsac::capture;

let mut running = capture!(system, rate: 44_100, channels: 2).start()?; // RunningCapture
if let Some(buffer) = running.read_buffer()? { let _ = buffer.data().len(); }
// `running` stops the capture automatically when dropped.
```

## Core types

| Type / trait | Module | Purpose |
|---|---|---|
| `AudioCaptureBuilder` | `api` | Configure a capture (target + format) and `build()` / `start()` it. |
| `AudioCapture` | `api` | Lifecycle handle: `start`/`stop`, reads, `subscribe`, callback, diagnostics. |
| `RunningCapture` | `api` | RAII guard from `builder.start()`; `Deref`/`DerefMut` to `AudioCapture`, `Drop` stops it. |
| `CaptureTarget` | `core::config` | What to capture: `SystemDefault` / `Device` / `Application` / `ApplicationByName` / `ProcessTree`. |
| `StreamConfig`, `AudioFormat`, `SampleFormat` | `core::config` | Format/buffer configuration. |
| `AudioBuffer` | `core::buffer` | Interleaved `f32` chunk + format metadata + metering. |
| `AudioError`, `AudioResult` | `core::error` | 22-variant error taxonomy + result alias. |
| `CapturingStream`, `AudioDevice`, `DeviceEnumerator` | `core::interface` | Backend traits (advanced use). |
| `DeviceInfo`, `DeviceEvent`, `DeviceWatcher` | `core::interface` | Device metadata + hot-plug watching. |
| `StreamStats`, `BackpressureReport`, `AudioSource` | `core::introspection` | Diagnostics + source discovery. |
| `AudioSink`, `NullSink`, `ChannelSink`, `WavFileSink` | `sink` | Downstream sink adapters (wired by the consumer). |

## Building a capture

`AudioCaptureBuilder` is a chainable builder. All fields default to
`CaptureTarget::SystemDefault`, 48 kHz, 2 channels, F32, no buffer-size
preference.

```rust
let builder = AudioCaptureBuilder::new()
    .with_target(CaptureTarget::Application(ApplicationId("1234".into())))
    .sample_rate(48_000)        // one of 22050,32000,44100,48000,88200,96000
    .channels(2)                // 1..=32
    .sample_format(SampleFormat::F32)
    .buffer_size(Some(1024));   // Option<usize>; None = backend default
```

### String-driven targeting

```rust
let b = AudioCaptureBuilder::new().target_str("app:1234")?; // fallible; builder unchanged on error
let b = AudioCaptureBuilder::new().try_target_str("name:VLC"); // infallible; keeps prior target on error
```

The grammar (case-insensitive scheme): `system` / `default`, `device:<id>`
(first-colon split, so `device:hw:0,0` keeps `hw:0,0`), `app:<id>`,
`name:<name>`, `tree:<pid>` / `pid:<pid>`. `CaptureTarget` also implements
`Display`, `FromStr`, and `TryFrom<&str>` directly, round-tripping exactly.

### Preflight and build

```rust
builder.preflight()?;            // cheap, device-independent validation (no device opened)
let capture = builder.build()?;  // validate + resolve device + (non-Linux) negotiate format
```

`preflight()` rejects unsupported sample rates
(`InvalidParameter{param:"sample_rate"}`), bad channel counts
(`ConfigurationError`), and platform-unsupported targets (`PlatformNotSupported`).
`build()` runs `preflight()` first, so the two cannot drift. On non-Linux
platforms `build()` negotiates the closest supported format when the device does
not advertise the exact requested one (preferring F32 at the requested rate,
then F32 at the requested channel count, then any F32) rather than hard-failing.

### Build-and-start (RAII)

```rust
let mut running = builder.start()?;     // builds, starts, returns RunningCapture
// `running` derefs to AudioCapture; dropping it stops the stream.
let capture = running.into_inner();     // escape the guard without stopping
```

## The `AudioCapture` lifecycle

```rust
capture.start()?;          // create the OS stream (idempotent on a running stream;
                           //   errors on a stopped one — build a new AudioCapture)
capture.is_running();      // -> bool
capture.uptime();          // -> Option<Duration>, monotonic, None before start / after stop
capture.format();          // -> Option<AudioFormat>, negotiated delivery format
capture.config();          // -> &AudioCaptureConfig
capture.stop()?;           // stop the OS stream; idempotent
// Drop also best-effort stops a running stream.
```

> **`&mut self` vs `&self`:** the read methods (`read_buffer`,
> `read_buffer_blocking`, `buffers_iter`) and the lifecycle/callback methods
> (`start`, `stop`, `set_callback`, `clear_callback`) take `&mut self`. The query
> and subscription methods (`subscribe`, `is_running`, `uptime`, `format`,
> `config`, `overrun_count`, `is_under_backpressure`, `stream_stats`,
> `backpressure_report`) take `&self`. The module documents `AudioCapture` as
> `Send + Sync`, but there is no compile-time assertion enforcing it yet (tracked),
> and because reads take `&mut self`, sharing one handle across threads for reads
> requires external synchronization.

## Consuming audio

`rsac` is pull-first; all modes read from the same ring buffer, so prefer one
primary consumer.

### Non-blocking / blocking pull

```rust
match capture.read_buffer()? {              // Ok(None) = no data yet
    Some(buffer) => process(&buffer),
    None => std::thread::sleep(std::time::Duration::from_millis(1)),
}
let buffer = capture.read_buffer_blocking()?; // blocks until data
```

`read_buffer()`/`read_buffer_blocking()` error if the stream is not initialized
or not running. Handle `Ok(None)` with a short sleep, break on a fatal error
(`e.is_fatal()`), and retry on recoverable ones.

### Blocking iterator

```rust
for result in capture.buffers_iter() {
    let buffer = result?;                    // AudioResult<AudioBuffer>
    process(&buffer);
}
// Ends on StreamEnded after draining the buffered tail; surfaces other errors.
```

### Channel subscription

```rust
let rx = capture.subscribe()?;               // mpsc::Receiver<AudioBuffer>
while let Ok(buffer) = rx.recv() { process(&buffer); }
```

`subscribe()` spawns a background `rsac-subscribe` thread that exits when the
stream stops/errors or the `Receiver` is dropped. It polls a ~1 ms sleep on an
empty ring and delivers `AudioBuffer` values (the terminating error is not
delivered; for error-aware or latency-critical consumers prefer the blocking
read or the async stream).

### Async stream (feature `async-stream`)

```rust
use futures_util::StreamExt;
let mut stream = capture.audio_data_stream()?;   // AsyncAudioStream, waker-driven
while let Some(result) = stream.next().await {
    let buffer = result?;
}
```

Without the feature, `audio_data_stream()` returns `PlatformNotSupported`.

### Callback (push)

```rust
capture.set_callback(|buffer: &AudioBuffer| {    // register BEFORE start()
    let level = buffer.rms_dbfs();
    log::info!("RMS: {level:.1} dBFS");
})?;
capture.start()?;                                // moves the closure into a pump thread
```

The callback runs on a dedicated pump thread (never the OS audio thread). A
fatal read (`StreamEnded`) stops the pump; transient errors are logged and
retried. `clear_callback()` tears the pump down. See
[ADR-0002](designs/0002-callback-delivery.md).

## AudioBuffer

```rust
buf.data() -> &[f32];           buf.into_data() -> Vec<f32>;
buf.channels() -> u16;          buf.sample_rate() -> u32;     buf.format() -> &AudioFormat;
buf.len() / is_empty() / num_frames() / samples_per_channel() / duration();
buf.channel_data(ch) -> Option<Vec<f32>>;          // allocating de-interleave
buf.timestamp() -> Option<Duration>;               // stream position of the first sample (frames offered / rate); gaps = producer-side drops
```

### Level metering (RT-safe, allocation-free)

Read-only observability metrics (not DSP); `#[inline]`, allocation-free,
lock-free, and non-finite-sample tolerant — safe on the audio callback thread.

```rust
buf.rms();        buf.peak();         // 0.0 for empty/silence, never NaN
buf.rms_dbfs();   buf.peak_dbfs();    // NEG_INFINITY at silence; 0.0 dBFS at full scale
buf.channel_rms(ch) -> Option<f32>;   // strided; None if ch out of range
buf.channel_peak(ch) -> Option<f32>;  // Some(0.0) for an empty-but-existing channel
```

## Device enumeration and watching

```rust
use rsac::get_device_enumerator;

let enumerator = get_device_enumerator()?;
for device in enumerator.enumerate_devices()? {     // Vec<Box<dyn AudioDevice>>
    let info: DeviceInfo = device.describe();        // owned metadata snapshot
    println!("{}: {} ({:?}, default={})", info.id, info.name, info.kind, info.is_default);
}
let default = enumerator.default_device()?;          // Box<dyn AudioDevice>
```

`DeviceInfo` (`#[non_exhaustive]`) carries `id`, `name`, `kind` (`Input`/`Output`),
`is_default`, and `default_format` (the first `supported_formats()` entry, or
`None` — including on Linux/PipeWire by design).

### Watching for device changes

```rust
let watcher = enumerator.watch(Box::new(|event: DeviceEvent| {
    match event {
        DeviceEvent::DeviceAdded { id, name, kind } => { /* … */ }
        DeviceEvent::DeviceRemoved { id } => { /* … */ }
        DeviceEvent::DefaultChanged { id, kind } => { /* … */ }
        DeviceEvent::StateChanged { id, available } => { /* … */ }
        _ => {}  // DeviceEvent is #[non_exhaustive]
    }
}))?; // -> DeviceWatcher (RAII): dropping it unregisters the OS listener
```

The handler runs on the backend's **OS notification thread** (never the RT audio
thread), so it may allocate and lock. Dropping the `DeviceWatcher` unregisters
the OS listener and joins the notify thread. `watch()` defaults to
`PlatformNotSupported` unless the backend's
`PlatformCapabilities::supports_device_change_notifications` is `true`.

> **Per-platform divergence (tracked):** Windows and macOS hand events off via a
> bounded `sync_channel(64)` + helper thread (drop-on-full); Linux invokes the
> handler **directly on the PipeWire loop thread** (no helper thread, no bounded
> backpressure) because PipeWire's `!Send` loop objects make same-thread
> invocation natural. See
> [`API_DESIGN.md §9`](architecture/API_DESIGN.md#9-device-enumeration-deviceinfo-and-device-watching).

## Source discovery and diagnostics

```rust
use rsac::{list_audio_sources, list_audio_applications, check_audio_capture_permission};

for source in list_audio_sources()? {          // system default + devices + apps
    let target = source.to_capture_target();    // feed straight into the builder
}
let apps = list_audio_applications()?;          // best-effort; empty if unsupported
let perm = check_audio_capture_permission();    // Granted/NotDetermined/Denied/NotRequired
```

```rust
let stats: StreamStats = capture.stream_stats();
let _ = stats.buffers_pushed;       // enqueued by the producer
let _ = stats.buffers_captured;     // delivered to the consumer
let _ = stats.buffers_dropped;      // lost to ring-buffer overflow (== overruns)
let _ = stats.dropped_ratio();      // dropped / (captured + dropped), zero-guarded
let _ = stats.uptime;               // ZERO when not started

let bp: BackpressureReport = capture.backpressure_report();
let _ = bp.window;                  // recent-window span the tallies cover
let _ = bp.drop_rate;               // surfaces sustained loss the legacy bool misses
let _ = bp.is_under_backpressure;   // legacy consecutive-drop flag, carried unchanged
```

`StreamStats` and `BackpressureReport` are `#[non_exhaustive]`; build them via
`Default` + field assignment and match with `..`. Counters are cheap `Relaxed`
loads on the non-RT query path. As of 0.4.0, `BackpressureReport` is a **windowed**
view (rsac-cfe4): `pushed`/`dropped`/`drop_rate` cover a bounded recent window read
from the producer's alloc-free sliding ring, so a sustained 1-in-N loss the
consecutive-drop bool resets away is still surfaced. `window` is an estimate of the
span those tallies cover (buffer size × count ÷ negotiated rate), falling back to
`Duration::ZERO` only when the span cannot be attributed (no stream, or unknown
buffer size / sample rate).

## Errors

Every fallible call returns `AudioResult<T>` = `Result<T, AudioError>`.
`AudioError` is a manually-implemented (no `thiserror`, not `Clone`) enum of 22
variants across 7 `ErrorKind` categories (`Configuration`, `Device`, `Stream`,
`Backend`, `Application`, `Platform`, `Internal`), each with a `Recoverability`
hint (`Recoverable`, `TransientRetry`, `Fatal`).

```rust
match capture.read_buffer() {
    Ok(Some(buffer)) => process(&buffer),
    Ok(None) => { /* no data yet */ }
    Err(e) if e.is_fatal() => return Err(e),   // e.g. StreamEnded — terminal
    Err(_e) => { /* recoverable: retry */ }
}
```

`AudioError::StreamEnded` (Fatal, `ErrorKind::Stream`) is the clean
end-of-stream signal (see [ADR-0003](designs/0003-terminal-stream-error.md)).

> `AudioError` is **not** `#[non_exhaustive]` (tracked) — external code should
> still match with a trailing `_ =>` arm and rely on `kind()` /
> `recoverability()` / `is_fatal()` for classification.

## Sinks

The `AudioSink` trait and the bundled sinks are exported and driven by
`drain_to(sink)` — the built-in background-thread driver available on
`AudioCapture`, `RunningCapture`, and (behind `compose`) `Composition`. It
spawns an `rsac-drain` thread that owns the sink and pumps buffers off the same
ring the manual reads use, applying the recoverable-vs-fatal read/write policy
and finalizing with `flush()` + `close()` (the sink never runs on the OS RT
callback thread — ADR-0001). The lower-level manual-drain loop below remains
available when you want to own the read cadence yourself.

```rust
pub trait AudioSink: Send {
    fn write(&mut self, buffer: &AudioBuffer) -> AudioResult<()>;
    fn flush(&mut self) -> AudioResult<()> { Ok(()) }
    fn close(&mut self) -> AudioResult<()> { self.flush() }
}

let n = NullSink::new();
let (sink, rx) = ChannelSink::new();               // returns BOTH sink and receiver
let mut wav = WavFileSink::new("out.wav", &format)?; // feature `sink-wav`; needs the format
```

Built-in driver (`drain_to`) — the recommended path:

```rust
capture.start()?;                                  // negotiate the format first
let format = capture.format().unwrap_or_default(); // Some(_) once started
let wav = WavFileSink::new("out.wav", &format)?;   // feature `sink-wav`
let drain = capture.drain_to(wav)?;                // background rsac-drain thread
std::thread::sleep(std::time::Duration::from_secs(10));
drain.shutdown();                                  // flush + close + join the thread
capture.stop()?;
```

Manual drain pattern (lower-level, when you own the read cadence):

```rust
capture.start()?; // format() is Some(_) only after start negotiates it
let mut wav = WavFileSink::new("out.wav", &capture.format().unwrap_or_default())?;
// `read_buffer_blocking()` waits for data instead of returning early on a
// momentary empty ring (as the non-blocking `read_buffer()` would). It returns
// an error once the stream is no longer running, ending the drain cleanly.
while let Ok(buffer) = capture.read_buffer_blocking() {
    wav.write(&buffer)?;
}
wav.flush()?;
capture.stop()?;
```

## Composing multiple sources (`compose` feature)

Behind the opt-in `compose` feature ([ADR-0011](designs/0011-compose-feature.md)),
`rsac::compose` composes several capture targets into **one** interleaved
multi-channel stream. Sources are declared in *groups*: each group either
mixes down to Mono/Stereo (gain-weighted summation, per-source gain, optional
`clamp_output`) or passes a single source's native channels through
(`keep_channels()`). Groups append in declaration order; `channel_map()`
reports which output channel belongs to which group.

```rust
use rsac::compose::{CompositionBuilder, Group, GroupLayout};
use rsac::CaptureTarget;

let mut session = CompositionBuilder::new()
    .sample_rate(48_000) // session rate; mismatched sources are resampled
    .group(
        Group::new("voice")
            .source(CaptureTarget::ApplicationByName("discord".into()))
            .source_with_gain(CaptureTarget::ApplicationByName("zoom".into()), 0.8)
            .mixdown(GroupLayout::Mono), // → 1 composed channel
    )
    .group(
        Group::new("system")
            .source(CaptureTarget::SystemDefault)
            .keep_channels(), // → the endpoint's native channels
    )
    .build()?;

session.start()?;
let map = session.channel_map().unwrap();
loop {
    match session.read_chunk_nonblocking() {
        Ok(Some(buffer)) => { /* interleaved f32, map.channels() wide */ }
        Ok(None) => std::thread::sleep(std::time::Duration::from_millis(1)),
        Err(e) if e.is_fatal() => break, // composition ended and drained
        Err(e) => eprintln!("transient: {e}"),
    }
}
session.stop()?;
```

Key semantics (rustdoc on `rsac::compose` is authoritative):

- **Rate alignment** — sources delivering a different rate than the session
  rate are resampled with `rubato` on the dedicated non-RT compositor thread
  (Windows process loopback cannot autoconvert, so this is load-bearing).
- **Pacing** — a master-clock source (the first system/device source) paces
  output; behind sources are silence-padded, ahead sources bounded-trimmed;
  the clock is re-elected if the master ends, and a quantum-cadence wall-clock
  fallback keeps a stalled session alive. `session.stats()` exposes per-source
  `padded_frames` / `trimmed_frames` / `resampling` / `ended` counters.
- **Delivery** — `Composition` implements the same `CapturingStream` contract
  as a single capture; `session.drain_to(WavFileSink::new(...)?)` records a
  multi-channel WAV. Explicit `stop()` ends readability immediately (tail
  discarded); read until the fatal terminal first if you need every buffer.
- **Ownership** — the composition owns its inner captures; don't read the same
  sources through other handles while it runs.

Recording a composed stream to a multi-channel WAV:

```rust
let format = rsac::core::interface::CapturingStream::format(&session);
let wav = WavFileSink::new("composed.wav", &format)?; // channels = composed width
let drain = session.drain_to(wav)?;                   // background rsac-drain thread
std::thread::sleep(std::time::Duration::from_secs(10));
drain.shutdown();                                     // flush + finalize header
```

## Feature flags

- `feat_windows` / `feat_linux` / `feat_macos` — platform backends (default on;
  pair with the matching `target_os`).
- `async-stream` — enables `AudioCapture::audio_data_stream()`.
- `sink-wav` — enables `WavFileSink`.
- `compose` — enables `rsac::compose` multi-source channel composition
  (adds `rubato` + `audioadapter-buffers`).
- `cli` — the demo binaries' dependencies (`clap`, `color-eyre`, `ctrlc`,
  `env_logger`); not in defaults, so library consumers don't pull them.
- `tracing` — enables `rsac::install_default_tracing` (the `rsac_event!`/
  `rsac_span!` macros are always available, falling back to `log` when off).
- `test-utils` — shared test helpers for integration tests and binding crates.

See [`docs/features.md`](features.md) for the full matrix and
[`docs/architecture/API_DESIGN.md`](architecture/API_DESIGN.md) for the design
rationale.
