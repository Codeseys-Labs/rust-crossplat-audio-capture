# ADR 0008 — Hand-rolled `CachePadded` for false-sharing mitigation in `BridgeShared`

**Status:** Accepted
**Date:** 2026-05-30
**Scope:** `src/bridge/ring_buffer.rs` (`CachePadded<T>`, `CACHE_LINE_BYTES`, and the
`BridgeShared` counter layout)
**Verdict:** Use a small internal `#[repr(align(64))]` `CachePadded<T>` newtype (not a
`crossbeam` dependency) to separate the producer-hot diagnostic counters from the
single consumer-hot counter onto distinct cache lines. Target a 64-byte line. Record
that the padding is currently **partial** — the per-push drop-window atoms and cursor
are deliberately unpadded.

## 1. Context

`BridgeShared` holds the lock-free diagnostic counters both sides of the bridge update
without taking a lock. On every audio callback the **producer** writes
`buffers_pushed`, `buffers_dropped`, and `consecutive_drops`; on every pop the
**consumer** writes `buffers_popped`. If those two writers' counters land on the same
64-byte cache line, the line ping-pongs between the producing core and the consuming
core (false sharing): each write invalidates the other core's cached copy, adding p99
jitter to the real-time push — the exact tail-latency the bridge exists to avoid.

`rtrb` already pads its own head/tail this way for its internal cursors. `rsac-9348`
applied the same pattern to rsac's *extended* counters. The audit (2026-05-30, the
"CachePadded false-sharing mitigation" ADR gap) asked for this choice — the hand-rolled
newtype, the 64-vs-128-byte line size, and the partial-padding caveat — to be recorded.

## 2. Decision drivers

- **RT tail-latency:** the producer's push must not pay a cache-coherence stall caused by
  the consumer's pop writing an adjacent counter.
- **Minimal dependencies:** the seed's acceptance criteria preferred a tiny internal
  newtype over pulling in `crossbeam-utils` for one wrapper, to keep the dependency
  surface (and audit/supply-chain cost) small.
- **Zero call-site churn:** padding must be transparent — every existing
  `self.field.load(..)` / `.fetch_add(..)` / `.store(..)` must compile unchanged.
- **Honesty:** if the mitigation is partial, the doc must say which atomics are *not*
  isolated and why, rather than implying every shared atomic is on its own line.

## 3. Considered options

### Option A — Depend on `crossbeam_utils::CachePadded`
- ➕ Battle-tested; per-arch line size baked in; no local code to maintain.
- ➖ A whole dependency (and its transitive surface) for a single, trivial wrapper on a
   library that is otherwise dependency-light. Against the seed's acceptance criteria.
   Rejected.

### Option B — Hand-rolled `#[repr(align(64))] CachePadded<T>` newtype (CHOSEN)
A tuple newtype `pub(crate) struct CachePadded<T>(pub(crate) T);` with `#[repr(align(64))]`,
a `const fn new`, and transparent `Deref`/`DerefMut` so every call site is unchanged. Wrap
each producer-hot counter and the consumer-hot `buffers_popped` so the two writer groups
occupy distinct cache lines.
- ➕ Zero new dependencies; transparent (Deref-through) so no call-site churn; isolates the
   hot counters by forcing ≥64-byte alignment *and* rounding each wrapped value's size up to
   a full line so two adjacent wrapped values cannot share a line.
- ➖ Hard-codes the line size (see §4); does not auto-tune per target; padding currently
   applied to only the counter group, not every shared atomic (see §4).

### Option C — No padding, accept false sharing
- ➖ Leaves the producer's RT push exposed to coherence stalls from consumer pops on
   many-core systems — the precise jitter source the bridge is built to eliminate.
   Rejected.

## 4. Decision

**Option B.** Wrap the counters in a hand-rolled `CachePadded<T>` with these recorded
specifics:

- **Target a 64-byte cache line.** `#[repr(align(64))]` (the literal `64`, because
  `repr(align(N))` requires an integer literal). 64 bytes is the line size on every
  mainstream 64-bit target rsac builds for: **x86-64** and **aarch64** use 64-byte lines.
  **Apple silicon**'s 128-byte L1 line is effectively a pair of 64-byte sublines, so
  64-byte alignment still places the producer group and the consumer counter on distinct
  64-byte regions and removes the ping-pong in practice; we accept that on Apple silicon a
  128-byte pad would be marginally stricter but is not needed for the false-sharing win.
  The named constant `CACHE_LINE_BYTES = 64` exists only to pin the literal in the
  alignment regression tests and is `#[cfg(test)]`-gated (no runtime use), because the
  `repr(align(..))` attribute cannot reference a `const`.

- **Which atomics are padded.** The producer-written group — `buffers_pushed`,
  `buffers_dropped`, `consecutive_drops` — and the lone consumer-written `buffers_popped`
  are each wrapped in `CachePadded`, so the producer group and the consumer counter sit on
  distinct lines. Two tests lock this in: `cache_padded_is_cache_line_aligned` (asserts
  `align_of::<CachePadded<AtomicU64/U32>>() >= 64` and the size rounds up to a full line)
  and `producer_and_consumer_counters_on_distinct_cache_lines` (asserts `buffers_popped`
  does not share a line with any producer counter in a live `BridgeShared`).

- **Padding is currently PARTIAL (recorded honestly).** The per-push **drop-window**
  state — the `drop_window: [AtomicU64; DROP_WINDOW_SLOTS]` array and its
  `drop_window_cursor: AtomicU64` — is **not** wrapped in `CachePadded`. These are written
  by the producer on every push attempt (`record_drop_window`), so in principle they could
  false-share with the adjacent unpadded fields (`negotiated`, `push_panicked`, the waker).
  They are left unpadded deliberately for now: (1) they are producer-only writes (the
  consumer only *reads* them in `drop_window_snapshot`, off the RT thread), so the
  cross-core write/write ping-pong the padding targets does not apply to them in the same
  way as the producer-vs-consumer counter pair; and (2) the primary, measured win is
  separating the producer-hot counters from the consumer-hot `buffers_popped`. Fully
  isolating the drop-window onto its own line is a possible future refinement, not a
  shipped guarantee.

## 5. Consequences

- `BridgeShared`'s producer-hot counters and consumer-hot counter are on distinct 64-byte
  lines; the RT push no longer eats a coherence stall from the consumer's pop on many-core
  machines.
- The wrapper is transparent: all counter accesses go through `Deref`/`DerefMut`, so no
  call site changed and `CachePadded` adds no runtime cost beyond the alignment/padding.
- No new dependency was added for the mitigation.
- The false-sharing guarantee covers **only** the counter group, not every atomic in
  `BridgeShared`. The drop-window atomics and cursor remain unpadded; a doc/code reader
  must not assume every shared atomic is line-isolated. Tightening this (padding the
  drop-window group) is left as an optional future refinement.
- The 64-byte target is a deliberate floor, not a per-architecture optimum; Apple
  silicon's 128-byte line would permit a stricter pad if a future measurement shows it
  matters.

## 6. References

- Audit "CachePadded false-sharing mitigation" ADR gap (lower priority than the
  device-watch ADR) — `docs/reviews/rsac-architecture-critique-2026-05-30.md`.
- `src/bridge/ring_buffer.rs` — `CachePadded<T>`, `CACHE_LINE_BYTES`, the `BridgeShared`
  counter fields and their cache-line-layout doc, the unpadded `drop_window` /
  `drop_window_cursor`, and the `cache_padded_*` /
  `producer_and_consumer_counters_on_distinct_cache_lines` tests.
- Seed `rsac-9348` (the false-sharing fix that introduced `CachePadded`).
