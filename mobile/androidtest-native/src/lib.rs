//! TEST-ONLY Android cdylib for the instrumented **playback**-capture tier
//! (rsac-e6d3). Produces `librsac.so`; it is never published and never ships
//! in any consumer artifact (it lives under `/mobile`, excluded from the
//! crates.io tarball, and is only built by `ci-android-emu.yml` into the
//! git-ignored `mobile/android/src/androidTest/jniLibs/x86_64/`).
//!
//! # Why this crate exists, and why its lib name is `rsac`
//!
//! The shipped consent flow (`RsacProjection`, `mobile/android`) hard-codes
//! `System.loadLibrary("rsac")` and mints the `MediaProjection` token through
//! `nativeRetainProjection` — a symbol registered by **rsac's** `JNI_OnLoad`.
//! The playback test must then hand that token straight back into rsac's
//! public capture API (`with_android_projection`). For the mint and the
//! consume to touch the **same** `MediaProjection` `GlobalRef` and the same
//! session/registry statics, both must live in **one** loaded `librsac.so`:
//! two separate `.so`s each statically linking rsac would have disjoint
//! statics, and a token minted in one is not safely consumable in the other.
//!
//! So this cdylib:
//!   1. `pub use rsac;` — links the whole rsac rlib in, re-exporting its
//!      `#[no_mangle] JNI_OnLoad` (the exact keep-alive trick
//!      `mobile/android-native` uses). Loading this `.so` therefore runs
//!      rsac's `RegisterNatives` (CaptureBridge / RsacProjection / RsacDevices)
//!      just like the production `librsac.so`.
//!   2. Adds the name-mangled `Java_ai_codeseys_rsac_NativePlaybackDriver_*`
//!      exports below, resolved by the JVM's standard lazy JNI lookup when the
//!      Kotlin `NativePlaybackDriver` first calls them — no second
//!      `JNI_OnLoad`, no `RegisterNatives` of our own, nothing that could
//!      clash with rsac's registration.
//!
//! The **mic** instrumented tier (`RsacFramesInstrumentedTest`) is untouched
//! and separate: it drives the shipped C ABI through its own
//! `librsac_ffi.so` + C shim and loads those, never this `.so`. Keep the two
//! test tiers loading disjoint library sets (see the Kotlin side).
//!
//! # Honesty
//!
//! A pass here is **emulator-verified**, never device-verified. Content is
//! never inspected — only frame counts and the negotiated format.
//!
//! - The `SystemDefault` tier proves the MediaProjection → FGS →
//!   `AudioPlaybackCapture` → `CaptureBridge` → JNI ingest → bridge → public
//!   read path delivers frames under a real app uid.
//! - The `Application` / `ApplicationByName` / `ProcessTree` tiers
//!   (`driveTargetedPlaybackCapture`) are **self-capture**: the test targets
//!   its own uid / package / pid. They verify the UID-filter PLUMBING
//!   (target → `resolve_match_uid` → `addMatchingUid` → frames from a
//!   matching-uid app) — NOT that capturing a *different* app's audio works or
//!   is correctly scoped. Cross-app UID filtering stays UNVERIFIED here.
//! - `driveEnumerateDevices` proves rsac's public device-enumeration facade
//!   returns a non-empty list through the AAR (rsac-ad8a); it inspects no
//!   audio.

// Re-export rsac so its #[no_mangle] JNI_OnLoad (and the whole Android backend
// it registers) is linked into this cdylib. Present on every target — on a
// non-Android host it is just the rsac rlib re-export and compiles to nothing
// platform-specific (mirrors mobile/android-native/src/lib.rs).
pub use rsac;

// The driver export exists only where rsac's Android playback API does. On any
// other target the crate collapses to the `pub use rsac` above, so a
// `cargo build --workspace` on macOS/Linux/Windows still compiles cleanly.
#[cfg(all(target_os = "android", feature = "feat_android"))]
mod driver {
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use jni_sys::{jint, jlong, jlongArray, jobject, jstring, JNIEnv};

    use rsac::{
        get_device_enumerator, AndroidProjectionToken, ApplicationId, AudioCaptureBuilder,
        CaptureTarget, DeviceKind, ProcessId, SampleFormat,
    };

