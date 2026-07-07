# macOS 26 (Tahoe) Process Tap API Compatibility Fix

**Date:** 2026-03-15  
**File changed:** `src/audio/macos/tap.rs`  
**Platforms affected:** macOS 26.x (Tahoe) — backward compatible with macOS 14.4–15

---

## Problem

On macOS 26.3 (Tahoe), the `CATapDescription` class changed its API surface:

| Selector | macOS 14.4–15 | macOS 26 |
|---|---|---|
| `setProcesses:exclusive:` | ✅ Available | ❌ **Removed** |
| `setProcesses:` (separate) | ❌ | ✅ Available |
| `setExclusive:` (separate) | ❌ | ✅ Available |
| `initStereoMixdownOfProcesses:` | ✅ Available | ✅ Available |
| `initStereoGlobalTapButExcludeProcesses:` | ✅ Available | ✅ Available |
| `setPrivateTap:` | ✅ Available | ❌ **Removed** |

The existing `new()` (single-process capture) and `new_tree()` (process-tree capture) methods both relied on `setProcesses:exclusive:`, which no longer exists on macOS 26. This caused per-process audio capture to fail.

System-wide capture via `new_system()` was already fixed in a prior change (it uses `initStereoGlobalTapButExcludeProcesses:`).

## Solution

### Approach

Use `initStereoMixdownOfProcesses:` as the preferred initializer for per-process taps, with a multi-level fallback chain for backward compatibility.

**Key insight:** `initStereoMixdownOfProcesses:` takes an `NSArray` of **AudioObjectIDs** (not PIDs). This requires translating PIDs to AudioObjectIDs via `kAudioHardwarePropertyTranslatePIDToProcessObject`.

The Swift reference implementation (`AudioCap/ProcessTap.swift`) confirms this:
```swift
let tapDescription = CATapDescription(stereoMixdownOfProcesses: [objectID])
```

### Fallback chain (in order)

1. **Path 1 — `initStereoMixdownOfProcesses:`** (macOS 26+, preferred)
   - Translate each PID → AudioObjectID via `kAudioHardwarePropertyTranslatePIDToProcessObject`
   - Create `NSArray` of AudioObjectIDs as `NSNumber(unsignedInt:)`
   - Call `initStereoMixdownOfProcesses:` on the alloc'd `CATapDescription`

2. **Path 2 — `setProcesses:exclusive:`** (macOS 14.4–15, fallback)
   - `alloc` + `init` the `CATapDescription`
   - Call `setProcesses:exclusive:` with PID-based `NSArray`

3. **Path 3 — Separate `setProcesses:` + `setExclusive:`** (macOS 26 edge case)
   - For macOS 26 when PID→AudioObjectID translation failed
   - `alloc` + `init`, then call `setProcesses:` and `setExclusive:` separately

4. **Error** — if none of the above work

---

## Code Changes

### New constant

```rust
/// kAudioHardwarePropertyTranslatePIDToProcessObject = 'id2p'
const K_AUDIO_HARDWARE_PROPERTY_TRANSLATE_PID_TO_PROCESS_OBJECT: u32 =
    ((b'i' as u32) << 24) | ((b'd' as u32) << 16) | ((b'2' as u32) << 8) | (b'p' as u32);
```

FourCC value `'id2p'` sourced from `CoreAudio/AudioHardware.h` in the macOS SDK. Not available in `coreaudio-sys` 0.2.17, so defined manually.

### New helper: `translate_pid_to_audio_object_id()`

```rust
unsafe fn translate_pid_to_audio_object_id(pid: u32) -> Option<sys::AudioObjectID>
```

Translates a process PID to its CoreAudio `AudioObjectID` using `kAudioHardwarePropertyTranslatePIDToProcessObject`. Returns `None` if the process isn't registered with CoreAudio (e.g., not producing audio).

### New helper: `create_process_tap_description()`

```rust
unsafe fn create_process_tap_description(
    ca_tap_class: &Class,
    pids: &[u32],
    operation: &str,
) -> AudioResult<id>
```

Shared logic for creating a `CATapDescription` with process targeting. Implements the 3-level fallback chain. Used by both `new()` and `new_tree()` to eliminate code duplication.

### Modified: `new()` (single-process capture)

- Replaced inline `alloc` → `init` → `setProcesses:exclusive:` with call to `create_process_tap_description()`
- Changed `setPrivateTap:` to be guarded with `respondsToSelector:` (matching `new_system()`)
- Property setters (`setName:`, `setUUID:`, `setMuteBehavior:`, `setMixdown:`) unchanged

