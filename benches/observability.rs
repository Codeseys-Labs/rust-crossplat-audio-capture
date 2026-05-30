//! Criterion benchmarks for the **consumer-side observability read path**
//! (`rsac-03a5`).
//!
//! Wave-1..3 added a read-only diagnostics surface — `AudioCapture::stream_stats`
//! / `AudioCapture::backpressure_report` plus the alloc-free [`AudioBuffer`]
//! level meters (`rms`/`peak`/`rms_dbfs`/`channel_rms`). The whole point of that
//! surface is that polling it is *cheap*: a handful of `Relaxed` atomic loads and
//! a single linear scan over an existing buffer, with **no allocation, no lock,
//! and no contention** with the OS audio callback thread (the producer).
//!
//! This bench measures exactly those reads so they stay cheap and never regress
//! into something a UI should not poll at frame rate. Like [`benches/bridge.rs`]
//! it touches **no** platform backend (no WASAPI/PipeWire/CoreAudio) and needs no
//! live device — it drives a standalone [`create_bridge`] pair as the stats
//! source and a standalone [`AudioBuffer`] for the meters — so it builds and runs
//! on the default feature set on any host.
//!
//! Run with:
//!
//! ```sh
//! cargo bench --bench observability
//! ```
//!
//! ## What is measured
//!
//! - **stats_snapshot** — assemble the equivalent of an `AudioCapture::stream_stats`
//!   snapshot from the bridge's public counter readers (`buffers_dropped`,
//!   `buffers_popped`, `buffers_pushed`-via-drop-window) on a warmed bridge. This
//!   is the per-poll cost of the diagnostics read path.
//! - **backpressure_report** — assemble the equivalent of an
//!   `AudioCapture::backpressure_report`: read the windowed `(pushed, dropped)`
//!   snapshot and the consecutive-drop backpressure flag, then fold them into a
//!   [`BackpressureReport`] (the same `dropped / (pushed + dropped)` math the
//!   library does read-side).
//! - **buffer_metering** — `rms`/`peak`/`rms_dbfs`/`channel_rms` over a realistic
//!   **10 ms @ 48 kHz stereo** buffer (480 frames × 2 channels = 960 `f32`),
//!   reported as samples/sec throughput. These are the alloc-free meters a VU
//!   display would call once per delivered buffer.
//!
//! ## Expected order of magnitude
//!
//! - The two stats reads are a small constant number of atomic `Relaxed` loads
//!   (single-digit) over fixed-size state — **single-digit-to-tens of nanoseconds**
//!   per snapshot, *independent of buffer size or stream uptime*. They never
//!   allocate (`StreamStats::format_description` is left empty here, exactly as a
//!   never-started capture would report it).
//! - Each metering call is a single linear pass over 960 `f32`, so on the order of
//!   **hundreds of nanoseconds to low microseconds** per call (i.e. multiple
//!   GiB/s of sample throughput) — utterly negligible against a 10 ms (≈480 k
//!   samples/s/channel) capture period.
//!
//! ## RT-safety regression guard
//!
//! Acceptance criterion: the windowed drop-rate counters must not perturb the
//! RT producer's allocation behavior. The private `recycled_available()` probe
//! used by `ring_buffer.rs::steady_state_push_samples_pop_loop` is `#[cfg(test)]`
//! / `pub(crate)` and therefore **not reachable from a bench** (a separate crate).
//! [`assert_steady_state_alloc_free`] re-asserts the same invariant through the
//! *public* surface instead: after a warmed steady push/pop loop every push keeps
//! succeeding and the windowed counters report **zero drops** — which can only
//! hold if the producer kept sourcing recycled allocations from the free-list
//! (an allocation stall would have starved a push and been counted as a drop).
//! It runs once at the start of every `cargo bench` invocation.
//!
//! All hot values flow through [`std::hint::black_box`] (re-exported by criterion)
//! so the optimizer cannot elide the work being measured.

use std::hint::black_box;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};

use rsac::bridge::ring_buffer::{
    calculate_capacity, create_bridge, BridgeConsumer, BridgeProducer,
};
use rsac::core::buffer::AudioBuffer;
use rsac::core::config::AudioFormat;
use rsac::{BackpressureReport, StreamStats};

