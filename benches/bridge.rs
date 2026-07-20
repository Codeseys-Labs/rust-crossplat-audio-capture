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
//! - **bridge_ab** (feature `bridge-zerocopy` only) — the default `AudioBuffer`
//!   ring (`create_bridge`) vs. the opt-in zero-copy `SampleRing` plane
//!   (`create_sample_ring`) on the identical producer-push + consumer-drain
//!   round trip, across mono/stereo × small/typical/large chunk sizes. A
//!   `bridge_ab_push/*` sibling times ONLY the producer push (`iter_custom`,
//!   drain untimed) — the isolated producer-side cost ADR-0006 §6
//!   promote-criterion #1 names, which the round trip structurally masks
//!   (SampleRing pays a consumer-side reconstruction copy the default ring's
//!   moved `Vec` never does). This is the A/B data ADR-0006's
//!   promote-or-remove decision needs (see
//!   `docs/designs/0006-bridge-zerocopy-samplering.md` §6). A no-op when the
//!   feature is off, since `SampleRing*` does not exist without it.
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

// ── bridge_ab: SampleRing vs. AudioBuffer (ADR-0006 promote-or-remove data) ─
//
// `--features bridge-zerocopy` only. A/Bs the default `AudioBuffer` ring
// (`create_bridge`) against the opt-in zero-copy `SampleRing` plane
// (`create_sample_ring`, `docs/designs/0006-bridge-zerocopy-samplering.md`)
// on the IDENTICAL producer-push + consumer-drain round trip, at the same
// chunk sizes, same sample counts, same warm-up discipline as the rest of
// this file — so the numbers are apples-to-apples and feed ADR-0006 §6
// promote-criterion #1. Group/benchmark names are stable and greppable
// (`bridge_ab/samplering/...` vs. `bridge_ab/audiobuffer/...`) so `bench.yml`
// and any future extraction script can select them by substring.
#[cfg(feature = "bridge-zerocopy")]
mod bridge_ab {
    use std::hint::black_box;
    use std::time::{Duration, Instant};

    use criterion::{BenchmarkId, Criterion, Throughput};

    use rsac::bridge::ring_buffer::{
        calculate_capacity, create_bridge, create_sample_ring, BridgeConsumer, BridgeProducer,
        SampleRingConsumer, SampleRingProducer,
    };
    use rsac::core::config::AudioFormat;

    /// One (channels, frames) case per row of the A/B matrix. Frame counts mirror
    /// the sizes already used elsewhere in this file: a small ~10 ms WASAPI-style
    /// packet (480 frames, see `WASAPI_PACKET_FRAMES` above), the typical 1024-frame
    /// period the rest of `bridge.rs` benches against (`FRAMES`/`SAMPLES` above),
    /// and a large 4096-frame period (matches `SCRATCH_MIN_FRAMES` /
    /// `TAP_BUFFER_FRAMES` sizing used by the Android/iOS backends).
    struct Case {
        label: &'static str,
        channels: u16,
        frames: usize,
    }

    const CASES: &[Case] = &[
        Case {
            label: "mono_small",
            channels: 1,
            frames: super::WASAPI_PACKET_FRAMES, // 480
        },
        Case {
            label: "stereo_small",
            channels: 2,
            frames: super::WASAPI_PACKET_FRAMES, // 480
        },
        Case {
            label: "mono_typical",
            channels: 1,
            frames: super::FRAMES, // 1024
        },
        Case {
            label: "stereo_typical",
            channels: 2,
            frames: super::FRAMES, // 1024
        },
        Case {
            label: "mono_large",
            channels: 1,
            frames: 4096,
        },
        Case {
            label: "stereo_large",
            channels: 2,
            frames: 4096,
        },
    ];

    /// Build a deterministic interleaved slice of `samples` `f32`s — the same
    /// generator shape as [`super::make_slice`], parameterized on size so every
    /// case (and both sides of the A/B) pushes an identical payload.
    fn make_case_slice(samples: usize) -> Vec<f32> {
        (0..samples).map(|i| (i as f32) * 1e-4).collect()
    }

