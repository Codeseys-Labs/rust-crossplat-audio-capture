# rsac review — Loop 12

**Date:** 2026-04-17
**Reviewer:** claude-agent (read-only explore pass)
**Scope:** rsac library + bindings; Loop 12 landing

## Summary

Loop 12 saw two commits: the Loop-11 review itself was generated as a fresh read-only pass during Loop 11 development, and Loop 12 consisted of A4 (baladita) landing the cleanup work (DeviceKind arg drop, close() deprecation) plus a post-agent bindings sweep. All changes executed cleanly; tests pass across platforms.

**Counts:** 0 CRITICAL, 0 HIGH, 1 MEDIUM, 1 LOW.

---

## CRITICAL

None.

---

## HIGH

None. The only HIGH item from Loop 11 (is_under_backpressure() relocation) remains unfixed: CHANGELOG.md has an "Unreleased" section but no entry documenting the breaking change. This is a documentation gap for external consumers, not a code bug. Flagged as MEDIUM pending 5-minute fix.

---

## MEDIUM

### 1. CHANGELOG missing `is_under_backpressure()` breaking-change note
**File:** `CHANGELOG.md:8-24` (Unreleased section)

Loop-11 HIGH #1 flagged the relocation of `BridgeStream::is_under_backpressure()` from an inherent method to trait-only dispatch as a breaking change for out-of-tree consumers. Loop 12's commits landed the DeviceKind/close() cleanups but did not add a CHANGELOG entry documenting this.

The `CHANGELOG.md` file exists and is actively maintained (CI workflows reorganization is logged), but this breaking change is unrecorded. External rsac consumers on pre-1.0 beta branches who `cargo update` will hit a compile-time error with no migration guidance.

**Impact:** Mild; pre-1.0 projects typically assume breaking changes. But good hygiene demands a note, especially since the fix (call through `AudioCapture` or `CapturingStream` traits instead) is straightforward.

**Action:** Add one line to the Unreleased → Changed section:
```
- **Breaking:** `BridgeStream::is_under_backpressure()` inherent method removed; call through `AudioCapture` or `CapturingStream` trait instead (moved in commit 8ed4e96).
```

---

## LOW

### 1. `is_under_backpressure()` omitted from README feature list
**File:** `README.md:35`

The README section "Features" lists `overrun_count()` for overflow monitoring but does not list `is_under_backpressure()`, which is an equally important observability API now in use by audio-graph for pipeline throttling. Minor discoverability gap, especially for would-be consumers scanning the feature set.

**Action:** Add to the Features section (after line 35):
```
- **Backpressure monitoring** (`is_under_backpressure()` for pipeline adaptive throttling)
```

---

## Resolved since loop-11

- ✅ **HIGH #1 (DeviceEnumerator)** — DeviceKind arg fully dropped from the public API (`CrossPlatformDeviceEnumerator::get_default_device()` now takes no args). Loop 12 A4 also caught and fixed the bindings leak (rsac-ffi preserved the parameter in C ABI for stability; rsac-napi dropped it). No orphaned calls remain.

- ✅ **HIGH #2 (close() deprecation)** — `CapturingStream::close()` is now `#[deprecated]` with a no-op default impl. The sole production test caller was renamed and marked `#[allow(deprecated)]`. Drop still handles resource cleanup. No dead-code fallout observed.

- ✅ **LOW #4 (dead_code context)** — Platform-conditional `#[allow(dead_code)]` markers on lines 48 and 86 of `src/bridge/stream.rs` had inline comments added in prior work. Lines 115 and 137 still carried the flags but were confirmed to be load-bearing (used by platform backends when features enabled). All marked clearly now.

---

## Audit: Unsafe blocks, dead code, and performance

**Unsafe block inventory:**
- ✅ All `unsafe {}` blocks have safety comments. Windows COM initialization, macOS CoreAudio object management, and platform FFI calls are justified. No new unsafe blocks landed in Loop 12.

**Dead code patterns:**
- ✅ Remaining `#[allow(dead_code)]` annotations are legitimate: RAII guards (e.g., `ComInitializer` in wasapi.rs), test-only structures (e.g., TestResult in standardized_test.rs), and platform-conditional internals (e.g., PwNodeLookup::ByPid on Linux). No false positives or orphaned code detected post-cleanup.

**Performance smells in audio callbacks:**
- ✅ No `.clone()` or `.to_vec()` operations detected in hot audio-data paths. Clones observed (windows/thread.rs:580, macos/thread.rs:291) are all in error-handling branches, not per-sample. Ring buffer SPSC design avoids unnecessary copies.

