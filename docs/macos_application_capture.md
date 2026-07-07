# macOS Application-Level Audio Capture

How to capture audio from a specific application (or process subtree) on macOS
with the `rsac` library. On macOS this is implemented with a **CoreAudio Process
Tap** plus an aggregate device; you drive it entirely through the public
`rsac` API — no platform-specific code in your program.

> All code below uses only the current public API (`use rsac::...`). See
> [`docs/API.md`](API.md) for the full API tour.

## Overview

Application capture records the audio output of a single targeted process (or a
process and its descendants) rather than the whole system mix. You select what
to capture with a [`CaptureTarget`](../src/core/config.rs) and hand it to
[`AudioCaptureBuilder`](../src/api.rs); the macOS backend resolves the target to
a Process Tap behind the scenes.

## Prerequisites

1. **macOS 14.4 or newer.** CoreAudio Process Tap (the mechanism rsac uses for
   per-application capture) is only available on macOS 14.4+. The backend uses a
   3-path `CATapDescription` fallback to remain compatible across macOS 14.4–15
   (Sonoma/Sequoia) and macOS 26 (Tahoe). See
   [macOS Version Compatibility](MACOS_VERSION_COMPATIBILITY.md) and the
   [macOS 26 Process Tap fix](MACOS26_PROCESS_TAP_FIX.md) for details — you do
   not need to handle any of this yourself.

2. **`Info.plist` — `NSAudioCaptureUsageDescription`.** The application that
   *uses this library* must declare the `NSAudioCaptureUsageDescription` key.
   The OS shows this string when it prompts the user for audio-capture
   permission on the first Process Tap attempt.

   ```xml
   <?xml version="1.0" encoding="UTF-8"?>
   <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
   <plist version="1.0">
   <dict>
       <!-- ... other keys ... -->
       <key>NSAudioCaptureUsageDescription</key>
       <string>This app captures other applications' audio to [record / visualize / analyze it].</string>
       <!-- ... other keys ... -->
   </dict>
   </plist>
   ```

## Permissions: it is "Audio Capture", not "Screen Recording"

Process Tap is gated by the **Audio Capture** TCC service
(`kTCCServiceAudioCapture`). This is a **distinct, stricter** privacy permission
from:

- **Screen Recording** (`kTCCServiceScreenCapture`) — used by ScreenCaptureKit /
  window capture. rsac does **not** use this and you should **not** tell users
  to enable it.
- **Microphone** — input-device capture. Process Tap captures application
  *output*, not the mic, so the Microphone permission is also not what's
  required.

The dependency is declared via `NSAudioCaptureUsageDescription` in `Info.plist`
(above); macOS prompts the user on the first Process Tap attempt and records the
grant/denial under `kTCCServiceAudioCapture`.

You can query the current status through the public API:

```rust
use rsac::check_audio_capture_permission;
use rsac::core::introspection::PermissionStatus;

match check_audio_capture_permission() {
    PermissionStatus::Granted => { /* ready to capture */ }
    PermissionStatus::NotDetermined => { /* the OS will prompt on first tap */ }
    PermissionStatus::Denied => { /* guide the user to System Settings */ }
    PermissionStatus::NotRequired => { /* non-macOS platforms */ }
    _ => {} // PermissionStatus is #[non_exhaustive]
}
```

If permission is denied, guide the user to **System Settings → Privacy &
Security → Audio Capture** for your app — not Screen Recording, not Microphone.

## Choosing a capture target

macOS supports three application-oriented `CaptureTarget` variants, each
resolving to a different Process Tap on the backend
(see [`src/audio/macos/thread.rs`](../src/audio/macos/thread.rs)):

| Target | How it resolves on macOS | Captures |
|---|---|---|
| `CaptureTarget::ApplicationByName(name)` | Enumerates running apps, matches `name` **exactly, case-insensitively** against the localized app name (e.g. `"Safari"`, `"Music"`), then taps that PID | One app's audio |
| `CaptureTarget::Application(ApplicationId(pid))` | Parses the `ApplicationId` string as a numeric PID, then taps it | One process's audio session |
| `CaptureTarget::ProcessTree(ProcessId(pid))` | Taps the parent PID **and its child processes** (multi-PID tap) | A whole process subtree |

> `ApplicationByName` uses **exact** case-insensitive name matching (not
> substring), consistent across all three platforms — so `"Music"` will not
> accidentally resolve to `"Apple Music"`.

Convenience constructors (from [`src/core/introspection.rs`](../src/core/introspection.rs)):