    /// Warm the default `AudioBuffer` ring's free-list, mirroring [`super::warm_up`].
    fn warm_up_audiobuffer(
        producer: &mut BridgeProducer,
        consumer: &mut BridgeConsumer,
        slice: &[f32],
        channels: u16,
        sample_rate: u32,
    ) {
        for _ in 0..64 {
            producer.push_samples_or_drop(slice, channels, sample_rate);
            let _ = consumer.pop();
        }
    }

    /// Warm the `SampleRing`'s sample + metadata rings the same way — pushing
    /// and draining a few cycles before timing starts — so both sides of the A/B
    /// are measured in steady state, not during ring warm-up.
    fn warm_up_sample_ring(
        producer: &mut SampleRingProducer,
        consumer: &mut SampleRingConsumer,
        slice: &[f32],
        channels: u16,
        sample_rate: u32,
    ) {
        for _ in 0..64 {
            producer.push_samples_or_drop(slice, channels, sample_rate);
            let _ = consumer.pop();
        }
    }

    /// The default `AudioBuffer` ring side of the A/B: identical push+pop round
    /// trip to [`super::bench_push_pop_roundtrip`], parameterized over the case
    /// matrix so it can be compared directly against the `SampleRing` side below.
    fn bench_audiobuffer_side(c: &mut Criterion) {
        let sample_rate = super::SAMPLE_RATE;
        let capacity = calculate_capacity(Some(64), 4);

        let mut group = c.benchmark_group("bridge_ab/audiobuffer");
        for case in CASES {
            let samples = case.frames * case.channels as usize;
            let slice = make_case_slice(samples);
            group.throughput(Throughput::Elements(samples as u64));
            group.bench_with_input(BenchmarkId::from_parameter(case.label), case, |b, case| {
                let (mut producer, mut consumer) = create_bridge(capacity, AudioFormat::default());
                warm_up_audiobuffer(
                    &mut producer,
                    &mut consumer,
                    &slice,
                    case.channels,
                    sample_rate,
                );

                b.iter(|| {
                    let pushed = producer.push_samples_or_drop(
                        black_box(&slice),
                        case.channels,
                        sample_rate,
                    );
                    black_box(pushed);
                    let popped = consumer.pop();
                    black_box(popped);
                });
            });
        }
        group.finish();
    }

    /// The zero-copy `SampleRing` side of the A/B: identical push+pop round trip
    /// and case matrix, but through `create_sample_ring` instead of
    /// `create_bridge` — the producer writes straight into the ring's
    /// uninitialized slots (no `Vec`/`AudioBuffer` on this call) via
    /// `write_chunk_uninit` + `CopyToUninit` (see `SampleRingProducer` docs in
    /// `src/bridge/ring_buffer.rs`), and the consumer reconstructs an
    /// `AudioBuffer` equivalent to what the default ring would deliver.
    fn bench_samplering_side(c: &mut Criterion) {
        let sample_rate = super::SAMPLE_RATE;
        let capacity_chunks = calculate_capacity(Some(64), 4);

        let mut group = c.benchmark_group("bridge_ab/samplering");
        for case in CASES {
            let samples = case.frames * case.channels as usize;
            let slice = make_case_slice(samples);
            // Ring sized PER CASE (`capacity_chunks * this case's samples`), not
            // by the matrix maximum: a shared largest-case ring (~2 MB) would
            // make the linear producer/consumer cursor sweep touch cold cache
            // lines on the small/mono cases — a footprint asymmetry the
            // AudioBuffer side (tiny recycled Vec working set) never pays, and
            // therefore bias, not signal.
            let sample_capacity = capacity_chunks * samples;
            group.throughput(Throughput::Elements(samples as u64));
            group.bench_with_input(BenchmarkId::from_parameter(case.label), case, |b, case| {
                let (mut producer, mut consumer) =
                    create_sample_ring(sample_capacity, capacity_chunks, AudioFormat::default());
                warm_up_sample_ring(
                    &mut producer,
                    &mut consumer,
                    &slice,
                    case.channels,
                    sample_rate,
                );

                b.iter(|| {
                    let pushed = producer.push_samples_or_drop(
                        black_box(&slice),
                        case.channels,
                        sample_rate,
                    );
                    black_box(pushed);
                    let popped = consumer.pop();
                    black_box(popped);
                });
            });
        }
        group.finish();
    }

