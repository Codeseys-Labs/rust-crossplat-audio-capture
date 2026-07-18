//! JNI layer for the Android playback-capture path (rsac-77f1).
//!
//! The playback-capture tiers (`SystemDefault`, `Application*`,
//! `ProcessTree`) ride `AudioPlaybackCaptureConfiguration`, which can only
//! be attached to a **Java** `AudioRecord` — so the capture loop lives in
//! the rsac AAR (`mobile/android`, `CaptureBridge.kt`) and this module is
//! the boundary between that Java loop and the Rust bridge:
//!
//! ```text
//! Kotlin (AAR)                              Rust (this module)
//! ────────────                              ──────────────────
//! CaptureBridge.readLoop()
//!   audioRecord.read(FloatArray)
//!   nativePush(session, buf, …)   ────────► native_push():
//!                                             GetFloatArrayRegion into the
//!                                             session-lifetime scratch,
//!                                             push_samples_guarded_stamped()
//!   loop exits (error / stop)     ────────► native_session_ended():
//!                                             spontaneous ⇒ terminal Error
//!                                             (ADR-0010 / ADR-0003)
//! RsacProjection consent flow     ────────► native_retain_projection():
//!                                             NewGlobalRef ⇒ opaque token
//! ```
//!
//! # Dependency decision: `jni-sys` (raw), not the `jni` crate, not in-tree
//!
//! This module calls ~15 JNI functions. The full `jni` crate would add
//! cesu8/combine/thiserror for conveniences we don't need; hand-transcribing
//! the 233-entry `JNIEnv` vtable in-tree (the `aaudio.rs` convention) is the
//! one place where the in-tree-FFI convention *inverts* its own rationale —
//! a single misplaced vtable field silently calls the wrong JVM function
//! (UB with no compiler diagnostic). `jni-sys` is the canonical,
//! declarations-only transcription (no transitive deps, no build.rs). See
//! the manifest note next to the dependency.
//!
//! # Session registry (why tokens are ids, not pointers)
//!
//! `CaptureBridge.stop()` joins its read thread with a **bounded** timeout —
//! it cannot *prove* no `nativePush` is still in flight when it returns. A
//! raw `Box`-pointer session token (the aaudio callback-context pattern)
//! would therefore be a use-after-free hazard on the reclaim path. Instead
//! the session token is a **registry id**: `native_push` looks the id up in
//! a process-global map and clones the `Arc<IngestSession>` — a late call
//! after unregistration finds nothing (no-op), and an in-flight call keeps
//! the session alive via its `Arc` until the push completes. Ids come from a
//! monotonically increasing counter, so ABA reuse is impossible.
//!
//! The registry `Mutex` + per-session `Mutex` are acceptable here because
//! the Java capture thread is **not** an OS real-time callback thread
//! (`AudioRecord.read` is a buffered blocking read — the ADR-0001 adaptation
//! recorded in `docs/MOBILE_BACKEND_DESIGN.md`). The no-alloc rule *does*
//! bind: the copy target is the session-lifetime scratch buffer, sized once
//! at registration and never grown in `native_push` (an oversized period is
//! dropped + counted, exactly like the AAudio callback's discipline).
//!
//! # Class caching (why `FindClass` only happens in `JNI_OnLoad`)
//!
//! `FindClass` on a Rust-attached thread uses the *system* class loader,
//! which cannot see the host app's (or the AAR's) classes — a classic JNI
//! trap. `JNI_OnLoad` runs with the class loader that called
//! `System.loadLibrary` (the app loader, via `RsacProjection`), so every
//! class this module ever needs is resolved there and cached as a
//! `GlobalRef`; method ids are cached alongside (valid for the class's
//! lifetime per the JNI spec). If the AAR classes are absent — an NDK-only
//! consumer that ships `librsac.so` without the Kotlin glue — the cache
//! records that honestly and the playback factory fails with guidance,
//! while the mic slice keeps working.
//!
//! # Lockstep contract (CI-guarded)
//!
//! The method names + signatures registered here must match the `external
//! fun` declarations in `CaptureBridge.kt` / `RsacProjection.kt`. The
//! host-run drift guard in `src/audio/mod.rs`
//! (`jni_lockstep` tests) asserts both sides of the contract from the
//! source text, so a rename on either side fails `cargo test --lib` on
//! every platform.

#![cfg(all(target_os = "android", feature = "feat_android"))]

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::os::raw::c_char;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use jni_sys::{
    jclass, jfloatArray, jint, jlong, jmethodID, jobject, jstring, jvalue, JNIEnv, JNINativeMethod,
    JavaVM, JNI_EDETACHED, JNI_ERR, JNI_OK, JNI_VERSION_1_6,
};

use crate::bridge::ring_buffer::{BridgeProducer, BridgeShared};
use crate::bridge::state::StreamState;
use crate::core::error::{AudioError, AudioResult};
use crate::core::interface::{DeviceEventHandler, DeviceWatcher};

use super::thread::count_external_drop;

// ── JNI vtable access ────────────────────────────────────────────────────

/// Calls a `JNIEnv` vtable entry by name.
///
/// Every entry in `jni-sys` is an `Option<fn>`; on ART the table is fully
/// populated, so a `None` here means the process is unrecoverably broken —
/// the `expect` panic is contained by the `catch_unwind` at every
/// JVM-entered boundary and surfaces as an error on Rust-entered paths.
macro_rules! jni_call {
    ($env:expr, $name:ident $(, $arg:expr)*) => {
        ((**$env).$name.expect(concat!("JNI vtable missing ", stringify!($name))))(
            $env $(, $arg)*
        )
    };
}

/// Decodes a `java.lang.String` local ref into a Rust `String`.
///
/// Uses `GetStringChars` (UTF-16), **not** `GetStringUTFChars`: on ART the
/// latter returns *modified UTF-8* (CESU-8), in which supplementary-plane
/// code points (emoji, rare CJK) are encoded as 6-byte surrogate-pair
/// sequences that are invalid standard UTF-8 — `CStr::to_string_lossy` would
/// mangle them to U+FFFD. UTF-16 + [`String::from_utf16_lossy`] round-trips
/// the full BMP + supplementary planes. Returns `None` if the JVM copy
/// fails.
///
/// # Safety
///
/// `env` must be a valid `JNIEnv` for the current thread and `jstr` a live
/// `java.lang.String` local ref. The chars are released before returning.
unsafe fn jstring_to_string(env: *mut JNIEnv, jstr: jstring) -> Option<String> {
    // SAFETY: `jstr` is a live java.lang.String ref; chars are released
    // before this function returns.
    let chars = unsafe { jni_call!(env, GetStringChars, jstr, ptr::null_mut()) };
    if chars.is_null() {
        return None;
    }
    let len = unsafe { jni_call!(env, GetStringLength, jstr) };
    // SAFETY: `chars` points to `len` valid `jchar` (u16) UTF-16 code units
    // owned by the JVM until released; `len` is non-negative from a live
    // String.
    let units = unsafe { std::slice::from_raw_parts(chars, len.max(0) as usize) };
    let text = String::from_utf16_lossy(units);
    unsafe { jni_call!(env, ReleaseStringChars, jstr, chars) };
    Some(text)
}

// ── Cached JavaVM ────────────────────────────────────────────────────────

/// The process-global `JavaVM`, cached by [`JNI_OnLoad`].
///
/// Null until the library is loaded via `System.loadLibrary` from Java. A
/// pure-NDK process (no ART) never runs `JNI_OnLoad`, and every entry point
/// in this module fails softly in that state.
static JAVA_VM: AtomicPtr<JavaVM> = AtomicPtr::new(ptr::null_mut());

/// Returns the cached `JavaVM`, or an actionable error when the library was
/// never loaded through Java.
fn java_vm() -> AudioResult<*mut JavaVM> {
    let vm = JAVA_VM.load(Ordering::Acquire);
    if vm.is_null() {
        return Err(AudioError::StreamCreationFailed {
            reason: "JNI is not initialized: librsac.so was not loaded through \
                     Java (System.loadLibrary), so no JavaVM is available. \
                     Android playback capture requires the rsac AAR \
                     (mobile/android) to load the native library — see \
                     mobile/android/README.md"
                .to_string(),
            context: None,
        });
    }
    Ok(vm)
}

