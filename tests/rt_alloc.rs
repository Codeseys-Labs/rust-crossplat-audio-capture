//! Allocation-counting integration test for the real-time bridge producer
//! (seeds `rsac-e40c`, hardened by `rsac-0940`).
//!
//! ADR-0001 (`docs/designs/0001-rt-allocation-guarantee.md`) claims
//! [`BridgeProducer::push_samples_or_drop`] is **allocation-free in steady
//! state**, with at most a single bounded one-time growth during warm-up or when
//! the callback period grows. Nothing measured that claim — this test does.
//!
//! ## How it works
//!
//! A [`CountingAllocator`] wraps the standard [`System`] allocator and bumps an
//! [`AtomicUsize`] on every `alloc`/`realloc`/`dealloc`. It is installed as the
//! process-wide `#[global_allocator]`. Because that counter is **process-global**,
//! *any* thread that allocates during a measured region inflates the count — and
//! libtest runs `#[test]` fns on freshly spawned threads, so with multiple tests
//! in this binary even a mutex-serialized measurement could be perturbed by a
//! parallel test thread's incidental allocations (thread spawn, harness
//! bookkeeping, panic machinery). The robust fix (`rsac-0940`) is structural:
//! **all measured scenarios are merged into a single `#[test]`** and run
//! sequentially on one thread, so no sibling test thread can ever exist during a
//! measured region — the proofs pass identically with or without
//! `--test-threads=1`. (Filtering the allocator by thread id was considered and
//! rejected: `std::thread::current()` uses TLS and can itself allocate inside
//! the alloc hook — a recursion hazard — and an OS-TID syscall per allocation
//! would add platform-specific FFI for no additional rigor.) The counting
//! allocator is test code only and never reaches the shipped library —
//! `#[global_allocator]` here applies solely to this test executable.
//!
//! [`with_alloc_count`] snapshots the allocation counter (alloc + realloc — the
//! operations that actually request heap memory) around a closure and returns how
//! many allocations occurred *inside* it. The measurement harness is careful not
//! to allocate within the measured region (no formatting, no growth) so the count
//! reflects only the code under test.
//!
//! The merged proof:
//!   1. Warms the free-list by pushing+popping a fixed 1024-frame stereo period
//!      until the producer recycles allocations instead of allocating.
//!   2. Asserts **zero** allocations across >= 1000 steady-state push/pop cycles
//!      at the constant period (untimed and stamped variants).
//!   3. Asserts the **saturated-ring drop path** — every push rejected with
//!      `Err(Full)`, its `Vec` reclaimed into scratch, the stream position still
//!      advancing — is also allocation-free (the reclaim path is exactly what a
//!      stalled consumer exercises on the RT thread).
//!   4. Asserts a period **increase** beyond the seeded capacity triggers at most
//!      a *bounded one-time* allocation, after which the larger period is again
//!      allocation-free (ADR-0001 "bounded one-time growth").

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rsac::bridge::ring_buffer::{create_bridge, BridgeConsumer, BridgeProducer};
use rsac::core::config::AudioFormat;

// ── Counting global allocator ─────────────────────────────────────────────

/// Number of `alloc` calls observed (new heap requests).
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
/// Number of `realloc` calls observed (grow/shrink of an existing block).
static REALLOCS: AtomicUsize = AtomicUsize::new(0);
/// Number of `dealloc` calls observed (frees). Tracked for completeness; the
/// steady-state guarantee is about *allocations*, not frees.
static DEALLOCS: AtomicUsize = AtomicUsize::new(0);