    /// Producer-side-only push cost — the metric ADR-0006 §6 promote-criterion
    /// #1 actually names ("producer-side win: lower p99 push cost and/or fewer
    /// copies"). The round-trip groups above cannot isolate it: on the pop
    /// side, `SampleRing` pays a second payload memcpy (ring → reconstructed
    /// `AudioBuffer`) while the default ring MOVES its recycled `Vec` out with
    /// no copy — so a producer-side win is structurally masked in a round
    /// trip. Here `iter_custom` times ONLY `push_samples_or_drop`; the drain
    /// that keeps the ring unsaturated (steady-state recycle path, never the
    /// drop path) happens outside the timed region. The `Instant` read
    /// overhead is identical on both sides, so the comparison stays fair.
    fn bench_push_only_sides(c: &mut Criterion) {
        let sample_rate = super::SAMPLE_RATE;
        let capacity_chunks = calculate_capacity(Some(64), 4);

        let mut group = c.benchmark_group("bridge_ab_push/audiobuffer");
        for case in CASES {
            let samples = case.frames * case.channels as usize;
            let slice = make_case_slice(samples);
            group.throughput(Throughput::Elements(samples as u64));
            group.bench_with_input(BenchmarkId::from_parameter(case.label), case, |b, case| {
                let (mut producer, mut consumer) =
                    create_bridge(capacity_chunks, AudioFormat::default());
                warm_up_audiobuffer(
                    &mut producer,
                    &mut consumer,
                    &slice,
                    case.channels,
                    sample_rate,
                );
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let start = Instant::now();
                        let pushed = producer.push_samples_or_drop(
                            black_box(&slice),
                            case.channels,
                            sample_rate,
                        );
                        total += start.elapsed();
                        black_box(pushed);
                        let _ = consumer.pop();
                    }
                    total
                });
            });
        }
        group.finish();

        let mut group = c.benchmark_group("bridge_ab_push/samplering");
        for case in CASES {
            let samples = case.frames * case.channels as usize;
            let slice = make_case_slice(samples);
            // Per-case ring sizing — same cache-fairness rationale as the
            // round-trip group above.
            let sample_capacity = capacity_chunks * samples;
            group.throughput(Throughput::Elements(samples as u64));
            group.bench_with_input(BenchmarkId::from_parameter(case.label), case, |b, case| {
                let (mut producer, mut consumer) =
                    create_sample_ring(sample_capacity, capacity_chunks, AudioFormat::default());
                warm_up_sample_ring(
                    &mut producer,
                    &mut consumer,
                    &slice,
                    case.channels,
                    sample_rate,
                );
                b.iter_custom(|iters| {
                    let mut total = Duration::ZERO;
                    for _ in 0..iters {
                        let start = Instant::now();
                        let pushed = producer.push_samples_or_drop(
                            black_box(&slice),
                            case.channels,
                            sample_rate,
                        );
                        total += start.elapsed();
                        black_box(pushed);
                        let _ = consumer.pop();
                    }
                    total
                });
            });
        }
        group.finish();
    }

    pub(super) fn bench_bridge_ab(c: &mut Criterion) {
        bench_audiobuffer_side(c);
        bench_samplering_side(c);
        bench_push_only_sides(c);
    }
}

#[cfg(feature = "bridge-zerocopy")]
fn bench_bridge_ab(c: &mut Criterion) {
    bridge_ab::bench_bridge_ab(c);
}

#[cfg(not(feature = "bridge-zerocopy"))]
fn bench_bridge_ab(_c: &mut Criterion) {
    // No-op without the feature: `SampleRing*` does not exist, so there is
    // nothing to A/B. Kept as a real (empty) criterion target rather than
    // conditionally omitted from `criterion_group!` so the group list below
    // stays a single unconditional statement regardless of feature state.
}

criterion_group!(
    benches,
    bench_push_throughput,
    bench_push_pop_roundtrip,
    bench_capacity_sweep,
    bench_wasapi_byte_decode,
    bench_bridge_ab
);
criterion_main!(benches);