// ── Class / method-id cache ──────────────────────────────────────────────

/// Framework + AAR classes and method ids resolved once in [`JNI_OnLoad`].
///
/// `jclass` fields are JNI **GlobalRefs** (process-wide, valid on any
/// thread, never freed — the cache lives for the process). `jmethodID`s are
/// valid as long as their class is not unloaded, which the GlobalRefs
/// prevent.
pub(super) struct JniCache {
    /// `java/lang/Integer` GlobalRef. Never read after `JNI_OnLoad` — held
    /// (like the other `_`-prefixed class refs) purely to pin the class
    /// loaded so the cached method ids stay valid for the process lifetime.
    pub _integer_class: jclass,
    /// `Integer.intValue()I`.
    pub integer_int_value: jmethodID,
    /// `android/app/ActivityThread` (GlobalRef) — application-context lookup.
    pub activity_thread_class: jclass,
    /// `ActivityThread.currentApplication()Landroid/app/Application;` (static).
    pub current_application: jmethodID,
    /// `android/media/projection/MediaProjection` GlobalRef (validity anchor).
    pub _projection_class: jclass,
    /// `MediaProjection.stop()V`.
    pub projection_stop: jmethodID,
    /// `java/lang/Throwable` GlobalRef (validity anchor).
    pub _throwable_class: jclass,
    /// `Throwable.toString()Ljava/lang/String;`.
    pub throwable_to_string: jmethodID,
    /// The AAR half — `None` when the rsac AAR classes are not on the
    /// application class path (NDK-only consumer). Playback capture is then
    /// unavailable (actionable error); the mic slice is unaffected.
    pub aar: Option<AarCache>,
}

/// The `ai.codeseys.rsac` (AAR) classes + method ids.
pub(super) struct AarCache {
    /// `ai/codeseys/rsac/CaptureBridge` (GlobalRef).
    pub capture_bridge_class: jclass,
    /// `CaptureBridge.<init>(Landroid/media/projection/MediaProjection;JIIII)V`.
    pub bridge_ctor: jmethodID,
    /// `CaptureBridge.start()V`.
    pub bridge_start: jmethodID,
    /// `CaptureBridge.stop()V`.
    pub bridge_stop: jmethodID,
    /// `ai/codeseys/rsac/RsacCaptureService` (GlobalRef).
    pub service_class: jclass,
    /// `RsacCaptureService.registerBridge(Lai/codeseys/rsac/CaptureBridge;)V` (static).
    pub service_register_bridge: jmethodID,
    /// `RsacCaptureService.unregisterBridge(Lai/codeseys/rsac/CaptureBridge;)V` (static).
    pub service_unregister_bridge: jmethodID,
    /// `ai/codeseys/rsac/PackageResolver` (GlobalRef).
    pub resolver_class: jclass,
    /// `PackageResolver.uidForPackage(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/Integer;` (static).
    pub resolver_uid_for_package: jmethodID,
    /// The device-enumeration half — `None` when `RsacDevices` is absent
    /// (an older AAR paired with a newer `librsac.so`). Kept optional so a
    /// missing enumeration class degrades only `enumerate_input_device_records`
    /// (and the device-selection capability), never the already-resolved
    /// playback-capture classes above.
    pub devices: Option<DevicesCache>,
}

/// The `ai.codeseys.rsac.RsacDevices` class + method ids (rsac-ad8a).
pub(super) struct DevicesCache {
    /// `ai/codeseys/rsac/RsacDevices` (GlobalRef) — input-device enumeration.
    pub class: jclass,
    /// `RsacDevices.inputDevices(Landroid/content/Context;)Ljava/lang/String;` (static).
    pub input_devices: jmethodID,
    /// The change-notification half (rsac-d3e2) — `None` when this AAR's
    /// `RsacDevices` predates `registerDeviceCallback` /
    /// `unregisterDeviceCallback`, or the `nativeDevicesChanged`
    /// `RegisterNatives` failed. Degrades change notifications only
    /// (`watch()` errors, the capability reports `false`); the
    /// `inputDevices` enumeration path (rsac-ad8a) is unaffected.
    pub callback: Option<DeviceCallbackCache>,
}

/// The `RsacDevices` change-notification method ids (rsac-d3e2).
pub(super) struct DeviceCallbackCache {
    /// `RsacDevices.registerDeviceCallback(Landroid/content/Context;J)Z` (static).
    pub register: jmethodID,
    /// `RsacDevices.unregisterDeviceCallback(J)V` (static).
    pub unregister: jmethodID,
}

// SAFETY: the raw pointers in the cache are JNI GlobalRefs and method ids —
// both are process-global and explicitly valid across threads per the JNI
// spec (GlobalRefs until deleted, which never happens for this
// process-lifetime cache; method ids while their class stays loaded, which
// the GlobalRefs guarantee). No interior mutation occurs after `JNI_OnLoad`
// publishes the cache through the `OnceLock`.
unsafe impl Send for JniCache {}
// SAFETY: see the `Send` justification — immutable after publication.
unsafe impl Sync for JniCache {}

/// The cache, populated exactly once by [`JNI_OnLoad`].
static CACHE: OnceLock<JniCache> = OnceLock::new();

/// Returns the populated cache, or an actionable error when `JNI_OnLoad`
/// never ran (library not loaded through Java).
pub(super) fn cache() -> AudioResult<&'static JniCache> {
    CACHE.get().ok_or_else(|| AudioError::StreamCreationFailed {
        reason: "JNI class cache is empty: JNI_OnLoad has not run (librsac.so \
                 was not loaded via System.loadLibrary). Load the library \
                 through the rsac AAR (RsacProjection.isNativeAvailable()) \
                 before creating a playback capture"
            .to_string(),
        context: None,
    })
}

/// Returns the AAR class cache, or an actionable error when the Kotlin glue
/// is not on the class path.
pub(super) fn aar_cache() -> AudioResult<&'static AarCache> {
    cache()?
        .aar
        .as_ref()
        .ok_or_else(|| AudioError::StreamCreationFailed {
            reason: "the rsac AAR classes (ai.codeseys.rsac.*) were not found \
                     on the application class path when librsac.so was loaded. \
                     Android playback capture is orchestrated through the AAR's \
                     CaptureBridge/RsacCaptureService (mobile/android) — add \
                     the rsac AAR to the app. Microphone capture \
                     (CaptureTarget::Device) does not need the AAR"
                .to_string(),
            context: None,
        })
}

// ── Thread attachment ────────────────────────────────────────────────────

/// A `JNIEnv` valid for the current thread, detaching on drop **only** when
/// this guard performed the attachment (a thread that was already attached
/// — e.g. a JVM thread calling into Rust — must not be detached out from
/// under its Java frames).
pub(super) struct AttachedEnv {
    env: *mut JNIEnv,
    attached_here: bool,
}

impl AttachedEnv {
    /// The raw env pointer, valid on the current thread for the guard's
    /// lifetime.
    pub(super) fn env(&self) -> *mut JNIEnv {
        self.env
    }
}

impl Drop for AttachedEnv {
    fn drop(&mut self) {
        if self.attached_here {
            if let Ok(vm) = java_vm() {
                // SAFETY: `vm` is the live process JavaVM; this thread was
                // attached by this guard and holds no Java frames above us.
                unsafe {
                    let _ = ((**vm)
                        .DetachCurrentThread
                        .expect("JNI vtable missing DetachCurrentThread"))(
                        vm
                    );
                }
            }
        }
    }
}

/// Attaches the current thread to the JVM (or reuses an existing
/// attachment) and returns the env guard.
pub(super) fn attach_current_thread() -> AudioResult<AttachedEnv> {
    let vm = java_vm()?;
    let mut env: *mut c_void = ptr::null_mut();
    // SAFETY: `vm` is the live process JavaVM; `env` is a valid out-pointer.
    let res = unsafe {
        ((**vm).GetEnv.expect("JNI vtable missing GetEnv"))(vm, &mut env, JNI_VERSION_1_6)
    };
    if res == JNI_OK && !env.is_null() {
        return Ok(AttachedEnv {
            env: env.cast::<JNIEnv>(),
            attached_here: false,
        });
    }
    if res != JNI_EDETACHED {
        return Err(AudioError::InternalError {
            message: format!("JavaVM::GetEnv failed with JNI error {}", res),
            source: None,
        });
    }
    // SAFETY: as above; null args selects the default attachment.
    let res = unsafe {
        ((**vm)
            .AttachCurrentThread
            .expect("JNI vtable missing AttachCurrentThread"))(vm, &mut env, ptr::null_mut())
    };
    if res != JNI_OK || env.is_null() {
        return Err(AudioError::InternalError {
            message: format!("JavaVM::AttachCurrentThread failed with JNI error {}", res),
            source: None,
        });
    }
    Ok(AttachedEnv {
        env: env.cast::<JNIEnv>(),
        attached_here: true,
    })
}

