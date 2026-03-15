# macOS Version Compatibility Guide

> **Reference document for macOS audio capture API compatibility across versions.**
> Covers the Process Tap API landscape, version-specific behavior, known issues,
> and recommended action items for rsac's macOS backend.

---

## Table of Contents

1. [macOS Version Support Matrix](#1-macos-version-support-matrix)
2. [API Changes Across Versions](#2-api-changes-across-versions)
3. [Current rsac Approach (What We Do Now)](#3-current-rsac-approach-what-we-do-now)
4. [Known Issues & Risks](#4-known-issues--risks)
5. [What's Needed for Pre-14.4 macOS Support](#5-whats-needed-for-pre-144-macos-support)
6. [What's Needed for macOS 15 (Sequoia) Support](#6-whats-needed-for-macos-15-sequoia-support)
7. [What's Needed for Future macOS Versions](#7-whats-needed-for-future-macos-versions)
8. [Recommended Action Items (Prioritized)](#8-recommended-action-items-prioritized)

---

## 1. macOS Version Support Matrix

| macOS Version | Codename | System Capture | App Capture | Process Tree | Device Enum | Notes |
|:---:|:---|:---:|:---:|:---:|:---:|:---|
| < 12.0 | Pre-Monterey | ❌ | ❌ | ❌ | ✅ (basic) | No modern capture APIs |
| 12.0–12.2 | Monterey | ❌ | ❌ | ❌ | ✅ (basic) | ScreenCaptureKit not yet available |
| 12.3–13.x | Monterey/Ventura | ❌¹ | ❌¹ | ❌ | ✅ (basic) | ScreenCaptureKit exists but no Process Tap |
| 14.0–14.3 | Sonoma (early) | ❌ | ❌ | ❌ | ✅ (basic) | `CATapDescription` class not yet introduced |
| **14.4–14.x** | **Sonoma** | **✅** | **✅** | **✅** | **✅** | Process Tap API introduced. Uses `setProcesses:exclusive:` |
| **15.x** | **Sequoia** | **✅**² | **✅**² | **✅**² | **✅** | Same API as Sonoma. **UNTESTED on real hardware** |
| **26.x** | **Tahoe (beta)** | **✅** | **✅** | **✅** | **✅** | Breaking changes: uses `initStereoMixdownOfProcesses:` with AudioObjectIDs |

**Legend:**
- ✅ = Supported and implemented in rsac
- ❌ = Not supported (API does not exist)
- ¹ = ScreenCaptureKit could provide a partial fallback (not implemented)
- ² = Expected to work but not yet verified on real hardware

### Minimum Deployment Target

rsac's macOS backend **requires macOS 14.4+** for any capture functionality. The
[`CATapDescription`](src/audio/macos/tap.rs:124) class availability check is the
first thing verified at runtime — if the class doesn't exist, all capture modes
return an error immediately.

Device enumeration (listing audio devices, getting default device) works on all
macOS versions since it uses standard CoreAudio `AudioObject` APIs.

---

## 2. API Changes Across Versions

### macOS 14.4 (Sonoma) — Process Tap Introduced

The `CATapDescription` Objective-C class was introduced in macOS 14.4, providing
per-process and system-wide audio capture for the first time via CoreAudio.

**New APIs:**

| API | Purpose |
|:---|:---|
| `CATapDescription` class | Describes a process tap configuration |
| `AudioHardwareCreateProcessTap()` | Creates a hardware process tap from a description |
| `AudioHardwareDestroyProcessTap()` | Destroys a previously created process tap |
| `initStereoGlobalTapButExcludeProcesses:` | Creates a system-wide tap excluding specified processes |
| `initStereoMixdownOfProcesses:` | Creates a tap for specific processes (takes PID `NSNumber`s) |
| `setProcesses:exclusive:` | Combined setter: sets target processes and exclusivity mode |
| `setPrivateTap:` | Marks the tap as private (not visible to other apps) |
| `setName:` | Sets a descriptive name for the tap |
| `setUUID:` | Sets a UUID identifier (required for aggregate device) |
| `setMuteBehavior:` | Controls mute behavior (`CATapUnmuted = 0`) |
| `setMixdown:` | Enables stereo mixdown of captured audio |
| `kAudioAggregateDeviceTapListKey` | Aggregate device dictionary key for tap list |
| `kAudioAggregateDeviceTapAutoStartKey` | Aggregate device dictionary key for auto-start |

**Architecture pattern** (required for all capture modes):

```
CATapDescription → AudioHardwareCreateProcessTap → tap_id
                                                      ↓
Default Output Device UID + tap_id → Aggregate Device → AUHAL reads from aggregate
```

The raw `tap_id` cannot be used directly with AUHAL. It must be wrapped in an
aggregate device that combines the tap with the system's default output device.

### macOS 15 (Sequoia) — No Known Changes

No Process Tap API changes have been identified between Sonoma and Sequoia. The
macOS 14.4 API surface (`setProcesses:exclusive:`, `setPrivateTap:`, etc.) is
expected to remain available.

**Status:** UNTESTED — needs verification on real Sequoia hardware.

### macOS 26 (Tahoe) — Breaking Changes

macOS 26 introduced several **breaking changes** to the `CATapDescription` API:

| Change | Old (14.4–15) | New (26+) |
|:---|:---|:---|
| Process targeting (combined) | `setProcesses:exclusive:` | **REMOVED** |
| Process targeting (separate) | N/A | `setProcesses:` + `setExclusive:` (separate calls) |
| Private tap | `setPrivateTap:` | **REMOVED** |
| `initStereoMixdownOfProcesses:` argument | `NSArray` of PID `NSNumber`s | `NSArray` of AudioObjectID `NSNumber`s |
| PID → AudioObjectID translation | N/A | `kAudioHardwarePropertyTranslatePIDToProcessObject` (`'id2p'`) |

**Key details:**

1. **`setProcesses:exclusive:` REMOVED** — The combined two-argument selector no
   longer exists. It is replaced by separate `setProcesses:` and `setExclusive:`
   calls.

2. **`setPrivateTap:` REMOVED** — This selector no longer responds. Calling it
   would cause an Objective-C exception. rsac guards this with
   [`respondsToSelector:`](src/audio/macos/tap.rs:163).

3. **`initStereoMixdownOfProcesses:` semantics changed** — On macOS 14.4–15, this
   initializer accepted an `NSArray` of PID values as `NSNumber(int:)`. On macOS
   26, it expects AudioObjectID values as `NSNumber(unsignedInt:)`. PIDs must
   first be translated via `kAudioHardwarePropertyTranslatePIDToProcessObject`.

4. **New property: `kAudioHardwarePropertyTranslatePIDToProcessObject`** — Selector
   `'id2p'` on `kAudioObjectSystemObject`. Takes a `pid_t` qualifier and returns
   the corresponding `AudioObjectID`. Defined manually in rsac since
   `coreaudio-sys` 0.2.17 does not include it:

   ```rust
   // src/audio/macos/tap.rs:72-73
   const K_AUDIO_HARDWARE_PROPERTY_TRANSLATE_PID_TO_PROCESS_OBJECT: u32 =
       ((b'i' as u32) << 24) | ((b'd' as u32) << 16) | ((b'2' as u32) << 8) | (b'p' as u32);
   ```

---

## 3. Current rsac Approach (What We Do Now)

### 3.1 Process/Application/Tree Capture: 3-Level Fallback

The function [`create_process_tap_description()`](src/audio/macos/tap.rs:752) in
[`tap.rs`](src/audio/macos/tap.rs) implements a 3-path fallback strategy for
creating a `CATapDescription` with process targeting:

#### Path 1: `initStereoMixdownOfProcesses:` with AudioObjectIDs (macOS 26+)

```rust
// src/audio/macos/tap.rs:757-838
// 1. Alloc CATapDescription
// 2. Check if initStereoMixdownOfProcesses: is available via respondsToSelector:
// 3. Translate each PID → AudioObjectID via kAudioHardwarePropertyTranslatePIDToProcessObject
// 4. Create NSArray of NSNumber(unsignedInt: audio_obj_id)
// 5. Call initStereoMixdownOfProcesses: with the AudioObjectID array
// 6. If successful → return the initialized CATapDescription
// 7. If any step fails → fall through to Path 2
```

This path is preferred on macOS 26+ because `initStereoMixdownOfProcesses:` now
expects AudioObjectIDs, not PIDs.

#### Path 2: `setProcesses:exclusive:` with PIDs (macOS 14.4–15)

```rust
// src/audio/macos/tap.rs:892-898
// 1. Alloc + init CATapDescription (plain init)
// 2. Create NSArray of NSNumber(int: pid)
// 3. Check if setProcesses:exclusive: is available via respondsToSelector:
// 4. Call setProcesses:exclusive: with (pids_nsarray, NO)
// 5. Return the configured CATapDescription
```

This is the traditional approach that works on macOS 14.4 through 15.x.

#### Path 3: Separate `setProcesses:` + `setExclusive:` (macOS 26 edge case)

```rust
// src/audio/macos/tap.rs:900-913
// 1. (Reuses alloc+init from Path 2 attempt)
// 2. Check if setProcesses: AND setExclusive: are available separately
// 3. Call setProcesses: with pids_nsarray
// 4. Call setExclusive: with NO
// 5. Return the configured CATapDescription
```

This is the fallback for macOS 26 when Path 1 fails (e.g., PID→AudioObjectID
translation returned no results because the target process isn't producing audio
yet).

### 3.2 System-Wide Capture

[`CoreAudioProcessTap::new_system()`](src/audio/macos/tap.rs:432) uses a simpler
path:

```rust
// src/audio/macos/tap.rs:448-465
// 1. Create empty NSArray (no processes to exclude)
// 2. Call initStereoGlobalTapButExcludeProcesses: with the empty array
// 3. This captures ALL system audio
```

### 3.3 `respondsToSelector:` Guards

All optional or version-sensitive selectors are guarded with the helper function
[`msg_send_responds_to()`](src/audio/macos/tap.rs:1237):

```rust
// src/audio/macos/tap.rs:1237-1240
unsafe fn msg_send_responds_to(obj: id, sel: Sel) -> bool {
    let responds: BOOL = msg_send![obj, respondsToSelector: sel];
    responds == YES
}
```

Current guard points:

| Location | Selector | Purpose |
|:---|:---|:---|
| [`tap.rs:163`](src/audio/macos/tap.rs:163) | `setPrivateTap:` | Removed in macOS 26 |
| [`tap.rs:334`](src/audio/macos/tap.rs:334) | `setPrivateTap:` | Same guard for tree tap |
| [`tap.rs:455`](src/audio/macos/tap.rs:455) | `initStereoGlobalTapButExcludeProcesses:` | Verify system tap API exists |
| [`tap.rs:498`](src/audio/macos/tap.rs:498) | `setPrivateTap:` | Same guard for system tap |
| [`tap.rs:764`](src/audio/macos/tap.rs:764) | `initStereoMixdownOfProcesses:` | Check Path 1 availability |
| [`tap.rs:894`](src/audio/macos/tap.rs:894) | `setProcesses:exclusive:` | Check Path 2 availability |
| [`tap.rs:903-904`](src/audio/macos/tap.rs:903) | `setProcesses:` / `setExclusive:` | Check Path 3 availability |

### 3.4 Aggregate Device Pattern

All capture modes (system, app, process tree) go through the same aggregate device
creation pattern in [`build_aggregate_device_dict()`](src/audio/macos/tap.rs:1106):

```
CATapDescription → AudioHardwareCreateProcessTap → tap_id
                                                      ↓
Default Output Device UID ─────────────────────→ Aggregate Device Dict
                                                      ↓
                                     AudioHardwareCreateAggregateDevice
                                                      ↓
                                          aggregate_device_id → AUHAL
```

The aggregate device dictionary structure:

```json
{
  "name": "rsac-agg-{pid}",
  "uid": "rsac-agg-uid-{pid}",
  "master": "<default_output_device_uid>",
  "private": true,
  "stacked": false,
  "tap_auto_start": true,
  "subdevices": [{ "uid": "<default_output_device_uid>" }],
  "taps": [{ "uid": "<tap_uuid>", "drift_compensation": true }]
}
```

---

## 4. Known Issues & Risks

### 🔴 High Priority

#### 4.1 `CStr::from_ptr()` without null check — Potential UB/crash

**Files:** [`tap.rs:192`](src/audio/macos/tap.rs:192), [`tap.rs:363`](src/audio/macos/tap.rs:363), [`tap.rs:527`](src/audio/macos/tap.rs:527)

**Problem:** `NSString::UTF8String()` can return `NULL` if the NSString contains
characters that cannot be represented as UTF-8 or if the NSString itself is in an
invalid state. Passing a null pointer to `CStr::from_ptr()` is **undefined
behavior** and will typically manifest as a segfault.

**Current code:**

```rust
// src/audio/macos/tap.rs:191-194
let uuid_nsstring: id = msg_send![tap_uuid, UUIDString];
let uuid_cstr = cocoa::foundation::NSString::UTF8String(uuid_nsstring);
let tap_uuid_str = std::ffi::CStr::from_ptr(uuid_cstr)  // ← UB if uuid_cstr is NULL
    .to_string_lossy()
    .into_owned();
```

**Risk:** Low probability (UUID strings are always valid ASCII), but the pattern
is unsound. If triggered, it causes an immediate crash with no error context.

**Fix:**

```rust
let uuid_nsstring: id = msg_send![tap_uuid, UUIDString];
let uuid_cstr = cocoa::foundation::NSString::UTF8String(uuid_nsstring);
if uuid_cstr.is_null() {
    return Err(AudioError::BackendError {
        backend: "CoreAudio".into(),
        operation: "process_tap".into(),
        message: "UUID NSString returned null UTF8String pointer".into(),
        context: None,
    });
}
let tap_uuid_str = std::ffi::CStr::from_ptr(uuid_cstr)
    .to_string_lossy()
    .into_owned();
```

**Note:** [`coreaudio.rs:100-107`](src/audio/macos/coreaudio.rs:100) already
demonstrates the correct pattern with a null check — the same should be applied
consistently in `tap.rs`.

---

#### 4.2 Aggregate device UID collision

**File:** [`tap.rs:1181`](src/audio/macos/tap.rs:1181)

**Problem:** The aggregate device UID uses the format `"rsac-agg-uid-{pid}"`, which
is deterministic based on the target PID. If two concurrent captures target the
same PID (or two system-wide captures use PID 0), their aggregate device UIDs
will collide, causing `AudioHardwareCreateAggregateDevice` to fail or produce
unpredictable behavior.

**Current code:**

```rust
// src/audio/macos/tap.rs:1180-1181
let k_uid = CFString::new(AGG_DEVICE_UID_KEY);
let v_uid = CFString::new(&format!("rsac-agg-uid-{}", pid));
```

**Fix:** Use a UUID instead of PID for the aggregate device UID:

```rust
let agg_uuid = uuid::Uuid::new_v4();
let v_uid = CFString::new(&format!("rsac-agg-uid-{}", agg_uuid));
```

Or, since rsac already creates an `NSUUID` for the tap, reuse the tap's UUID
string:

```rust
let v_uid = CFString::new(&format!("rsac-agg-uid-{}", tap_uuid_str));
```

---

#### 4.3 Heap allocation in real-time callback

**File:** [`thread.rs:228`](src/audio/macos/thread.rs:228)

**Problem:** `data.to_vec()` performs a heap allocation on CoreAudio's real-time
audio thread. Real-time audio threads should never allocate, lock, or block.
While this works in practice (the allocator is fast enough for most workloads),
under memory pressure or with large buffer sizes it can cause audio glitches.

**Current code:**

```rust
// src/audio/macos/thread.rs:225-231
let data: &[f32] = args.data.buffer;

if !data.is_empty() {
    let audio_buffer = AudioBuffer::new(data.to_vec(), channels, sample_rate);  // ← allocates
    // Push to ring buffer — if full, silently dropped (back-pressure)
    producer.push_or_drop(audio_buffer);
}
```

**Fix:** Pre-allocate a scratch buffer or use a pool allocator. Alternatively,
push the raw `&[f32]` slice directly into the ring buffer and let the consumer
construct the `AudioBuffer`:

```rust
// Option A: Pre-allocated scratch buffer
// (requires AudioBuffer to support borrowing or a fixed-capacity buffer)

// Option B: Direct slice push to ring buffer
// (requires BridgeProducer API changes to accept &[f32])
```

This is a known trade-off documented in the code comment at
[`thread.rs:221-222`](src/audio/macos/thread.rs:221):
`"Vec allocation is acceptable for initial impl (optimize with scratch buffer later)"`.

---

### 🟡 Medium Priority

#### 4.4 `kAudioObjectPropertyElementMaster` deprecated since macOS 12.0

**Files (7 occurrences):**
- [`tap.rs:609`](src/audio/macos/tap.rs:609)
- [`tap.rs:704`](src/audio/macos/tap.rs:704)
- [`tap.rs:942`](src/audio/macos/tap.rs:942)
- [`tap.rs:1018`](src/audio/macos/tap.rs:1018)
- [`tap.rs:1052`](src/audio/macos/tap.rs:1052)
- [`coreaudio.rs:35`](src/audio/macos/coreaudio.rs:35) (import)
- [`coreaudio.rs:178`](src/audio/macos/coreaudio.rs:178) (usage)

**Problem:** `kAudioObjectPropertyElementMaster` was deprecated in macOS 12.0
(Monterey) and replaced by `kAudioObjectPropertyElementMain`. The constant value
is the same (0), so there is no runtime impact, but it generates deprecation
warnings with newer SDKs and signals outdated code.

**Fix:** Replace all occurrences:

```rust
// Before
mElement: kAudioObjectPropertyElementMaster,

// After
mElement: kAudioObjectPropertyElementMain,  // or just 0u32
```

If `coreaudio-sys` 0.2.17 does not export `kAudioObjectPropertyElementMain`,
define it manually:

```rust
/// `kAudioObjectPropertyElementMain` (replaces deprecated kAudioObjectPropertyElementMaster)
const K_AUDIO_OBJECT_PROPERTY_ELEMENT_MAIN: u32 = 0;
```

---

#### 4.5 Unused Cargo dependencies

**File:** [`Cargo.toml:83-96`](Cargo.toml:83)

**Problem:** Three `objc2-*` crates are declared as dependencies but not imported
or used anywhere in the macOS source code:

- `objc2-core-audio` (v0.3.2)
- `objc2-core-audio-types` (v0.3.2)
- `objc2-core-foundation` (v0.3.2)

These were likely added in anticipation of an `objc2` migration but are currently
dead weight, increasing compile times and dependency surface.

**Fix:** Remove from `[target.'cfg(target_os = "macos")'.dependencies]` in
`Cargo.toml`, or reserve behind a feature flag if migration is planned.

---

#### 4.6 `AVFoundation` framework linked but unused

**File:** [`build.rs:149`](build.rs:149)

**Problem:** The build script links `AVFoundation.framework`:

```rust
println!("cargo:rustc-link-lib=framework=AVFoundation"); // For AVAudioFormat, AVAudioFile
```

However, no code in the macOS backend uses `AVFoundation` types (`AVAudioFormat`,
`AVAudioFile`, etc.). All audio handling uses CoreAudio directly.

**Fix:** Remove the link directive unless a concrete use is planned:

```rust
// build.rs — remove this line:
// println!("cargo:rustc-link-lib=framework=AVFoundation");
```

---

#### 4.7 Device enumeration incomplete

**File:** [`coreaudio.rs:264-271`](src/audio/macos/coreaudio.rs:264)

**Problem:** `MacosDeviceEnumerator::enumerate_devices()` only returns the
default output device, not all audio devices. The `TODO` comment acknowledges this:

```rust
fn enumerate_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice>>> {
    // For now, return the default output device (suitable for loopback capture).
    // TODO: Full enumeration of all output devices.
    match self.default_device() {
        Ok(device) => Ok(vec![device]),
        Err(_) => Ok(vec![]),
    }
}
```

**Fix:** Use `kAudioHardwarePropertyDevices` to enumerate all audio devices:

```rust
fn enumerate_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice>>> {
    let addr = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyDevices,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: 0, // kAudioObjectPropertyElementMain
    };
    // 1. Get data size → count of AudioObjectIDs
    // 2. Allocate Vec<AudioObjectID>
    // 3. AudioObjectGetPropertyData to fill the vec
    // 4. Wrap each in MacosAudioDevice
}
```

---

#### 4.8 `get_all_audio_process_object_ids()` is dead code

**File:** [`tap.rs:938-1004`](src/audio/macos/tap.rs:938)

**Problem:** The function [`get_all_audio_process_object_ids()`](src/audio/macos/tap.rs:938)
is only called from tests ([`tap.rs:1254`](src/audio/macos/tap.rs:1254)). It was
originally intended for system-wide capture via
`initStereoMixdownOfProcesses:` targeting all audio processes, but the current
implementation uses `initStereoGlobalTapButExcludeProcesses:` instead (which is
simpler and more correct).

**Fix:** Either:
- Move to a `#[cfg(test)]` block (it's useful as a diagnostic test)
- Remove entirely if not needed
- Gate behind a feature flag if future use is planned

---

### 🟢 Low Priority / Future

#### 4.9 Migration from `objc` 0.2 to `objc2`

**All macOS source files**

**Problem:** The current codebase uses the `objc` 0.2 crate with runtime message
dispatch via `msg_send![]`. This provides no compile-time type safety for
Objective-C selectors — a typo in a selector name only manifests as a runtime
crash (ObjC exception → Rust panic).

The `objc2` ecosystem provides compile-time checked selector dispatch with typed
method signatures. The `objc2-core-audio` crate (already in `Cargo.toml` but
unused) would provide type-safe wrappers for CoreAudio types.

**Impact:** Currently manageable because all selectors are runtime-guarded with
`respondsToSelector:`. A migration would add static guarantees but is a
significant effort.

**Recommendation:** Consider for a future phase. The `objc2` crates in
`Cargo.toml` suggest this was already being explored.

---

#### 4.10 `coreaudio-sys` 0.2.17 missing newer symbols

**File:** [`tap.rs:67-73`](src/audio/macos/tap.rs:67)

**Problem:** The `coreaudio-sys` crate (v0.2.17) does not include
`kAudioHardwarePropertyTranslatePIDToProcessObject` because it was added in macOS
26. rsac defines it manually using the FourCC encoding:

```rust
const K_AUDIO_HARDWARE_PROPERTY_TRANSLATE_PID_TO_PROCESS_OBJECT: u32 =
    ((b'i' as u32) << 24) | ((b'd' as u32) << 16) | ((b'2' as u32) << 8) | (b'p' as u32);
```

This is correct and functional, but means newer CoreAudio symbols will need
similar manual definitions until `coreaudio-sys` is updated.

**Recommendation:** Monitor `coreaudio-sys` releases for macOS 26 symbol coverage.
Consider contributing the symbol upstream.

---

## 5. What's Needed for Pre-14.4 macOS Support

For macOS versions before 14.4, the `CATapDescription` class does not exist.
All three capture modes (system, app, process tree) depend on it. Here are
potential alternative approaches:

### System Capture (macOS 12.3–14.3)

| Approach | macOS Version | Complexity | Quality |
|:---|:---|:---|:---|
| **ScreenCaptureKit audio** | 12.3+ | Medium | Good |
| Virtual audio device (BlackHole/Soundflower) | Any | Low (user setup) | Good |
| Deprecated `AudioDeviceCreateIOProcID` on output | Any | High | Poor |

**ScreenCaptureKit** (`SCStreamConfiguration` with `capturesAudio = true`) is the
most viable option. It captures system audio as a stream and can exclude specific
apps. Available from macOS 12.3 (Monterey).

### Application Capture (macOS 13+)

| Approach | macOS Version | Complexity | Quality |
|:---|:---|:---|:---|
| **ScreenCaptureKit per-app audio** | 13+ | Medium | Good |
| No alternative | < 13 | N/A | N/A |

macOS 13 (Ventura) added `SCContentFilter` with app-level filtering, enabling
per-application audio capture through ScreenCaptureKit.

### Process Tree Capture

Not possible without Process Tap (macOS 14.4+). ScreenCaptureKit does not provide
process-tree granularity.

### Recommendation

```
┌──────────────────────┬───────────────────────────────────────┐
│ Minimum macOS        │ Strategy                               │
├──────────────────────┼───────────────────────────────────────┤
│ macOS 14.4+ (current)│ Full Process Tap (all features)       │
│ macOS 13.0–14.3      │ ScreenCaptureKit (system + app only)  │
│ macOS 12.3–12.x      │ ScreenCaptureKit (system only)        │
│ < macOS 12.3         │ Not supported                          │
└──────────────────────┴───────────────────────────────────────┘
```

**Priority:** Low. The current macOS 14.4+ minimum is reasonable for rsac's target
audience. Pre-14.4 fallback should only be added if there is strong user demand.

---

## 6. What's Needed for macOS 15 (Sequoia) Support

### Current Status: Expected to Work, UNTESTED

macOS 15 (Sequoia) should work via **Path 2** (`setProcesses:exclusive:`) since
this selector was not removed until macOS 26. The `respondsToSelector:` guards
would correctly select Path 2 on Sequoia.

### Verification Needed

1. **Test on real Sequoia hardware:**
   ```bash
   # System capture
   cargo run --features feat_macos -- record --duration 5 /tmp/test.wav

   # Application capture
   cargo run --features feat_macos -- capture --app "Music" --duration 5

   # Run selector probe test
   cargo test --features feat_macos --lib audio::macos::tap::tests::test_catap_description_available_selectors -- --nocapture
   ```

2. **Confirm selector availability** — The probe test at
   [`tap.rs:1287-1352`](src/audio/macos/tap.rs:1287) will show which selectors
   respond on Sequoia. Expected results:

   | Selector | Expected on 15.x |
   |:---|:---|
   | `setProcesses:exclusive:` | YES |
   | `setPrivateTap:` | YES |
   | `initStereoMixdownOfProcesses:` | YES |
   | `setProcesses:` (separate) | Possibly YES |

3. **No code changes expected** — If selectors match Sonoma behavior, no changes
   are needed.

---

## 7. What's Needed for Future macOS Versions

### Pattern: Always Guard with `respondsToSelector:`

The key defensive pattern rsac uses — and should continue to use — is checking
selector availability at runtime before calling:

```rust
// Pattern: Guard before calling any ObjC selector that may be removed
if msg_send_responds_to(obj, sel!(someSelector:)) {
    let _: () = msg_send![obj, someSelector: value];
} else {
    log::debug!("someSelector: not available on this macOS version");
    // Fall through to alternative
}
```

This pattern successfully handled the macOS 26 `setPrivateTap:` removal without
a crash.

### Monitoring Checklist

For each new macOS major version:

1. **Run the selector probe test** to discover API surface changes
2. **Check** if `initStereoMixdownOfProcesses:` argument semantics changed
3. **Check** if `initStereoGlobalTapButExcludeProcesses:` still exists
4. **Check** if new initializers or properties were added to `CATapDescription`
5. **Review WWDC sessions** for CoreAudio/Process Tap announcements
6. **Test all capture modes** on real hardware

### Potential Future Improvements

| Improvement | Benefit |
|:---|:---|
| Runtime macOS version detection | Report version in logs and error messages |
| `PlatformCapabilities` version awareness | Accurately report what works on the current OS |
| Build-time `MACOS_DEPLOYMENT_TARGET` | Compile-time gating of version-specific code paths |
| `objc2` migration | Compile-time selector safety eliminates runtime crash risk |

### Runtime Version Detection Example

```rust
fn get_macos_version() -> Option<(u32, u32, u32)> {
    let output = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    let version_str = String::from_utf8(output.stdout).ok()?;
    let parts: Vec<u32> = version_str.trim().split('.')
        .filter_map(|p| p.parse().ok())
        .collect();
    match parts.as_slice() {
        [major, minor, patch] => Some((*major, *minor, *patch)),
        [major, minor] => Some((*major, *minor, 0)),
        _ => None,
    }
}
```

This is already partially implemented in [`build.rs:152-171`](build.rs:152) for
compile-time warnings, but a runtime equivalent would be useful for
`PlatformCapabilities` reporting.

---

## 8. Recommended Action Items (Prioritized)

### Must-Do (Safety & Correctness)

| # | Action | Severity | Effort | Files |
|:---:|:---|:---:|:---:|:---|
| 1 | **Add null check before `CStr::from_ptr()`** | 🔴 High | Low | [`tap.rs:192`](src/audio/macos/tap.rs:192), [`tap.rs:363`](src/audio/macos/tap.rs:363), [`tap.rs:527`](src/audio/macos/tap.rs:527) |
| 2 | **Use UUID for aggregate device UID** (prevent collision) | 🔴 High | Low | [`tap.rs:1181`](src/audio/macos/tap.rs:1181) |
| 3 | **Eliminate heap alloc in RT callback** | 🔴 High | Medium | [`thread.rs:228`](src/audio/macos/thread.rs:228) |

### Should-Do (Compatibility & Cleanup)

| # | Action | Severity | Effort | Files |
|:---:|:---|:---:|:---:|:---|
| 4 | **Replace deprecated `kAudioObjectPropertyElementMaster`** | 🟡 Medium | Low | 7 locations across [`tap.rs`](src/audio/macos/tap.rs), [`coreaudio.rs`](src/audio/macos/coreaudio.rs) |
| 5 | **Remove unused `objc2-*` deps** from Cargo.toml | 🟡 Medium | Low | [`Cargo.toml:83-96`](Cargo.toml:83) |
| 6 | **Remove unused AVFoundation link** | 🟡 Medium | Low | [`build.rs:149`](build.rs:149) |
| 7 | **Complete device enumeration** (all devices, not just default) | 🟡 Medium | Medium | [`coreaudio.rs:264`](src/audio/macos/coreaudio.rs:264) |
| 8 | **Add runtime macOS version detection** to `PlatformCapabilities` | 🟡 Medium | Medium | [`capabilities.rs`](src/core/capabilities.rs) |

### Nice-to-Have (Future)

| # | Action | Severity | Effort | Files |
|:---:|:---|:---:|:---:|:---|
| 9 | **Test on macOS 15 (Sequoia)** real hardware | 🟢 Low | Low | N/A |
| 10 | **Evaluate `objc2` migration** for type-safe ObjC bindings | 🟢 Low | High | All macOS files |
| 11 | **Evaluate ScreenCaptureKit** as fallback for pre-14.4 | 🟢 Low | High | New module |
| 12 | **Move `get_all_audio_process_object_ids()` to `#[cfg(test)]`** | 🟢 Low | Low | [`tap.rs:938`](src/audio/macos/tap.rs:938) |

---

## Appendix: File Reference

| File | Purpose |
|:---|:---|
| [`src/audio/macos/tap.rs`](src/audio/macos/tap.rs) | Process Tap lifecycle: create/destroy tap + aggregate device |
| [`src/audio/macos/thread.rs`](src/audio/macos/thread.rs) | AUHAL AudioUnit setup, RT callback, `MacosPlatformStream` |
| [`src/audio/macos/coreaudio.rs`](src/audio/macos/coreaudio.rs) | Device enumeration, app enumeration, ASBD conversion, error mapping |
| [`src/audio/macos/mod.rs`](src/audio/macos/mod.rs) | Module re-exports |
| [`build.rs`](build.rs) | macOS framework linking, version check |
| [`Cargo.toml`](Cargo.toml) | Dependency declarations (macOS section) |
| [`docs/MACOS26_PROCESS_TAP_FIX.md`](docs/MACOS26_PROCESS_TAP_FIX.md) | Detailed macOS 26 breaking changes and fix documentation |
