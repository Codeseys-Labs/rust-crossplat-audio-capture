# rsac Architecture Audit — Loop 25

**Date:** 2026-04-24
**Reviewer:** Claude architecture-audit agent (read-only)
**Scope:** rsac root library (NOT apps/audio-graph submodule)

## Executive Summary

**Overall Assessment: ARCHITECTURE HEALTHY**

The rsac library successfully encapsulates its vision of a clean cross-platform audio capture library with per-application process taps. The streaming-first pull-model design is well-realized through a unified `AudioCaptureBuilder` → `AudioCapture` → `CapturingStream` API. All three platforms (Windows WASAPI, Linux PipeWire, macOS CoreAudio Process Tap) implement the same trait contract via a lock-free `rtrb` ring buffer bridge, creating a coherent, maintainable architecture.

**Top 3 Concerns:**
1. **Binary Cruft (MEDIUM)**: Seven binaries in `src/bin/` include stale artifacts (`run_tests.rs` with "TODO: Rewrite to use new API", `test_report_generator.rs` orphaned from CI).
2. **Dead Code Markers (MEDIUM)**: Multiple `#[allow(dead_code)]` suppressions in `src/bridge/` suggest incomplete transitions or untested code paths requiring audit.
3. **Feature Flag Documentation Gap (LOW)**: Documentation claims platform features gate compilation, but actually `#[cfg(target_os)]` is the real gate; features are semantic markers.

## Vision Alignment (Score: 8.0/10)

**Declared Vision** (README.md):
> "A streaming-first audio capture library for Rust. Captures system audio, per-application audio, and process-tree audio on Windows (WASAPI), Linux (PipeWire), and macOS (CoreAudio Process Tap)."

| Dimension | Rating | Evidence |
|-----------|--------|----------|
| Library encapsulates vision | 9/10 | Three platforms fully supported; per-app/process-tree capture is primary API focus |
| API ergonomics expose it | 8/10 | `CaptureTarget::ApplicationByName("firefox")` exposes unique selling point clearly; lacks `FromStr` |
| Docs communicate it | 8/10 | README pivots quickly to app capture; missing "Why rsac?" competitive positioning |
| Cross-platform consistency | 7/10 | All targets work; version requirements (macOS 14.4+, Windows 10 21H1+, Linux PipeWire 0.3.44+) documented but scattered |

## Findings by Dimension

### 1. Public API Ergonomics: STRONG (9/10)

**Single Entry Point:** `AudioCaptureBuilder` is the ONLY public facade. No competing APIs.

- `src/lib.rs:31`: Clean re-export of `AudioCapture, AudioCaptureBuilder`
- `src/api.rs`: Simple builder with `.with_target()`, `.sample_rate()`, `.channels()`, `.build()`
- Type hierarchy coherent: `CaptureTarget` (5 variants) → `AudioCapture` → `CapturingStream` → `AudioBuffer`
- No platform-specific leaks in public API (all `#[cfg]` gates are internal only)

**Minor gaps:**
- No `prelude` module; requires importing individual types
- `CaptureTarget` lacks `FromStr` impl (CLI parsing must be manual)

### 2. Cross-Platform Abstraction: EXCELLENT (9/10)

All three backends implement the same internal `PlatformStream` trait. Ring buffer bridge is the single shared pattern: `rtrb` SPSC ring buffer everywhere; OS callbacks push native format → convert to `f32` → write to ring buffer; user reads via `CapturingStream`.

Platform-specific error mapping is consistent (OSStatus / HRESULT / PipeWire → `AudioError::Backend` with `BackendContext`).

Extensibility: adding a 4th platform is straightforward — add `src/audio/ios/`, implement `PlatformStream`, add `#[cfg]` dispatch.

**Minor platform leaks:**
- `src/core/introspection.rs` exposes `PlatformAppInfo` enum (platform-specific metadata)
- macOS permission checking returns platform-specific `PermissionStatus` that's meaningless on Windows/Linux

### 3. Error Model: WELL-DESIGNED (8/10)

21 categorized variants. Three-level classification: `ErrorKind` (7 categories), `Recoverability`, `BackendContext`. Consistent across platforms.

**Minor gap:** Variants are somewhat coarse. Users needing to retry on specific OS codes must parse the message. No built-in granular retry logic.

### 4. Feature Flags: MOSTLY CLEAN (7/10)

`default = ["feat_windows", "feat_linux", "feat_macos"]`, `async-stream`, `sink-wav`, `test-utils`.

