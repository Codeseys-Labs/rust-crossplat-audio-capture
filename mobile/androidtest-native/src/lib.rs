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
//! A pass here is **emulator-verified** for the `SystemDefault` playback tier
//! only. It proves the MediaProjection → FGS → `AudioPlaybackCapture` →
//! `CaptureBridge` → JNI ingest → bridge → public read path delivers frames
//! under a real app uid. It says NOTHING about the
//! `Application`/`ApplicationByName`/`ProcessTree` UID-filtered tiers, and it
//! is never device-verified. Content is never inspected — only frame counts
//! and the negotiated format.

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

    use rsac::{AndroidProjectionToken, AudioCaptureBuilder, CaptureTarget, SampleFormat};

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

    /// Drives `CaptureTarget::SystemDefault` playback capture through rsac's
    /// PUBLIC API end-to-end, mirroring `tests/android_emu_smoke.rs`:
    ///
    /// `from_raw(token) → with_target(SystemDefault) → with_android_projection
    /// → sample_rate → channels → build() → start() → format() → bounded
    /// read_buffer() poll (count frames, never content) → request_stop()`.
    ///
    /// Returns `[errorCode, buffers, frames, negRate, negChannels,
    /// negSampleFormat]`. On a build/start/format failure the negotiated
    /// fields stay 0/0/-1 and `last_error()` carries the `AudioError` text.
    fn drive(token_raw: i64, sample_rate: i32, channels: i32, timeout_ms: i32) -> [i64; 6] {
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
            .with_target(CaptureTarget::SystemDefault)
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
            drive(token_raw, sample_rate, channels, timeout_ms)
        }))
        .unwrap_or_else(|_| {
            set_last_error("driver panicked (contained at the JNI boundary)".to_string());
            [-1, 0, 0, 0, 0, -1]
        });

        // SAFETY: `env` is a valid JNIEnv for this instrumentation thread. The
        // vtable is fully populated on ART; a missing entry means the process
        // is unrecoverably broken (contained by the catch_unwind above only on
        // the Rust-entered `drive`, so guard the raw calls explicitly).
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

    // ── Compile-time signature guards ────────────────────────────────────
    // Pin the exported fn pointers to their JNI-expected shapes so a
    // parameter drift is a compile error, mirroring jni.rs's _NATIVE_* asserts.
    const _DRIVE: extern "system" fn(*mut JNIEnv, jobject, jlong, jint, jint, jint) -> jlongArray =
        Java_ai_codeseys_rsac_NativePlaybackDriver_drivePlaybackCapture;
    const _LAST_ERROR: extern "system" fn(*mut JNIEnv, jobject) -> jstring =
        Java_ai_codeseys_rsac_NativePlaybackDriver_lastNativeError;

    #[allow(dead_code)]
    fn _assert_guards_referenced() {
        let _ = _DRIVE;
        let _ = _LAST_ERROR;
    }
}
