# ADR 0007 — Period-derived ring sizing (`calculate_capacity_for_period`) and `buffer_size` semantics

**Status:** Accepted
**Date:** 2026-05-30
**Scope:** `src/bridge/ring_buffer.rs` (`calculate_capacity_for_period` and the
`PERIOD_*` constants), `src/core/config.rs` (`StreamConfig::buffer_size`), the three
backends' `calculate_capacity` call sites
**Verdict:** Provide a pure, period-derived ring-sizing function as the intended future
sizing model, alongside the existing static `calculate_capacity`. Record its sizing
math, why `channels` is accepted-but-ignored, what `buffer_size` actually means (a ring
*slot* count, not frames), and the honest current state: **backends still call the
static `calculate_capacity`, and `buffer_size` is honored only on Windows.**

## 1. Context

Each ring slot in the bridge holds exactly one callback period's worth of audio (one
`AudioBuffer`). The number of slots is therefore the number of callback periods of slack
the consumer has before the producer must drop. The original sizing helper,
`calculate_capacity(requested, min)`, is **period-blind**: it returns the requested slot
count or a flat default of 64, rounded up to a power of two.

A flat 64 slots means very different amounts of wall-clock slack depending on the
period: a 64-frame period at 48 kHz fires a callback every ~1.3 ms (64 slots ≈ 85 ms of
slack), while a 1024-frame period fires every ~21 ms (64 slots ≈ 1.4 s of slack —
needlessly large memory/latency). The audit (2026-05-30, DF-04 / PERF-01 and the
sizing-model ADR gap) flagged this, plus two related honesty gaps:

- `StreamConfig::buffer_size` is documented as "desired buffer size in **frames**"
  (`config.rs` ~`pub buffer_size: Option<usize>`) but is consumed as a ring **slot
  count** (number of `AudioBuffer`s), and only one backend reads it at all.
- A period-aware sizing function, `calculate_capacity_for_period`, was built and tested
  (`rsac-b655`) but is **called by no backend**.

This ADR records the period-derived sizing model and reconciles the `buffer_size`
contract with what the code actually does.

## 2. Decision drivers

- **Right-sizing:** the ring should cover several callback periods of slack regardless of
  period size — enough to ride out a reader-thread scheduling hiccup, without buffering so
  much that end-to-end latency balloons.
- **Honesty:** docs must describe what the code does today (static sizing, Windows-only
  `buffer_size`), not the intended end-state, while still recording the model the backends
  are meant to adopt.
- **Testability / low risk:** the sizing decision should be a pure function with no I/O or
  backend state, so it can be adopted backend-by-backend without coupling, and the static
  default must remain bit-for-bit unchanged for backends that have not adopted it.

## 3. Considered options

### Option A — Replace `calculate_capacity` outright with a period-aware sizer
- ➕ One sizing model; no two-functions-side-by-side confusion.
- ➖ Forces every backend to learn its negotiated period *before* it can size the ring,
   and changes the historical default for backends that cannot. A flag-day change to a
   load-bearing constant on a bugfix-class branch. Rejected.

### Option B — Add a pure period-derived sizer alongside the static one; adopt per-backend (CHOSEN)
Add `calculate_capacity_for_period(period_frames, channels)` as a pure function that
backends can adopt independently once they know the negotiated period, keeping
`calculate_capacity` as the fallback and the still-current default. Sizing model:

1. **Degenerate/unknown → historical default.** `period_frames == 0 || channels == 0`
   returns `PERIOD_FALLBACK_CAPACITY` (`64`) — identical to the static default, so a
   backend that cannot learn its period behaves exactly as before.
2. **Base headroom.** `PERIOD_HEADROOM_BUFFERS` = `12` periods of slack — the middle of
   the 8–16× band the design targets.
3. **Scale-up for sub-reference periods.** A reference period of
   `REFERENCE_FRAMES = RT_BUFFER_SAMPLE_CAPACITY / 2 = 1024` frames-per-channel is the
   cadence the free-list buffers (ADR-0001) are tuned around. Smaller periods fire
   callbacks proportionally more often, so the headroom is multiplied by
   `REFERENCE_FRAMES.div_ceil(period_frames).max(1)` to keep roughly constant *wall-clock*
   slack; periods at or above the reference keep the base multiplier of 1 (never scaled
   below). Example: a 64-frame period → `ceil(1024/64) = 16×` → `12 * 16 = 192`.
