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
//! - **wasapi_byte_decode** — the WASAPI capture hot-path byte→f32 conversion
//!   (PU-7, seed `rsac-7876`), comparing the previous scalar `from_le_bytes`
//!   loop against the new bulk `slice::align_to::<f32>()` reinterpret. Pure
//!   conversion, no WASAPI/COM — so it builds and runs on any host. See the
//!   `wasapi_byte_decode` group below for the measured before/after.
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

// ── PU-7: WASAPI byte→f32 conversion micro-benchmark ───────────────────────
//
// The WASAPI capture thread (`src/audio/windows/thread.rs`) reads raw interleaved
// F32LE bytes from the device and must turn them into `&[f32]` before pushing to
// the bridge. PU-7 (seed `rsac-7876`) replaced the per-sample scalar
// `f32::from_le_bytes` loop (which also required a `VecDeque` + O(n)
// `make_contiguous` upstream) with a single bulk `slice::align_to::<f32>()`
// reinterpret, mirroring the Linux/PipeWire path.
//
// `slice::align_to` is used deliberately instead of `bytemuck::cast_slice`, which
// would *panic* on a misaligned slice; `align_to` instead splits off any
// unaligned head/tail (empty in practice, since the source is word-aligned).
//
// These two benches isolate exactly that conversion on a realistic packet, with
// NO WASAPI/COM dependency (raw bytes are synthesized), so they build and run on
// any host. They directly quantify the PU-7 before/after.

/// A realistic WASAPI shared-mode packet: ~10ms at 48kHz stereo f32.
/// 480 frames × 2 channels = 960 interleaved f32 = 3840 bytes.
const WASAPI_PACKET_FRAMES: usize = 480;
const WASAPI_PACKET_SAMPLES: usize = WASAPI_PACKET_FRAMES * CHANNELS as usize; // 960
const WASAPI_PACKET_BYTES: usize = WASAPI_PACKET_SAMPLES * 4; // 3840

/// Build the f32-aligned backing buffer for a realistic WASAPI shared-mode
/// packet, holding `WASAPI_PACKET_SAMPLES` little-endian f32 samples — the byte
/// layout WASAPI delivers in shared mode.
///
/// The buffer is allocated as a `Vec<f32>` (whose data pointer is 4-byte
/// aligned), not a `Vec<u8>` (alignment 1, no f32-alignment guarantee). Viewing
/// its bytes via [`wasapi_bytes`] then guarantees `align_to::<f32>()` yields an
/// empty head/tail, matching the real capture path; otherwise the bench could
/// split off a non-empty head and measure a different path than production
/// (whose head/tail are always empty — see `bytes_to_f32_aligned` in
/// `src/audio/windows/thread.rs`).
fn make_wasapi_samples() -> Vec<f32> {
    let mut samples: Vec<f32> = Vec::with_capacity(WASAPI_PACKET_SAMPLES);
    for i in 0..WASAPI_PACKET_SAMPLES {
        samples.push((i as f32) * 1e-4);
    }
    samples
}

/// Reinterpret the f32-aligned fixture as the little-endian byte buffer WASAPI
/// delivers. On the little-endian hosts WASAPI runs on this is the exact F32LE
/// layout; because the source is a `Vec<f32>`, the returned slice is 4-byte
/// aligned with no head/tail under `align_to::<f32>()`.
fn wasapi_bytes(samples: &[f32]) -> &[u8] {
    // SAFETY: every f32 is fully initialized and any bit pattern is a valid u8.
    let (head, bytes, tail) = unsafe { samples.align_to::<u8>() };
    debug_assert!(
        head.is_empty() && tail.is_empty(),
        "f32 slice reinterprets to bytes with no head/tail"
    );
    debug_assert_eq!(bytes.len(), WASAPI_PACKET_BYTES);
    bytes
}

/// PU-7 before/after: scalar `from_le_bytes` loop vs. bulk `align_to` reinterpret.
///
/// `old_scalar_from_le_bytes` reproduces the pre-PU-7 hot path: decode every 4
/// bytes individually and push into a reused `Vec<f32>`. `new_align_to_bulk`
/// reproduces the PU-7 path: one `slice::align_to::<f32>()` reinterpret with no
/// per-sample work and no staging Vec. Both sum the samples through `black_box`
/// so the optimizer cannot elide the decode.
fn bench_wasapi_byte_decode(c: &mut Criterion) {
    let samples_fixture = make_wasapi_samples();
    let bytes = wasapi_bytes(&samples_fixture);

    let mut group = c.benchmark_group("wasapi_byte_decode");
    // One iteration decodes WASAPI_PACKET_SAMPLES f32 values; report samples/sec.
    group.throughput(Throughput::Elements(WASAPI_PACKET_SAMPLES as u64));

    // Pre-PU-7: per-sample scalar decode into a reused Vec<f32>.
    group.bench_function("old_scalar_from_le_bytes", |b| {
        let mut samples: Vec<f32> = Vec::with_capacity(WASAPI_PACKET_SAMPLES);
        b.iter(|| {
            samples.clear();
            for chunk in black_box(bytes).chunks_exact(4) {
                samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
            black_box(samples.as_slice());
        });
    });

    // PU-7: bulk reinterpret via slice::align_to — no per-sample work, no Vec.
    group.bench_function("new_align_to_bulk", |b| {
        b.iter(|| {
            // SAFETY: every bit pattern is a valid f32, and the input bytes are
            // fully initialized — same invariant as the WASAPI capture path.
            let (_head, samples, _tail) = unsafe { black_box(bytes).align_to::<f32>() };
            black_box(samples);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_push_throughput,
    bench_push_pop_roundtrip,
    bench_capacity_sweep,
    bench_wasapi_byte_decode
);
criterion_main!(benches);
