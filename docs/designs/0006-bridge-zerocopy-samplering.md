# ADR 0006 — `bridge-zerocopy` `SampleRing`: an opt-in, default-off alternative data plane

**Status:** Accepted
**Date:** 2026-05-30
**Scope:** `src/bridge/ring_buffer.rs` (`SampleRingProducer`/`SampleRingConsumer`,
`create_sample_ring`, `ChunkMeta`), `Cargo.toml` feature `bridge-zerocopy`
**Verdict:** Ship a parallel sample-domain SPSC ring behind the **default-off**
`bridge-zerocopy` feature as an internal A/B alternative to the default `AudioBuffer`
ring. It is implemented and unit-tested but **wired into no backend and no benchmark
today**; record the promote-or-remove criteria so it does not rot as undocumented
near-dead surface.

## 1. Context

The default bridge data plane moves whole [`AudioBuffer`]s through an
`rtrb::RingBuffer<AudioBuffer>` (see ADR-0001). On the producer (OS callback) side this
is allocation-free *in steady state* — a recycled `Vec<f32>` is pulled from the
free-list return ring, filled, and pushed — but it is not *zero-copy*: each callback
still copies samples into a `Vec`-backed `AudioBuffer`
(`push_samples_or_drop_inner`, `ring_buffer.rs` ~`vec.extend_from_slice(data)`).

`VISION.md` advertises "zero-copy (ring buffer → consumer without intermediate Vec)".
The default path delivers the weaker (and honest) "alloc-free producer in steady
state" guarantee, not literal zero-copy. The audit (2026-05-30, finding PERF-02 / the
data-plane ADR gap) recommended either softening the VISION wording **or** providing a
real zero-copy path and recording the decision.

`rsac-3616` introduced that path: a **sample-domain** SPSC ring that writes interleaved
`f32` straight into the ring's uninitialized slots, avoiding the per-callback
`Vec`/`AudioBuffer` entirely. Because it is a behavioral alternative to a load-bearing
hot path — and is currently unwired — its existence, its constraints, and the criteria
for keeping or deleting it need a recorded decision rather than living only in
doc-comments.

## 2. Decision drivers

- **Honesty:** the default path is alloc-free, not zero-copy; a real zero-copy plane
  should be either delivered as an explicit, measurable option or its claim retracted.
- **RT-safety first (ADR-0001):** any alternative must also be allocation-free and
  lock-free on the producer's callback thread.
- **No regression risk to the default:** the shipped behavior must not change for any
  consumer who does not opt in. The public `BridgeProducer`/`BridgeConsumer` surface and
  the backends must be untouched.
- **No dead code rot:** an unwired alternative plane is exactly the kind of surface that
  silently bit-rots; it must carry an explicit promote-or-remove gate.

## 3. Considered options

### Option A — Make the default path zero-copy in place
Rework the single `AudioBuffer` ring so the producer writes samples directly into ring
storage with no intermediate `Vec`.
- ➕ One data plane; the VISION claim becomes true everywhere with no feature flag.
- ➖ The ring payload is `AudioBuffer` (owned `Vec<f32>` + metadata); making it truly
   zero-copy means the *consumer* must hand the user a slice that borrows ring storage,
   which fights the owned-`AudioBuffer`-per-chunk contract the public API and downstreams
   rely on. A large, risky change to the headline RT path on a bugfix-class branch.
   Rejected as too invasive for the current wave.

### Option B — Add a parallel `SampleRing` plane behind a default-off feature (CHOSEN)
Introduce `SampleRingProducer`/`SampleRingConsumer` (`create_sample_ring`) gated on
`bridge-zerocopy` (`Cargo.toml`: `bridge-zerocopy = []`, **not** in `default`). The
producer reserves slots with `rtrb::Producer::<f32>::write_chunk_uninit`, copies the
interleaved `f32` straight into the `MaybeUninit<f32>` slots with
`CopyToUninit::copy_to_uninit`, then `commit_all()` — **no `Vec`, no `AudioBuffer`** on
the producer call. A tiny parallel `rtrb::RingBuffer<ChunkMeta>` sidecar carries
`(len, channels, sample_rate, timestamp_nanos)` so the consumer can reconstruct an
`AudioBuffer` equivalent to what the default ring would deliver. The user-visible
allocation (the reconstructed `Vec<f32>`) is performed on the **non-RT consumer thread**
in `SampleRingConsumer::pop`, exactly mirroring the default plane's division of labor.
- ➕ Delivers a genuine zero-copy *producer* path that can be measured against the
   default, with zero risk to the default plane (feature-gated, separate types).
