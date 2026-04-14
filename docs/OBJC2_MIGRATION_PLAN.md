# `cocoa`/`objc` → `objc2` Migration Plan

> **Status:** Phase 1 complete (`coreaudio.rs` migrated), Phase 2 planned (`tap.rs`)  
> **Estimated effort:** 18–28 hours  
> **Branch:** `feat/objc2-migration` (to be created)

## Background

The `cocoa` (0.26.1) and `objc` (0.2.7) crates are deprecated:

- **`cocoa`**: docs.rs says *"deprecated in favour of the objc2 crates"*
- **`objc`**: Last release Oct 2019, repo (`SSheldon/rust-objc`) is archived
- **`objc2`**: v0.6.4, actively maintained, provides type-safe message sending

### Affected Files

| File | Callsites | Complexity |
|---|---|---|
| `src/audio/macos/coreaudio.rs` | ~12 | Low — all have typed `objc2` replacements |
| `src/audio/macos/tap.rs` | ~65 | Medium — `CATapDescription` is a private class |

### Can `objc2` and `objc` coexist during migration?

**Yes.** They bind to the same Objective-C runtime. Migrate one file at a time:
1. `coreaudio.rs` first (simpler)
2. `tap.rs` second (complex)
3. Remove `cocoa`/`objc` deps after both are done

---

## Phase 1: `coreaudio.rs` (2–4 hours)

### Import Changes

```rust
// REMOVE:
use cocoa::base::{id, nil};
use cocoa::foundation::{NSArray, NSString};
use objc::{class, msg_send, sel, sel_impl};

// ADD:
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send, sel};
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_foundation::{NSArray, NSString};
```

### Migration Map

| Line | Current | Replacement |
|---|---|---|
| 84 | `class!(NSWorkspace)` | `NSWorkspace::sharedWorkspace()` |
| 85 | `msg_send![workspace_class, sharedWorkspace]` | (merged above) |
| 86 | `msg_send![shared_workspace, runningApplications]` | `.runningApplications()` |
| 97 | `msg_send![running_apps_nsarray, count]` | `.count()` |
| 100 | `msg_send![running_apps_nsarray, objectAtIndex: i]` | `.objectAtIndex(i)` |
| 105 | `msg_send![app, processIdentifier]` | `.processIdentifier()` |
| 107 | `msg_send![app, localizedName]` | `.localizedName()` → `Option<Retained<NSString>>` |
| 109 | `NSString::UTF8String(name_nsstring)` | `.to_string()` |
| 121 | `msg_send![app, bundleIdentifier]` | `.bundleIdentifier()` → `Option<Retained<NSString>>` |
| 123 | `NSString::UTF8String(bundle_id_nsstring)` | `.to_string()` |

### Before/After

```rust
// BEFORE:
let workspace_class = class!(NSWorkspace);
let shared_workspace: id = msg_send![workspace_class, sharedWorkspace];
let running_apps_nsarray: id = msg_send![shared_workspace, runningApplications];
let count: usize = msg_send![running_apps_nsarray, count];
for i in 0..count {
    let app: id = msg_send![running_apps_nsarray, objectAtIndex: i];
    let pid: i32 = msg_send![app, processIdentifier];
    let name_nsstring: id = msg_send![app, localizedName];
    let c_str_ptr = NSString::UTF8String(name_nsstring);
    // ... CStr::from_ptr ...
}

// AFTER:
let shared_workspace = unsafe { NSWorkspace::sharedWorkspace() };
let running_apps = shared_workspace.runningApplications();
let count = running_apps.count();
for i in 0..count {
    let app = running_apps.objectAtIndex(i);
    let pid = unsafe { app.processIdentifier() };
    let name_str = match unsafe { app.localizedName() } {
        Some(ns) => ns.to_string(),
        None => String::from("<Unknown>"),
    };
}
```

---

## Phase 2: `tap.rs` (12–18 hours)

### Type Mapping

| Old (`cocoa`/`objc`) | New (`objc2`) |
|---|---|
| `id` | `*mut AnyObject` |
| `nil` | `std::ptr::null_mut::<AnyObject>()` |
| `Class` | `AnyClass` |
| `Class::get("Foo")` | `AnyClass::get(c"Foo")` (takes `&CStr`) |
| `BOOL` / `YES` / `NO` | `bool` (auto-converts) or `Bool::YES`/`Bool::NO` |
| `NSAutoreleasePool::new(nil)` | `NSAutoreleasePool::new()` from `objc2-foundation` |
| `NSString::alloc(nil).init_str(s)` | `NSString::from_str(s)` |
| `cocoa::foundation::NSString::UTF8String(s)` | `s.to_string()` |
| `sel_impl!` | Not needed in `objc2` |

### Group A: NSAutoreleasePool (4 sites)

Lines: 128, 303, 451, 1319 (test)

```rust
// BEFORE:
let _pool = NSAutoreleasePool::new(nil);

// AFTER (preserves control flow with early returns):
let _pool = unsafe { NSAutoreleasePool::new() };
```

### Group B: `Class::get("CATapDescription")` (5 sites)

Lines: 131, 306, 454, 1303, 1316 (tests)

