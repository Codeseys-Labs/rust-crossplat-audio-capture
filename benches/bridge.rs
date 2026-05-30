//! Criterion benchmarks for the bridge data plane (`rsac::bridge::ring_buffer`).
//!
//! This is the **baseline** all later perf work compares against (seeds
//! `rsac-603b`). It measures the real-time hot path the OS audio callback
//! drives — [`BridgeProducer::push_samples_or_drop`] — plus the producer→consumer
//! round trip and how capacity affects steady-state throughput.
//!
//! Run with:
//!
//! ```sh
//! cargo bench --bench bridge
//! ```
//!
//! These benchmarks touch **no** platform backend (no WASAPI/PipeWire/CoreAudio),
//! so they build and run on the default feature set on any host.
//!
//! ## What is measured
//!
//! - **push_throughput** — `push_samples_or_drop` of a 1024-frame stereo slice
//!   (2048 `f32`) in steady state. Each iteration pushes one buffer and pops one
//!   so the ring never saturates: this exercises the free-list recycle path
//!   (ADR-0001) rather than the drop path.
//! - **push_pop_roundtrip** — full producer→consumer round trip latency: one
//!   `push_samples_or_drop` immediately followed by one `pop`.
//! - **capacity_sweep** — `push_throughput` repeated across a range of ring
//!   capacities from [`calculate_capacity`] ({16, 32, 64, 128, 256}) to show how
//!   ring sizing affects the steady-state cost.
//!
//! All hot values flow through [`std::hint::black_box`] (re-exported by criterion)
//! so the optimizer cannot elide the work being measured.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use rsac::bridge::ring_buffer::{calculate_capacity, create_bridge};
use rsac::core::config::AudioFormat;

/// A 1024-frame stereo period: 1024 frames × 2 channels = 2048 interleaved `f32`.
/// This matches the realistic worst-case callback period the bridge is tuned for
/// (see `RT_BUFFER_SAMPLE_CAPACITY` / ADR-0001), so the recycled allocations fit
/// without re-growing — i.e. this measures the steady-state alloc-free path.
const FRAMES: usize = 1024;
const CHANNELS: u16 = 2;
const SAMPLE_RATE: u32 = 48_000;
const SAMPLES: usize = FRAMES * CHANNELS as usize; // 2048

/// Build a deterministic 1024-frame stereo slice to push every iteration.
fn make_slice() -> Vec<f32> {
    (0..SAMPLES).map(|i| (i as f32) * 1e-4).collect()
}

/// Warm the free-list so the producer is allocation-free before we start timing:
/// push a buffer then pop it, recycling the drained `Vec` back to the producer,
/// for a few cycles. After this the steady-state hot path reuses allocations.
fn warm_up(
    producer: &mut rsac::bridge::ring_buffer::BridgeProducer,
    consumer: &mut rsac::bridge::ring_buffer::BridgeConsumer,
    slice: &[f32],
) {
    for _ in 0..64 {
        producer.push_samples_or_drop(slice, CHANNELS, SAMPLE_RATE);
        let _ = consumer.pop();
    }
}

/// `push_samples_or_drop` throughput in steady state, reported in elements
/// (samples) per second. Each timed iteration pushes one 1024-frame stereo slice
/// and pops one buffer, so the ring stays unsaturated and we measure the
/// free-list recycle (alloc-free) path rather than the drop path.
fn bench_push_throughput(c: &mut Criterion) {
    let slice = make_slice();
    let capacity = calculate_capacity(Some(64), 4);

    let mut group = c.benchmark_group("bridge");
    // One iteration processes SAMPLES f32 samples; report samples/sec.
    group.throughput(Throughput::Elements(SAMPLES as u64));
    group.bench_function("push_throughput", |b| {
        let (mut producer, mut consumer) = create_bridge(capacity, AudioFormat::default());
        warm_up(&mut producer, &mut consumer, &slice);

        b.iter(|| {
            let pushed = producer.push_samples_or_drop(black_box(&slice), CHANNELS, SAMPLE_RATE);
            black_box(pushed);
            // Drain immediately so the ring never saturates → keeps us on the
            // steady-state recycle path. The popped buffer recycles its backing
            // allocation to the producer for the next iteration.
            let popped = consumer.pop();
            black_box(popped);
        });
    });
    group.finish();
}

/// Full producer→consumer round-trip: push one buffer, then pop it. This is the
/// end-to-end latency of moving one audio period through both rings (the data
/// ring and the free-list return ring).
fn bench_push_pop_roundtrip(c: &mut Criterion) {
    let slice = make_slice();
    let capacity = calculate_capacity(Some(64), 4);

    let mut group = c.benchmark_group("bridge");
    group.throughput(Throughput::Elements(SAMPLES as u64));
    group.bench_function("push_pop_roundtrip", |b| {
        let (mut producer, mut consumer) = create_bridge(capacity, AudioFormat::default());
        warm_up(&mut producer, &mut consumer, &slice);

        b.iter(|| {
            producer.push_samples_or_drop(black_box(&slice), CHANNELS, SAMPLE_RATE);
            let buf = consumer
                .pop()
                .expect("round-trip pop must yield the pushed buffer");
            // Touch the payload so the round trip can't be optimized away.
            black_box(buf.data()[0]);
        });
    });
    group.finish();
}

/// Capacity sensitivity sweep: repeat the steady-state push/pop hot path over a
/// range of ring capacities derived from [`calculate_capacity`]. Larger rings
/// trade memory for more slack before back-pressure; this baseline shows whether
/// (and how much) capacity affects steady-state per-period cost.
fn bench_capacity_sweep(c: &mut Criterion) {
    let slice = make_slice();

    let mut group = c.benchmark_group("bridge_capacity_sweep");
    group.throughput(Throughput::Elements(SAMPLES as u64));

    for requested in [16usize, 32, 64, 128, 256] {
        let capacity = calculate_capacity(Some(requested), 4);
        group.bench_with_input(
            BenchmarkId::from_parameter(capacity),
            &capacity,
            |b, &capacity| {
                let (mut producer, mut consumer) = create_bridge(capacity, AudioFormat::default());
                warm_up(&mut producer, &mut consumer, &slice);

                b.iter(|| {
                    producer.push_samples_or_drop(black_box(&slice), CHANNELS, SAMPLE_RATE);
                    let popped = consumer.pop();
                    black_box(popped);
                });
            },
        );
    }
    group.finish();
}

criterion_group!(
    benches,
    bench_push_throughput,
    bench_push_pop_roundtrip,
    bench_capacity_sweep
);
criterion_main!(benches);