4. **Clamp.** The raw slot count is clamped to
   `PERIOD_MIN_CAPACITY ..= PERIOD_MAX_CAPACITY` = `8 ..= 1024` — the floor keeps very
   large periods from producing a uselessly small ring; the ceiling caps memory/latency
   for tiny periods.
5. **Power-of-two rounding.** The clamped value is rounded **up** to the next power of two,
   matching the ring's preferred sizing (the same policy `calculate_capacity` uses).

So `calculate_capacity_for_period(1024, 2) == 16` (12 → 16), `2048 → 16`, `256 → 64`
(12×4=48 → 64), `64 → 256` (192 → 256). The result is always a power of two within
`8..=1024` (unit-tested across `1..=65536` frames × `{1,2,6,8}` channels).
- ➕ Pure, testable, adoptable backend-by-backend, no change to the static default.
- ➖ Two sizing functions live side by side until backends migrate; this ADR is the
   record of that intentional interim state.

### Option C — Multiply the slot count by `channels`
- ➖ Each slot already holds the **whole interleaved period** regardless of channel width,
   so multiplying by channels would over-size the ring by the channel count for no slack
   benefit. Rejected — see §4 on why `channels` is accepted but ignored.

## 4. Decision

**Option B.** `calculate_capacity_for_period` is the intended period-derived sizing model
with the math above; `calculate_capacity` remains the fallback and the still-current
production default. The following details are recorded as load-bearing:

- **`channels` is accepted-but-ignored, on purpose.** The parameter is in the signature so
  callers pass the full negotiated stream shape (and so the signature is stable if a future
  policy needs it), but the body does `let _ = channels;`. The per-channel `period_frames`
  alone determines callback cadence, and each ring slot already holds the entire
  interleaved period, so the slot count does **not** additionally multiply by channels. A
  unit test (`capacity_for_period_independent_of_channels`) locks this in: `512`-frame
  periods at 1/2/4/6/8 channels all return the same capacity.

- **`StreamConfig::buffer_size` is a ring *slot* count, not frames.** The value is passed
  straight into `calculate_capacity(config.buffer_size, 4)` as the requested number of
  `AudioBuffer` *slots*. The field doc originally said "desired buffer size in frames",
  which was misleading; it has since been corrected to "ring-buffer depth in
  buffers/slots" (see §6). This ADR is the record that slot-count is the real semantics.

- **`buffer_size` is honored only on Windows today (honest state).** Only the WASAPI
  backend threads the request through: `calculate_capacity(config.buffer_size, 4)`. The
  Linux, CoreAudio, and macOS backends hardcode `calculate_capacity(None, 4)` (= 64),
  ignoring `config.buffer_size` entirely.

- **No backend calls `calculate_capacity_for_period` today (honest state).** Its only call
  sites are its own unit tests (`capacity_for_period_*`). All three backends still use the
  static `calculate_capacity`. Wiring it requires each backend to surface its negotiated
  period (WASAPI `GetBufferSize`, the PipeWire negotiated buffer size, the CoreAudio IOProc
  frame count), which is deferred to a follow-up.

## 5. Consequences

- The bridge ships **two** sizing functions: the static `calculate_capacity` (in
  production on all three backends) and the period-derived `calculate_capacity_for_period`
  (pure, fully unit-tested, unwired). This is an intentional interim state, not an
  oversight.
- Ring sizing is currently period-blind in production: a 1024-frame WASAPI stream and a
  64-frame one both get a 64-slot ring (modulo an explicit `buffer_size` request on
  Windows), so small-period streams have less wall-clock slack than the period-aware model
  would give them.
- `buffer_size` requests are silently ignored on Linux/macOS; downstreams that set it and
  expect a deeper ring get the default 64 there. Until backends adopt the period sizer,
  this is documented as Windows-only.
