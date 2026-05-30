# ADR 0001 — Real-time producer allocation guarantee

**Status:** Accepted
**Date:** 2026-05-29
**Scope:** `src/bridge/ring_buffer.rs` — the `BridgeProducer::push_samples_*`
family. The alloc-free recycle logic lives in the shared core
`BridgeProducer::push_samples_or_drop_inner`; the public entry points
(`push_samples_or_drop`, `push_samples_or_drop_at`, `push_samples_reporting`) all
delegate to it, so every push variant shares the identical guarantee.
**Verdict:** The RT producer is **allocation-free in steady state**, with a single
bounded warm-up/resize cost. Fix the scratch-shrink bug and pre-size buffers to the
negotiated period so the steady-state guarantee actually holds.

## 1. Context

The branch `fix/rt-safety-bridge-and-platform-p0` introduced a "free-list return
ring": the consumer recycles drained `Vec<f32>` allocations back to the producer so
the OS audio callback thread (`push_samples_or_drop_inner`, reached via
`push_samples_or_drop`) does not allocate. This is the library's headline
real-time-safety property — its #1 documented rule is "no allocation/lock/blocking
on the RT callback thread."

A deep-dive audit (2026-05-29) found the guarantee is weaker than advertised:

1. **Scratch-shrink bug** — when the free-list ring is empty the producer does
   `std::mem::take(&mut self.scratch)` (leaving a capacity-0 Vec). The success arm of
   the subsequent `push` never restores `scratch`; only the ring-full arm does. So
   after one free-list-empty *successful* push, `scratch` is permanently capacity-0,
   and every later free-list-empty push allocates a fresh `Vec` on the RT thread until
   a recycled Vec or a ring-full rejection happens to refill it.
2. **Fixed seed capacity** — seeds and scratch are `Vec::with_capacity(1024)`, but
   CoreAudio routinely delivers >1024 samples (1024 stereo frames = 2048 samples), so
   `extend_from_slice` reallocates inside the callback on first (and every larger)
   packet.

Allocating on an audio callback thread risks a page-fault / lock inside the global
allocator → priority inversion → audible glitches. This is precisely the failure the
design exists to prevent.

## 2. Decision drivers

- Correctness-first: the stated RT-safety rule must be true, not aspirational.
- Honesty: if the guarantee is conditional, the condition must be documented and the
  steady state must actually be allocation-free.
- Minimal surface change: this is a bug-fix branch, not a redesign.

## 3. Considered options

### Option A — Keep "allocation-free" unconditional, redesign to a fixed slab pool
Pre-allocate a fixed set of max-size sample buffers at `create_bridge` time and never
touch the allocator again; drop on pool exhaustion.
- ➕ Strongest guarantee (truly zero allocation forever).
- ➖ Larger change; needs a known max frame count up front; wastes memory when periods
  are small; doesn't match the existing recycle design on this bug-fix branch.

### Option B — Steady-state guarantee + fix the two holes (CHOSEN)
Keep the free-list recycle design. (1) In the success arm, refill `scratch` best-effort
from a recycled Vec so the single-slot fallback is never zero-capacity. (2) Size seeds
and scratch from a named constant tuned to a realistic worst-case period
(`RT_BUFFER_SAMPLE_CAPACITY`), and let recycled Vecs grow to the high-water mark so
steady state converges to zero allocation. Document the guarantee as: *allocation-free
in steady state; bounded one-time growth during warm-up or when the period grows.*
- ➕ Small, surgical, matches the branch's design; eliminates the confirmed bug; honest.
- ➖ First few callbacks (or a period increase) may still allocate once — bounded and
  amortized, but not literally zero on the very first packet.

### Option C — Do nothing, just document "steady-state only"
- ➖ Leaves the scratch-shrink bug, which causes *repeated* (not one-time) RT
  allocations under normal producer/consumer jitter. Rejected: documenting a bug is not
  fixing it.

## 4. Decision

**Option B.** The recycle design is sound; the defects are a missing `scratch` refill
on the success path and an under-sized seed capacity. Fix both, add a regression test
that drives the free-list empty repeatedly and asserts `scratch` capacity never
collapses to 0, and document the guarantee precisely.

## 5. Consequences

- `push_samples_or_drop_inner`'s success arm refills `scratch` from `free_rx` (a
  recycled buffer) when it used the scratch fallback; if no recycled buffer is yet
  available it restores a `Vec::with_capacity(RT_BUFFER_SAMPLE_CAPACITY)` so the next
  `extend_from_slice` reuses that capacity instead of growing from zero.
- Seed/scratch capacity comes from a single named constant, `RT_BUFFER_SAMPLE_CAPACITY`
  (= 2048 interleaved samples, a 1024-frame stereo worst-case period). Recycled buffers
  grow to fit larger periods and are retained at the high-water mark.
- Doc comment on the method and `VISION.md` principle #3 are reworded to "allocation-free
  in steady state" rather than implying unconditional.
- A unit test (`scratch_never_shrinks_to_zero_after_underrun`) guards the regression.
  The end-to-end empirical proof — a process-wide counting allocator asserting zero
  steady-state heap growth across thousands of producer cycles — lives in the
  `tests/rt_alloc.rs` integration test.

## 6. References

- Audit critique H3 (2026-05-29 deep-dive), `docs/reviews/` family.
- `BridgeProducer::push_samples_or_drop_inner` (the shared alloc-free core) and the
  `RT_BUFFER_SAMPLE_CAPACITY` constant, both in `src/bridge/ring_buffer.rs`.
- Regression test `scratch_never_shrinks_to_zero_after_underrun`
  (`src/bridge/ring_buffer.rs`) and the `tests/rt_alloc.rs` allocator-probe integration
  test.
