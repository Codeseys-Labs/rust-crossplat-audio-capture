# Performance Considerations

## Overview

rsac is a **capture-first** library: its performance story is about getting audio
off the OS callback thread and to the consumer with the smallest possible
real-time (RT) risk, *not* about signal processing. There is no general-purpose
DSP, encoding, or SIMD kernel in rsac — those are non-goals
(see [`VISION.md`](../VISION.md)) and belong to downstream consumers. The one
scope amendment is the opt-in `compose` feature (ADR-0011), whose mixdown and
`rubato` resampling run **exclusively on a dedicated non-RT compositor
thread** — never on an OS audio callback — so it does not change the RT story
described here. This document describes the parts of the pipeline that
actually affect latency and RT-safety, and is honest about which optimisations
are wired into the shipping path versus implemented-but-not-yet-wired.

## Critical path: the producer/consumer bridge

```mermaid
graph LR
    A[OS audio callback thread] --> B[BridgeProducer push]
    B --> C[lock-free SPSC ring]
    C --> D[BridgeConsumer pop]
    D --> E[AudioBuffer to consumer]
```

1. **OS audio callback thread** — each backend's capture callback
   (`audio/windows/thread.rs`, `audio/macos/thread.rs`,
   `audio/linux/thread.rs`) hands interleaved `f32` samples to
   `BridgeProducer::push_samples_or_drop`. This is the one truly
   latency-critical thread: it must never allocate, lock, or block.