// ── Exception handling ───────────────────────────────────────────────────

/// Clears any pending Java exception, returning its `toString()` rendering
/// when one was pending.
///
/// # Safety
///
/// `env` must be a valid `JNIEnv` for the current thread.
pub(super) unsafe fn take_exception_message(env: *mut JNIEnv) -> Option<String> {
    // SAFETY: ExceptionOccurred/ExceptionClear are legal with a pending
    // exception (they are the query/clear primitives themselves).
    let throwable = unsafe { jni_call!(env, ExceptionOccurred) };
    if throwable.is_null() {
        return None;
    }
    unsafe { jni_call!(env, ExceptionClear) };

    let mut message = None;
    if let Some(cache) = CACHE.get() {
        // SAFETY: exception is cleared (calls are legal again); `throwable`
        // is a live local ref; the method id matches Throwable.toString().
        let jstr = unsafe {
            jni_call!(
                env,
                CallObjectMethodA,
                throwable,
                cache.throwable_to_string,
                ptr::null()
            )
        };
        // toString itself may throw — clear and fall back to the generic text.
        let nested = unsafe { jni_call!(env, ExceptionOccurred) };
        if !nested.is_null() {
            unsafe {
                jni_call!(env, ExceptionClear);
                jni_call!(env, DeleteLocalRef, nested);
            }
        } else if !jstr.is_null() {
            // SAFETY: `jstr` is a live java.lang.String local ref; the helper
            // releases the chars before returning.
            message = unsafe { jstring_to_string(env, jstr) };
        }
        if !jstr.is_null() {
            unsafe { jni_call!(env, DeleteLocalRef, jstr) };
        }
    }
    unsafe { jni_call!(env, DeleteLocalRef, throwable) };
    Some(message.unwrap_or_else(|| "a Java exception with no readable message".to_string()))
}

// ── Session registry ─────────────────────────────────────────────────────

/// Per-capture ingest state shared between the playback stream and the
/// Java-entered natives ([`native_push`] / [`native_session_ended`]).
pub(super) struct IngestSession {
    /// Producer + scratch, locked per push. Uncontended in practice: exactly
    /// one Java capture thread pushes per session.
    inner: Mutex<IngestInner>,
    /// Bridge shared state — terminal signaling from
    /// [`native_session_ended`] (ADR-0010).
    shared: Arc<BridgeShared>,
    /// Liveness flag shared with `AndroidPlaybackStream::is_active`.
    is_active: Arc<AtomicBool>,
}

struct IngestInner {
    producer: BridgeProducer,
    /// Session-lifetime copy target for `GetFloatArrayRegion` — sized once
    /// at registration, never grown in `native_push` (ADR-0001 adapted: an
    /// oversized period is dropped + counted, not allocated for).
    scratch: Vec<f32>,
}

/// The live-session registry. See the module docs for why sessions are
/// registry ids rather than raw pointers.
static SESSIONS: OnceLock<Mutex<HashMap<i64, Arc<IngestSession>>>> = OnceLock::new();

/// Monotonic session-id source (ids are never reused — ABA-proof).
static NEXT_SESSION_ID: AtomicI64 = AtomicI64::new(1);

fn sessions() -> &'static Mutex<HashMap<i64, Arc<IngestSession>>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Registers a new ingest session and returns its opaque id (the `session`
/// argument the Kotlin `CaptureBridge` passes back on every `nativePush`).
pub(super) fn register_session(
    producer: BridgeProducer,
    shared: Arc<BridgeShared>,
    is_active: Arc<AtomicBool>,
    scratch_capacity_samples: usize,
) -> i64 {
    let id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    let session = Arc::new(IngestSession {
        inner: Mutex::new(IngestInner {
            producer,
            scratch: Vec::with_capacity(scratch_capacity_samples),
        }),
        shared,
        is_active,
    });
    sessions()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(id, session);
    id
}

/// Removes a session from the registry (idempotent).
///
/// A `nativePush`/`nativeSessionEnded` racing this call either already
/// cloned the `Arc` (the session outlives the in-flight call) or finds
/// nothing (no-op) — both are safe; that is the point of the registry.
pub(super) fn unregister_session(id: i64) {
    sessions()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&id);
}

/// Looks up a live session by id.
fn find_session(id: i64) -> Option<Arc<IngestSession>> {
    sessions()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&id)
        .cloned()
}

// ── Device-watcher registry (rsac-d3e2) ──────────────────────────────────

/// Per-watch state shared between [`watch_input_devices`] and the
/// Java-entered [`native_devices_changed`].
struct DeviceWatchEntry {
    /// Handler + previous id-set, locked per callback fire. Uncontended in
    /// practice: exactly one AAR `HandlerThread` fires per watcher.
    inner: Mutex<DeviceWatchInner>,
}

struct DeviceWatchInner {
    /// The consumer's event handler (`FnMut(DeviceEvent) + Send + 'static`).
    on_event: DeviceEventHandler,
    /// The real input-device ids seen by the previous enumeration — the diff
    /// baseline (`"default"` / `"playback-capture"` are never diffed; they
    /// are always present).
    previous: HashSet<i32>,
}

/// The live-watcher registry. Same id-not-pointer discipline as [`SESSIONS`]:
/// `unregisterDeviceCallback` joins the AAR `HandlerThread` with a **bounded**
/// timeout, so it cannot *prove* no `nativeDevicesChanged` is still in flight
/// — a stale id after unregistration finds nothing (no-op), and an in-flight
/// call keeps the entry alive via its cloned `Arc`.
static WATCHERS: OnceLock<Mutex<HashMap<i64, Arc<DeviceWatchEntry>>>> = OnceLock::new();

/// Monotonic watcher-id source (ids are never reused — ABA-proof).
static NEXT_WATCHER_ID: AtomicI64 = AtomicI64::new(1);

fn watchers() -> &'static Mutex<HashMap<i64, Arc<DeviceWatchEntry>>> {
    WATCHERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Registers a new device watcher and returns its opaque id (the `handle`
/// the Kotlin `AudioDeviceCallback` passes back on every
/// `nativeDevicesChanged`).
fn register_watcher(on_event: DeviceEventHandler, initial: HashSet<i32>) -> i64 {
    let id = NEXT_WATCHER_ID.fetch_add(1, Ordering::Relaxed);
    let entry = Arc::new(DeviceWatchEntry {
        inner: Mutex::new(DeviceWatchInner {
            on_event,
            previous: initial,
        }),
    });
    watchers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(id, entry);
    id
}

/// Removes a watcher from the registry (idempotent).
fn unregister_watcher(id: i64) {
    watchers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&id);
}

/// Looks up a live watcher by id.
fn find_watcher(id: i64) -> Option<Arc<DeviceWatchEntry>> {
    watchers()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&id)
        .cloned()
}

// ── JVM-entered natives ──────────────────────────────────────────────────