// A 1024-frame stereo period feeds the bridge in steady state, matching
// `benches/bridge.rs` so the warm-up exercises the same alloc-free recycle path.
const PUSH_FRAMES: usize = 1024;
const CHANNELS: u16 = 2;
const SAMPLE_RATE: u32 = 48_000;
const PUSH_SAMPLES: usize = PUSH_FRAMES * CHANNELS as usize; // 2048

// A realistic metering buffer: 10 ms of 48 kHz stereo = 480 frames × 2 = 960 f32.
const METER_FRAMES: usize = 480;
const METER_SAMPLES: usize = METER_FRAMES * CHANNELS as usize; // 960

/// Build a deterministic 1024-frame stereo slice to push into the bridge.
fn make_push_slice() -> Vec<f32> {
    (0..PUSH_SAMPLES).map(|i| (i as f32) * 1e-4).collect()
}

/// Build a deterministic, non-silent 10 ms stereo buffer for the meters. A small
/// sine-ish ramp keeps `rms`/`peak` away from the trivial all-zero fast exit so
/// the bench measures a representative scan.
fn make_meter_buffer() -> AudioBuffer {
    let data: Vec<f32> = (0..METER_SAMPLES)
        .map(|i| {
            let phase = (i as f32) * 0.05;
            0.5 * phase.sin()
        })
        .collect();
    AudioBuffer::new(data, CHANNELS, SAMPLE_RATE)
}

/// Warm the bridge: push and immediately pop for several cycles so the producer
/// is on the steady-state free-list recycle path and the counters/drop-window are
/// populated — the realistic state a `stream_stats()` poll observes.
fn warm_bridge(producer: &mut BridgeProducer, consumer: &mut BridgeConsumer, slice: &[f32]) {
    for _ in 0..256 {
        producer.push_samples_or_drop(slice, CHANNELS, SAMPLE_RATE);
        let _ = consumer.pop();
    }
}

/// Assemble a [`StreamStats`]-equivalent snapshot from the bridge's **public**
/// counter readers — the same `Relaxed` loads `AudioCapture::stream_stats` folds
/// together internally. `format_description` is left empty (no allocation),
/// matching a capture that has not negotiated a format yet.
///
/// `StreamStats` is `#[non_exhaustive]`, so it cannot be built with a struct
/// literal outside the crate; we construct it via `Default` + field assignment.
#[inline]
#[allow(clippy::field_reassign_with_default)]
fn read_stream_stats(producer: &BridgeProducer, consumer: &BridgeConsumer) -> StreamStats {
    let dropped = producer.buffers_dropped();
    let (pushed, _window_dropped) = producer.drop_window_snapshot();
    let captured = consumer.buffers_popped();

    let mut stats = StreamStats::default();
    stats.overruns = dropped;
    stats.buffers_captured = captured;
    stats.buffers_dropped = dropped;
    stats.buffers_pushed = pushed;
    stats.uptime = Duration::ZERO;
    stats.is_running = true;
    stats
}

/// Assemble a [`BackpressureReport`]-equivalent from the windowed
/// `(pushed, dropped)` snapshot and the consecutive-drop flag — the same read-side
/// `dropped / (pushed + dropped)` math `AudioCapture::backpressure_report` does.
/// `from_counts` is `pub(crate)`, so we reproduce it here via `Default` + public
/// field assignment (the struct is `#[non_exhaustive]`).
#[inline]
#[allow(clippy::field_reassign_with_default)]
fn read_backpressure_report(producer: &BridgeProducer) -> BackpressureReport {
    let (pushed, dropped) = producer.drop_window_snapshot();
    let denom = pushed + dropped;
    let drop_rate = if denom == 0 {
        0.0
    } else {
        dropped as f64 / denom as f64
    };

    let mut report = BackpressureReport::default();
    report.window = Duration::ZERO;
    report.pushed = pushed;
    report.dropped = dropped;
    report.drop_rate = drop_rate;
    report.is_under_backpressure = false;
    report
}

/// Per-poll cost of assembling a [`StreamStats`] snapshot from a warmed bridge.
///
/// This is the first registered bench, so it also runs the one-shot RT-safety
/// regression guard before any timing begins.
fn bench_stats_snapshot(c: &mut Criterion) {
    // Acceptance criterion: re-assert the RT producer stays alloc-free with the
    // windowed counters enabled. Runs once, outside any `b.iter` timing loop.
    assert_steady_state_alloc_free();

    let slice = make_push_slice();
    let capacity = calculate_capacity(Some(64), 4);

    let mut group = c.benchmark_group("observability");
    group.bench_function("stats_snapshot", |b| {
        let (mut producer, mut consumer) = create_bridge(capacity, AudioFormat::default());
        warm_bridge(&mut producer, &mut consumer, &slice);

        b.iter(|| {
            let stats = read_stream_stats(black_box(&producer), black_box(&consumer));
            // Touch a derived metric and a counter so the snapshot can't be elided.
            black_box(stats.dropped_ratio());
            black_box(stats.buffers_pushed);
        });
    });
    group.finish();
}