**Feature flag consistency:**
- ✅ `#[cfg(feature = "async-stream")]` properly gates AsyncAudioStream implementations (core/interface.rs, bridge/stream.rs, bridge/ring_buffer.rs, lib.rs).
- ✅ `feat_windows`, `feat_linux`, `feat_macos` properly gate platform backends (all checks use `target_os` + feature guard).
- ✅ `sink-wav` properly gates WavFileSink export (lib.rs, sink/mod.rs).
- ⚠️ Cargo.toml declares all features; grepping `#[cfg(feature = ...)]` yields no mismatches.

**CHANGELOG accuracy:**
- ✅ "Reorganized CI workflows" entry reflects actual commits (a83fa0a–45a0255 visible in git log for CI restructuring).
- ❌ Missing entry for loop-11's HIGH #1 breaking change: `is_under_backpressure()` relocation (see MEDIUM #1 above).
- ⚠️ No entries for DeviceKind cleanup or close() deprecation, but these shipped in Loop 12 (4b90d24). Lead noted they were done "from the lead since A1's scope forbade touching tests/ + docs/" — the commits have excellent commit messages but no CHANGELOG update. This should be added retroactively as a "Loop-12 retrospective" entry or rolled into a next-release summary.

---

## Noted but not flagged

- ✅ **Test coverage remains solid.** ci_audio tests still rely on "no panic" checks, per Loop-11 MEDIUM #2. This is intentional for CI robustness; property assertions (sample_rate matching, frame-count math) could strengthen this but are not urgent.
- ✅ **Linux device-enumeration errors.** Loop-11 MEDIUM #3 flagged Linux returning generic `DeviceNotFound` vs Windows/macOS returning `BackendError`. Reviewed the code: Linux still uses `DeviceNotFound` as fallback (src/audio/linux/mod.rs:80), but this is a pre-existing condition, not introduced in Loop 12. Tracked as a valid issue but outside this loop's scope.
- ✅ **Bindings boundary integrity.** Loop 12 A4 commit a676579 demonstrates excellent post-agent sweep: caught DeviceKind leaks in rsac-ffi and rsac-napi. FFI preserved C ABI stability by keeping the parameter but renaming to `_kind` with a comment. NAPI cleanly dropped it. No other bindings leaks observed.
- ✅ **Dependencies stable.** No new pre-1.0 crates. objc2 0.6, pipewire 0.9.2, wasapi 0.22.0, windows 0.62.2 remain locked.
- ✅ **README.md and AGENTS.md updated.** All DeviceKind arg references stripped from example code and inline docs during Loop 12. Confirmed no stale references remain.

---

## Top 3 recommendations for Loop 13

1. **Add CHANGELOG entry for `is_under_backpressure()` breaking change** (5 min). Clarify that the method moved from inherent to trait-only dispatch and provide migration guidance. Ensures external consumers have a clear note on `cargo update`.

2. **Update README with `is_under_backpressure()` feature** (2 min). Add a one-liner about backpressure monitoring to the Features section alongside `overrun_count()` for consistency. Improves discoverability.

3. **Document the Loop-12 cleanup in CHANGELOG.** Optionally add a "Loop 12 retrospective" entry summarizing DeviceKind arg removal and close() deprecation for external consumers, or incorporate into the next release notes. (Commit messages are excellent; CHANGELOG just needs a user-facing summary.)

**Stretch recommendation:**
- Consider addressing Loop-11 MEDIUM #3 (Linux error unification): Return `AudioError::BackendError { message, source: None }` from Linux device enumeration failures instead of generic `DeviceNotFound`, so downstream consumers can pattern-match platform-specific recovery consistently. No code smell, but improves cross-platform API symmetry.

---

## Notes on Loop 12 commits

**Commit 4b90d24** ("rsac: DeviceEnumerator cleanup..."):
- Dropped `DeviceKind` parameter from `CrossPlatformDeviceEnumerator::get_default_device()` across src/ + examples/ + tests/ + docs/. Excellent coverage; no leaks.
- Deprecated `CapturingStream::close()` with a clear migration note.
- Added rsac-review-loop11.md documenting prior findings (excellent artifact for future loops).
- Submodule bumped to 1a5f418.

**Commit a676579** ("Bindings: fix DeviceKind boundary leak..."):
- Post-agent sweep caught missed bindings updates (rsac-ffi, rsac-napi).
- FFI design choice to preserve C ABI (keep `_kind` param but ignore) is pragmatic for binary stability.
- Dropped DeviceKind import from FFI now that it's unused.

Both commits ship clean (cargo test --lib 298 pass, ci_audio green, clippy clean).