```rust
// BEFORE:
let ca_tap_description_class = Class::get("CATapDescription");

// AFTER:
let ca_tap_description_class = AnyClass::get(c"CATapDescription");
```

### Group C: NSString creation (3 sites)

Lines: 150, 326, 495

```rust
// BEFORE:
let tap_name_nsstring = NSString::alloc(nil).init_str(tap_name_str);

// AFTER:
let tap_name_nsstring = NSString::from_str(tap_name_str);
```

### Group D: Foundation class!() macros → typed APIs (8 sites)

| Line | Current | Replacement |
|---|---|---|
| 162, 338, 507 | `class!(NSUUID)` + `msg_send![cls, UUID]` | `NSUUID::UUID()` |
| 467 | `msg_send![class!(NSArray), array]` | `NSArray::new()` |
| 805, 880 | `msg_send![class!(NSNumber), numberWithUnsignedInt: v]` | `NSNumber::new_u32(v)` |
| 813, 900 | `msg_send![class!(NSArray), arrayWithObjects:count:]` | `NSArray::from_retained_slice(...)` |

### Group E: CATapDescription msg_send! calls (~30 sites) — **STAY RAW**

`CATapDescription` is a **private CoreAudio class** not in any `objc2` framework crate. All its selectors (`initStereoMixdownOfProcesses:`, `setProcesses:exclusive:`, etc.) must remain as raw `objc2::msg_send!`.

Key syntax change: `objc2` requires **commas between arguments**:

```rust
// BEFORE (objc 0.2):
msg_send![obj, setProcesses: arr exclusive: NO]

// AFTER (objc2 0.6):
msg_send![obj, setProcesses: arr, exclusive: false]
```

And `YES`/`NO` → `true`/`false` (auto-converts):

```rust
// BEFORE:
let _: () = msg_send![tap_desc_obj, setPrivateTap: YES];

// AFTER:
let _: () = msg_send![tap_desc_obj, setPrivateTap: true];
```

### Group F: `msg_send_responds_to()` helper (line 1264)

```rust
// BEFORE:
unsafe fn msg_send_responds_to(obj: id, sel: Sel) -> bool {
    let responds: BOOL = msg_send![obj, respondsToSelector: sel];
    responds == YES
}

// AFTER:
unsafe fn msg_send_responds_to(obj: *mut AnyObject, sel: Sel) -> bool {
    msg_send![obj, respondsToSelector: sel]
}
```

### Group G: FFI declarations (line 698–712)

```rust
// BEFORE:
fn AudioHardwareCreateProcessTap(description: id, ...) -> OSStatus;

// AFTER:
fn AudioHardwareCreateProcessTap(description: *mut AnyObject, ...) -> OSStatus;
```

### 3-Path Fallback (lines 774–949) — Risk: LOW

The macOS version-specific fallback code (`create_process_tap_description()`) uses `respondsToSelector:` guards and raw `msg_send!` to `CATapDescription`. The migration is mechanical — only import paths and `bool` handling change. No semantic changes to the fallback decision tree.

---

## Phase 3: Cargo.toml Updates (0.5 hours)

### Add

```toml
[target.'cfg(target_os = "macos")'.dependencies]
objc2 = { version = "0.6.4", features = ["exception"] }
objc2-foundation = { version = "0.3.2", features = [
    "NSString", "NSArray", "NSValue", "NSUUID",
    "NSAutoreleasePool", "NSObject",
] }
objc2-app-kit = { version = "0.3.2", features = [
    "NSWorkspace", "NSRunningApplication",
] }
```

### Remove

```toml
cocoa = "0.26.1"             # REMOVE
objc = { version = "0.2.7" } # REMOVE
```

### Keep

```toml
core-foundation = "0.10.1"     # Still maintained (Servo)
core-foundation-sys = "0.8.7"  # Still maintained
coreaudio-rs = "0.14.0"        # Not ObjC-related
coreaudio-sys = "0.2.17"       # Raw FFI, no ObjC
```

### Delete commented-out exploration artifacts

Remove the commented-out `objc2-core-audio`, `objc2-core-audio-types`, `objc2-core-foundation` lines — they aren't needed.

---

## Testing Strategy

1. After Phase 1: `cargo check --features feat_macos` + macOS unit tests
2. After Phase 2: Full test suite + all capture modes on real macOS hardware
3. After Phase 3: Verify `cocoa`/`objc` absent from `cargo tree`
4. CI: Linux/Windows should be unaffected (no macOS deps compiled)

---

## Framework Crate Reference

| ObjC Class | `objc2` Crate | Feature |
|---|---|---|
| `NSWorkspace` | `objc2-app-kit` | `NSWorkspace` |
| `NSRunningApplication` | `objc2-app-kit` | `NSRunningApplication` |
| `NSString` | `objc2-foundation` | `NSString` |
| `NSArray` | `objc2-foundation` | `NSArray` |
| `NSNumber` | `objc2-foundation` | `NSValue` |
| `NSUUID` | `objc2-foundation` | `NSUUID` |
| `NSAutoreleasePool` | `objc2-foundation` | `NSAutoreleasePool` |
| `CATapDescription` | **NONE** (private) | Raw `msg_send!` only |
