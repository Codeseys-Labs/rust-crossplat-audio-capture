# ADR 0011 — Multi-source channel composition in-crate behind an opt-in `compose` feature

**Status:** Accepted
**Date:** 2026-07-04
**Scope:** `src/compose/` (new top-layer module), `Cargo.toml` (`compose` feature,
optional `rubato` + `audioadapter-buffers` deps), `src/lib.rs` (feature-gated
re-export), `scripts/check-module-dag.sh` (new layer edges), `VISION.md`
(out-of-scope table amendment)
**Verdict:** rsac gains an **opt-in** multi-source composition layer: a
`CompositionBuilder` takes *groups* of `CaptureTarget`s, mixes each group down to
Mono/Stereo (gain-weighted plain summation) or passes a single source's native
channels through, and appends the groups — in declaration order — into **one**
interleaved-f32 multi-channel stream that implements the existing
`CapturingStream` contract. Sources whose negotiated rate differs from the
session rate are resampled with `rubato` on a dedicated non-RT compositor
thread. Everything lives behind the `compose` cargo feature; with the feature
off, the crate's dependency graph and API are unchanged.

## 1. Context

VISION.md (pre-amendment) declared stream mixing **out of scope**: "Mixing
requires downstream-specific decisions: (a) what sample-rate to mix at
(resampling cost), (b) per-source gain, (c) clipping / limiter strategy, (d)
real-time vs. buffered." The supported multi-source story was N independent
`AudioCapture` instances, hand-merged by the consumer.

That stance left rsac's headline capability — simultaneous capture of system /
application / process-tree audio — without a usable *combined* delivery form.
Real consumers (recording apps, streaming tools, transcription pipelines) want
"capture these apps as one voice channel, the game as stereo, the system mix as
its own channels" — i.e. **channel composition**, not generic DSP. Building it
downstream repeatedly re-solves the same four decisions the vision deferred,
plus two problems only the capture layer sees clearly:

1. **Heterogeneous rates are unavoidable.** Windows process loopback cannot use
   `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM` (`src/audio/windows/wasapi.rs`), so an
   application capture may deliver 44.1 kHz while the system capture delivers
   48 kHz. Any composition **requires** resampling; it is not an optional nicety.
2. **Alignment needs capture-side knowledge.** Sources start at different
   times, deliver different buffer sizes, and app taps go silent when the app
   stops playing. Aligning them needs a pacing policy tied to how each backend
   actually delivers data.

The project owner explicitly re-scoped composition as a core capability
(2026-07-04 planning session): "if we select multiple sources I'd like the
option to either group some into a single channel or keep them in separate
channels … for multiple groups … complex multi-channel audio capture usable
for downstream anything."

## 2. Decision drivers

- **Zero cost when unused.** Consumers who don't compose must pay no new
  dependencies (rubato pulls realfft/num-complex) and see no API change.
- **One data plane, one contract.** The composed output must speak
  `CapturingStream` so every existing consumer path (`drain_to` sinks, WAV
  recording, async stream, metering, backpressure stats) works unchanged.
- **RT-safety preserved (ADR-0001).** Resampling/mixing may allocate — so it
  must happen on a dedicated non-RT thread, never an OS callback thread. The
  composed stream reuses the `BridgeStream` ring (mandated pattern: "do not
  bypass `BridgeStream<S>`").
- **Honest scope.** Composition ≠ DSP suite. No effects, no limiter, no
  encoding. Plain summation with per-source gain; clipping strategy stays the
  consumer's choice (optional clamp flag, default off — f32 headroom is legal
  in the pipeline and in f32 WAV).
- **Bindings-ready.** In-crate placement lets the C/Python/Node/Go bindings
  expose composition later without cross-crate version coupling.

## 3. Considered options

### Option A — In-crate `compose` feature (chosen)

New `src/compose/` module at the top of the module DAG
(`core → bridge → audio → api → compose`), gated by a `compose` cargo feature
that alone pulls `rubato`. The compositor owns N inner `AudioCapture`s and
produces composed buffers into a `BridgeStream` ring.

- ✅ Zero default-build cost; single crate to version/publish; bindings can bind it.
- ✅ Can reuse internal (`pub(crate)`) `BridgeStream`/`PlatformStream` machinery —
  terminal semantics (ADR-0003), overrun counters, async waker all come free.
- ➖ Grows the crate's feature matrix (mitigated by the new CI powerset job).

### Option B — Separate `rsac-mixer` workspace crate

- ✅ Keeps the core crate's vision statement untouched.
- ❌ A 7th manifest in the CI version-lockstep gate; another publish pipeline.
- ❌ Cannot reach `pub(crate)` bridge internals — would re-implement ring/state
  machinery or force those internals public.
- ❌ Bindings would need a second native dependency.

### Option C — Fan-in only (backlog item M3 `combine_sources()`)

Tagged `Receiver<(SourceId, AudioBuffer)>` fan-in; grouping/mixdown stays a
documented recipe.

- ✅ Cheapest; no new deps.
- ❌ Does not deliver channel grouping; every consumer still re-solves
  resampling + alignment + interleaving — the actual hard parts.

## 4. Decision

**Option A.** Sub-decisions, resolved in the same planning session:

1. **Rate reconciliation:** `rubato` (v3, MSRV 1.85 ≤ our 1.87 floor) resamples
   any source whose delivered rate ≠ the session rate (default 48 kHz,
   builder-configurable) on the compositor thread.
2. **API shape:** groups of sources; per group `GroupLayout::{Mono, Stereo}`
   mixdown (per-source gain, plain summation, optional saturating clamp
   default-off) or `GroupLayout::KeepChannels` (v1: exactly one source per
   keep-channels group). Groups append in declaration order; a `ChannelMap`
   reports which output channels belong to which group.
3. **Pacing:** master-clock alignment. The master is the first system/device
   source (device clocks tick through silence; app taps do not), else the first
   source. Per-source FIFOs are silence-padded when behind and bounded-trimmed
   when drifting ahead; a wall-clock fallback tick keeps the session alive if
   the master stalls. Per-source `padded_frames` / `trimmed_frames` counters are
   exposed. Timestamp-based drift correction is **deferred** (seed `rsac-ec25`,
   blocked on backends populating `AudioBuffer::timestamp`).
4. **Delivery:** the compositor thread pushes composed interleaved-f32 buffers
   through a `BridgeProducer` into a `BridgeStream` whose `PlatformStream` impl
   stops the inner captures — so the public `Composition` handle inherits the
   proven read/terminal/async semantics instead of re-implementing them.

## 5. Consequences

- VISION.md's out-of-scope table is amended: "stream mixing" moves from
  out-of-scope to "in-scope behind the `compose` feature"; resampling remains
  out-of-scope *except* internally for composition alignment.
- The compositor owns consumption of its inner captures (single logical
  consumer per ring); users must not read the inner captures directly. This is
  documented on the public API.
- The `compose` feature joins the CI feature powerset and the
  `feature-combo-doctests` job; the MSRV job is the tripwire for rubato's floor.
- Composition quality bounds: v1 has no timestamp alignment, so very long
  sessions bound drift by the FIFO trim policy rather than correcting it;
  documented as a known limitation.
- FFI/Python/Node/Go exposure is a follow-up epic, not part of this change.
