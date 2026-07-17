---
name: rsac-buffer-size-is-ring-slot-count
description: |
  rsac semantic trap: AudioCaptureBuilder::buffer_size() is the bridge RING
  SLOT COUNT on Linux/Windows (calculate_capacity, ADR-0007), NOT
  frames-per-buffer. Use when: (1) writing overflow/backpressure/overrun
  tests that need the ring to fill, (2) a stalled-consumer test logs
  "dropped stayed 0" / "pushed=N, dropped=0" despite a busy producer,
  (3) reasoning about estimate_window_span() vs ring capacity, (4) adding
  assertions over backpressure_report()'s windowed tallies (never assert
  monotonicity on sliding-window counters).
author: Claude Code
version: 1.0.0
date: 2026-07-17
---

# rsac: `buffer_size()` is the ring slot count, not frames-per-buffer

## Problem

`AudioCaptureBuilder::buffer_size(Some(n))` reads like "n frames per
buffer" (that's how `estimate_window_span()` in `src/api.rs` consumes it
for window attribution). But on Linux and Windows the SAME value is
honored as the **bridge ring's slot count**: `calculate_capacity(config.buffer_size, 4)`
in `src/audio/linux/mod.rs` (and the WASAPI equivalent) rounds it up to a
power of two and sizes the rtrb ring with it (ADR-0007 closed the
"Linux ignores buffer_size" gap this way).

So one knob controls two things at once:
- window-span attribution (frames per buffer × buffers ÷ rate), and
- how many buffers a stalled consumer can leave unread before drops start.

## Context / Trigger Conditions

- A backpressure/overrun integration test sets `buffer_size(Some(1024))`
  "so the window is attributable" and then stalls the consumer expecting
  drops — CI logs `pushed=117, dropped=0` after the full deadline and the
  assertion `dropped > 0` panics. (Live failure: PR #58 first run —
  a 1024-slot ring at ~23 buffers/sec needs ~44s to fill; the test waited 5s.)
- Any test whose pass condition is "the ring overflowed".

## Solution

Use a SMALL buffer_size in overflow-seeking tests. `buffer_size(Some(8))`
fills in well under a second with a stalled consumer at typical PipeWire
callback rates, while still giving `estimate_window_span()` a nonzero
frames-per-buffer so `window != Duration::ZERO` stays assertable.
Verified live: `pushed=8, dropped=4, drop_rate=0.3333, window=2ms` on the
Linux deterministic leg.

Sizing rule of thumb: ring fill time ≈ slots ÷ (sample_rate / frames_per_period).
Keep `slots × period` well under the test's poll deadline.

## Second lesson: no monotonicity assertions on windowed counters

`backpressure_report()`'s `pushed`/`dropped` come from a sliding,
slot-resetting window (`drop_window`: 8 slots × 128 attempts,
`src/bridge/ring_buffer.rs`). Tallies legitimately DECREASE as old slots
roll out. Asserting "non-decreasing across successive reads" treats
windowed counters like lifetime totals and can fail correct behavior.
Two independent reviewers flagged this on the same PR (one as
speculative, CodeRabbit as confirmed) — the assertion was removed.
Lifetime totals live elsewhere (`overrun_count`, `buffers_pushed`) —
assert monotonicity on those instead if needed.

## Verification

The stalled-consumer test breaks out of its poll loop within 1–2
iterations and logs a nonzero `dropped` with a small nonzero `window`.

## Notes

- macOS may derive ring capacity differently — the trap is confirmed for
  Linux (`calculate_capacity` call site) and Windows (seed rsac-ec25
  tracks period-derived sizing as future work; if that lands, this skill
  needs a version bump).
- The dual meaning is a latent API-design wart; if `buffer_size` is ever
  split into `ring_slots` + `period_frames`, deprecate this skill.
- See also: `headless-ci-audio-hangs` (same test-suite family).