/// `CaptureBridge.nativePush` — ingests one read period from the Java
/// capture loop.
///
/// Copies `frames * channels` samples out of the Java array into the
/// session-lifetime scratch (no per-call allocation) and pushes them via
/// [`BridgeProducer::push_samples_guarded_stamped`] (ring-full ⇒ drop +
/// overrun count; never blocks the Java thread). Degenerate arguments, an
/// unknown session id, or a period larger than the scratch are counted as
/// drops or ignored — this function never throws back into Java and never
/// lets a panic cross the JNI boundary.
///
/// # Safety
///
/// Invoked only by the JVM through the `RegisterNatives` registration with
/// the lockstep signature `(J[FIII)V`: `env` is valid for the current
/// thread and `buf` is a live `float[]` local reference.
unsafe extern "system" fn native_push(
    env: *mut JNIEnv,
    _class: jclass,
    session: jlong,
    buf: jfloatArray,
    frames: jint,
    channels: jint,
    sample_rate: jint,
) {
    // Unwind containment: a panic crossing a JNI frame is UB.
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if buf.is_null() || frames <= 0 || channels <= 0 || sample_rate <= 0 {
            return;
        }
        let Some(session) = find_session(session) else {
            // Unknown/stale session (normal during teardown) — drop silently.
            return;
        };
        let Some(samples) = (frames as usize).checked_mul(channels as usize) else {
            let inner = session
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            count_external_drop(&inner.producer);
            return;
        };

        // Bounds-check against the actual Java array so GetFloatArrayRegion
        // can never raise ArrayIndexOutOfBounds (which would leave a pending
        // exception on the Java capture thread).
        // SAFETY: `buf` is a live float[] reference per the JVM contract.
        let array_len = unsafe { jni_call!(env, GetArrayLength, buf) };
        if array_len < 0 || samples > array_len as usize {
            let inner = session
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            count_external_drop(&inner.producer);
            return;
        }

        let mut inner = session
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if samples > inner.scratch.capacity() {
            // Oversized period: drop + count instead of growing the scratch
            // (ADR-0001 adapted — see the module docs).
            count_external_drop(&inner.producer);
            return;
        }
        // Within capacity ⇒ resize never reallocates.
        inner.scratch.resize(samples, 0.0);
        // SAFETY: `buf` holds at least `samples` floats (checked above) and
        // the scratch holds exactly `samples` writable f32s.
        unsafe {
            jni_call!(
                env,
                GetFloatArrayRegion,
                buf,
                0,
                samples as jint,
                inner.scratch.as_mut_ptr()
            );
        }
        // A JVM error during the region copy (e.g. OOME) leaves a pending
        // exception; clear it and drop the period rather than pushing torn
        // data or throwing back into the capture loop.
        // SAFETY: env is valid; take_exception_message only clears/queries.
        if unsafe { take_exception_message(env) }.is_some() {
            count_external_drop(&inner.producer);
            return;
        }

        let IngestInner { producer, scratch } = &mut *inner;
        producer.push_samples_guarded_stamped(scratch, channels as u16, sample_rate as u32);
    }));
}

/// `CaptureBridge.nativeSessionEnded` — the Java read loop has exited.
///
/// Called exactly once per bridge when the capture thread terminates. Two
/// cases:
///
/// - **rsac-initiated stop**: `AndroidPlaybackStream` unregisters the
///   session *before* asking Kotlin to stop, so this lookup finds nothing —
///   no-op (the graceful `Running → Stopping` transition was already driven
///   by the stop path).
/// - **Spontaneous end** (projection revoked, foreground service destroyed,
///   `AudioRecord` death): the session is still registered — drive the
///   bridge to the terminal `Error` state (ADR-0010's "producer died"
///   contract) so a parked reader observes the Fatal `StreamEnded`
///   (ADR-0003) instead of hanging, and clear the liveness flag.
///
/// # Safety
///
/// Invoked only by the JVM through the `RegisterNatives` registration with
/// the lockstep signature `(J)V`.
unsafe extern "system" fn native_session_ended(_env: *mut JNIEnv, _class: jclass, session: jlong) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Some(session) = find_session(session) else {
            return;
        };
        unregister_session_arc(&session);
        session.is_active.store(false, Ordering::SeqCst);
        // Terminal poison (fatal sibling of signal_done — same tail as the
        // AAudio error callback): sticky Error + wake both reader kinds.
        session.shared.state.force_set(StreamState::Error);
        session.shared.notify_wake();
        #[cfg(feature = "async-stream")]
        session.shared.waker.wake();
        log::warn!(
            "Android playback capture ended spontaneously (projection revoked, \
             foreground service destroyed, or AudioRecord death); bridge driven \
             to terminal Error (ADR-0010)"
        );
    }));
}

/// Removes a session by identity (used by [`native_session_ended`], which
/// holds the `Arc` rather than knowing its id).
fn unregister_session_arc(session: &Arc<IngestSession>) {
    sessions()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|_, live| !Arc::ptr_eq(live, session));
}

/// `RsacProjection.nativeRetainProjection` — wraps the consent-granted
/// `MediaProjection` in a JNI `GlobalRef` and returns it as the opaque
/// [`AndroidProjectionToken`](crate::core::config::AndroidProjectionToken)
/// value.
///
/// Ownership transfers to Rust: the ref is released (`DeleteGlobalRef` +
/// `MediaProjection.stop()`) when the owning capture is dropped
/// (`AndroidPlaybackStream::stop_and_close`). Returns `0` when the ref
/// cannot be created — a `0` token later fails stream creation with an
/// actionable error rather than being interpreted.
///
/// # Safety
///
/// Invoked only by the JVM through the `RegisterNatives` registration with
/// the lockstep signature `(Landroid/media/projection/MediaProjection;)J`.
unsafe extern "system" fn native_retain_projection(
    env: *mut JNIEnv,
    _class: jclass,
    projection: jobject,
) -> jlong {
    catch_unwind(AssertUnwindSafe(|| {
        if projection.is_null() {
            return 0;
        }
        // SAFETY: `projection` is a live local ref per the JVM contract.
        let global = unsafe { jni_call!(env, NewGlobalRef, projection) };
        global as jlong
    }))
    .unwrap_or(0)
}

/// `RsacDevices.nativeDevicesChanged` — the AAR `AudioDeviceCallback` fired
/// (rsac-d3e2).
///
/// Re-enumerates the input-device list and diffs it against the watcher's
/// previous id-set ([`super::diff_device_events`]), invoking `on_event` for
/// each add/remove. Runs on the AAR's dedicated `HandlerThread` — non-RT and
/// alloc-safe, satisfying the `DeviceEventHandler` delivery contract. The
/// callback also fires for output-only topology changes; those diff to empty
/// against the input-only enumeration and emit nothing.
///
/// An enumeration failure skips the fire entirely (the previous set is kept)
/// rather than treating it as "all devices removed" — a transient Java
/// failure must not flap the consumer's device list.
///
/// # Safety
///
/// Invoked only by the JVM through the `RegisterNatives` registration with
/// the lockstep signature `(J)V`. Never panics across the JNI boundary.
unsafe extern "system" fn native_devices_changed(_env: *mut JNIEnv, _class: jclass, handle: jlong) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Some(entry) = find_watcher(handle) else {
            // Unknown/stale watcher (normal during teardown) — no-op.
            return;
        };
        // The HandlerThread is a JVM thread, so the attach guard inside
        // enumerate_input_device_records reuses the existing attachment
        // (GetEnv succeeds; no detach on drop).
        let Ok(raw) = enumerate_input_device_records() else {
            return;
        };
        let current = super::parse_input_device_records(&raw);
        let mut inner = entry
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (events, next) = super::diff_device_events(&inner.previous, &current);
        for event in events {
            (inner.on_event)(event);
        }
        inner.previous = next;
    }));
}

// ── JNI_OnLoad ───────────────────────────────────────────────────────────

/// Resolves a class as a GlobalRef, or `None` (exception cleared) when it
/// is not on the class path.
///
/// # Safety
///
/// `env` must be valid for the current thread; `name` must be a
/// NUL-terminated binary class name.
unsafe fn find_class_global(env: *mut JNIEnv, name: &'static std::ffi::CStr) -> Option<jclass> {
    // SAFETY: per the function contract.
    let local = unsafe { jni_call!(env, FindClass, name.as_ptr()) };
    if local.is_null() {
        unsafe {
            jni_call!(env, ExceptionClear);
        }
        return None;
    }
    // SAFETY: `local` is a live local ref.
    let global = unsafe { jni_call!(env, NewGlobalRef, local) };
    unsafe { jni_call!(env, DeleteLocalRef, local) };
    if global.is_null() {
        return None;
    }
    Some(global)
}

/// Resolves a (static or instance) method id, clearing the exception and
/// returning `None` on failure.
///
/// # Safety
///
/// `env` valid for the current thread; `class` a live class ref; `name`/
/// `sig` NUL-terminated.
unsafe fn get_method(
    env: *mut JNIEnv,
    class: jclass,
    name: &'static std::ffi::CStr,
    sig: &'static std::ffi::CStr,
    is_static: bool,
) -> Option<jmethodID> {
    // SAFETY: per the function contract.
    let id = unsafe {
        if is_static {
            jni_call!(env, GetStaticMethodID, class, name.as_ptr(), sig.as_ptr())
        } else {
            jni_call!(env, GetMethodID, class, name.as_ptr(), sig.as_ptr())
        }
    };
    if id.is_null() {
        unsafe {
            jni_call!(env, ExceptionClear);
        }
        return None;
    }
    Some(id)
}