2. **Lock-free SPSC ring** — the bridge is a single-producer/single-consumer
   ring built on [`rtrb`](https://docs.rs/rtrb) (`src/bridge/ring_buffer.rs`).
   Producer-hot and consumer-hot atomics are separated with a hand-rolled
   `#[repr(align(64))] CachePadded` newtype to avoid false sharing.

3. **Consumer side** — the non-RT consumer thread pops `AudioBuffer`s
   (`read_buffer`, `read_buffer_blocking`, `subscribe`, the async stream). All
   unavoidable allocation is deliberately pushed here, off the RT thread.

## RT-safety: the allocation-free producer guarantee (ADR-0001)

The headline RT-safety property is recorded in
[ADR-0001](designs/0001-rt-allocation-guarantee.md):

> **The RT producer (`push_samples_or_drop`) is allocation-free in steady
> state, with a single bounded warm-up/resize cost.**

This is delivered by a **free-list return ring**: the consumer recycles drained
`Vec<f32>` allocations back to the producer, so in steady state the producer
reuses buffers instead of asking the global allocator. The wording is
deliberately *steady state*, not unconditional — the first few callbacks (or a
period increase past the high-water mark) may allocate once; that cost is
bounded and amortised, after which it converges to zero allocation. ADR-0001
documents why the alternative "fixed slab pool" (truly zero allocation forever)
was not chosen on this branch.

Two design points matter here:

- **Seed/scratch sizing** comes from a single named constant tuned for a
  realistic worst-case callback period; recycled buffers are allowed to grow to
  the high-water mark so steady state converges to no allocation. The
  scratch-shrink defect described in ADR-0001 §1 is fixed on **both** the
  success and ring-full arms of the push, and locked in by the
  `scratch_never_shrinks_to_zero_after_underrun` regression test.
- **Drop-on-full, never block.** When the ring is full the producer drops the
  packet and bumps a `Relaxed` diagnostic counter rather than blocking the
  callback thread. Overruns surface to the consumer via
  `StreamStats`/`BackpressureReport` (see below), so an audio glitch never
  becomes a priority inversion.

### Verifying the guarantee

`tests/rt_alloc.rs` installs a process-wide counting `#[global_allocator]`,
drives 2000 steady-state push/pop cycles, and asserts the producer's heap
allocations stay within a bounded one-time warm-up. It is the single empirical
proof of ADR-0001.

> **Known gap (tracked, critique TC-01):** `rt_alloc.rs` is a harness
> integration test and is **not** currently run by CI (CI runs only
> `cargo test --lib`, `--test ci_audio`, and `--doc`). The guarantee is real and
> regression-tested locally, but it is not yet gated against regression in CI.
> Run it explicitly with `cargo test --test rt_alloc` (per-platform, since
> allocator behaviour can differ by target).

## Metering is alloc-free and RT-callback-safe

`AudioBuffer`'s level meters are read-only observability metadata, not signal
processing, and are explicitly safe to call on the audio callback thread:

- `rms()`, `peak()`, `rms_dbfs()`, `peak_dbfs()` (`src/core/buffer.rs`) iterate
  the existing sample slice and reduce to a scalar. They are `#[inline]` and
  perform **no allocation** — no intermediate `Vec`, no lock.
- `channel_rms(ch)` / `channel_peak(ch)` reduce over a **strided** view of the
  interleaved data (`iter().skip(ch).step_by(channels)`), so per-channel meters
  also allocate nothing. (Contrast `channel_data(ch)`, which *does* allocate a
  `Vec` to deinterleave — prefer the strided meters on the hot path.)
- Non-finite samples (`NaN`/`±inf`) are skipped; an empty buffer yields `0.0`
  (linear) / `f32::NEG_INFINITY` (dBFS), never `NaN`.

`benches/observability.rs` measures the consumer-side read path
(`stream_stats` / `backpressure_report` assembly plus the `AudioBuffer` meters)
and embeds an RT-safety regression guard proving these reads are cheap,
non-locking, and allocation-free.

## Backpressure and diagnostics (consumer-side, cheap reads)

`StreamStats` and `BackpressureReport` (`src/core/introspection.rs`) are
`#[non_exhaustive]` snapshots assembled from `Relaxed` atomic counters
(frames/buffers delivered, overruns, drops). Reading them does not lock the data
plane and does not allocate. `StreamStats` carries **lifetime** totals;
`BackpressureReport` is, since 0.4.0 (rsac-cfe4), a **windowed** view —
`pushed`/`dropped`/`drop_rate` are summed over the producer's fixed, alloc-free
sliding ring of `(pushed, dropped)` slots (advanced on every push path with
`Relaxed` adds, so the RT producer stays lock-free/alloc-free), with `window`
estimated from the buffer size and negotiated rate. This surfaces a sustained
1-in-N loss the consecutive-drop bool resets away, without fabricating rates.

## Per-platform capture-thread notes

The capture threads differ in how they hand bytes to the producer; this is the
main per-platform performance divergence today.

- **Linux (PipeWire)** does the efficient thing: it reinterprets the contiguous
  byte buffer to `&[f32]` in one bulk `align_to::<f32>()` cast
  (`audio/linux/thread.rs`) and pushes that slice in a single copy.
- **Windows (WASAPI)** currently does more work per packet: it copies OS bytes
  into a reused `VecDeque`, calls `make_contiguous()` (an O(n) rotation), runs a
  scalar `f32::from_le_bytes` loop, and then the producer copies again into the
  ring. This is correct but not optimal; mirroring the Linux bulk-reinterpret is
  a tracked low-priority improvement (critique PERF-03). It does **not** affect
  the RT-allocation guarantee — the reused `VecDeque` is pre-grown.
- **macOS (CoreAudio)** pushes the IOProc's interleaved `f32` directly.

## Ring sizing and the `buffer_size` setting

The ring depth is chosen by `calculate_capacity(requested, min)` in
`src/bridge/ring_buffer.rs`.

> **Honest status — `buffer_size` is honored only on Windows (tracked, critique
> DF-04/PERF-01).** `StreamConfig.buffer_size` is threaded into
> `calculate_capacity(config.buffer_size, 4)` on WASAPI
> (`audio/windows/wasapi.rs`), but the macOS and Linux backends call
> `calculate_capacity(None, 4)` (= 64 slots) and **ignore** the requested size.
> Also note `buffer_size` is consumed as a *ring slot count* (number of
> `AudioBuffer`s), not a frame count, despite the field's "frames" wording.

### Period-aware sizing (implemented, not yet wired)

A smarter sizing function, `calculate_capacity_for_period(period_frames,
channels)` (`src/bridge/ring_buffer.rs`), derives ring depth from the negotiated
OS period (≈12-period headroom, sub-reference scale-up, clamped to `8..=1024`,
rounded to a power of two; the `channels` parameter is accepted but currently
ignored).

> **Honest status — implemented and unit-tested, but called by no backend
> (tracked, critique PERF-01).** Every backend uses the static
> `calculate_capacity` above; `calculate_capacity_for_period` has zero call
> sites outside its own tests. It is reserved for wiring once each backend knows
> its negotiated period.

## Zero-copy sample ring — prototyped, measured, removed

An opt-in `SampleRing` producer plane once lived behind a `bridge-zerocopy`
feature: it wrote interleaved `f32` straight into the ring's uninitialised slots
via `rtrb`'s `write_chunk_uninit` + `CopyToUninit`, avoiding the per-buffer `Vec`
on the producer. It was never wired into a backend, A/B-benchmarked against the
default `AudioBuffer` ring, and **removed** on 2026-07-20 under
[ADR-0006](designs/0006-bridge-zerocopy-samplering.md)'s promote-or-remove gate.

The producer-side win was small, target-specific (present on x86_64, absent on
Apple Silicon and on large chunks), and immaterial against the ~10 ms callback
budget; end-to-end the plane *lost* in every measured environment, because the
consumer had to pay a reconstruction copy the default ring's moved `Vec` never
does. The shipping default remains *allocation-free in steady state* (not
literally copy-free), and that is the honest characterisation. ADR-0006 §6
records the full data.

## What rsac does *not* optimise (by design)

These were once aspirational and are explicitly out of scope — rsac is capture,
not DSP:

- No SIMD signal-processing kernels or buffer-pool DSP.
- No in-place transform pipeline. `AudioProcessor` (`src/core/processing.rs`) is
  an intentionally empty, fenced-off extension point rsac will never populate
  with DSP.
- Encoding (MP3/AAC/Opus), playback, VAD, and AEC are downstream concerns.
  Downstreams own general-purpose resampling (e.g. `rubato`) and encoding
  (e.g. `hound`/`symphonia`). The exception is the opt-in `compose` feature
  (ADR-0011): its per-group mixdown and internal rate-alignment resampling run
  on a dedicated non-RT compositor thread, off the critical path this document
  covers.

## Benchmarks

Two criterion benches ship in-tree (`harness = false`, so they do not affect
`cargo build`/`cargo test`):

- `benches/bridge.rs` — the producer/consumer data plane (steady-state push
  throughput, push→pop round trip, a capacity sweep, and the WASAPI byte→f32
  decode micro-benchmark).
- `benches/observability.rs` — the consumer-side read path (`stream_stats`,
  `backpressure_report`, and `AudioBuffer` meters) with an RT-safety regression
  guard.

```bash
cargo bench --bench bridge
cargo bench --bench observability
```

## Optimisation guidelines for consumers

1. **Keep the RT thread clean.** If you supply a callback (native or via a
   binding), do no allocation, locking, or blocking inside it — copy out and
   return. rsac wraps FFI callbacks in `catch_unwind`, but it cannot make your
   work RT-safe.
2. **Prefer the strided meters.** Use `rms_dbfs()` / `peak_dbfs()` /
   `channel_rms()` / `channel_peak()` instead of hand-rolling RMS over
   `data()`, and avoid `channel_data()` on the hot path (it allocates).
3. **Watch backpressure.** Poll `stream_stats()` / `is_under_backpressure()`;
   rising overrun counts mean your consumer is too slow and packets are being
   dropped at the producer.
4. **Size the ring (Windows) or drain faster.** On Windows, raise
   `buffer_size` (ring slots) if you cannot drain promptly; on macOS/Linux the
   ring depth is currently fixed (see the sizing note above), so the lever is
   consumer throughput.