- ➕ Keeps the sample ring a pure `f32` ring, which is the precondition for the
   `write_chunk_uninit` + `CopyToUninit` fast path — metadata is kept out-of-band.
- ➖ Two rings must stay synchronized: a chunk is only useful if **both** its samples and
   its `ChunkMeta` are committed. Solved by an all-or-nothing commit discipline (§4).
- ➖ Net "win" depends on the backend feeding it contiguous interleaved `f32`; until a
   backend is wired, the gain is unproven on real hardware.

### Option C — Just soften the VISION wording, ship no zero-copy plane
- ➕ Smallest surface; no new code.
- ➖ Forecloses a real, low-risk perf option the team wanted to keep available for
   interleaved-`f32` backends (PipeWire/CoreAudio). Rejected in favor of B *plus* the
   VISION wording fix (which the docs-reconciliation work does separately).

## 4. Decision

**Option B.** Land the `SampleRing` plane behind the default-off `bridge-zerocopy`
feature as an internal A/B alternative, with the following invariants recorded:

1. **Default-off, opt-in only.** `default = ["feat_windows", "feat_linux", "feat_macos"]`
   — `bridge-zerocopy` is not in it. With the feature off, none of `SampleRing*`,
   `create_sample_ring`, or `ChunkMeta` compiles, and the default `AudioBuffer` ring is
   the only data plane. The public `BridgeProducer`/`BridgeConsumer` surface is
   unchanged either way.

2. **All-or-nothing two-ring commit (no metadata desync).** `push_samples_or_drop_at`
   requires room in **both** the sample ring (`write_chunk_uninit(data.len())`) **and**
   the metadata sidecar (`meta.slots() > 0`) before committing anything. If either lacks
   room, the whole chunk is dropped (`drop_chunk` increments `buffers_dropped` /
   `consecutive_drops` and records the drop window) and nothing is committed — so the
   consumer never observes a sample chunk without its `ChunkMeta`, nor vice versa. The
   consumer (`SampleRingConsumer::pop`) reads `meta` first, then pops exactly `meta.len`
   samples; a missing-samples case (which the commit discipline makes unreachable) is
   handled by returning `None` rather than panicking.

3. **Shares the diagnostics + RT contract.** `SampleRingProducer` reuses the same
   `Arc<BridgeShared>` counters (`buffers_pushed`/`buffers_dropped`/`consecutive_drops`,
   the sliding drop window, the async waker) and is allocation-free and lock-free on the
   producer call, so ADR-0001's RT-safety guarantee holds for this plane too.

4. **`rtrb` 0.3.4 pin.** The plane depends on `rtrb`'s `write_chunk_uninit` +
   `CopyToUninit`. `Cargo.toml` pins `rtrb = "0.3.4"` with the comment recording *why*
   the floor matters ("0.3.4: write_chunk_uninit + CopyToUninit for bridge-zerocopy").
   Do not relax this floor while `bridge-zerocopy` exists.