/// Builds the AAR half of the cache, registering the natives on
/// `CaptureBridge` / `RsacProjection`. Returns `None` (exceptions cleared)
/// when any AAR class/method/registration is unavailable.
///
/// # Safety
///
/// `env` must be valid for the current thread and running under the
/// application class loader (i.e. called from `JNI_OnLoad`).
unsafe fn build_aar_cache(env: *mut JNIEnv) -> Option<AarCache> {
    // SAFETY: all calls below follow the find_class_global/get_method
    // contracts; env comes from JNI_OnLoad.
    unsafe {
        let capture_bridge_class = find_class_global(env, c"ai/codeseys/rsac/CaptureBridge")?;
        let bridge_ctor = get_method(
            env,
            capture_bridge_class,
            c"<init>",
            c"(Landroid/media/projection/MediaProjection;JIIII)V",
            false,
        )?;
        let bridge_start = get_method(env, capture_bridge_class, c"start", c"()V", false)?;
        let bridge_stop = get_method(env, capture_bridge_class, c"stop", c"()V", false)?;

        let service_class = find_class_global(env, c"ai/codeseys/rsac/RsacCaptureService")?;
        let service_register_bridge = get_method(
            env,
            service_class,
            c"registerBridge",
            c"(Lai/codeseys/rsac/CaptureBridge;)V",
            true,
        )?;
        let service_unregister_bridge = get_method(
            env,
            service_class,
            c"unregisterBridge",
            c"(Lai/codeseys/rsac/CaptureBridge;)V",
            true,
        )?;

        let resolver_class = find_class_global(env, c"ai/codeseys/rsac/PackageResolver")?;
        let resolver_uid_for_package = get_method(
            env,
            resolver_class,
            c"uidForPackage",
            c"(Landroid/content/Context;Ljava/lang/String;)Ljava/lang/Integer;",
            true,
        )?;

        // RsacDevices is OPTIONAL: its absence (older AAR, newer librsac.so)
        // must degrade only device enumeration, not the playback-capture
        // classes resolved above. find_class_global clears the pending
        // ClassNotFoundException on failure, so the cache build continues.
        let devices = (|| {
            let class = find_class_global(env, c"ai/codeseys/rsac/RsacDevices")?;
            let input_devices = get_method(
                env,
                class,
                c"inputDevices",
                c"(Landroid/content/Context;)Ljava/lang/String;",
                true,
            )?;
            // The change-notification half (rsac-d3e2) is itself optional
            // within RsacDevices: an AAR whose RsacDevices predates the
            // callback methods still enumerates (rsac-ad8a) but reports no
            // change notifications. get_method clears the pending
            // NoSuchMethodError on failure, so the build continues.
            let callback = (|| {
                let register = get_method(
                    env,
                    class,
                    c"registerDeviceCallback",
                    c"(Landroid/content/Context;J)Z",
                    true,
                )?;
                let unregister =
                    get_method(env, class, c"unregisterDeviceCallback", c"(J)V", true)?;
                // Register the Java→Rust native ON the RsacDevices class.
                //
                // LOCKSTEP: name + signature must match the `external fun`
                // in RsacDevices.kt — guarded by the host-run `jni_lockstep`
                // tests in src/audio/mod.rs.
                let natives = [JNINativeMethod {
                    name: c"nativeDevicesChanged".as_ptr() as *mut c_char,
                    signature: c"(J)V".as_ptr() as *mut c_char,
                    fnPtr: native_devices_changed as *mut c_void,
                }];
                let res = jni_call!(
                    env,
                    RegisterNatives,
                    class,
                    natives.as_ptr(),
                    natives.len() as jint
                );
                if res != JNI_OK {
                    jni_call!(env, ExceptionClear);
                    return None;
                }
                Some(DeviceCallbackCache {
                    register,
                    unregister,
                })
            })();
            Some(DevicesCache {
                class,
                input_devices,
                callback,
            })
        })();

        // Natives on CaptureBridge (companion `@JvmStatic external` methods
        // compile to static natives on the enclosing class).
        //
        // LOCKSTEP: names + signatures must match the `external fun`
        // declarations in CaptureBridge.kt / RsacProjection.kt — guarded by
        // the host-run `jni_lockstep` tests in src/audio/mod.rs.
        let bridge_natives = [
            JNINativeMethod {
                name: c"nativePush".as_ptr() as *mut c_char,
                signature: c"(J[FIII)V".as_ptr() as *mut c_char,
                fnPtr: native_push as *mut c_void,
            },
            JNINativeMethod {
                name: c"nativeSessionEnded".as_ptr() as *mut c_char,
                signature: c"(J)V".as_ptr() as *mut c_char,
                fnPtr: native_session_ended as *mut c_void,
            },
        ];
        let res = jni_call!(
            env,
            RegisterNatives,
            capture_bridge_class,
            bridge_natives.as_ptr(),
            bridge_natives.len() as jint
        );
        if res != JNI_OK {
            jni_call!(env, ExceptionClear);
            return None;
        }

        let projection_helper_class = find_class_global(env, c"ai/codeseys/rsac/RsacProjection")?;
        let projection_natives = [JNINativeMethod {
            name: c"nativeRetainProjection".as_ptr() as *mut c_char,
            signature: c"(Landroid/media/projection/MediaProjection;)J".as_ptr() as *mut c_char,
            fnPtr: native_retain_projection as *mut c_void,
        }];
        let res = jni_call!(
            env,
            RegisterNatives,
            projection_helper_class,
            projection_natives.as_ptr(),
            projection_natives.len() as jint
        );
        // The helper-object class ref is only needed for registration.
        jni_call!(env, DeleteGlobalRef, projection_helper_class);
        if res != JNI_OK {
            jni_call!(env, ExceptionClear);
            return None;
        }

        Some(AarCache {
            capture_bridge_class,
            bridge_ctor,
            bridge_start,
            bridge_stop,
            service_class,
            service_register_bridge,
            service_unregister_bridge,
            resolver_class,
            resolver_uid_for_package,
            devices,
        })
    }
}