**Documentation discrepancy:** `docs/features.md` claims platform features "control compilation," but they're actually semantic markers — `#[cfg(target_os)]` is the real gate. Fix needed: clarify in features.md.

### 5. Binary Cruft: MODERATE (6/10)

| Binary | Lines | Status | Verdict |
|--------|-------|--------|---------|
| `app_capture_test.rs` | 360 | Active | KEEP |
| `pipewire_test.rs` | 316 | Active | KEEP |
| `wasapi_session_test.rs` | 389 | Active | KEEP |
| `standardized_test.rs` | 472 | Active | KEEP |
| `run_tests.rs` | 78 | **STALE** | **DELETE** — Line 32 has "TODO: Rewrite to use new API" |
| `test_report_generator.rs` | 277 | **STALE** | **DELETE** — Orphaned from CI |
| `pipewire_diagnostics.rs` | 157 | Active | Consider MIGRATE to scripts/ |

### 6. Binding Parity: GOOD (7.5/10)

Core Builder → Capture → Stream flow works identically in FFI, NAPI, Python. `CaptureTarget` enum properly mapped. Asymmetries: sink adapters not exposed in FFI/NAPI (acceptable). FFI uses thread-local storage for error reporting (potential thread-safety issue to audit).

### 7. Vision Gaps & Competitive Positioning

**rsac's unique strengths:**
1. Per-app audio capture — ProcessTap (macOS 14.4+), Process Loopback (Windows 10 21H1+), PipeWire node mapping (Linux). **No other Rust library does this cleanly.**
2. Process-tree capture
3. Streaming-first architecture
4. Backpressure monitoring (`is_under_backpressure()`)

**vs. competitors:**
- **CPAL:** rsac wins for per-app capture; CPAL wins for ecosystem maturity
- **portaudio-rs:** rsac wins (portaudio doesn't support ProcessTap)
- **rodio:** playback-focused, not a competitor

**Missing from README:** No "Why rsac?" section. Add to strengthen positioning.

### 8. Technical Debt: MINIMAL (8.5/10)

- `src/bin/run_tests.rs:32` TODO stub
- `src/audio/linux/mod.rs` format query returns empty (TODO)
- `src/bridge/stream.rs` — 5 `#[allow(dead_code)]` suppressions
- `src/bridge/ring_buffer.rs` — 2 suppressions
- Unsafe code (macOS private `msg_send!()` for CATapDescription, `windows` crate FFI) all justified

## Specific Recommendations

### HIGH PRIORITY (before next release)

1. **Delete stale binaries** (15 min): `src/bin/run_tests.rs`, `src/bin/test_report_generator.rs`. Remove `[[bin]]` entries from Cargo.toml.
2. **Audit `#[allow(dead_code)]` suppressions in bridge/** (1h): delete unused or document necessity.
3. **Document platform version requirements in README** (30 min): consolidate Windows 10 21H1+, macOS 14.4+, Linux PipeWire 0.3.44+.

### MEDIUM PRIORITY

4. **Implement `CaptureTarget::FromStr`** (2h) — CLI parsing
5. **Create `prelude` module** (1h) — `use rsac::prelude::*;`
6. **Tighten Windows `unwrap()` audit** (3h)

### LOW PRIORITY

7. **Add "Why rsac?" competitive section to README** (1h)
8. **Implement Linux `supported_formats()` query** (2h)

## Cruft Inventory

```bash
git rm src/bin/run_tests.rs          # Stale stub (TODO in code, line 32)
git rm src/bin/test_report_generator.rs  # Orphaned from CI workflows
# Then edit Cargo.toml [[bin]] entries (lines ~192-197)
```

Safe migrate to scripts/: `src/bin/pipewire_diagnostics.rs` (diagnostic tool, not library code).

## Top 5 for Loop 26+

1. Delete stale binaries + audit dead_code (HIGH)
2. README: add "Why rsac?" + consolidate platform version requirements (MEDIUM)
3. `CaptureTarget::FromStr` + `prelude` module (MEDIUM)
4. `docs/features.md` clarify platform features are semantic markers, not compile gates (LOW)
5. Linux `supported_formats()` implementation (LOW)

## Verdict

**ARCHITECTURE: HEALTHY**

rsac cleanly encapsulates its vision. Public API ergonomic, cross-platform abstractions excellent, error handling comprehensive. Minor cruft (stale binaries, dead_code markers) should be cleaned up, but no structural refactoring needed. Production-ready and extensible.