- Adopting the period sizer later is a localized, low-risk change per backend (swap the
  `calculate_capacity(None, 4)` call for `calculate_capacity_for_period(period, channels)`
  once the negotiated period is known), and the degenerate-input fallback guarantees no
  behavior change for backends that cannot learn their period.

## 6. Follow-up (tracked)

### Current state (as of this revision)

- **`StreamConfig::buffer_size` doc — corrected (done).** The field doc in
  `src/core/config.rs` previously said "desired buffer size in **frames**". It has been
  rewritten to state the real semantics: a **ring-buffer depth in buffers/slots** (a slot
  count, each slot holding one whole interleaved callback period), fed into
  `calculate_capacity(requested, 4)`, and **honored only on Windows (WASAPI) today** — the
  Linux (PipeWire) and macOS (CoreAudio) backends hardcode `calculate_capacity(None, 4)`
  (= 64) and ignore the field. The doc also notes the
  `AudioCaptureBuilder::buffer_size_frames` setter is a backward-compat alias whose
  "frames" name is historical and likewise denotes slots, not frames. This was the
  low-risk, doc-only half of the reconciliation; it touches **no** backend or sizing code.

- **`calculate_capacity_for_period` — built and tested, but unwired (unchanged).** The
  pure period-derived sizer exists in `src/bridge/ring_buffer.rs` and is fully unit-tested
  (`capacity_for_period_*`), but **no backend calls it**; all three still use the static
  `calculate_capacity`. Wiring it (and/or threading `buffer_size` through Linux/macOS) is a
  separate, larger change that touches the three backend files (WASAPI `GetBufferSize`, the
  PipeWire negotiated buffer size, the CoreAudio IOProc frame count) and is intentionally
  **not** part of the doc-only correction above.

### Remaining work (deferred)

- **Thread the negotiated period into ring sizing on every backend.** Adopt
  `calculate_capacity_for_period(period, channels)` (or at least thread `config.buffer_size`)
  uniformly across WASAPI / PipeWire / CoreAudio once each backend can surface its
  negotiated period, so ring sizing is period-aware (and `buffer_size` is honored) on every
  platform rather than Windows-only.
- If `bridge-zerocopy` (ADR-0006) is promoted, feed the same negotiated period into
  `create_sample_ring`'s `sample_capacity`/`max_chunks`.

### Promote criteria (when to consider this fully resolved)

1. At least the macOS and Linux backends size their bridge from the negotiated period
   (via `calculate_capacity_for_period`) or honor `config.buffer_size`, so the
   "Windows-only" caveat can be dropped from the field doc.
2. The degenerate-input fallback contract (`period_frames == 0 || channels == 0 →`
   `PERIOD_FALLBACK_CAPACITY` = 64) remains exercised by tests, guaranteeing no behavior
   change for a backend that cannot learn its period.
3. The static `calculate_capacity` default stays bit-for-bit unchanged for any backend that
   has not migrated (regression-guarded by
   `calculate_capacity_unchanged_alongside_period_variant`).

## 7. References

- ADR-0001 — `docs/designs/0001-rt-allocation-guarantee.md` (`RT_BUFFER_SAMPLE_CAPACITY`,
  the 1024-frame reference period this sizer is tuned around).
- ADR-0006 — `docs/designs/0006-bridge-zerocopy-samplering.md` (would consume the same
  period if promoted).
- Audit findings DF-04 / PERF-01 and the "calculate_capacity_for_period sizing model +
  buffer_size semantics" ADR gap — `docs/reviews/rsac-architecture-critique-2026-05-30.md`.
- `src/bridge/ring_buffer.rs` — `calculate_capacity`, `calculate_capacity_for_period`,
  `PERIOD_HEADROOM_BUFFERS`/`PERIOD_FALLBACK_CAPACITY`/`PERIOD_MIN_CAPACITY`/
  `PERIOD_MAX_CAPACITY`, and the `capacity_for_period_*` tests.
- `src/core/config.rs` — `StreamConfig::buffer_size`.
- Backend call sites: `src/audio/windows/wasapi.rs` (`calculate_capacity(config.buffer_size, 4)`),
  `src/audio/linux/mod.rs`, `src/audio/macos/thread.rs`, `src/audio/macos/coreaudio.rs`
  (all `calculate_capacity(None, 4)`).