/// A [`GlobalAlloc`] that forwards to [`System`] while counting every operation.
///
/// Counting uses `Relaxed` atomics: the measured regions run single-threaded (a
/// single merged `#[test]` — see the module docs), so the only requirement is
/// that the bumps are visible to the same thread in program order, which
/// `Relaxed` already guarantees. We deliberately do **no** allocation,
/// locking, or formatting inside these methods so the allocator itself never
/// perturbs the count it is measuring.
struct CountingAllocator(System);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        self.0.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        DEALLOCS.fetch_add(1, Ordering::Relaxed);
        self.0.dealloc(ptr, layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        REALLOCS.fetch_add(1, Ordering::Relaxed);
        self.0.realloc(ptr, layout, new_size)
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        self.0.alloc_zeroed(layout)
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator(System);

/// Current count of heap *allocations* (alloc + realloc). `dealloc` is excluded:
/// the RT guarantee is "no new heap requests", and a recycled buffer being freed
/// later on the consumer side is not an RT-thread allocation.
fn alloc_count() -> usize {
    ALLOCS.load(Ordering::Relaxed) + REALLOCS.load(Ordering::Relaxed)
}

/// Run `f` and return the number of heap allocations (alloc + realloc) that
/// occurred inside it.
///
/// For the count to be meaningful the closure must not allocate as an incidental
/// side effect of being called; callers keep the measured region free of
/// formatting/growth and surface any results via captured `&mut` locals (which
/// do not allocate) rather than by returning owned, heap-backed values.
fn with_alloc_count<F: FnOnce()>(f: F) -> usize {
    let before = alloc_count();
    f();
    let after = alloc_count();
    after - before
}

// ── Fixtures ───────────────────────────────────────────────────────────────

const CHANNELS: u16 = 2;
const SAMPLE_RATE: u32 = 48_000;

/// 1024-frame stereo period (2048 interleaved f32) — the realistic worst-case
/// callback period the bridge seeds for (ADR-0001 `RT_BUFFER_SAMPLE_CAPACITY`).
const STEADY_FRAMES: usize = 1024;
const STEADY_SAMPLES: usize = STEADY_FRAMES * CHANNELS as usize; // 2048

/// Measure the allocations of **one** `push_samples_or_drop` (the RT producer
/// path ADR-0001 governs), returning `(allocs, pushed)`.
///
/// Only the producer call is inside the measured region. `pop` is deliberately
/// **not** measured here: by design [`BridgeConsumer::pop`] *moves* the ring's
/// buffer to the user with no clone (`rsac-17d1`) and recycles a spare
/// `Vec<f32>` back to the producer's free-list — allocating that spare on the
/// consumer side when its pool runs dry. That allocation is intentional and
/// happens on the non-RT consumer thread. The RT guarantee is solely about the
/// producer, so the test isolates it.
fn measure_push(producer: &mut BridgeProducer, slice: &[f32]) -> (usize, bool) {
    let mut pushed = false;
    let allocs = with_alloc_count(|| {
        pushed = producer.push_samples_or_drop(slice, CHANNELS, SAMPLE_RATE);
    });
    (allocs, pushed)
}

/// Drive the free-list to steady state: enough push/pop cycles that the producer
/// reuses recycled allocations rather than allocating. The seed is `min(cap, 8)`
/// buffers; a few hundred cycles is far more than enough to converge. `pop`
/// recycles a spare `Vec` back to the producer's free-list on every cycle.
fn warm_up(producer: &mut BridgeProducer, consumer: &mut BridgeConsumer, slice: &[f32]) {
    for _ in 0..512 {
        producer.push_samples_or_drop(slice, CHANNELS, SAMPLE_RATE);
        let _ = consumer.pop();
    }
}

// ── Proof scenarios ────────────────────────────────────────────────────────
//
// Each scenario is a plain fn (not a `#[test]`): the single merged test at the
// bottom runs them sequentially on one thread so the process-global allocation
// counter is never perturbed by a sibling test thread (see the module docs).

/// Steady-state `push_samples_or_drop` at a constant 1024-frame stereo period
/// performs **zero** heap allocations once the free-list is warm (ADR-0001).
///
/// The measured region covers only the producer's push (`pop` recycles the
/// allocation back between measurements, outside the count — see [`measure_push`]).
fn push_samples_or_drop_is_alloc_free_in_steady_state() {
    // The slice and bridge are built BEFORE any measured region, so their
    // allocations are not counted.
    let slice: Vec<f32> = (0..STEADY_SAMPLES).map(|i| (i as f32) * 1e-4).collect();
    let (mut producer, mut consumer) = create_bridge(64, AudioFormat::default());

    warm_up(&mut producer, &mut consumer, &slice);

    // Steady state: >= 1000 cycles at the constant period. Each cycle measures the
    // producer push in isolation, then pops (unmeasured) to recycle the buffer.
    const CYCLES: usize = 2000;
    let mut total_push_allocs = 0usize;
    let mut pushes_ok = 0u64;
    for _ in 0..CYCLES {
        let (allocs, pushed) = measure_push(&mut producer, &slice);
        total_push_allocs += allocs;
        if pushed {
            pushes_ok += 1;
        }
        // Recycle a spare back to the producer's free-list (the pop hands the
        // user the ring's buffer moved, not cloned, and may allocate the spare
        // on the consumer side — by design, outside the measured region).
        let _ = consumer.pop();
    }

    assert_eq!(
        total_push_allocs, 0,
        "push_samples_or_drop must be allocation-free across {CYCLES} steady-state \
         cycles at a constant 1024-frame stereo period (ADR-0001); observed \
         {total_push_allocs} allocations on the producer hot path"
    );

    // Sanity: every cycle pushed successfully (ring never saturates because we pop
    // each iteration), so the zero-alloc result is genuine, not a dead loop.
    assert_eq!(
        pushes_ok, CYCLES as u64,
        "every steady-state push should have succeeded"
    );
}

/// The stream-position-stamping variants (`push_samples_or_drop_stamped` /
/// `push_samples_guarded_stamped`) are what the platform backends now run on
/// the RT path (rsac-ec25 wiring), so ADR-0001's zero-allocation guarantee is
/// proved for them too: the stamp is pure integer math over a plain `u64`
/// nanosecond accumulator (rsac-1b8c) — no clock syscall, no allocation.
fn stamped_push_is_alloc_free_in_steady_state() {
    let slice: Vec<f32> = (0..STEADY_SAMPLES).map(|i| (i as f32) * 1e-4).collect();
    let (mut producer, mut consumer) = create_bridge(64, AudioFormat::default());

    warm_up(&mut producer, &mut consumer, &slice);

    const CYCLES: usize = 2000;
    let mut total_push_allocs = 0usize;
    let mut pushes_ok = 0u64;
    for i in 0..CYCLES {
        // Alternate between the plain and panic-guarded stamped variants so
        // both RT entry points (Windows thread / PipeWire+CoreAudio callbacks)
        // are covered by the same proof.
        let mut pushed = false;
        let allocs = with_alloc_count(|| {
            pushed = if i % 2 == 0 {
                producer.push_samples_or_drop_stamped(&slice, CHANNELS, SAMPLE_RATE)
            } else {
                producer.push_samples_guarded_stamped(&slice, CHANNELS, SAMPLE_RATE)
            };
        });
        total_push_allocs += allocs;
        if pushed {
            pushes_ok += 1;
        }
        let _ = consumer.pop();
    }

    assert_eq!(
        total_push_allocs, 0,
        "stamped pushes must be allocation-free across {CYCLES} steady-state \
         cycles (ADR-0001); observed {total_push_allocs} allocations on the \
         producer hot path"
    );
    assert_eq!(
        pushes_ok, CYCLES as u64,
        "every steady-state stamped push should have succeeded"
    );
}

/// The **saturated-ring drop path** is allocation-free too (`rsac-0940`): with
/// no consumer pops, every stamped push is rejected with `Err(Full)` and its
/// `Vec` is reclaimed into the producer's scratch slot for reuse — the exact
/// path a stalled consumer forces onto the RT thread. This was previously
/// unmeasured; ADR-0001's guarantee covers it just as much as the happy path.
///
/// The scenario also proves the stream position keeps advancing through the
/// drops (rsac-1b8c gap semantics): the first post-drop push must be stamped
/// past the whole dropped duration.
fn saturated_ring_drop_path_is_alloc_free() {
    let slice: Vec<f32> = (0..STEADY_SAMPLES).map(|i| (i as f32) * 1e-4).collect();
    // Small ring so saturation is immediate; free-list seeded with min(4,8)=4.
    let (mut producer, mut consumer) = create_bridge(4, AudioFormat::default());

    // Fill the ring WITHOUT popping (unmeasured): 4 successful stamped pushes
    // consume the seeded free-list vecs and saturate the ring.
    const WARM_PUSHES: u64 = 4;
    for _ in 0..WARM_PUSHES {
        assert!(
            producer.push_samples_or_drop_stamped(&slice, CHANNELS, SAMPLE_RATE),
            "warm pushes into an empty ring must succeed"
        );
    }

    // Measured: N stamped pushes that ALL drop. Each exercises the
    // free-list-empty → scratch → Err(Full) → reclaim-into-scratch cycle plus
    // the position advance, under the allocation counter.
    const DROPS: u64 = 1000;
    let mut all_dropped = true;
    let allocs = with_alloc_count(|| {
        for _ in 0..DROPS {
            if producer.push_samples_or_drop_stamped(&slice, CHANNELS, SAMPLE_RATE) {
                all_dropped = false;
            }
        }
    });

    assert!(
        all_dropped,
        "with no consumer pops the saturated ring must reject every push"
    );
    assert_eq!(
        allocs, 0,
        "the saturated-ring drop path (Err(Full) + scratch reclaim + position \
         advance) must be allocation-free on the producer (ADR-0001 / rsac-0940); \
         observed {allocs} allocations across {DROPS} dropped pushes"
    );
    assert_eq!(
        producer.buffers_dropped(),
        DROPS,
        "every measured push must have been counted as a drop"
    );

    // Position advanced through the drops (rsac-1b8c): drain the delivered
    // buffers, then the next successful push must be stamped past the whole
    // cumulative gap. Each push advances by the same floored per-period value,
    // so the expected cumulative stamp is exact.
    while consumer.pop().is_some() {}
    assert!(producer.push_samples_or_drop_stamped(&slice, CHANNELS, SAMPLE_RATE));
    let after = consumer.pop().expect("post-drop buffer must be delivered");
    let per_push_nanos = (STEADY_FRAMES as u64) * 1_000_000_000 / u64::from(SAMPLE_RATE);
    let expected = Duration::from_nanos((WARM_PUSHES + DROPS) * per_push_nanos);
    assert_eq!(
        after.timestamp(),
        Some(expected),
        "the stream position must keep advancing through the dropped pushes \
         (gap semantics — rsac-1b8c)"
    );
}

/// A callback-period **increase** beyond the seeded buffer capacity triggers at
/// most a *bounded one-time* allocation on the producer; once the recycled buffers
/// have grown to the new high-water mark, the larger period is allocation-free
/// again (ADR-0001 "bounded one-time growth").
fn period_increase_triggers_bounded_one_time_allocation() {
    // Start at the seeded steady period (fits the pre-sized buffers).
    let small: Vec<f32> = (0..STEADY_SAMPLES).map(|i| (i as f32) * 1e-4).collect();
    // A larger period — double the frames — exceeds the seeded buffer capacity
    // (2048 samples), forcing each recycled Vec to grow once to fit.
    let large_frames = STEADY_FRAMES * 2;
    let large_samples = large_frames * CHANNELS as usize; // 4096
    let large: Vec<f32> = (0..large_samples).map(|i| (i as f32) * 1e-4).collect();

    let (mut producer, mut consumer) = create_bridge(64, AudioFormat::default());

    // Warm to steady state at the small period — buffers are sized for it.
    warm_up(&mut producer, &mut consumer, &small);

    // First transition to the larger period: each in-flight recycled buffer must
    // grow once (a realloc) to fit the bigger slice. The number of distinct
    // buffers in circulation is bounded (free-list seed min(cap,8)=8 plus the
    // ring's worth), so the total growth allocations are bounded — NOT one per
    // cycle. We measure only the producer pushes; pops recycle (unmeasured).
    const GROWTH_CYCLES: usize = 256;
    let mut growth_allocs = 0usize;
    for _ in 0..GROWTH_CYCLES {
        let (allocs, _pushed) = measure_push(&mut producer, &large);
        growth_allocs += allocs;
        let _ = consumer.pop();
    }

    // Bounded: growth is one-time per circulating buffer, not per cycle. The pool
    // is small (≤ seed + ring), so the count must be far below GROWTH_CYCLES.
    assert!(
        growth_allocs <= 64,
        "period increase must trigger at most a bounded one-time growth on the \
         producer, not a per-cycle allocation; observed {growth_allocs} allocations \
         over {GROWTH_CYCLES} cycles"
    );

    // After the growth pass every circulating buffer has grown to fit the larger
    // period, so it must now be allocation-free on the producer hot path.
    const STEADY_CYCLES: usize = 1000;
    let mut steady_allocs = 0usize;
    for _ in 0..STEADY_CYCLES {
        let (allocs, _pushed) = measure_push(&mut producer, &large);
        steady_allocs += allocs;
        let _ = consumer.pop();
    }

    assert_eq!(
        steady_allocs, 0,
        "once recycled buffers have grown to the new high-water mark, the larger \
         period must be allocation-free again (ADR-0001); observed {steady_allocs} \
         allocations on the producer hot path"
    );
}

// ── The single merged test ─────────────────────────────────────────────────

/// Run every allocation proof sequentially on one thread (`rsac-0940`).
///
/// One `#[test]` means libtest never has a sibling test thread alive while a
/// region is being measured, so the process-global counting allocator observes
/// ONLY the code under test — the proofs are deterministic with or without
/// `--test-threads=1`. Scenario names appear in assertion messages, so a
/// failure still pinpoints which proof broke.
#[test]
fn rt_alloc_proofs() {
    push_samples_or_drop_is_alloc_free_in_steady_state();
    stamped_push_is_alloc_free_in_steady_state();
    saturated_ring_drop_path_is_alloc_free();
    period_increase_triggers_bounded_one_time_allocation();
}