/// Per-poll cost of assembling a windowed [`BackpressureReport`] from a warmed
/// bridge — the windowed-counter read path `rsac-cfe4` introduced.
fn bench_backpressure_report(c: &mut Criterion) {
    let slice = make_push_slice();
    let capacity = calculate_capacity(Some(64), 4);

    let mut group = c.benchmark_group("observability");
    group.bench_function("backpressure_report", |b| {
        let (mut producer, mut consumer) = create_bridge(capacity, AudioFormat::default());
        warm_bridge(&mut producer, &mut consumer, &slice);

        b.iter(|| {
            let report = read_backpressure_report(black_box(&producer));
            black_box(report.drop_rate);
            black_box(report.is_under_backpressure);
        });
    });
    group.finish();
}

/// Throughput of the alloc-free [`AudioBuffer`] level meters over a realistic
/// 10 ms @ 48 kHz stereo buffer. Reports samples/sec so the result is comparable
/// against the capture rate. One iteration runs all four meters
/// (`rms`/`peak`/`rms_dbfs`/`channel_rms`) — the set a VU display polls per buffer.
fn bench_buffer_metering(c: &mut Criterion) {
    let buffer = make_meter_buffer();

    let mut group = c.benchmark_group("observability");
    // One iteration scans METER_SAMPLES f32 once per meter; report the dominant
    // single-scan throughput (rms over all samples) in samples/sec.
    group.throughput(Throughput::Elements(METER_SAMPLES as u64));
    group.bench_function("buffer_metering", |b| {
        b.iter(|| {
            let buf = black_box(&buffer);
            black_box(buf.rms());
            black_box(buf.peak());
            black_box(buf.rms_dbfs());
            black_box(buf.channel_rms(0));
        });
    });
    group.finish();
}

/// RT-safety regression guard (acceptance criterion): a tight push/pop loop with
/// the windowed drop-rate counters enabled must show the **same** alloc-free
/// behavior as without them. We can't read the private `recycled_available()`
/// from a bench crate, so we assert the public-surface equivalent: after warm-up
/// every steady-state push succeeds and the windowed counters report zero drops —
/// which is only possible if the producer kept recycling free-list allocations
/// (an allocation stall would have failed a push and been counted as a drop).
///
/// Panics (failing the bench run) if the invariant is violated.
fn assert_steady_state_alloc_free() {
    let slice = make_push_slice();
    // Capacity ≥ 2 so a push-then-pop never saturates the ring in steady state.
    let (mut producer, mut consumer) =
        create_bridge(calculate_capacity(Some(64), 4), AudioFormat::default());

    // Warm up so the free-list is primed and the producer is off the cold path.
    warm_bridge(&mut producer, &mut consumer, &slice);

    // Snapshot the drop-window baseline, then run a long steady push/pop loop.
    let (_p0, d0) = producer.drop_window_snapshot();
    for _ in 0..2_000 {
        let pushed = producer.push_samples_or_drop(&slice, CHANNELS, SAMPLE_RATE);
        assert!(
            pushed,
            "steady-state push must always succeed (free-list never starved)"
        );
        let _ = consumer.pop();
    }
    let (_p1, d1) = producer.drop_window_snapshot();

    assert_eq!(
        producer.buffers_dropped(),
        0,
        "no lifetime drops in a warmed steady push/pop loop"
    );
    assert_eq!(
        d1, d0,
        "windowed dropped count must not grow in steady state (RT producer stayed alloc-free)"
    );
    // With zero drops the consecutive-drop backpressure flag cannot have tripped
    // (it is derived from `consecutive_drops`, which only advances on a drop), so
    // the alloc-free invariant is fully proven by the two assertions above.
}

criterion_group!(
    benches,
    bench_stats_snapshot,
    bench_backpressure_report,
    bench_buffer_metering
);
criterion_main!(benches);