### Modified: `new_tree()` (process-tree capture)

- Same refactoring as `new()`: replaced inline tap description creation with `create_process_tap_description()`
- `setPrivateTap:` now guarded with `respondsToSelector:`
- Process tree discovery via `sysinfo` unchanged

### New test: `test_translate_pid_to_audio_object_id`

Tests the PID→AudioObjectID translation with:
- Own process PID (may succeed — test process has an AudioObjectID)
- PID 0 (kernel — should return None)
- PID 999999999 (non-existent — should return None)

---

## Test Results

### Unit tests: All 5 passed

```
cargo test --features feat_macos --lib audio::macos::tap::tests -- --nocapture
```

| Test | Result |
|---|---|
| `test_catap_description_class_exists` | ✅ PASS |
| `test_catap_description_available_selectors` | ✅ PASS |
| `test_enumerate_audio_process_objects` | ✅ PASS (34 objects) |
| `test_translate_pid_to_audio_object_id` | ✅ PASS (own PID→Some, PID 0→None, bad PID→None) |
| `test_new_system_creates_tap` | ✅ PASS (tap_id=137, agg_id=138) |

Selector survey from `test_catap_description_available_selectors` on macOS 26:
```
setProcesses:exclusive: → NO    ← confirms the issue
setProcesses:           → YES   ← separate setter available
setExclusive:           → YES   ← separate setter available
setPrivateTap:          → NO    ← removed, correctly guarded
initStereoMixdownOfProcesses: → YES  ← our preferred path
```

### Integration test: App capture by PID (`--pid`)

**Test 1: Process tree via parent shell PID** (looping `afplay`)
```
record --pid 43844 --duration 3 /tmp/app_pid_test.wav
```
- Discovered 2 PIDs: [43844, 44108]
- PID 44108 → AudioObjectID 136
- **Used `initStereoMixdownOfProcesses:` ✅**
- Result: 143,872 frames, 1.10 MB WAV

**Test 2: Direct PID (`say` command)**
```
record --pid 450 --duration 3 ./say_pid_test.wav
```
- PID 450 → AudioObjectID 143
- **Used `initStereoMixdownOfProcesses:` ✅**
- Result: 143,872 frames, 1.10 MB WAV

**Test 3: Firefox PID** (Spotify playing in browser)
```
record --pid 61948 --duration 5 ./firefox_pid_test.wav
```
- 19 PIDs in process tree, 1 audio-producing child → AudioObjectID 134
- **Used `initStereoMixdownOfProcesses:` ✅**
- Result: 239,616 frames, 1.83 MB WAV

### Integration test: App capture by name (`--app`)

**Test: `--app Firefox`** (Spotify playing)
```
record --app Firefox --duration 5 ./firefox_app_test.wav
```
- PID 61948 → AudioObjectID 134
- **Used `initStereoMixdownOfProcesses:` ✅**
- Result: 240,128 frames, 1.83 MB WAV

### Edge case: Process not in CoreAudio registry

When a process PID doesn't translate to an AudioObjectID (e.g., ephemeral `afplay` processes that exit between loop iterations), the fallback to separate `setProcesses:` + `setExclusive:` is used. On macOS 26, this fallback fails with OSStatus 560947818 (`!pid`), indicating the separate setters also need AudioObjectIDs, not PIDs.

**This is not a regression** — the old code (`setProcesses:exclusive:`) would have failed entirely on macOS 26 regardless. The fix now works for all processes that are actively producing audio (which is the expected use case for audio capture).

---

## Backward Compatibility

| macOS Version | Path Used | Status |
|---|---|---|
| macOS 26+ (Tahoe) | `initStereoMixdownOfProcesses:` | ✅ Working |
| macOS 14.4–15 | `setProcesses:exclusive:` | ✅ Backward compat (untested on older macOS, selector guard in place) |
| macOS < 14.4 | Error: CATapDescription not found | Expected (Process Tap requires 14.4+) |

---

## Files Modified

| File | Change Summary |
|---|---|
| `src/audio/macos/tap.rs` | Added constant, 2 helpers, refactored `new()` + `new_tree()`, guarded `setPrivateTap:`, added test |

## Related

- Prior fix: `new_system()` in same file already used `initStereoGlobalTapButExcludeProcesses:`
- Reference: `reference/AudioCap/AudioCap/ProcessTap/ProcessTap.swift` line 92
- Reference: `reference/AudioCap/AudioCap/ProcessTap/CoreAudioUtils.swift` lines 62–76

