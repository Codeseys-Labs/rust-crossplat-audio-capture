# rsac review — Loop 10

**Date:** 2026-04-17
**Reviewer:** claude-agent (read-only explore pass)
**Scope:** rsac library core + bindings (not audio-graph)

## Summary

Fresh pass over the rsac library crate and its FFI / Node.js / Python
bindings. Codebase is well-architected; most findings are maintainability and
API-surface polish, not correctness or security.

**Counts:** 0 CRITICAL, 2 HIGH, 2 MEDIUM, 2 LOW, plus 5 positive confirmations.

---

## CRITICAL

None.

---

## HIGH

### 1. `DeviceEnumerator` trait vs. wrapper method mismatch
**Files:** `src/core/interface.rs:205`, `src/audio/mod.rs:97-124`,
`src/api.rs:171,202`, `src/main.rs:195`

`DeviceEnumerator::default_device()` is the trait method, but public consumers
call `CrossPlatformDeviceEnumerator::get_default_device(kind: DeviceKind)` —
which ignores its `_kind` argument. Hidden redirection + incomplete refactor
from an earlier kind-based device API.

**Impact:** Confusing API; consumers must call the wrapper, not the trait
method directly, and the `DeviceKind` they pass is silently discarded.

**Action:** Either implement kind-based selection in `get_default_device()`,
or rename and drop the unused parameter.

### 2. `CapturingStream::close()` is near-dead public interface
**File:** `src/core/interface.rs:142` (trait), `src/bridge/stream.rs:699`
(sole production caller), `src/api.rs:593-602` (Drop does the real work)

The trait method `fn close(self: Box<Self>)` takes ownership to stop the
stream, but Drop impls already call `stop()`. Only one explicit production
caller. Bindings don't use it.

**Impact:** Maintenance burden with no real value beyond `stop()` + drop.

**Action:** Either make `close()` optional with a default no-op, move to
`#[cfg(test)]`, or delete and rely on Drop.

---

## MEDIUM

### 3. Diagnostic `BridgeStream::buffers_{dropped,read}` leak into the public API
**File:** `src/bridge/stream.rs:145, 152`

Both `pub fn buffers_dropped(&self) -> u64` and `pub fn buffers_read(&self)
-> u64` are marked `#[allow(dead_code)]`. They exist for tests; they expose
internal ring-buffer counters at the public surface.

`CapturingStream::overrun_count()` already covers the overrun use case for
external consumers.

**Action:** Downgrade to `pub(crate)` and let tests in `super` access them.

### 4. Unused `DeviceKind` parameter — corollary to #1
**File:** `src/audio/mod.rs:99`

Docstring says "kept for backward compatibility" but no external caller
actually used the parameter meaningfully. Paired with #1: the right fix
probably drops both the wrapper AND the parameter.

---

## LOW

### 5. `#[allow(dead_code)]` in platform-conditional code lacks context
**Files:** `src/bridge/stream.rs:48,86`, `src/bridge/ring_buffer.rs:60`

Several items are marked `#[allow(dead_code)]` but are *actually used* by
platform backends when feature flags are enabled. Future readers can't tell
"genuinely dead" from "feature-conditional."

**Action:** Add a one-line comment: `// Platform-conditional: used when feat_*
is enabled.`

### 6. `AudioSink::close()` mirrors the `CapturingStream::close()` near-dead state
**Files:** `src/sink/traits.rs`, with callers only in `src/sink/{channel,
null, wav}.rs` tests

Same story as HIGH #2 but for sinks. Consider the same remediation.

---

## Noted but not flagged (positive confirmations)

- ✅ **Platform-specific duplication is justified.** Each backend
  (WASAPI/PipeWire/CoreAudio) has distinct OS APIs; ~1000 LOC of per-platform
  code is appropriate — not bad abstraction.
- ✅ **Test coverage is solid.** Unit tests in `src/api.rs` + integration
  tests in `tests/ci_audio/` cover system capture, device enumeration,
  app capture, stream lifecycle across all three platforms.
- ✅ **Bindings have minimal API drift.** FFI, napi, Python bindings mirror
  the core API. Minor: `list_audio_sources()` not yet in FFI (see MEDIUM
  recommendation below).
- ✅ **Dependency health is good.** Pre-1.0 crates (`objc2 0.6`,
  `pipewire 0.9.2`, `coreaudio 0.14`) are mature and widely used.
- ✅ **README and ARCHITECTURE.md are current** except for one stale
  reference: README mentions a `pipe_to()` method that doesn't exist in
  `src/api.rs`.

---

## Top 3 recommendations for Loop 11

1. **Resolve `DeviceEnumerator` trait vs. wrapper.** Pick one canonical path;
   drop the unused `DeviceKind` parameter or implement it. Touches
   `core/interface.rs`, `audio/mod.rs`, `api.rs`, `main.rs`.

2. **Simplify `close()` methods on streams and sinks.** Move to optional
   trait default (no-op) or `#[cfg(test)]`. Touches `core/interface.rs`,
   `sink/traits.rs`, `bridge/stream.rs`.

3. **Add `list_audio_sources()` to the FFI binding.** audio-graph now uses
   this entry point for unified source discovery; the FFI binding should
   match so C consumers aren't left behind. Touches
   `bindings/rsac-ffi/src/lib.rs`.

4. **Micro:** fix the stale `pipe_to()` reference in `README.md`.