5. **No backend uses it today; the A/B bench now does (honest status).** The only call
   sites of `create_sample_ring` / `SampleRing*` in the tree are
   `#[cfg(all(test, feature = "bridge-zerocopy"))] mod sample_ring_tests` (data
   round-trip, timestamp preservation, FIFO+wrap, atomic-drop-when-full,
   `available_chunks`) and, as of the `bridge_ab` groups in `benches/bridge.rs`
   (feature-gated on `bridge-zerocopy`), the promote-criterion #1 benchmark itself. No
   `src/audio/*` backend constructs a `SampleRing` — that remains unwired (promote
   criterion #2, §6). The `Cargo.toml` "A/B'd in benches/bridge.rs" comment is now
   accurate for the benchmark half of the claim; the wiring half is still open.

## 5. Consequences

- The repository carries a fully-implemented, unit-tested second data plane that ships in
  every build's *source* but compiles into nothing unless a consumer enables
  `bridge-zerocopy`. It is explicitly an **A/B candidate**, not a supported public API.
- `VISION.md`'s "zero-copy" promise is delivered *only* by this opt-in path; the default
  path is "alloc-free producer in steady state." The docs-reconciliation work narrows the
  default-path wording accordingly (tracked separately); this ADR is the authority for
  why a zero-copy path nonetheless exists.
- Because nothing wires it, the plane risks bit-rot. This ADR fixes the
  promote-or-remove gate (§6) so a future maintainer has an unambiguous decision rule
  rather than an orphaned feature.
- The `rtrb` dependency floor is now load-bearing for this feature and must not regress
  below 0.3.4 while the feature exists.

## 6. Promote-or-remove criteria (tracked)

This plane must be **either promoted or removed** — it must not remain indefinitely as
an unwired, unbenchmarked feature.

**Status update (rsac-1da3):** as of `bench.yml`, criterion benches now execute on a
weekly schedule + `workflow_dispatch` (previously `cargo bench` had zero CI callers —
this ADR's promote-criteria data could never accumulate). The workflow archives
`target/criterion/**` baselines for both the default feature set and `--features
bridge-zerocopy`.

**Status update (rsac-6508):** `benches/bridge.rs` now carries the `bridge_ab`
groups (`bridge_ab/samplering/*` vs. `bridge_ab/audiobuffer/*`, behind
`--features bridge-zerocopy`) that A/B `SampleRing` against the default
`AudioBuffer` ring on the identical producer-push + consumer-drain round trip,
at mono/stereo × small/typical/large chunk sizes. `bench.yml`'s weekly
`bridge-zerocopy` leg now exercises it every run, so promote-criterion #1's
data starts accumulating from this point forward. The *decision* itself is
still open — it needs repeatable data across multiple runs/targets, not a
single sample — and criteria #2 (backend wiring) and #3 (`rt_alloc` probe on
the `SampleRing` producer) remain unimplemented.

**Promote** (wire into a backend) when *all* hold:
1. The `bridge_ab` benchmark in `benches/bridge.rs` (behind `--features
   bridge-zerocopy`) shows a measurable, repeatable producer-side win (lower
   p99 push cost and/or fewer copies) on at least one supported target, across
   multiple accumulated `bench.yml` runs. This also makes the existing
   `Cargo.toml` "A/B'd in benches/bridge.rs" comment true.
2. At least one interleaved-`f32` backend (PipeWire or CoreAudio, which already hand the
   producer a contiguous `&[f32]` — see the Linux `align_to::<f32>()` reinterpret) is
   wired to it behind the feature, with the negotiated period feeding
   `create_sample_ring`'s `sample_capacity`/`max_chunks` (coordinate with ADR-0007).
3. An `rt_alloc`-style allocation probe confirms the `SampleRing` producer is also
   allocation-free in steady state.

**Remove** (delete `SampleRing*`, `create_sample_ring`, `ChunkMeta`, the
`bridge-zerocopy` feature, and the `rtrb` pin comment) if, by the next minor release
after this ADR, no backend is wired and the benchmark shows no advantage — and instead
keep only the (already accepted) "alloc-free producer in steady state" wording for the
default plane.

## 7. References

- ADR-0001 — `docs/designs/0001-rt-allocation-guarantee.md` (default-path alloc-free
  guarantee this plane parallels).
- ADR-0007 — `docs/designs/0007-capacity-period-sizing.md` (period→ring sizing that would
  feed `create_sample_ring` if promoted).
- Audit findings PERF-02 and the "bridge-zerocopy SampleRing alternative data plane" ADR
  gap — `docs/reviews/rsac-architecture-critique-2026-05-30.md`.
- `src/bridge/ring_buffer.rs` — `SampleRingProducer`/`SampleRingConsumer`,
  `create_sample_ring`, `ChunkMeta`, and `sample_ring_tests`.
- `Cargo.toml` — `bridge-zerocopy = []` (default-off) and the `rtrb = "0.3.4"` pin.
