# ADR 0003 — Distinguish terminal stream end from recoverable read errors

**Status:** Accepted
**Date:** 2026-05-29
**Scope:** `src/core/error.rs`, `src/bridge/ring_buffer.rs`, `src/bridge/stream.rs`
**Verdict:** Add a new `AudioError::StreamEnded` variant (Fatal, `ErrorKind::Stream`)
emitted when a read fails because the stream is terminal. Keep `StreamReadError`
classified `Recoverable` for genuinely transient read failures.

## 1. Context

`AudioError::recoverability()` classifies `StreamReadError` as **Recoverable**
(see the `StreamReadError` arm of `AudioError::recoverability` in
`src/core/error.rs`). But the bridge emitted `StreamReadError { reason: "Stream
stopped" }` precisely when the stream had reached a *terminal* state —
`Stopped`/`Closed`/`Error` (the non-readable / terminal-read path in
`src/bridge/stream.rs`, which consults the `BridgeShared` stream-state in
`src/bridge/ring_buffer.rs`). Terminal is permanent.

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
- FFI mapping (as shipped — supersedes the original proposal). The original draft
   proposed reusing the **recoverable** `RSAC_ERROR_STREAM_READ` group to avoid ABI
   churn. The implementation instead maps `StreamEnded` to the **fatal**
   `RSAC_ERROR_STREAM_FAILED` group (alongside `StreamCreationFailed`/`StreamStartFailed`/
   `StreamStopFailed`), explicitly **not** the recoverable `RSAC_ERROR_STREAM_READ`
   group (which keeps `StreamReadError`/`BufferOverrun`/`BufferUnderrun`). This is the
   better decision and the one in `rsac::map_rsac_error` (see
   `bindings/rsac-ffi/src/lib.rs`): grouping the terminal signal with the recoverable
   read errors would have told C callers to *retry* a stream that is permanently done —
   re-introducing at the FFI boundary the exact busy-wait this ADR fixes in Rust.
   Mapping it to the fatal group lets a C consumer tell "done, stop" (`STREAM_FAILED`)
   from "transient, retry" (`STREAM_READ`) using the existing ABI codes, so **no new
   code or ABI churn was needed** — the original ABI-stability goal is met *and* the
   recoverability semantics are correct. (A dedicated `RSAC_ERROR_STREAM_ENDED` code, or
   a companion `rsac_error_recoverability()` accessor for finer fidelity, remains a
   possible future enhancement.)
- Tests assert a stopped stream yields `StreamEnded` and `is_fatal() == true`.

## 6. References

- Audit findings M11, L10 (2026-05-29 deep-dive); FFI-mapping amendment from the
   2026-05-30 architecture critique (ADR-R2 / adr-review row 0003).
- `AudioError::StreamEnded` and `AudioError::recoverability` in `src/core/error.rs`;
   the terminal-state emit sites in `BridgeConsumer::pop_blocking`
   (`src/bridge/ring_buffer.rs`) and `BridgeStream::read_chunk`/`try_read_chunk`
   (`src/bridge/stream.rs`); the FFI mapping in `rsac::map_rsac_error`
   (`bindings/rsac-ffi/src/lib.rs`).

## Amendment 2026-07-17 (rsac-feb4)

`BridgeStream::non_readable_error`'s `Created`-state (recoverable,
`StreamReadError`) arm previously formatted the live `StreamState` into the
message (`"Stream is in Created state, cannot read"`). It now reuses the
shared `REASON_NOT_RUNNING` constant (`src/core/error.rs`) verbatim, dropping
the `Created` detail from the diagnostic text. This is what makes
`AudioError::lifecycle_stage()` — a new structured accessor returning
`LifecycleStage::{NotInitialized, NotRunning, Unknown}` — a pure
string-equality match against the canonical `REASON_*` constants used at
every `StreamReadError` construction site, with no heuristics and no risk of
construction/classification drift. No test asserted on the `Created` wording
(confirmed by grep); this is a diagnostic-text change only, not a behavior or
API-shape change to `StreamReadError` (which remains `{ reason: String }` —
adding a field or `#[non_exhaustive]`-ing the variant would each be a
semver-major delta per `cargo-semver-checks 0.48.0`, empirically verified).