---

## Follow-up hardening (rsac-catap-safety / rsac-ptree / rsac-ead3-teardown)

A later pass (from the macOS/CoreAudio deep critique) hardened `tap.rs` and the
device-alive listener without changing the 3-path fallback behaviour above.

### 1. ObjC exception safety — `guard_objc`

`CATapDescription` is a private/undocumented class whose selectors' argument
shapes shift across macOS 14.4–26. If one raises an `NSException`, that foreign
exception unwinds into Rust. With objc2's default `msg_send!` (this crate
enables `objc2`'s `"exception"` feature but **not** `"catch-all"`), such an
unwind **is not caught by `std::panic::catch_unwind`** and typically **aborts
the process**.

Fix: a `guard_objc(operation, closure) -> AudioResult<R>` shim built on
[`objc2::exception::catch`](https://docs.rs/objc2/latest/objc2/exception/fn.catch.html)
(the correct primitive; feature `"exception"` already enabled). Every may-throw
`CATapDescription` message send now goes through it, so a version-shape mismatch
degrades to a categorized `AudioError::BackendError` (with the thrown
NSException's `name: reason` surfaced for diagnostics) instead of aborting.

The previous `test_new_system_creates_tap` relied on `std::panic::catch_unwind`
to "catch ObjC exceptions" — which did not actually work. It now documents that
ObjC exceptions arrive as `Err(AudioError::BackendError)` and only nets a
genuine Rust panic defensively.

### 2. Centralized selector configuration — `configure_tap_description`

The shared property setters (`setName:`, `setUUID:`, `setMuteBehavior:`,
`setPrivateTap:`, `setMixdown:`) were copy-pasted across `new`, `new_tree`, and
`new_system`. They are now issued from one exception-guarded helper,
`configure_tap_description`, so exception handling, the `respondsToSelector:`
guard for the removed-in-26 `setPrivateTap:`, and the `setMuteBehavior:` `i64`
(`NSInteger`/`'q'`) argument-type fix all live in one auditable place.

### 3. ObjC ownership / nil-init leak-safety audit

`create_process_tap_description`'s ownership contract is now documented and
consistent: on a `init…` returning **nil OR throwing**, the initializer has
already consumed (released) the alloc'd receiver per the ObjC "init consumes
self" rule, so the caller must **not** release it (it falls through with a fresh
`alloc`); when `init…` was **never called** (selector unavailable / no PIDs) the
uninitialized alloc is released. The fallback path also releases a live
`init_obj` before any error return, closing a latent leak where a thrown setter
would skip the manual `release`.

### 4. ProcessTree = full descendant closure (not just direct children)

`new_tree` previously targeted only the parent + its **direct** children. It now
walks the `parent → child` relation breadth-first over a single `sysinfo`
snapshot to target the parent + **all transitive descendants** (children,
grandchildren, …), via the pure, unit-tested `collect_process_tree_pids` helper
(cycle-safe: each PID visited once). This matters because browsers/Electron apps
emit audio from grandchild helper processes a direct-children-only tap would
miss. Still snapshot-based/best-effort — a process forking after the snapshot is
not captured (re-enumeration on tree change remains future work).

### 5. Device-alive listener teardown race guard

`DeviceAliveContext` gained a `tearing_down: AtomicBool`.
`remove_device_alive_listener` sets it (Release) **before**
`AudioObjectRemovePropertyListener`, and `device_alive_listener_proc` checks it
(Acquire) and no-ops when set. This prevents a device death that races an
explicit stop/Drop from `force_set(Error)`-poisoning the bridge (sticky terminal
`Error` would otherwise outrank the graceful `Stopping`) and misreporting an
intentional teardown as a Fatal device-death `StreamEnded`. See ADR-0010 §4.

### Verification note (host limitation)

These changes were authored on a **Windows** host with no macOS toolchain, so
the `feat_macos` backend (which links the CoreAudio/AppKit frameworks) could not
be compiled or run here. The new logic was structured so the platform-agnostic
pieces (`collect_process_tree_pids`, `guard_objc` behaviour, the teardown-guard
flag semantics) are covered by `#[cfg(all(test, target_os = "macos"))]` unit
tests that run on the macOS CI runner (Blacksmith `blacksmith-6vcpu-macos-15`)
and locally per the Local Testing Guide. Run:

```bash
cargo test --features feat_macos --lib audio::macos -- --nocapture
```

on a macOS 14.4+ host to exercise them (hardware/TCC-gated Process Tap tests
remain `#[ignore]`).
