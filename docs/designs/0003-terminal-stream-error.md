# ADR 0003 — Distinguish terminal stream end from recoverable read errors

**Status:** Accepted
**Date:** 2026-05-29
**Scope:** `src/core/error.rs`, `src/bridge/ring_buffer.rs`, `src/bridge/stream.rs`
**Verdict:** Add a new `AudioError::StreamEnded` variant (Fatal, `ErrorKind::Stream`)
emitted when a read fails because the stream is terminal. Keep `StreamReadError`
classified `Recoverable` for genuinely transient read failures.

## 1. Context

`AudioError::recoverability()` classifies `StreamReadError` as **Recoverable**
(`error.rs:232-234`). But the bridge emits `StreamReadError { reason: "Stream stopped" }`
precisely when the stream has reached a *terminal* state — `Stopped`/`Closed`/`Error`
(`ring_buffer.rs:331-335`, `stream.rs:160-167`). Terminal is permanent.

The audit (finding M11) showed the consequence: a consumer that loops
`while let Err(e) = read { if e.is_fatal() { break } }` — exactly the pattern used in the
repo's own `ci_audio` tests (`system_capture.rs:105-111`) — **never breaks** on a
dead stream and busy-waits until an outer timeout. The public `is_recoverable()`/
`is_fatal()` contract is actively misleading on the single most common terminal signal.

## 2. Decision drivers

- The recoverability hint is a load-bearing public API; consumers branch on it.
- "Stream ended normally" and "a transient read hiccup" are semantically different and
  deserve different recoverability.
- Backward compatibility: `StreamReadError` is widely constructed; don't change its
  meaning where it is genuinely transient.

## 3. Considered options

### Option A — Reclassify `StreamReadError` as Fatal
- ➕ One-line change.
- ➖ `StreamReadError` is also the natural variant for transient/non-terminal read
   problems; making it Fatal mislabels those the other way. Loses information. Rejected.

### Option B — Add `AudioError::StreamEnded` (Fatal), emit on terminal (CHOSEN)
New variant `StreamEnded { reason: String }`, `ErrorKind::Stream`, `recoverability ==
Fatal`. The bridge returns `StreamEnded` when a read is refused/aborted because the
stream is in a terminal state; `StreamReadError` stays `Recoverable` for transient
cases. Consumers' `is_fatal()` loops now terminate correctly; `StreamEnded` reads as a
clean end-of-stream.
- ➕ Precise semantics; fixes the busy-wait; preserves `StreamReadError` meaning;
   additive (new variant, not a behavior change to an existing one).
- ➖ New variant → must thread through every exhaustive `match` (`kind()`, `Display`,
   FFI `map_rsac_error`, bindings). Mechanical but real.

### Option C — Sentinel: return `Ok(None)` forever once terminal
- ➖ Conflates "no data right now" with "stream is dead"; breaks blocking readers that
   wait for data; the iterator already mis-handles `Ok(None)` (finding H2). Rejected.

## 4. Decision

**Option B.** Add `StreamEnded`. Emit it from `BridgeConsumer::pop_blocking` and
`BridgeStream::read_chunk`/`try_read_chunk` when the state is terminal; reserve
`StreamReadError` for transient/non-terminal failures (e.g. mutex poison surfaced as
`InternalError`, or a genuine partial-read error). Update `kind()`, `Display`,
`recoverability()` (explicit arm → Fatal), the FFI error map, and the `ci_audio` loop
helpers to treat `StreamEnded` as clean termination.

## 5. Consequences

- `AudioError` grows from 21 → 22 variants; update the "21 variants" docs/tests
   (coordinated with the docs-reconciliation task).
- `recoverability()`'s catch-all is replaced with an explicit exhaustive match (also
   closes audit L10) so future variants can't silently default to Fatal.
- FFI: add `RSAC_ERROR_STREAM_ENDED` (or map to existing `RSAC_ERROR_STREAM_READ` with a
   distinct code) — chosen: reuse `RSAC_ERROR_STREAM_READ` group to avoid ABI churn, but
   document the distinction; revisit if a separate code is needed.
- Tests assert a stopped stream yields `StreamEnded` and `is_fatal() == true`.

## 6. References

- Audit findings M11, L10 (2026-05-29 deep-dive).
- `src/core/error.rs:108-242`, `src/bridge/ring_buffer.rs:320-348`,
   `src/bridge/stream.rs:159-198`.
