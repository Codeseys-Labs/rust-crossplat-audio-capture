# rsac review — Loop 11

**Date:** 2026-04-17
**Reviewer:** claude-agent (read-only explore pass)
**Scope:** rsac library + bindings (not audio-graph)

## Summary

Fresh pass over rsac after loops 9–10 landed (toolchain pin, clippy 1.95 fixes,
backpressure wiring, Loop-10 DeviceEnumerator cleanup in progress during this
review). One breaking-but-undocumented API change, two medium-severity
correctness/observability gaps, two low-severity doc polish items.

**Counts:** 0 CRITICAL, 1 HIGH, 2 MEDIUM, 2 LOW.

---

## CRITICAL

None.

---

## HIGH

### 1. Breaking API change: `is_under_backpressure()` relocation lacks migration note
**File:** `src/bridge/stream.rs` (inherent method removed loop-8; trait-only
path now).

The inherent method on `BridgeStream` was deleted when the trait impl was
confirmed as the live dispatch path (see commit `8ed4e96`). That's a correct
cleanup — but an *external* consumer (e.g. a downstream app that was depending
on `BridgeStream::is_under_backpressure` rather than calling through
`AudioCapture` or `CapturingStream`) will see a compile-time break on a `cargo
update`.

**Impact:** Nil for this workspace (audio-graph calls through `AudioCapture`
which dispatches via the trait). Real for any *other* rsac consumer on the
outside. We are still pre-1.0, so breaking is acceptable, but a note in the
README or CHANGELOG would be kind.

**Action:** Add a one-line entry to `CHANGELOG.md` (or create one if missing)
noting the method relocation. Alternatively, add a thin inherent wrapper back
with `#[deprecated]` for one minor-version cycle.

---

## MEDIUM

### 2. Integration tests rely on "no panic" assertions
**File:** `tests/ci_audio/stream_lifecycle.rs:108` (example); similar
elsewhere in `tests/ci_audio/`

Several ci_audio tests explicitly state the important thing is "no panic /
crash" rather than asserting specific invariants (buffer format stability,
frame count reasonableness, monotonic overrun_count semantics). Intentional
for CI robustness across heterogeneous hardware, but it means a regression
that silently produces bogus-but-well-formed buffers slips through.

**Action:** Add targeted property assertions alongside the existing no-panic
checks — e.g. "if a buffer comes out, its sample_rate matches the requested
sample_rate", "frame count > 0 implies data.len() == frames × channels",
"overrun_count is non-decreasing across reads". Keep the no-panic backbone.

### 3. Linux device enumeration error variants diverge from Windows/macOS
**File:** `src/audio/linux/mod.rs:65-83` vs `src/audio/{macos,windows}/mod.rs`

Windows and macOS return `AudioError::BackendError { .. }` with context on
device-enumeration failures. Linux returns the generic
`AudioError::DeviceNotFound` without context. Downstream consumers pattern-
matching errors for platform-specific recovery can't distinguish "backend
busted" from "device really not there" on Linux.

**Action:** Return `AudioError::BackendError { message, source: None }` with
a descriptive message on Linux enumeration failures.

---

## LOW

### 4. `#[allow(dead_code)]` annotations still missing context in two spots
**File:** `src/bridge/stream.rs:115, 137`

Loop-10 LOW #5 flagged under-documented platform-conditional `dead_code`
markers. Most have been fixed; lines 115 and 137 still lack the
"Platform-conditional: used when feat_* is enabled" comment. Noise-level,
easy to fix when you're next in that file.

### 5. README feature list omits `is_under_backpressure()`
**File:** `README.md:35`

The newly-added observability API that audio-graph uses for pipeline
throttling isn't in README's features section alongside `overrun_count()`.
Minor discoverability gap for would-be consumers.

---

## Resolved since loop-10

- ✅ **HIGH #2** (partial) — `CapturingStream::close()` now has a sensible
  default; callers typically rely on `stop()` or Drop. (Loop-11 A1 agent
  landed this cleanup during this review pass.)
- ✅ **MEDIUM #3** (confirmed) — `BridgeStream::buffers_{dropped,read}` remain
  public but the documentation now makes their purpose clear; gating to
  `pub(crate)` is deferred as a breaking change.
- ✅ **LOW #5** (partial) — Most platform-conditional `#[allow(dead_code)]`
  markers now carry context comments (two still outstanding, see LOW #4 above).
- ✅ **TOCTOU fix landed** — `AudioCapture::is_running` redundant atomic was
  removed; stream state is the single source of truth.

---

## Noted but not flagged

- ✅ Platform backends remain independent without forced abstractions.
- ✅ Test count: integration tests cover lifecycle, device enumeration, capture
  tiers across all three platforms.
- ✅ Dependencies are stable; no new pre-1.0 crates introduced in loops 9–11.
- ✅ CI matrix intact: lint + Linux/macOS/Windows unit + ARM64 cross-compile +
  bindings + downstream (audio-graph) check.
- ✅ No new `unsafe {}` blocks without safety comments.

---

## Top 3 recommendations for Loop 12

1. **Document the `is_under_backpressure()` relocation.** Add a CHANGELOG
   entry (create `CHANGELOG.md` at rsac root if missing) so external rsac
   consumers have a migration note. 5 minutes of work.

2. **Strengthen ci_audio test assertions.** Replace "no panic" with property
   assertions (format stability, frame-count math, monotonic overrun). Keeps
   the CI-friendly nature but catches silent-wrong-output regressions.

3. **Unify Linux error variants with Windows/macOS.** Return
   `AudioError::BackendError { .. }` from Linux device enumeration failures
   instead of the generic `DeviceNotFound`. Improves cross-platform error
   pattern-matching on the consumer side.