/// JVM library-load hook: caches the `JavaVM`, resolves + caches every
/// class/method this module uses (under the application class loader — see
/// the module docs), and registers the natives on the AAR classes.
///
/// Never fails the load: a missing AAR (NDK-only consumer) or even a
/// missing framework class leaves the corresponding cache half empty and
/// the playback factory failing with actionable guidance, while the AAudio
/// mic slice (which needs no JNI) keeps working.
///
/// # Safety
///
/// Called only by the JVM during `System.loadLibrary` with a valid `vm`.
#[no_mangle]
pub unsafe extern "system" fn JNI_OnLoad(vm: *mut JavaVM, _reserved: *mut c_void) -> jint {
    catch_unwind(AssertUnwindSafe(|| {
        JAVA_VM.store(vm, Ordering::Release);

        let mut env: *mut c_void = ptr::null_mut();
        // SAFETY: `vm` is the live JavaVM handed to JNI_OnLoad; `env` is a
        // valid out-pointer. JNI_OnLoad runs on an attached thread.
        let res = unsafe {
            ((**vm).GetEnv.expect("JNI vtable missing GetEnv"))(vm, &mut env, JNI_VERSION_1_6)
        };
        if res != JNI_OK || env.is_null() {
            // JNI < 1.6 has been extinct on Android forever; refuse the load.
            return JNI_ERR;
        }
        let env = env.cast::<JNIEnv>();

        // SAFETY: env is valid (above) and this is the OnLoad thread, so
        // FindClass resolves against the application class loader.
        let cache_value = unsafe {
            let integer_class = find_class_global(env, c"java/lang/Integer");
            let activity_thread_class = find_class_global(env, c"android/app/ActivityThread");
            let projection_class =
                find_class_global(env, c"android/media/projection/MediaProjection");
            let throwable_class = find_class_global(env, c"java/lang/Throwable");
            match (
                integer_class,
                activity_thread_class,
                projection_class,
                throwable_class,
            ) {
                (
                    Some(integer_class),
                    Some(activity_thread_class),
                    Some(projection_class),
                    Some(throwable_class),
                ) => {
                    let integer_int_value =
                        get_method(env, integer_class, c"intValue", c"()I", false);
                    let current_application = get_method(
                        env,
                        activity_thread_class,
                        c"currentApplication",
                        c"()Landroid/app/Application;",
                        true,
                    );
                    let projection_stop = get_method(env, projection_class, c"stop", c"()V", false);
                    let throwable_to_string = get_method(
                        env,
                        throwable_class,
                        c"toString",
                        c"()Ljava/lang/String;",
                        false,
                    );
                    match (
                        integer_int_value,
                        current_application,
                        projection_stop,
                        throwable_to_string,
                    ) {
                        (
                            Some(integer_int_value),
                            Some(current_application),
                            Some(projection_stop),
                            Some(throwable_to_string),
                        ) => Some(JniCache {
                            _integer_class: integer_class,
                            integer_int_value,
                            activity_thread_class,
                            current_application,
                            _projection_class: projection_class,
                            projection_stop,
                            _throwable_class: throwable_class,
                            throwable_to_string,
                            aar: build_aar_cache(env),
                        }),
                        _ => None,
                    }
                }
                _ => None,
            }
        };

        match cache_value {
            Some(cache) => {
                let aar_present = cache.aar.is_some();
                // Publish the device-enumeration availability to the
                // capabilities layer (core can't reach into audio/ — module
                // DAG), so supports_device_selection reports honestly
                // (rsac-ad8a).
                crate::core::capabilities::set_android_device_enumeration_available(
                    cache.aar.as_ref().is_some_and(|aar| aar.devices.is_some()),
                );
                // Same pattern for the change-notification gate (rsac-d3e2):
                // supports_device_change_notifications is true only when the
                // RsacDevices callback methods resolved AND the
                // nativeDevicesChanged RegisterNatives succeeded.
                crate::core::capabilities::set_android_device_change_notifications_available(
                    cache
                        .aar
                        .as_ref()
                        .and_then(|aar| aar.devices.as_ref())
                        .is_some_and(|d| d.callback.is_some()),
                );
                let _ = CACHE.set(cache);
                log::debug!(
                    "rsac JNI_OnLoad: class cache ready (AAR classes {})",
                    if aar_present {
                        "present — playback capture available"
                    } else {
                        "absent — playback capture unavailable, mic slice unaffected"
                    }
                );
            }
            None => {
                // Framework classes missing — leave the cache empty; every
                // playback entry point reports the actionable cache error.
                log::warn!(
                    "rsac JNI_OnLoad: could not resolve framework classes; \
                     playback capture will be unavailable"
                );
            }
        }
        JNI_VERSION_1_6
    }))
    .unwrap_or(JNI_ERR)
}

// ── Typed call wrappers (used by playback.rs) ────────────────────────────

/// Constructs, registers, and starts a Kotlin `CaptureBridge`, returning a
/// GlobalRef to it.
///
/// Mirrors the documented Kotlin lifecycle:
/// `CaptureBridge(...)` → `RsacCaptureService.registerBridge` → `start()`.
/// On any failure every partial step is rolled back (unregister + local ref
/// cleanup) and the pending Java exception is folded into the error.
pub(super) fn create_and_start_bridge(
    projection: jobject,
    session: i64,
    sample_rate: u32,
    channels: u16,
    match_uid: i32,
    frames_per_read: i32,
) -> AudioResult<jobject> {
    let aar = aar_cache()?;
    let guard = attach_current_thread()?;
    let env = guard.env();

    // ── Construct ────────────────────────────────────────────────────
    let args = [
        jvalue { l: projection },
        jvalue { j: session },
        jvalue {
            i: sample_rate.min(i32::MAX as u32) as jint,
        },
        jvalue {
            i: jint::from(channels),
        },
        jvalue { i: match_uid },
        jvalue { i: frames_per_read },
    ];
    // SAFETY: class/ctor come from the cache (valid for the process);
    // `args` matches the ctor signature (MediaProjection, long, int, int,
    // int, int); `projection` is a live GlobalRef owned by the caller.
    let bridge = unsafe {
        jni_call!(
            env,
            NewObjectA,
            aar.capture_bridge_class,
            aar.bridge_ctor,
            args.as_ptr()
        )
    };
    // SAFETY: env valid; queries/clears the pending exception only.
    if let Some(msg) = unsafe { take_exception_message(env) } {
        return Err(AudioError::StreamCreationFailed {
            reason: format!(
                "CaptureBridge construction failed: {}. Common causes: the \
                 RECORD_AUDIO runtime permission is not granted, the \
                 MediaProjection token was already consumed by another \
                 capture (one token = one session), or the mediaProjection \
                 foreground service is no longer running (API 34+ requires it \
                 foreground for the capture's lifetime; RsacProjection.request \
                 starts it — do not stop it before dropping the capture)",
                msg
            ),
            context: None,
        });
    }
    if bridge.is_null() {
        return Err(AudioError::StreamCreationFailed {
            reason: "CaptureBridge construction returned null without an exception".to_string(),
            context: None,
        });
    }

    // ── Anchor to the foreground service ─────────────────────────────
    let reg_args = [jvalue { l: bridge }];
    // SAFETY: static method on the cached service class; one object arg.
    unsafe {
        jni_call!(
            env,
            CallStaticVoidMethodA,
            aar.service_class,
            aar.service_register_bridge,
            reg_args.as_ptr()
        );
    }
    if let Some(msg) = unsafe { take_exception_message(env) } {
        // SAFETY: bridge is a live local ref.
        unsafe { jni_call!(env, DeleteLocalRef, bridge) };
        return Err(AudioError::StreamCreationFailed {
            reason: format!("RsacCaptureService.registerBridge failed: {}", msg),
            context: None,
        });
    }

    // ── Start the read loop ──────────────────────────────────────────
    // SAFETY: instance method on the live bridge object; no args.
    unsafe {
        jni_call!(env, CallVoidMethodA, bridge, aar.bridge_start, ptr::null());
    }
    if let Some(msg) = unsafe { take_exception_message(env) } {
        // Roll back the registration; ignore secondary failures.
        unsafe {
            jni_call!(
                env,
                CallStaticVoidMethodA,
                aar.service_class,
                aar.service_unregister_bridge,
                reg_args.as_ptr()
            );
            let _ = take_exception_message(env);
            jni_call!(env, DeleteLocalRef, bridge);
        }
        return Err(AudioError::StreamStartFailed {
            reason: format!(
                "CaptureBridge.start() failed: {}. Common causes: \
                 AudioRecord.startRecording() rejected (another app holds an \
                 exclusive capture, or the projection was revoked)",
                msg
            ),
        });
    }

    // ── Promote to a GlobalRef the stream can hold across threads ────
    // SAFETY: bridge is a live local ref.
    let global = unsafe { jni_call!(env, NewGlobalRef, bridge) };
    if global.is_null() {
        // NewGlobalRef can fail by *throwing* (e.g. OutOfMemoryError), and
        // JNI method calls are illegal while an exception is pending — the
        // rollback below would be skipped (or abort under CheckJNI). Clear
        // it first and fold its message into the returned error.
        // SAFETY: env valid; queries/clears the pending exception only.
        let thrown = unsafe { take_exception_message(env) };
        // Roll back: the bridge is already started and service-anchored.
        // Without this, the Java read thread + AudioRecord keep running
        // (service-pinned) while the Rust caller only unregisters the
        // ingest session — a capture-resource leak until the service dies.
        // Mirror the start-failure branch; ignore secondary failures so
        // teardown runs to completion.
        // SAFETY: bridge is a live local ref; method/class ids come from
        // the cache and match its class.
        unsafe {
            jni_call!(env, CallVoidMethodA, bridge, aar.bridge_stop, ptr::null());
            let _ = take_exception_message(env);
            jni_call!(
                env,
                CallStaticVoidMethodA,
                aar.service_class,
                aar.service_unregister_bridge,
                reg_args.as_ptr()
            );
            let _ = take_exception_message(env);
            jni_call!(env, DeleteLocalRef, bridge);
        }
        return Err(AudioError::InternalError {
            message: match thrown {
                Some(msg) => format!("NewGlobalRef failed for the CaptureBridge handle: {}", msg),
                None => "NewGlobalRef failed for the CaptureBridge handle".to_string(),
            },
            source: None,
        });
    }
    // SAFETY: bridge is a live local ref; the GlobalRef now owns the object.
    unsafe { jni_call!(env, DeleteLocalRef, bridge) };
    Ok(global)
}

