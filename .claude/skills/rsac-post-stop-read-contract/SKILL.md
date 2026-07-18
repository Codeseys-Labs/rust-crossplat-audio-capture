---
name: rsac-post-stop-read-contract
description: |
  rsac contract trap: stop() RELEASES the stream (restart-by-recreation),
  so a SEQUENTIAL stop-then-read yields the NotInitialized lifecycle error
  ("Stream is not initialized. Call start() first."), NOT the fatal
  StreamEnded terminal. Use when: (1) writing post-stop read assertions in
  tests or binding smokes, (2) a test asserting StreamEnded after stop()
  fails with "Stream is not initialized", (3) reasoning about
  terminal-observability (rsac-477d) vs the stop contract, (4) asserting
  error recoverability through the Go/C FFI (lossy code projection).
author: Claude Code
version: 1.0.0
date: 2026-07-18
---

# rsac: post-stop reads yield NotInitialized, not StreamEnded

## Problem

`AudioCapture::stop()` follows the **restart-by-recreation** contract
(`src/api.rs`): stopping RELEASES the stream (`self.stream = None`), so a
*sequential* stop-then-read takes the no-stream path and raises the
`NotInitialized` lifecycle error — "Stream is not initialized. Call
start() first."

The fatal `StreamEnded` terminal surfaces only while a terminal stream is
still **present**: reads that were parked or racing *across* the stop
(that's the rsac-477d terminal-observability fix). Post-hoc sequential
reads never see it.

Five independent parties encoded the same wrong assumption (assert fatal
StreamEnded after stop) in PR #61: three lane implementers, their
reviewers, AND CodeRabbit — whose suggested "assert the error's
fatal/terminal classification" fix was itself wrong. The first live E2E
run (CI 29621951762) failed all three binding smokes on exactly this.

## Context / Trigger Conditions

- A post-stop read assertion fails with
  `Stream read error: Stream is not initialized. Call start() first.`
- Writing stop/lifecycle assertions in binding smokes or integration tests
- A reviewer suggests "assert the terminal error is fatal after stop()"

## Solution

Assert the post-stop read raises a **documented lifecycle error** — accept
exactly the two wordings, reject everything else:

- `"not initialized"` — the sequential stop-then-read case (normal)
- `"stream ended"` — a read racing/parked across the stop (477d case)

Rejecting all else still catches the actual 477d regression class (the
`"Stream is not running"` recoverable-downgrade wording).

Per-binding shapes (from the fixed smokes):
- **napi**: `assert.throws(fn, err => /\[ERR_RSAC_STREAM\]/.test(err.message) && (/Stream ended/i.test(err.message) || /not initialized/i.test(err.message)))`
- **python**: `except rsac.StreamError as e:` then match the two substrings
  in `str(e).lower()`; any other `RsacError` subclass = failure
- **Go**: assert on the **message**, NOT `rsac.IsRecoverable(err)` — the
  lossy FFI code projection maps NotInitialized onto `ErrStreamRead`,
  which the Go layer classifies recoverable.

## Verification

The binding smoke logs
`post-stop read raised the documented lifecycle error: ok (…not initialized…)`
on the deterministic Linux CI leg.

## Notes

- Rust-side structured alternative: `AudioError::lifecycle_stage()`
  returns `LifecycleStage::NotInitialized` for this reason string — prefer
  it over prose matching where the Rust API is available (bindings only
  get the formatted message).
- The consensus-wrongness is the meta-lesson: when N reviewers agree on a
  contract nobody has executed, the agreement is evidence of a shared
  prior, not of correctness. E2E's first run is a contract test for the
  team's mental model.
- See also: `rsac-buffer-size-is-ring-slot-count` (sibling contract trap
  found the same way).
