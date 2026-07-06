# Interop Recipes — using rsac buffers with the Rust audio ecosystem

rsac deliberately ships **no** ecosystem adapter dependencies (see
[VISION.md](../VISION.md): capture-first, lean dependency graph). Interop is
nonetheless trivial because the delivery contract is the simplest possible
one: **interleaved `f32` samples + a format descriptor**:

```rust
let buffer: rsac::AudioBuffer = capture.read_buffer()?.unwrap();
let samples: &[f32] = buffer.data();     // interleaved: L R L R … (channel-major per frame)
let channels = buffer.channels();        // u16
let rate     = buffer.sample_rate();     // u32 Hz
let position = buffer.timestamp();       // Option<Duration>: stream position of first sample
```

Every recipe below is copy-paste against that surface — no rsac feature flags
needed. (For multi-source composition, `rsac::compose`'s `Composition` yields
the exact same `AudioBuffer` type; all recipes apply unchanged.)

## dasp (sample/frame DSP)

`dasp`'s slice tools work directly on `buffer.data()`:

```toml
dasp = { version = "0.11", features = ["slice"] }
```

```rust
use dasp::slice::ToFrameSlice;

// Stereo capture → &[[f32; 2]] frames without copying:
let frames: &[[f32; 2]] = buffer.data().to_frame_slice().expect("stereo");
let peak = frames
    .iter()
    .map(|f| f[0].abs().max(f[1].abs()))
    .fold(0.0f32, f32::max);

// Or per-sample processing via dasp::Sample on the flat slice:
use dasp::Sample;
let as_i16: Vec<i16> = buffer.data().iter().map(|s| s.to_sample::<i16>()).collect();
```

For rate conversion prefer `rubato` (what rsac itself uses internally for the
`compose` feature) over `dasp`'s interpolators for production quality.

## cpal / rodio (playback / monitoring)

Pipe captured audio to an output device by bridging through a channel — rsac's
`subscribe()` hands you an `mpsc::Receiver<AudioBuffer>` that a cpal output
callback can drain:

```rust
let rx = capture.subscribe()?; // or composition.subscribe()
let mut pending: std::collections::VecDeque<f32> = Default::default();

// Inside cpal's build_output_stream data callback:
move |out: &mut [f32], _| {
    while pending.len() < out.len() {
        match rx.try_recv() {
            Ok(buffer) => pending.extend(buffer.data().iter().copied()),
            Err(_) => break,
        }
    }
    for sample in out.iter_mut() {
        *sample = pending.pop_front().unwrap_or(0.0); // underrun → silence
    }
}
```

Match the output stream's sample rate/channels to `capture.format()` (or
resample with `rubato`). For `rodio`, wrap the same receiver in a
`rodio::Source` with `sample_rate()`/`channels()` answering from the captured
format.

## hound (WAV) — prefer the built-in sink

rsac already ships this path: `WavFileSink` (feature `sink-wav`) +
`drain_to()` writes single- or multi-channel WAV, including composed streams:

```rust
let format = capture.format().unwrap_or_default();
let sink = rsac::WavFileSink::new("out.wav", &format)?;
let drain = running.drain_to(sink)?; // background thread, flush/close on end
```

Hand-rolling with `hound` directly is only needed for exotic WAV specs
(e.g. int24): create a `hound::WavWriter` with `SampleFormat::Float`, 32 bits,
and `write_sample` each value of `buffer.data()` in order — the interleaving
already matches WAV frame order.

## symphonia / encoders (opus, mp3, aac)

Encoders want planar or interleaved PCM at a fixed rate; feed them
`buffer.data()` directly (interleaved f32 is the common input format, e.g.
`opus_rs`/`audiopus` accept `&[f32]` interleaved). Aggregate buffers to the
encoder's required frame size (e.g. 20 ms = `rate / 50` frames) with a small
`VecDeque<f32>` — the same pattern as the cpal recipe. `symphonia` itself is a
*decoder* library; you only need it on the playback/analysis side, where its
`SampleBuffer<f32>` layout is identical to rsac's (interleaved f32).

## Timestamps

`buffer.timestamp()` is the **stream position** of the buffer's first sample
(frames *offered* so far — delivered + dropped — ÷ rate) — not wall-clock
time. Two properties useful
for interop pipelines:

- consecutive buffers are contiguous (`t₂ = t₁ + frames₁/rate`) unless the
  producer dropped data, in which case the *gap* tells you exactly how much
  was lost — insert silence of that length to keep sync;
- A/V sync against a wall clock needs one anchor sample: pair the first
  buffer's arrival `Instant::now()` with its stream position, then extrapolate.