/// Stops a Kotlin `CaptureBridge` (idempotent on the Kotlin side),
/// detaches it from the foreground service, and releases the GlobalRef.
///
/// Exceptions are folded into log warnings — teardown always runs to
/// completion.
pub(super) fn stop_and_release_bridge(bridge: jobject) {
    if bridge.is_null() {
        return;
    }
    let Ok(aar) = aar_cache() else { return };
    let Ok(guard) = attach_current_thread() else {
        return;
    };
    let env = guard.env();
    // SAFETY: `bridge` is the live GlobalRef produced by
    // create_and_start_bridge; the cached method ids match its class.
    unsafe {
        jni_call!(env, CallVoidMethodA, bridge, aar.bridge_stop, ptr::null());
        if let Some(msg) = take_exception_message(env) {
            log::warn!("CaptureBridge.stop() threw: {}; continuing teardown", msg);
        }
        let args = [jvalue { l: bridge }];
        jni_call!(
            env,
            CallStaticVoidMethodA,
            aar.service_class,
            aar.service_unregister_bridge,
            args.as_ptr()
        );
        if let Some(msg) = take_exception_message(env) {
            log::warn!(
                "RsacCaptureService.unregisterBridge threw: {}; continuing teardown",
                msg
            );
        }
        jni_call!(env, DeleteGlobalRef, bridge);
    }
}

/// Stops the `MediaProjection` behind a consent token and releases its
/// GlobalRef — the "one token = one projection session" release contract.
pub(super) fn stop_and_release_projection(token_raw: i64) {
    if token_raw == 0 {
        return;
    }
    let Ok(cache) = cache() else { return };
    let Ok(guard) = attach_current_thread() else {
        return;
    };
    let env = guard.env();
    let projection = token_raw as jobject;
    // SAFETY: the token is the GlobalRef minted by native_retain_projection.
    // The raw handle reaches this delete site exactly once per projection: the
    // owning stream obtained it via `AndroidProjectionToken::try_consume`
    // (config.rs), whose shared single-owner latch lets at most one stream in a
    // token's clone lineage ever hold a deletable handle — so this runs exactly
    // once, never a double `DeleteGlobalRef`. (Only a 0 token is caught — by
    // the early return above and the `as_raw() == 0` check in
    // create_playback_capture; a stale non-zero handle is NOT detectable here
    // and is excluded by `from_raw`'s unsafe contract.)
    // stop() is idempotent on MediaProjection.
    unsafe {
        jni_call!(
            env,
            CallVoidMethodA,
            projection,
            cache.projection_stop,
            ptr::null()
        );
        if let Some(msg) = take_exception_message(env) {
            log::warn!("MediaProjection.stop() threw: {}; releasing anyway", msg);
        }
        jni_call!(env, DeleteGlobalRef, projection);
    }
}

/// Resolves an installed package's UID via the AAR's `PackageResolver`
/// (`PackageManager` lookup), using `ActivityThread.currentApplication()`
/// as the context.
pub(super) fn resolve_uid_for_package(package: &str) -> AudioResult<i32> {
    let cache = cache()?;
    let aar = aar_cache()?;
    let guard = attach_current_thread()?;
    let env = guard.env();

    // ── Application context ──────────────────────────────────────────
    // SAFETY: static method on the cached ActivityThread class; no args.
    let context = unsafe {
        jni_call!(
            env,
            CallStaticObjectMethodA,
            cache.activity_thread_class,
            cache.current_application,
            ptr::null()
        )
    };
    let exception = unsafe { take_exception_message(env) };
    if context.is_null() || exception.is_some() {
        return Err(AudioError::ApplicationNotFound {
            identifier: format!(
                "{} (could not obtain an application Context via \
                 ActivityThread.currentApplication(){}; resolve the package's \
                 UID yourself and use CaptureTarget::Application(uid) instead)",
                package,
                exception.map(|m| format!(": {}", m)).unwrap_or_default()
            ),
        });
    }

    // ── Package → UID ────────────────────────────────────────────────
    let package_cstr =
        std::ffi::CString::new(package).map_err(|_| AudioError::InvalidParameter {
            param: "application name".to_string(),
            reason: "package name contains an interior NUL byte".to_string(),
        })?;
    // SAFETY: NewStringUTF takes a NUL-terminated modified-UTF-8 string;
    // plain ASCII/UTF-8 package names are valid modified UTF-8.
    let jname = unsafe { jni_call!(env, NewStringUTF, package_cstr.as_ptr()) };
    if jname.is_null() {
        unsafe {
            let _ = take_exception_message(env);
            jni_call!(env, DeleteLocalRef, context);
        }
        return Err(AudioError::InternalError {
            message: "NewStringUTF failed for the package name".to_string(),
            source: None,
        });
    }
    let args = [jvalue { l: context }, jvalue { l: jname }];
    // SAFETY: static method on the cached resolver class; (Context, String)
    // args as cached.
    let boxed_uid = unsafe {
        jni_call!(
            env,
            CallStaticObjectMethodA,
            aar.resolver_class,
            aar.resolver_uid_for_package,
            args.as_ptr()
        )
    };
    let exception = unsafe { take_exception_message(env) };
    let result = if let Some(msg) = exception {
        Err(AudioError::ApplicationNotFound {
            identifier: format!("{} (PackageResolver.uidForPackage threw: {})", package, msg),
        })
    } else if boxed_uid.is_null() {
        Err(AudioError::ApplicationNotFound {
            identifier: format!(
                "{} (no installed package with that name is visible to this \
                 app; on API 30+ package visibility filtering may require a \
                 <queries> manifest declaration)",
                package
            ),
        })
    } else {
        // SAFETY: boxed_uid is a live java.lang.Integer; intValue()I.
        let uid = unsafe {
            jni_call!(
                env,
                CallIntMethodA,
                boxed_uid,
                cache.integer_int_value,
                ptr::null()
            )
        };
        match unsafe { take_exception_message(env) } {
            Some(msg) => Err(AudioError::InternalError {
                message: format!("Integer.intValue() threw: {}", msg),
                source: None,
            }),
            None => Ok(uid),
        }
    };

    // SAFETY: live local refs from this frame.
    unsafe {
        if !boxed_uid.is_null() {
            jni_call!(env, DeleteLocalRef, boxed_uid);
        }
        jni_call!(env, DeleteLocalRef, jname);
        jni_call!(env, DeleteLocalRef, context);
    }
    result
}