    /// Last human-readable failure, captured at the failing stage and read
    /// back by the Kotlin side via `lastNativeError()` on the same
    /// (single-threaded instrumentation) thread. `""` when the last drive had
    /// no failure.
    fn last_error() -> &'static Mutex<String> {
        static LAST_ERROR: OnceLock<Mutex<String>> = OnceLock::new();
        LAST_ERROR.get_or_init(|| Mutex::new(String::new()))
    }

    fn set_last_error(msg: String) {
        if let Ok(mut slot) = last_error().lock() {
            *slot = msg;
        }
    }

    /// rsac `SampleFormat` → the `rsac_sample_format_t` integer encoding the
    /// mic tier's assertion uses (I16=0, I24=1, I32=2, F32=3), so the Kotlin
    /// `negSampleFormat in 0..3` sanity check is shared across both tiers.
    fn sample_format_code(fmt: SampleFormat) -> i64 {
        match fmt {
            SampleFormat::I16 => 0,
            SampleFormat::I24 => 1,
            SampleFormat::I32 => 2,
            SampleFormat::F32 => 3,
        }
    }

    /// errorCode slots (out[0]); 0 == RSAC_OK-equivalent (no hard error).
    const CODE_OK: i64 = 0;
    const CODE_BUILD_FAILED: i64 = 1;
    const CODE_START_FAILED: i64 = 2;
    const CODE_FORMAT_NONE: i64 = 3;
    const CODE_READ_ERROR: i64 = 4;

    /// Drives one playback-capture `target` through rsac's PUBLIC API
    /// end-to-end, mirroring `tests/android_emu_smoke.rs`:
    ///
    /// `from_raw(token) → with_target(target) → with_android_projection →
    /// sample_rate → channels → build() → start() → format() → bounded
    /// read_buffer() poll (count frames, never content) → request_stop()`.
    ///
    /// The `target` is the ONLY thing that varies across the tiers
    /// (`SystemDefault` vs the `Application`/`ApplicationByName`/`ProcessTree`
    /// UID-filtered variants); the token lifecycle, format-sanity, poll, and
    /// teardown are identical, so both driver exports funnel through here.
    ///
    /// Returns `[errorCode, buffers, frames, negRate, negChannels,
    /// negSampleFormat]`. On a build/start/format failure the negotiated
    /// fields stay 0/0/-1 and `last_error()` carries the `AudioError` text.
    ///
    /// Token economics: exactly ONE `from_raw` per call, and the token is
    /// moved into `build()`, which consumes its single-owner deletion latch
    /// exactly once (rsac-3407). Every early-return arm below has already
    /// handed the token to `build()` (which owns release-on-drop on success,
    /// or reclaims/re-arms it on its own failure path), so no arm here leaks
    /// or double-consumes.
    fn drive(
        token_raw: i64,
        target: CaptureTarget,
        sample_rate: i32,
        channels: i32,
        timeout_ms: i32,
    ) -> [i64; 6] {
        let mut out: [i64; 6] = [CODE_OK, 0, 0, 0, 0, -1];
        set_last_error(String::new());

        // SAFETY: `token_raw` is the opaque jlong minted by
        // RsacProjection.nativeRetainProjection (rsac's JNI_OnLoad registered
        // it in THIS librsac.so) and is wrapped exactly once here — the token
        // is handed to a single build()/capture that owns its release on drop.
        // A 0 handle is caught by create_playback_capture with an actionable
        // error rather than dereferenced (see from_raw's contract).
        let token = unsafe { AndroidProjectionToken::from_raw(token_raw) };

        let mut capture = match AudioCaptureBuilder::new()
            .with_target(target)
            .with_android_projection(token)
            .sample_rate(sample_rate.max(0) as u32)
            .channels(channels.max(0) as u16)
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                set_last_error(format!("build(): {e}"));
                out[0] = CODE_BUILD_FAILED;
                return out;
            }
        };

        if let Err(e) = capture.start() {
            set_last_error(format!("start(): {e}"));
            out[0] = CODE_START_FAILED;
            return out;
        }

        // Negotiated format: for Android playback the CaptureBridge builds its
        // AudioRecord with the requested rate/channels in PCM_FLOAT (no
        // renegotiation), so this is the delivered format published before the
        // first push. None after a successful start is a real defect.
        match capture.format() {
            Some(fmt) => {
                out[3] = i64::from(fmt.sample_rate);
                out[4] = i64::from(fmt.channels);
                out[5] = sample_format_code(fmt.sample_format);
            }
            None => {
                set_last_error("format() returned None after start()".to_string());
                capture.request_stop();
                out[0] = CODE_FORMAT_NONE;
                return out;
            }
        }

        // Bounded non-blocking poll (Ok(None) => no data yet) so a silent /
        // blocked route cannot park past the deadline. Emulator playback is
        // synthetic: count frames, never inspect content. Same
        // buffers>=3 && frames>0 stop condition as the mic tier + the smoke.
        let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(0) as u64);
        let mut buffers: i64 = 0;
        let mut frames: i64 = 0;
        while Instant::now() < deadline && !(buffers >= 3 && frames > 0) {
            match capture.read_buffer() {
                Ok(Some(buf)) => {
                    let n = buf.num_frames();
                    if n > 0 {
                        buffers += 1;
                        frames += n as i64;
                    }
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => {
                    set_last_error(format!("read_buffer(): {e}"));
                    out[0] = CODE_READ_ERROR;
                    break;
                }
            }
        }
        out[1] = buffers;
        out[2] = frames;

        // Stop, then let Drop free the projection GlobalRef + bridge (the
        // teardown choke point). request_stop is the graceful producer
        // terminal; dropping `capture` releases the token per its one-session
        // contract.
        capture.request_stop();
        out
    }

    /// `NativePlaybackDriver.drivePlaybackCapture` — JNI signature `(JIII)[J`.
    ///
    /// Resolved by the JVM's standard lazy `Java_*` lookup after
    /// `System.loadLibrary("rsac")`; no `RegisterNatives` (that is rsac's
    /// `JNI_OnLoad`'s job for the shipped natives).
    ///
    /// Never lets a panic cross the JNI frame (UB): the whole body is
    /// `catch_unwind`-contained. Returns a fresh `long[6]`, or `null` only if
    /// the JVM array allocation itself fails (OOME already pending).
    #[no_mangle]
    pub extern "system" fn Java_ai_codeseys_rsac_NativePlaybackDriver_drivePlaybackCapture(
        env: *mut JNIEnv,
        _thiz: jobject,
        token_raw: jlong,
        sample_rate: jint,
        channels: jint,
        timeout_ms: jint,
    ) -> jlongArray {
        let out = catch_unwind(AssertUnwindSafe(|| {
            drive(
                token_raw,
                CaptureTarget::SystemDefault,
                sample_rate,
                channels,
                timeout_ms,
            )
        }))
        .unwrap_or_else(|_| {
            set_last_error("driver panicked (contained at the JNI boundary)".to_string());
            [-1, 0, 0, 0, 0, -1]
        });

        // SAFETY: `env` is a valid JNIEnv for this instrumentation thread. The
        // vtable is fully populated on ART; a missing entry means the process
        // is unrecoverably broken (contained by the catch_unwind above only on
        // the Rust-entered `drive`, so guard the raw calls explicitly).
        unsafe { new_long_array_6(env, &out) }
    }

    /// The `kind` discriminant of `driveTargetedPlaybackCapture` → the rsac
    /// [`CaptureTarget`] variant, resolving `arg` per the mission contract:
    ///
    /// | kind | target | `arg` |
    /// |---|---|---|
    /// | 0 | `SystemDefault` | ignored |
    /// | 1 | `Application(uid)` | NUMERIC app UID string (ADR-0013) |
    /// | 2 | `ApplicationByName(package)` | package name |
    /// | 3 | `ProcessTree(pid)` | decimal PID string |
    ///
    /// A pid that does not parse as `u32` (kind 3) is reported via
    /// `last_error()` and `None` — the caller turns that into a build-failed
    /// code without ever wrapping the token. `Application`/`ApplicationByName`
    /// carry `arg` verbatim (rsac's `resolve_match_uid` validates the UID
    /// string / resolves the package), so an empty/garbage value surfaces as a
    /// real `AudioError` from `build()` rather than being pre-judged here.
    fn target_for(kind: i32, arg: String) -> Option<CaptureTarget> {
        match kind {
            0 => Some(CaptureTarget::SystemDefault),
            1 => Some(CaptureTarget::Application(ApplicationId(arg))),
            2 => Some(CaptureTarget::ApplicationByName(arg)),
            3 => match arg.trim().parse::<u32>() {
                Ok(pid) => Some(CaptureTarget::ProcessTree(ProcessId(pid))),
                Err(e) => {
                    set_last_error(format!("ProcessTree arg {:?} is not a u32 pid: {e}", arg));
                    None
                }
            },
            other => {
                set_last_error(format!(
                    "unknown targeted-drive kind {other} (expected 0=SystemDefault, \
                     1=Application, 2=ApplicationByName, 3=ProcessTree)"
                ));
                None
            }
        }
    }

    /// Allocates a fresh Java `long[6]` and fills it from `out`, returning the
    /// array (or `null` only when the JVM allocation itself fails, OOME
    /// already pending). Shared by both driver exports so the JNI array
    /// handoff lives in one place.
    ///
    /// # Safety
    ///
    /// `env` must be a valid `JNIEnv` for the current thread.
    unsafe fn new_long_array_6(env: *mut JNIEnv, out: &[i64; 6]) -> jlongArray {
        // SAFETY: the vtable is fully populated on ART; a missing entry means
        // the process is unrecoverably broken.
        unsafe {
            let new_long_array = (**env)
                .NewLongArray
                .expect("JNI vtable missing NewLongArray");
            let arr = new_long_array(env, 6);
            if arr.is_null() {
                return arr; // OutOfMemoryError already pending on the JVM side.
            }
            let set_region = (**env)
                .SetLongArrayRegion
                .expect("JNI vtable missing SetLongArrayRegion");
            set_region(env, arr, 0, 6, out.as_ptr());
            arr
        }
    }

    /// `NativePlaybackDriver.driveTargetedPlaybackCapture` — JNI signature
    /// `(JILjava/lang/String;III)[J`.
    ///
    /// The UID-filtered-tier twin of [`drivePlaybackCapture`](
    /// Java_ai_codeseys_rsac_NativePlaybackDriver_drivePlaybackCapture): same
    /// `long[6]` slot contract, same token economics (one `from_raw`, one
    /// consume via `build()`), but the [`CaptureTarget`] is selected by `kind`
    /// + `arg` (see [`target_for`]).
    ///
    /// # HONESTY (load-bearing)
    ///
    /// This is a **self-capture** drive: the test APK targets its OWN uid /
    /// package / pid. A pass therefore verifies the UID-filter PLUMBING —
    /// target → `resolve_match_uid` → `addMatchingUid(matchUid)` → frames
    /// delivered from an app whose uid MATCHES the filter. It does NOT verify
    /// that capturing a DIFFERENT app's audio works or is correctly scoped;
    /// cross-app UID filtering stays unverified (see the Kotlin test header).
    ///
    /// Resolved by the JVM's standard lazy `Java_*` lookup after
    /// `System.loadLibrary("rsac")`; no `RegisterNatives` (that is rsac's
    /// `JNI_OnLoad`'s job for the shipped natives). Never lets a panic cross
    /// the JNI frame. Returns a fresh `long[6]`, or `null` only if the JVM
    /// array allocation itself fails.
    #[no_mangle]
    pub extern "system" fn Java_ai_codeseys_rsac_NativePlaybackDriver_driveTargetedPlaybackCapture(
        env: *mut JNIEnv,
        _thiz: jobject,
        token_raw: jlong,
        kind: jint,
        arg: jstring,
        sample_rate: jint,
        channels: jint,
        timeout_ms: jint,
    ) -> jlongArray {
        // SAFETY: `env` is valid for this instrumentation thread; `arg` is a
        // live java.lang.String local ref (or null, which decodes to "").
        let arg_str = unsafe { jstring_to_string(env, arg) };

        let out = catch_unwind(AssertUnwindSafe(|| match target_for(kind, arg_str) {
            Some(target) => drive(token_raw, target, sample_rate, channels, timeout_ms),
            // Bad kind / unparseable pid: last_error() already set by
            // target_for. The token was NEVER wrapped, so nothing to release.
            None => [CODE_BUILD_FAILED, 0, 0, 0, 0, -1],
        }))
        .unwrap_or_else(|_| {
            set_last_error("driver panicked (contained at the JNI boundary)".to_string());
            [-1, 0, 0, 0, 0, -1]
        });

        // SAFETY: `env` valid for this thread (as above).
        unsafe { new_long_array_6(env, &out) }
    }

    /// Decodes a `java.lang.String` local ref into an owned `String`, or `""`
    /// on a null ref or a failed JVM copy. Uses `GetStringUTFChars` (the
    /// driver's `arg`s are ASCII: a numeric uid/pid or a package name), and
    /// mirrors rsac's own defensive `env`-vtable handling.
    ///
    /// # Safety
    ///
    /// `env` must be a valid `JNIEnv` for the current thread and `jstr` either
    /// null or a live `java.lang.String` local ref.
    unsafe fn jstring_to_string(env: *mut JNIEnv, jstr: jstring) -> String {
        if jstr.is_null() {
            return String::new();
        }
        // SAFETY: vtable populated on ART; `jstr` is a live String ref.
        unsafe {
            let get_utf = (**env)
                .GetStringUTFChars
                .expect("JNI vtable missing GetStringUTFChars");
            let chars = get_utf(env, jstr, std::ptr::null_mut());
            if chars.is_null() {
                return String::new();
            }
            let text = std::ffi::CStr::from_ptr(chars)
                .to_string_lossy()
                .into_owned();
            let release = (**env)
                .ReleaseStringUTFChars
                .expect("JNI vtable missing ReleaseStringUTFChars");
            release(env, jstr, chars);
            text
        }
    }

    /// `NativePlaybackDriver.lastNativeError` — JNI signature
    /// `()Ljava/lang/String;`. The `AudioError` text captured at the last
    /// failing stage, or `""`.
    #[no_mangle]
    pub extern "system" fn Java_ai_codeseys_rsac_NativePlaybackDriver_lastNativeError(
        env: *mut JNIEnv,
        _thiz: jobject,
    ) -> jstring {
        let msg = last_error().lock().map(|s| s.clone()).unwrap_or_default();
        // NewStringUTF needs a NUL-terminated string; drop any interior NULs
        // (an AudioError message never contains one, but be defensive).
        let c = std::ffi::CString::new(msg.replace('\0', "")).unwrap_or_default();
        // SAFETY: `env` valid for this thread; `c` is a live NUL-terminated
        // buffer for the duration of the call.
        unsafe {
            let new_string_utf = (**env)
                .NewStringUTF
                .expect("JNI vtable missing NewStringUTF");
            new_string_utf(env, c.as_ptr())
        }
    }

    /// `NativePlaybackDriver.driveEnumerateDevices` — JNI signature
    /// `()Ljava/lang/String;`.
    ///
    /// Drives rsac's PUBLIC device-enumeration facade
    /// (`get_device_enumerator()?.enumerate_devices()`) — the exact path a
    /// consumer app uses — and returns a parseable summary the Kotlin side
    /// asserts on (rsac-ad8a):
    ///
    /// ```text
    /// count=<N>;<id>|<name>|<Input|Output>;<id>|<name>|<Input|Output>;…
    /// ```
    ///
    /// or `ERROR: <text>` on failure. The delimiters (`;` between records,
    /// `|` within a record) are stripped from any name so the grammar can't be
    /// broken by a device label.
    ///
    /// # How the Context reaches Rust (load-bearing)
    ///
    /// `AndroidDeviceEnumerator::enumerate_devices` obtains its `Context`
    /// entirely inside rsac's JNI layer via
    /// `ActivityThread.currentApplication()` (see
    /// `src/audio/android/jni.rs::enumerate_input_device_records`) — NO
    /// context-publication step is required from the caller. That call
    /// succeeds once `librsac.so`'s `JNI_OnLoad` has run (which
    /// `System.loadLibrary("rsac")` in `NativePlaybackDriver`'s initializer
    /// already did) AND an `Application` object exists — which it always does
    /// inside an instrumented test process. So this export needs no token, no
    /// projection, and no extra wiring beyond the library already being loaded.
    ///
    /// If the AAR `RsacDevices` class did not resolve at load, enumeration
    /// silently falls back to `[default-route sentinel, playback-capture]`
    /// (2 devices) — still a valid, honest result, just without the real
    /// input-device middle section.
    #[no_mangle]
    pub extern "system" fn Java_ai_codeseys_rsac_NativePlaybackDriver_driveEnumerateDevices(
        env: *mut JNIEnv,
        _thiz: jobject,
    ) -> jstring {
        let summary =
            catch_unwind(AssertUnwindSafe(enumerate_devices_summary)).unwrap_or_else(|_| {
                "ERROR: enumeration panicked (contained at the JNI boundary)".to_string()
            });

        let c = std::ffi::CString::new(summary.replace('\0', "")).unwrap_or_default();
        // SAFETY: `env` valid for this thread; `c` is a live NUL-terminated
        // buffer for the duration of the call.
        unsafe {
            let new_string_utf = (**env)
                .NewStringUTF
                .expect("JNI vtable missing NewStringUTF");
            new_string_utf(env, c.as_ptr())
        }
    }

    /// Renders rsac's enumerated devices into the flat summary string the
    /// Kotlin `devicesEnumerated` test parses. Never panics (the caller also
    /// `catch_unwind`s); enumeration errors become `ERROR: <text>`.
    fn enumerate_devices_summary() -> String {
        let enumerator = match get_device_enumerator() {
            Ok(e) => e,
            Err(e) => return format!("ERROR: get_device_enumerator: {e}"),
        };
        let devices = match enumerator.enumerate_devices() {
            Ok(d) => d,
            Err(e) => return format!("ERROR: enumerate_devices: {e}"),
        };

        let mut summary = format!("count={}", devices.len());
        for device in &devices {
            let kind = match device.kind() {
                Ok(DeviceKind::Input) => "Input",
                Ok(DeviceKind::Output) => "Output",
                Err(_) => "Unknown",
            };
            summary.push(';');
            summary.push_str(&sanitize_field(&device.id().0));
            summary.push('|');
            summary.push_str(&sanitize_field(&device.name()));
            summary.push('|');
            summary.push_str(kind);
        }
        summary
    }

    /// Strips the summary's structural delimiters (`;` `|`) from a field so a
    /// device id/name can never break the Kotlin parser's grammar. Names are
    /// otherwise arbitrary product strings.
    fn sanitize_field(s: &str) -> String {
        s.replace([';', '|'], " ")
    }

    // ── Compile-time signature guards ────────────────────────────────────
    // Pin the exported fn pointers to their JNI-expected shapes so a
    // parameter drift is a compile error, mirroring jni.rs's _NATIVE_* asserts.
    const _DRIVE: extern "system" fn(*mut JNIEnv, jobject, jlong, jint, jint, jint) -> jlongArray =
        Java_ai_codeseys_rsac_NativePlaybackDriver_drivePlaybackCapture;
    const _DRIVE_TARGETED: extern "system" fn(
        *mut JNIEnv,
        jobject,
        jlong,
        jint,
        jstring,
        jint,
        jint,
        jint,
    ) -> jlongArray = Java_ai_codeseys_rsac_NativePlaybackDriver_driveTargetedPlaybackCapture;
    const _DRIVE_ENUMERATE: extern "system" fn(*mut JNIEnv, jobject) -> jstring =
        Java_ai_codeseys_rsac_NativePlaybackDriver_driveEnumerateDevices;
    const _LAST_ERROR: extern "system" fn(*mut JNIEnv, jobject) -> jstring =
        Java_ai_codeseys_rsac_NativePlaybackDriver_lastNativeError;

    #[allow(dead_code)]
    fn _assert_guards_referenced() {
        let _ = _DRIVE;
        let _ = _DRIVE_TARGETED;
        let _ = _DRIVE_ENUMERATE;
        let _ = _LAST_ERROR;
    }
}
