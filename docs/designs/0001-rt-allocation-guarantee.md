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

## 5a. FFI-boundary panic guard (PS-4 / `rsac-5a48`)

`push_samples_guarded` wraps `push_samples_or_drop` in `std::panic::catch_unwind`
so a panic raised inside a push can never unwind out of an OS audio callback. It is
the production push used at the two **foreign C-callback** boundaries — the macOS
CoreAudio IOProc (`src/audio/macos/thread.rs`, `set_input_callback`) and the Linux
PipeWire `.process` callback (`src/audio/linux/thread.rs`), which is invoked from
inside `main_loop.iterate()`. An unwind across either of those frames is undefined
behavior, so the guard is genuinely needed there.

The Windows WASAPI capture loop runs on **rsac's own Rust thread** (not a foreign
callback), where an unwind is well-defined; it keeps the unguarded
`push_samples_or_drop`. This is the chosen resolution of PS-4 (option A: wire the
guard at the FFI sites rather than delete it).

### Why this does not weaken the ADR-0001 guarantee

The guard must stay **alloc-free on the happy path**, and it is: the
`catch_unwind` closure only borrows `&mut self` and the sample slice — it captures
no owned, heap-backed state and performs no allocation itself, so `catch_unwind`
adds only a thread-local landing-pad register on the no-panic path. The only heap
work in a guarded push is whatever `push_samples_or_drop_inner` already does, which
§4/§5 prove is zero in steady state. The `tests/rt_alloc.rs` probe exercises
`push_samples_or_drop` directly (the shared core both the guarded and unguarded
entry points funnel into), so the measured guarantee is unchanged by the choice of
entry point; a unit regression
(`push_samples_guarded_is_alloc_free_on_happy_path`) additionally asserts the
guarded wrapper itself is allocation-free in steady state.

## 5b. `ConsumerWake` notify is RT-safe by construction (PU-5 / `rsac-efb4`, `rsac-0ebe`)

PU-5 gave the **synchronous** blocking reader (`BridgeConsumer::pop_blocking`) a
real wake primitive, `ConsumerWake`, instead of a fixed busy-poll. Because that
primitive touches a `std::sync::Condvar`/`Mutex` pair, where and from what thread
its `notify` may fire is an ADR-0001 concern: a futex-backed signal on an OS audio
callback would reintroduce the very lock-on-RT hazard this ADR exists to forbid.
This section records the design and its load-bearing invariant.

### Design

`ConsumerWake` (`src/bridge/ring_buffer.rs`) is **always present** — not gated
behind the `async-stream` feature — so every build, async or not, gets a real
wakeup rather than degrading to a sleep/poll loop. It is three fields:

- `lock: Mutex<()>` — a trivial mutex whose *protected datum is `()`*. The real
  "did something change?" signal is the generation counter, not any guarded state.
  A poisoned lock is recovered in place (`into_inner`) so a panic elsewhere can
  never wedge the reader.
- `cvar: Condvar` — what a parked reader waits on.
- `generation: AtomicU64` — a monotonic notify counter that closes the classic
  lost-wakeup race **without** holding the lock across the producer's push.

`notify()` does `generation.fetch_add(1, Release)` **then** `cvar.notify_all()`,
and crucially holds **no lock** while signalling and **allocates nothing** (on
Linux it lowers to a single `FUTEX_WAKE`). Bumping the generation *before*
signalling lets a waiter detect — under its own lock, via the `since` snapshot it
took before its last ring/state re-check — a notify that raced that check.

`wait(since, timeout)` is the only side that takes the lock, and it runs **only on
the non-RT consumer thread**. If `generation != since` it returns immediately
(the signal would otherwise be lost); otherwise it parks for at most one
`WAKE_BACKSTOP_POLL` slice.

`WAKE_BACKSTOP_POLL` (= **1 ms**) is the **degrade-not-hang backstop**: a bounded
re-check that bounds worst-case latency on a *missed or absent* notify, never the
common path. It covers two cases — (a) the residual hairline race where a notify
lands in the instant the waiter is between its generation re-check and the kernel
park (the notifier holds no lock, so this window cannot be fully closed by the
generation alone), and (b) backends that deliberately do **not** notify (below).

### The invariant: notify fires only from non-RT sites

`notify` is wired into exactly the sites that run **off** the RT audio callback:

- the **Windows** WASAPI capture loop, which runs on **rsac's own Rust thread**
  (not an OS callback), via `BridgeProducer::notify_consumers()`; and
- every **terminal/ending state transition** — `BridgeProducer::signal_done`,
  `BridgeProducer::signal_error`, and `BridgeStream::stop` — via the shared
  `notify_wake()`.

It is deliberately **NOT** called from `push_samples_or_drop_inner`, the shared
hot path the **Linux (PipeWire)** and **macOS (CoreAudio)** backends drive from
their **real-time audio callbacks**. Adding a notify there would put a
(brief) lock-touching futex call on the RT thread — exactly the ADR-0001
violation §1 exists to prevent. Those backends instead rely on the retained
`WAKE_BACKSTOP_POLL` re-check in `pop_blocking` plus the terminal-state notify on
stop, so a parked reader still picks up RT-pushed data within ≤1 ms and is woken
promptly on stop, with **zero** wake-primitive cost on the RT push path.

This keeps the §4/§5 allocation guarantee intact: the RT push path that
`tests/rt_alloc.rs` probes never reaches `ConsumerWake::notify` at all.

### Regression coverage

`src/bridge/ring_buffer.rs` unit tests pin the design:
`consumer_wake_generation_advances_on_notify`,
`consumer_wake_wait_returns_immediately_when_generation_moved`,
`notify_consumers_bumps_wake_generation`,
`signal_done_and_error_bump_wake_generation`,
`push_then_notify_wakes_parked_pop_blocking_promptly`, and — the load-bearing
proof of the RT-backend degrade path —
`pop_blocking_picks_up_data_without_notify_via_backstop`, which pushes **without**
calling `notify_consumers()` (mirroring the RT-callback push path) and asserts the
reader still unblocks via the backstop alone.

## 6. References

- Audit critique H3 (2026-05-29 deep-dive), `docs/reviews/` family.
- `BridgeProducer::push_samples_or_drop_inner` (the shared alloc-free core) and the
  `RT_BUFFER_SAMPLE_CAPACITY` constant, both in `src/bridge/ring_buffer.rs`.
- Regression test `scratch_never_shrinks_to_zero_after_underrun`
  (`src/bridge/ring_buffer.rs`) and the `tests/rt_alloc.rs` allocator-probe integration
  test.
- `ConsumerWake`, `WAKE_BACKSTOP_POLL`, and `BridgeProducer::notify_consumers`
  (all in `src/bridge/ring_buffer.rs`) for the §5b wake-notify design (PU-5,
  seeds `rsac-efb4` / `rsac-0ebe`).