```rust
use rsac::core::config::CaptureTarget;

let by_name = CaptureTarget::app("Safari");     // → ApplicationByName("Safari")
let by_tree = CaptureTarget::pid(1234);         // → ProcessTree(ProcessId(1234))
```

## Discovering targets

Use the cross-platform public discovery helpers — do **not** reach into internal
`audio::macos::…` paths.

```rust
use rsac::{list_audio_applications, list_audio_sources};
use rsac::core::introspection::AudioSourceKind;

// All running applications that may be producing audio.
for source in list_audio_applications()? {
    if let AudioSourceKind::Application { pid, app_name, bundle_id } = &source.kind {
        println!("{app_name} (pid {pid}, bundle {bundle_id:?})");
    }
}

// Or the unified list (system default + devices + applications), where each
// discovered source can be turned straight into a CaptureTarget:
for source in list_audio_sources()? {
    let target = source.to_capture_target();
    let _ = target; // feed into AudioCaptureBuilder::with_target(...)
}
```

Note: a discovered application maps to `CaptureTarget::Application` (that single
app's session), not the whole process tree. To capture the subtree, construct
`CaptureTarget::pid(...)` / `CaptureTarget::ProcessTree(...)` explicitly.

## Full example

```rust
use rsac::api::AudioCaptureBuilder;
use rsac::core::config::{CaptureTarget, SampleFormat};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut capture = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::app("Safari")) // capture Safari's audio
        .sample_rate(48_000)                        // 22050/32000/44100/48000/88200/96000
        .channels(2)                                // 1..=32
        .sample_format(SampleFormat::F32)
        .build()?;

    capture.start()?;
    println!("Capturing application audio…");

    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < 5 {
        match capture.read_buffer()? {          // non-blocking pull
            Some(buffer) => {
                let _frames = buffer.num_frames();
                let _level = buffer.rms_dbfs(); // RT-safe metering
                // … process buffer.data() (interleaved f32) …
            }
            None => std::thread::sleep(std::time::Duration::from_millis(1)),
        }
    }

    capture.stop()?;
    Ok(())
}
```

To target a specific PID instead of a name, swap the target:

```rust
use rsac::core::config::{CaptureTarget, ApplicationId, ProcessId};

// One process's session:
let _ = CaptureTarget::Application(ApplicationId("1234".to_string()));
// A process and all its children:
let _ = CaptureTarget::ProcessTree(ProcessId(1234));
```

Other consumption styles (blocking reads, the `buffers_iter()` iterator,
`subscribe()` channel delivery, `set_callback(...)`, and the async stream) work
identically for application capture — see [`docs/API.md`](API.md).

## Limitations

- **macOS version:** strictly macOS 14.4+ (Process Tap requirement).
- **PID stability:** targeting is ultimately by PID. If the target application
  restarts, its PID changes and the stream will stop producing data. Re-resolve
  the target (e.g. re-run `list_audio_applications()` or re-issue
  `CaptureTarget::app(name)`).
- **Enumeration vs. audio activity:** `list_audio_applications()` lists running
  apps, not only those currently producing sound. A tap attaches regardless but
  yields no data while the app is silent.
- **Buffer sizing:** the `buffer_size` builder hint is honored only on Windows
  today; the macOS backend derives its ring capacity internally.

## Troubleshooting

- **Permission errors / silent capture:** confirm `NSAudioCaptureUsageDescription`
  is in your `Info.plist`, and that the user granted **Audio Capture** (System
  Settings → Privacy & Security → Audio Capture) — *not* Screen Recording and
  *not* Microphone. Check status with `check_audio_capture_permission()`.
- **`ApplicationNotFound`:** the name did not match any running app's localized
  name exactly (case-insensitively), or the PID was not numeric / not running.
  Enumerate with `list_audio_applications()` to see the exact names/PIDs.
- **Target quits:** when the app exits, reads stop yielding data and may return a
  fatal error. Break on `e.is_fatal()` and re-resolve the target.
- **No audio data:** verify the app is actually producing sound; on a fresh grant
  the first attempt may trigger the OS permission prompt.

## Best practices

- **Handle errors by class:** treat `e.is_fatal()` as terminal (re-select a
  target), retry recoverable errors, and handle `Ok(None)` from `read_buffer()`
  with a short sleep.
- **Re-enumerate on failure:** if a previously working target fails, re-run
  `list_audio_applications()` — the app may have restarted with a new PID.
- **Report the target:** surface which application is being captured and the
  capture status to the user.