/// Enumerates the current audio input devices via the AAR's
/// `RsacDevices.inputDevices`, returning the raw flat delimited record string
/// (`id␟type␟name` records joined by `␞`; see `RsacDevices.kt` for the wire
/// contract). The pure Rust parser (`super::parse_input_device_records`)
/// turns it into typed records.
///
/// Uses `ActivityThread.currentApplication()` as the `Context`, exactly like
/// [`resolve_uid_for_package`]. Any failure — no `JavaVM` (host-test /
/// pure-NDK), the AAR classes absent, a null context, or a pending Java
/// exception — returns `Err(DeviceEnumerationError)`. The sole caller
/// (`AndroidDeviceEnumerator::enumerate_devices`) treats **any** `Err` as
/// "fall back to the default-route sentinel + playback device", so this
/// never surfaces to users as a hard failure.
pub(super) fn enumerate_input_device_records() -> AudioResult<String> {
    let cache = cache()?;
    let devices =
        aar_cache()?
            .devices
            .as_ref()
            .ok_or_else(|| AudioError::DeviceEnumerationError {
                reason: "the loaded rsac AAR does not provide RsacDevices \
                     (older AAR paired with a newer librsac.so) — device \
                     enumeration falls back to the default route"
                    .to_string(),
                context: None,
            })?;
    let guard = attach_current_thread()?;
    let env = guard.env();

    // ── Application context ──────────────────────────────────────────
    // SAFETY: static method on the cached ActivityThread class; no args.
    let context = unsafe {
        jni_call!(
            env,
            CallStaticObjectMethodA,
            cache.activity_thread_class,
            cache.current_application,
            ptr::null()
        )
    };
    let exception = unsafe { take_exception_message(env) };
    if context.is_null() || exception.is_some() {
        return Err(AudioError::DeviceEnumerationError {
            reason: format!(
                "could not obtain an application Context via \
                 ActivityThread.currentApplication(){}",
                exception.map(|m| format!(": {}", m)).unwrap_or_default()
            ),
            context: None,
        });
    }

    // ── inputDevices(context) → String ───────────────────────────────
    let args = [jvalue { l: context }];
    // SAFETY: static method on the cached RsacDevices class; one Context arg.
    let jstr = unsafe {
        jni_call!(
            env,
            CallStaticObjectMethodA,
            devices.class,
            devices.input_devices,
            args.as_ptr()
        )
    };
    let exception = unsafe { take_exception_message(env) };
    let result = if let Some(msg) = exception {
        Err(AudioError::DeviceEnumerationError {
            reason: format!("RsacDevices.inputDevices threw: {}", msg),
            context: None,
        })
    } else if jstr.is_null() {
        // Kotlin returns "" on failure, never null; a null here means the
        // call did not complete normally.
        Err(AudioError::DeviceEnumerationError {
            reason: "RsacDevices.inputDevices returned null".to_string(),
            context: None,
        })
    } else {
        // SAFETY: `jstr` is a live java.lang.String local ref; the helper
        // releases the chars before returning.
        match unsafe { jstring_to_string(env, jstr) } {
            Some(text) => Ok(text),
            None => Err(AudioError::DeviceEnumerationError {
                reason: "GetStringChars failed for the device record string".to_string(),
                context: None,
            }),
        }
    };

    // SAFETY: live local refs from this frame.
    unsafe {
        if !jstr.is_null() {
            jni_call!(env, DeleteLocalRef, jstr);
        }
        jni_call!(env, DeleteLocalRef, context);
    }
    result
}

/// Subscribes to input-device change notifications via the AAR's
/// `AudioManager.registerAudioDeviceCallback` path (rsac-d3e2).
///
/// Seeds the diff baseline from a fresh enumeration **before** registering,
/// so Android's register-time immediate `onAudioDevicesAdded(current)` diffs
/// to empty — consumers receive only *changes*, never an initial-add flood
/// (a device added in the seed→register window is present in the immediate
/// fire's re-enumeration and absent from the seed, so it is still reported).
///
/// The returned watcher's teardown unregisters the Java side FIRST
/// (`RsacDevices.unregisterDeviceCallback`: `AudioManager` unregister +
/// bounded `HandlerThread` join) and only then drops the Rust entry — the
/// no-callback-after-free order. The `Arc` registry is belt-and-suspenders
/// for a timed-out join: an in-flight `nativeDevicesChanged` holds its own
/// clone and a stale id is a no-op.
pub(super) fn watch_input_devices(on_event: DeviceEventHandler) -> AudioResult<DeviceWatcher> {
    let cache = cache()?;
    let devices =
        aar_cache()?
            .devices
            .as_ref()
            .ok_or_else(|| AudioError::PlatformNotSupported {
                feature: "device change notifications".to_string(),
                platform: "android".to_string(),
            })?;
    let cb = devices
        .callback
        .as_ref()
        .ok_or_else(|| AudioError::PlatformNotSupported {
            feature: "device change notifications".to_string(),
            platform: "android".to_string(),
        })?;
    let guard = attach_current_thread()?;
    let env = guard.env();

    // ── Application context ──────────────────────────────────────────
    // SAFETY: static method on the cached ActivityThread class; no args.
    let context = unsafe {
        jni_call!(
            env,
            CallStaticObjectMethodA,
            cache.activity_thread_class,
            cache.current_application,
            ptr::null()
        )
    };
    let exception = unsafe { take_exception_message(env) };
    if context.is_null() || exception.is_some() {
        return Err(AudioError::BackendError {
            backend: "AAudio".to_string(),
            operation: "watch".to_string(),
            message: format!(
                "could not obtain an application Context via \
                 ActivityThread.currentApplication(){}",
                exception.map(|m| format!(": {}", m)).unwrap_or_default()
            ),
            context: None,
        });
    }

    // Seed `previous` from a fresh enumeration BEFORE registering, so the
    // register-time immediate onAudioDevicesAdded diffs to empty. A failed
    // seed enumeration degrades to an empty baseline (the immediate fire
    // then reports the current devices as adds — still correct data, just
    // without the flood suppression).
    let initial: HashSet<i32> = enumerate_input_device_records()
        .ok()
        .map(|raw| {
            super::parse_input_device_records(&raw)
                .iter()
                .map(|d| d.id)
                .collect()
        })
        .unwrap_or_default();
    let handle = register_watcher(on_event, initial);

    // ── RsacDevices.registerDeviceCallback(context, handle) ─────────
    let args = [jvalue { l: context }, jvalue { j: handle }];
    // SAFETY: static method on the cached RsacDevices class; (Context, long)
    // args as cached.
    let ok = unsafe {
        jni_call!(
            env,
            CallStaticBooleanMethodA,
            devices.class,
            cb.register,
            args.as_ptr()
        )
    };
    let exception = unsafe { take_exception_message(env) };
    // SAFETY: `context` is a live local ref from this frame.
    unsafe { jni_call!(env, DeleteLocalRef, context) };
    if exception.is_some() || ok == 0 {
        unregister_watcher(handle);
        return Err(AudioError::BackendError {
            backend: "AAudio".to_string(),
            operation: "watch".to_string(),
            message: format!(
                "RsacDevices.registerDeviceCallback failed (no AudioManager, \
                 or the handle was already registered){}",
                exception.map(|m| format!(": {}", m)).unwrap_or_default()
            ),
            context: None,
        });
    }

    // Teardown order: unregister Java (AudioManager callback + bounded
    // HandlerThread join) BEFORE dropping the Rust entry — mirror of the
    // playback path's "unregister session before asking Kotlin to stop".
    Ok(DeviceWatcher::from_teardown(Box::new(move || {
        unregister_device_callback_java(handle);
        unregister_watcher(handle);
    })))
}

/// Asks the AAR to tear down the `AudioDeviceCallback` registration for
/// `handle` (`RsacDevices.unregisterDeviceCallback`: `AudioManager`
/// unregister + `quitSafely` + bounded `HandlerThread` join). Exceptions are
/// folded into log warnings — teardown must not fail (mirrors
/// [`stop_and_release_bridge`]).
fn unregister_device_callback_java(handle: i64) {
    let Ok(aar) = aar_cache() else { return };
    let Some(devices) = aar.devices.as_ref() else {
        return;
    };
    let Some(cb) = devices.callback.as_ref() else {
        return;
    };
    let Ok(guard) = attach_current_thread() else {
        return;
    };
    let env = guard.env();
    let args = [jvalue { j: handle }];
    // SAFETY: static method on the cached RsacDevices class; one long arg.
    unsafe {
        jni_call!(
            env,
            CallStaticVoidMethodA,
            devices.class,
            cb.unregister,
            args.as_ptr()
        );
        if let Some(msg) = take_exception_message(env) {
            log::warn!(
                "RsacDevices.unregisterDeviceCallback threw: {}; continuing teardown",
                msg
            );
        }
    }
}

// ── Compile-time assertions ──────────────────────────────────────────────

/// The registered native fn pointers must have the exact JNI-expected
/// shapes; assigning them to typed fn pointers makes a signature drift a
/// compile error rather than a runtime crash.
const _NATIVE_PUSH: unsafe extern "system" fn(
    *mut JNIEnv,
    jclass,
    jlong,
    jfloatArray,
    jint,
    jint,
    jint,
) = native_push;
const _NATIVE_SESSION_ENDED: unsafe extern "system" fn(*mut JNIEnv, jclass, jlong) =
    native_session_ended;
const _NATIVE_RETAIN_PROJECTION: unsafe extern "system" fn(*mut JNIEnv, jclass, jobject) -> jlong =
    native_retain_projection;
const _NATIVE_DEVICES_CHANGED: unsafe extern "system" fn(*mut JNIEnv, jclass, jlong) =
    native_devices_changed;
