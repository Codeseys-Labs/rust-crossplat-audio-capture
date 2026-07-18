//! `AndroidPlatformStream` — the Android backend's [`PlatformStream`]
//! implementation, plus the AAudio open/start factory and the `extern "C"`
//! callbacks that feed the bridge.
//!
//! # Threading model
//!
//! Like macOS/iOS (and unlike the rsac-owned Windows capture loop), AAudio
//! manages its own threads: the data callback fires on an AAudio-owned
//! callback thread — **potentially a real-time thread**, because the stream
//! is opened with `AAUDIO_PERFORMANCE_MODE_LOW_LATENCY` — and the error
//! callback fires on a separate, non-real-time thread AAudio creates for it.
//! There is **no rsac-owned capture thread**; the module name follows the
//! per-backend convention, not an actual `std::thread`.
//!
//! # Real-time discipline (ADR-0001, full rules)
//!
//! The data callback must be treated as hard-RT:
//!
//! - **No allocation:** the i16→f32 conversion scratch is pre-allocated once
//!   at stream creation and never grown in the callback; a period that would
//!   exceed its capacity is dropped and counted (`overrun_count`), never
//!   allocated for. The f32 fast path pushes the incoming slice directly.
//! - **No locks:** the callback context is reached through a raw pointer
//!   (see the ownership dance below) — no `Mutex`, no `Condvar` (the
//!   consumer relies on the bounded backstop poll, exactly like the
//!   Linux/macOS RT push paths).
//! - **No panics escape:** the push is [`BridgeProducer::push_samples_guarded_stamped`]
//!   (foreign C-callback boundary — an unwind into AAudio would be UB), and
//!   the surrounding code is panic-free by construction.
//!
//! # Callback-context ownership dance
//!
//! AAudio callbacks receive an opaque `void* user_data`. rsac leaks one
//! `Box` per callback (a [`DataCallbackContext`] for the data callback and a
//! separate [`ErrorCallbackContext`] for the error callback — separate
//! allocations so the two callbacks never alias each other's state) and
//! hands the raw pointers to the stream builder. Lifetime:
//!
//! 1. **Leaked** (`Box::into_raw`) before `AAudioStreamBuilder_openStream`,
//!    because the builder needs the pointers up front.
//! 2. **Completed** between open and `AAudioStream_requestStart`: the
//!    delivered format/rate/channels are read back and written into the data
//!    context (no data callback can run before start, so this access is
//!    exclusive).
//! 3. **Reclaimed exactly once** (`Box::from_raw`) in the teardown choke
//!    point, strictly **after** `AAudioStream_requestStop` + a bounded
//!    state-quiesce wait + `AAudioStream_close` have returned — the point at
//!    which no callback can reference the contexts anymore. The teardown is
//!    serialized by a `Mutex` and made idempotent by nulling the pointers,
//!    so double-free is impossible.
//!
//! # Terminal signaling (ADR-0010 / ADR-0003)
//!
//! - Graceful stop (`stop_capture` / `Drop`): after the stream is closed the
//!   bridge is driven `Running → Stopping` (drainable tail) and a parked
//!   reader is woken — the [`BridgeProducer::signal_done`] semantics applied
//!   via the shared handle, because the producer itself lives inside the
//!   leaked data-callback context.
//! - Spontaneous death (`AAUDIO_ERROR_DISCONNECTED` et al. via the error
//!   callback): the bridge is forced to the terminal `Error` state (the
//!   [`BridgeProducer::signal_error`] semantics), so a blocked reader
//!   observes the Fatal `StreamEnded` instead of hanging — mirroring the
//!   macOS `kAudioDevicePropertyDeviceIsAlive` listener pattern.

#![cfg(all(target_os = "android", feature = "feat_android"))]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::bridge::ring_buffer::{BridgeProducer, BridgeShared};
use crate::bridge::state::StreamState;
use crate::bridge::stream::PlatformStream;
use crate::core::config::{AudioFormat, CaptureTarget, SampleFormat};
use crate::core::error::{AudioError, AudioResult, BackendContext};

use super::aaudio::{
    aaudio_data_callback_result_t, aaudio_format_t, aaudio_result_t, result_name, result_to_error,
    AAudioStream, AAudioStreamBuilder, AAudioStreamBuilder_delete, AAudioStreamBuilder_openStream,
    AAudioStreamBuilder_setChannelCount, AAudioStreamBuilder_setDataCallback,
    AAudioStreamBuilder_setDeviceId, AAudioStreamBuilder_setDirection,
    AAudioStreamBuilder_setErrorCallback, AAudioStreamBuilder_setFormat,
    AAudioStreamBuilder_setPerformanceMode, AAudioStreamBuilder_setSampleRate, AAudioStream_close,
    AAudioStream_getChannelCount, AAudioStream_getFormat, AAudioStream_getSampleRate,
    AAudioStream_getState, AAudioStream_requestStart, AAudioStream_requestStop,
    AAudioStream_waitForStateChange, AAudio_createStreamBuilder, AAUDIO_CALLBACK_RESULT_CONTINUE,
    AAUDIO_CALLBACK_RESULT_STOP, AAUDIO_DIRECTION_INPUT, AAUDIO_FORMAT_PCM_FLOAT,
    AAUDIO_FORMAT_PCM_I16, AAUDIO_FORMAT_UNSPECIFIED, AAUDIO_OK,
    AAUDIO_PERFORMANCE_MODE_LOW_LATENCY, AAUDIO_STREAM_STATE_STARTED, AAUDIO_STREAM_STATE_STARTING,
    AAUDIO_STREAM_STATE_STOPPING, AAUDIO_UNSPECIFIED,
};
use super::DEFAULT_INPUT_DEVICE_ID;

// ── Tuning constants ─────────────────────────────────────────────────────

/// Floor for the i16→f32 conversion scratch, in **frames**. AAudio callback
/// periods are typically one burst (≈ 96–1024 frames); this floor plus the
/// half-second sizing below leaves generous headroom so a legitimate period
/// can never force a callback-time allocation (ADR-0001: oversized periods
/// are dropped + counted, never allocated for).
const SCRATCH_MIN_FRAMES: usize = 4096;

/// Sizes the i16→f32 conversion scratch, in `f32` samples: half a second of
/// audio at the delivered rate (with [`SCRATCH_MIN_FRAMES`] as the floor),
/// times the channel count. One-time cost at 48 kHz stereo: 48 000 × 4 B ≈
/// 192 KiB — allocated on the setup thread, never in the callback.
pub(super) fn scratch_capacity_samples(sample_rate: u32, channels: u16) -> usize {
    ((sample_rate as usize) / 2).max(SCRATCH_MIN_FRAMES) * usize::from(channels.max(1))
}

// ── Sample conversion (pure logic, unit-tested) ──────────────────────────

/// Converts one signed 16-bit PCM sample to rsac's canonical `f32` range.
///
/// Divides by 32768 (`-i16::MIN`), so `i16::MIN` maps to exactly `-1.0` and
/// `i16::MAX` to `32767/32768` — the conventional asymmetric mapping used
/// across audio stacks. Pure integer→float math: no allocation, no panic.
#[inline]
fn i16_sample_to_f32(sample: i16) -> f32 {
    f32::from(sample) / 32768.0
}

/// Converts an interleaved i16 slice into `dst` as f32, **without ever
/// growing `dst`'s capacity** (ADR-0001: refusing beats reallocating on the
/// callback thread).
///
/// Returns `false` — leaving `dst` untouched and performing no allocation —
/// when `src.len()` exceeds `dst.capacity()`; the caller drops + counts the
/// period instead. Returns `true` after clearing and filling `dst` with
/// exactly `src.len()` converted samples.
fn convert_i16_to_f32_into(src: &[i16], dst: &mut Vec<f32>) -> bool {
    if src.len() > dst.capacity() {
        return false;
    }
    dst.clear();
    // Capacity was checked above, so this extend never reallocates.
    dst.extend(src.iter().map(|&s| i16_sample_to_f32(s)));
    true
}

// ── Callback contexts ────────────────────────────────────────────────────

/// State owned by the **data** callback (leaked `Box`; see the module docs'
/// ownership dance).
///
/// Accessed as `&mut` from the AAudio data-callback thread only: AAudio
/// serializes data callbacks per stream, nothing else touches this between
/// `requestStart` and the post-`close` reclamation, so the exclusive access
/// is sound without a lock (which the RT context forbids anyway, ADR-0001).
struct DataCallbackContext {
    /// Producer half of the lock-free bridge; pushes are alloc-free in
    /// steady state (free-list return ring, ADR-0001) and panic-guarded
    /// (foreign C-callback boundary).
    producer: BridgeProducer,
    /// Delivered channel count (read back from the open stream).
    channels: u16,
    /// Delivered sample rate in Hz (read back from the open stream).
    sample_rate: u32,
    /// Delivered sample format code (`AAUDIO_FORMAT_PCM_FLOAT` or
    /// `AAUDIO_FORMAT_PCM_I16`, validated at open).
    format: aaudio_format_t,
    /// Pre-allocated i16→f32 conversion buffer — never grown in the
    /// callback. Left empty (capacity 0) for f32-delivery streams, which
    /// push the incoming slice directly.
    scratch: Vec<f32>,
}

/// State owned by the **error** callback (separate leaked `Box`, so the
/// error thread never aliases the data callback's `&mut` context).
///
/// Holds only thread-safe handles: everything the disconnect path needs to
/// drive the bridge terminal (ADR-0010) and flip the liveness flag.
struct ErrorCallbackContext {
    /// Bridge shared state — used to force the terminal `Error` state and
    /// wake parked readers (the `signal_error` semantics; the producer
    /// itself lives in the data context).
    shared: Arc<BridgeShared>,
    /// Liveness flag shared with [`AndroidPlatformStream`]; cleared so
    /// `is_active()` reflects the disconnect.
    is_active: Arc<AtomicBool>,
}

/// Counts one producer-side dropped buffer **without pushing** (the period
/// never reached the ring — e.g. an oversized i16 period or an unexpected
/// format code).
///
/// Mirrors the accounting of `push_or_drop`'s drop arm on the shared bridge
/// counters, so the loss is visible through `overrun_count()` /
/// `buffers_dropped()` exactly like an ordinary ring-overflow drop.
/// Lock-free `Relaxed` adds — safe on the RT callback thread (same helper
/// shape as the iOS backend's `count_external_drop`). Shared with the JNI
/// playback-ingest path (`jni.rs`), which applies the same accounting to
/// periods that never reach the ring.
pub(super) fn count_external_drop(producer: &BridgeProducer) {
    let shared = producer.shared();
    shared.buffers_dropped.fetch_add(1, Ordering::Relaxed);
    shared.consecutive_drops.fetch_add(1, Ordering::Relaxed);
}

// ── extern "C" callbacks ─────────────────────────────────────────────────

/// AAudio data callback: pushes each captured period into the bridge.
///
/// Runs on AAudio's callback thread — **potentially real-time** (the stream
/// requests `LOW_LATENCY`), so the full ADR-0001 rules apply: no allocation
/// (pre-sized scratch, free-list-backed push), no locks, no logging, and no
/// panic may escape (the push is `push_samples_guarded_stamped`; everything
/// around it is panic-free by construction — checked arithmetic, no
/// indexing, no growth).
///
/// # Safety
///
/// Invoked only by AAudio with the contract registered at build time:
/// `user_data` is the leaked [`DataCallbackContext`] (valid until the
/// post-close reclamation, and never aliased — AAudio serializes data
/// callbacks and the setup/teardown paths only touch the context while no
/// callback can run); `audio_data` points at `num_frames` frames of
/// interleaved samples in the stream's delivered format, valid for the
/// duration of the call.
unsafe extern "C" fn data_callback(
    _stream: *mut AAudioStream,
    user_data: *mut c_void,
    audio_data: *mut c_void,
    num_frames: i32,
) -> aaudio_data_callback_result_t {
    if user_data.is_null() {
        // No context — nothing can be delivered; stop the callback stream.
        return AAUDIO_CALLBACK_RESULT_STOP;
    }
    // SAFETY: `user_data` is the leaked `Box<DataCallbackContext>` registered
    // at build time; it stays valid until reclaimed after `AAudioStream_close`
    // (which cannot race this call — close returns only once callbacks have
    // ceased). Exclusive `&mut` access is sound because AAudio serializes
    // data callbacks and no other code touches the context while the stream
    // is started (see the module-docs ownership dance).
    let ctx = unsafe { &mut *user_data.cast::<DataCallbackContext>() };

    // Once the bridge is terminal (explicit stop raced us, or the error
    // callback already poisoned the stream), tell AAudio to stop invoking
    // us. A single Relaxed-class atomic load — RT-safe.
    if ctx.producer.shared().state.is_terminal() {
        return AAUDIO_CALLBACK_RESULT_STOP;
    }

    if audio_data.is_null() || num_frames <= 0 {
        // Degenerate delivery; nothing to push this period.
        return AAUDIO_CALLBACK_RESULT_CONTINUE;
    }

    // `num_frames > 0` was checked, so the cast is lossless; the multiply is
    // checked so a pathological frame count can never produce a wrong slice
    // length (which would be UB), even on 32-bit targets.
    let frames = num_frames as usize;
    let samples = match frames.checked_mul(usize::from(ctx.channels)) {
        Some(n) => n,
        None => {
            count_external_drop(&ctx.producer);
            return AAUDIO_CALLBACK_RESULT_CONTINUE;
        }
    };

    match ctx.format {
        AAUDIO_FORMAT_PCM_FLOAT => {
            // SAFETY: for a PCM_FLOAT stream, `audio_data` points at
            // `num_frames * channels` valid, suitably-aligned f32 samples for
            // the duration of this call (AAudio delivery contract); every bit
            // pattern is a valid f32. The slice is read-only and not retained
            // past the call.
            let data = unsafe { std::slice::from_raw_parts(audio_data.cast::<f32>(), samples) };
            // Guarded (foreign C frame — unwind would be UB) + stream-position
            // stamped push. Ring-full ⇒ drop + count inside the producer;
            // never blocks, never allocates in steady state (ADR-0001).
            ctx.producer
                .push_samples_guarded_stamped(data, ctx.channels, ctx.sample_rate);
        }
        AAUDIO_FORMAT_PCM_I16 => {
            // SAFETY: for a PCM_I16 stream, `audio_data` points at
            // `num_frames * channels` valid, suitably-aligned i16 samples for
            // the duration of this call (AAudio delivery contract).
            let data = unsafe { std::slice::from_raw_parts(audio_data.cast::<i16>(), samples) };
            if convert_i16_to_f32_into(data, &mut ctx.scratch) {
                ctx.producer.push_samples_guarded_stamped(
                    &ctx.scratch,
                    ctx.channels,
                    ctx.sample_rate,
                );
            } else {
                // Period larger than the pre-sized scratch: drop + count
                // instead of allocating (ADR-0001). No logging here — this
                // may be an RT thread.
                count_external_drop(&ctx.producer);
            }
        }
        _ => {
            // Unreachable in practice: the format was validated right after
            // open and AAudio never changes it mid-stream. Count the loss
            // (visible via overrun_count) rather than silently spinning; no
            // logging on the RT thread.
            count_external_drop(&ctx.producer);
        }
    }

    AAUDIO_CALLBACK_RESULT_CONTINUE
}

/// AAudio error callback: the stream can no longer deliver audio (device
/// disconnect — `AAUDIO_ERROR_DISCONNECTED` = -899 — service death, …).
///
/// Runs on a dedicated **non-real-time** thread AAudio creates for error
/// delivery. Drives the bridge to the terminal `Error` state (the
/// `signal_error` semantics — ADR-0010's "producer died" contract) so a
/// parked reader observes the Fatal `StreamEnded` instead of hanging, and
/// clears the liveness flag so `is_active()` reflects the death. The stream
/// handle itself is still closed later by `stop_capture`/`Drop` (AAudio
/// forbids closing from inside the callback).
///
/// # Safety
///
/// Invoked only by AAudio with `user_data` = the leaked
/// [`ErrorCallbackContext`] registered at build time, valid until reclaimed
/// after `AAudioStream_close`. Only `Send + Sync` handles are touched
/// (atomics + `Arc`), so the cross-thread shared access is sound.
unsafe extern "C" fn error_callback(
    _stream: *mut AAudioStream,
    user_data: *mut c_void,
    error: aaudio_result_t,
) {
    if user_data.is_null() {
        return;
    }
    // SAFETY: see the function-level Safety section — leaked, live context;
    // shared (&) access to Sync fields only.
    let ctx = unsafe { &*user_data.cast::<ErrorCallbackContext>() };

    ctx.is_active.store(false, Ordering::SeqCst);

    // Terminal poison (ADR-0010, fatal sibling of signal_done): force the
    // sticky terminal Error and wake both reader kinds. Lock-free-to-signal
    // and alloc-free — the same tail signal_error() produces; replicated via
    // the shared handle because the BridgeProducer lives in the data
    // context (mirrors the macOS DeviceIsAlive listener).
    ctx.shared.state.force_set(StreamState::Error);
    ctx.shared.notify_wake();
    #[cfg(feature = "async-stream")]
    ctx.shared.waker.wake();

    // Diagnostics last, and unwind-contained: this is a foreign C frame, so
    // even a pathological panicking logger must not unwind into AAudio (UB).
    let _ = std::panic::catch_unwind(|| {
        log::warn!(
            "AAudio error callback: stream can no longer deliver audio \
             ({} / {}); bridge driven to terminal Error (ADR-0010)",
            result_name(error),
            error
        );
    });
}

// Compile-time guarantee that the leaked data context may be handed to
// AAudio's callback thread (it is constructed on the setup thread and used
// on the callback thread — a cross-thread move).
fn _assert_data_callback_context_send() {
    fn _assert<T: Send>() {}
    _assert::<DataCallbackContext>();
}

// The error context is *shared* with (not moved to) the error thread.
fn _assert_error_callback_context_send_sync() {
    fn _assert<T: Send + Sync>() {}
    _assert::<ErrorCallbackContext>();
}

// ── Target resolution (mic-only slice) ───────────────────────────────────

/// Returns `true` if a [`CaptureTarget::Device`] id selects the single
/// logical Android input device.
///
/// Accepts [`DEFAULT_INPUT_DEVICE_ID`] case-insensitively, plus the empty
/// string (the "default endpoint" convention shared with the Windows and
/// iOS backends).
fn is_default_input_id(id: &str) -> bool {
    id.is_empty() || id.eq_ignore_ascii_case(DEFAULT_INPUT_DEVICE_ID)
}

/// Resolves a [`CaptureTarget`] to the AAudio device id to route to, against
/// the Android **mic slice** (ADR-0013).
///
/// Returns [`AAUDIO_UNSPECIFIED`] for the default input route, or a positive
/// `AudioDeviceInfo` id (from the AAR's `AudioManager.getDevices` list,
/// rsac-ad8a) to pin via [`AAudioStreamBuilder_setDeviceId`].
///
/// | Target | Outcome |
/// |---|---|
/// | `Device("default")` (or `""`) | `Ok(AAUDIO_UNSPECIFIED)` — the default AAudio input |
/// | `Device(<positive int>)` | `Ok(id)` — pin that enumerated input device |
/// | `Device(other)` | [`AudioError::DeviceNotFound`] — non-numeric or non-positive id |
/// | `SystemDefault` | [`AudioError::PlatformNotSupported`] — served by the **playback-capture device** (`super::playback`, rsac-77f1), not the mic |
/// | `Application` / `ApplicationByName` / `ProcessTree` | [`AudioError::PlatformNotSupported`] — served by the playback-capture device (UID-filtered `AudioPlaybackCapture`; tree ≡ app on Android) |
///
/// A syntactically valid positive id that the OS later rejects is **not**
/// caught here — it surfaces from the open path as
/// [`AudioError::StreamCreationFailed`].
///
/// The match is intentionally exhaustive (no wildcard): a new
/// `CaptureTarget` variant must be classified here before the crate
/// compiles for Android.
fn resolve_mic_target(target: &CaptureTarget) -> AudioResult<i32> {
    match target {
        CaptureTarget::Device(id) if is_default_input_id(&id.0) => Ok(AAUDIO_UNSPECIFIED),
        CaptureTarget::Device(id) => match id.0.parse::<i32>() {
            Ok(device_id) if device_id > 0 => Ok(device_id),
            // Non-numeric or non-positive: not a routable enumerated id.
            _ => Err(AudioError::DeviceNotFound {
                device_id: id.0.clone(),
            }),
        },
        CaptureTarget::SystemDefault => Err(AudioError::PlatformNotSupported {
            feature: "system-audio capture through the Android microphone \
                      device: SystemDefault maps to AudioPlaybackCapture — all \
                      capturable playback, NOT the microphone (ADR-0013) — and \
                      is served by the playback-capture device \
                      (CaptureTarget::SystemDefault resolves there via \
                      default_device(); this mic device only serves \
                      CaptureTarget::Device(DeviceId(\"default\".into())))"
                .to_string(),
            platform: "android".to_string(),
        }),
        CaptureTarget::Application(_)
        | CaptureTarget::ApplicationByName(_)
        | CaptureTarget::ProcessTree(_) => Err(AudioError::PlatformNotSupported {
            feature: "per-application / process-tree capture through the \
                      Android microphone device: these map to \
                      AudioPlaybackCapture UID filters (ADR-0013; all of an \
                      Android app's processes share one UID, so tree ≡ app) \
                      and are served by the playback-capture device, not this \
                      mic device. This mic device only serves \
                      CaptureTarget::Device(DeviceId(\"default\".into()))"
                .to_string(),
            platform: "android".to_string(),
        }),
    }
}

// ── AndroidPlatformStream ────────────────────────────────────────────────

/// Raw AAudio handles for one capture stream, serialized by the `Mutex` in
/// [`AndroidPlatformStream`].
///
/// All three pointers are nulled as they are released, which is both the
/// idempotency guard for teardown and the double-free guard for the leaked
/// callback contexts.
struct StreamHandles {
    /// The open AAudio stream; null once closed.
    stream: *mut AAudioStream,
    /// Leaked data-callback context; reclaimed exactly once after close.
    data_ctx: *mut DataCallbackContext,
    /// Leaked error-callback context; reclaimed exactly once after close.
    error_ctx: *mut ErrorCallbackContext,
}

/// Platform-specific stream handle for Android (AAudio backend).
///
/// Wraps the live AAudio stream + the leaked callback contexts and
/// implements [`PlatformStream`] so it can be used with
/// [`BridgeStream`](crate::bridge::stream::BridgeStream).
///
/// # Shutdown
///
/// [`stop_capture`](PlatformStream::stop_capture) (and `Drop`, via the same
/// choke point) requests stop, waits (bounded) for the callback to quiesce,
/// closes the stream, reclaims the callback contexts, and then drives the
/// bridge `Running → Stopping` — the graceful producer terminal signal
/// (ADR-0010). The ordering matters: the contexts are reclaimed only after
/// `AAudioStream_close` returns (no callback can reference them anymore),
/// and the terminal transition happens only after the close (no callback
/// can push past the declared end).
pub(crate) struct AndroidPlatformStream {
    /// Raw AAudio handles, serialized by the `Mutex` for `&self` access.
    handles: Mutex<StreamHandles>,
    /// `true` while the stream is delivering audio. Cleared by the teardown
    /// choke point and by the error callback (device disconnect), so
    /// [`is_active`](PlatformStream::is_active) reflects both.
    is_active: Arc<AtomicBool>,
    /// Producer-terminal-signal handle (ADR-0010): a clone of the bridge's
    /// shared state, used to drive `Running → Stopping` (+ reader wake)
    /// once the stream is closed.
    terminal: Arc<BridgeShared>,
}

// SAFETY: `AndroidPlatformStream` carries raw pointers (`*mut AAudioStream`
// and the two leaked callback-context pointers), which are not `Send`/`Sync`
// by default — but `PlatformStream: Send` and `BridgeStream<S>: Sync`
// require both. This is sound because:
//
// - every dereference/FFI use of the pointers after construction goes
//   through the `Mutex<StreamHandles>` (only the teardown choke point
//   touches them), so no unsynchronized concurrent use can occur;
// - AAudio's lifecycle functions (`requestStop`/`waitForStateChange`/
//   `close`) may be called from any thread except the stream's own
//   callbacks — AAudio streams are not thread-safe *concurrently*, and the
//   Mutex provides exactly the required serialization;
// - the callback contexts are only reclaimed under that same Mutex, after
//   `AAudioStream_close` has returned (callbacks quiesced), so a
//   cross-thread teardown can never race a callback's access;
// - the remaining fields (`Arc<AtomicBool>`, `Arc<BridgeShared>`) are
//   `Send + Sync` already.
//
// Mirrors the `unsafe impl Send/Sync` discipline on `IosPlatformStream` /
// `MacosPlatformStream`.
unsafe impl Send for AndroidPlatformStream {}
// SAFETY: see the `Send` justification above — all interior pointer access
// is serialized by the `Mutex`.
unsafe impl Sync for AndroidPlatformStream {}

impl AndroidPlatformStream {
    /// Stops + closes the AAudio stream (once), reclaims the callback
    /// contexts, and signals the bridge terminal.
    ///
    /// Idempotent: the nulled `stream` pointer (under the `Mutex`) ensures
    /// the FFI teardown, the context reclamation, and the terminal
    /// transition run at most once; later calls are no-ops. Shared by
    /// [`stop_capture`](PlatformStream::stop_capture) and `Drop` so dropping
    /// the handle (without an explicit stop) also lands the stream terminal
    /// (ADR-0010).
    ///
    /// OS-call failures on this path are logged and *not* propagated: the
    /// teardown always runs to completion (close → reclaim → terminal), so
    /// a failed `requestStop` can never strand a parked reader or leak the
    /// contexts.
    fn stop_and_close(&self) -> AudioResult<()> {
        let mut handles = self.handles.lock().map_err(|_| AudioError::InternalError {
            message: "AAudio stream handles mutex poisoned".to_string(),
            source: None,
        })?;

        if handles.stream.is_null() {
            // Already torn down — idempotent no-op.
            return Ok(());
        }
        let stream = handles.stream;

        // 1. Ask AAudio to stop delivering data callbacks.
        // SAFETY: `stream` is the live stream opened by
        // `create_android_capture`; requestStop may be called from any
        // thread except the stream's own callbacks (we are on a consumer /
        // drop thread), and the Mutex serializes against other teardowns.
        let res = unsafe { AAudioStream_requestStop(stream) };
        if res != AAUDIO_OK {
            log::warn!(
                "AAudioStream_requestStop failed: {} ({}); continuing teardown",
                result_name(res),
                res
            );
        }

        // 2. Bounded quiesce: wait for the stream to leave the transitional
        // states so no data callback is in flight when we close + reclaim.
        // This matters on pre-API-30 devices, where close() alone had a
        // documented race with in-flight callbacks (API 30 split release/
        // close to fix it). Best-effort and bounded — never hangs teardown.
        wait_until_quiescent(stream);

        // 3. Close: releases the stream; AAudio delivers no callbacks after
        // this returns.
        // SAFETY: same liveness/serialization argument as requestStop; the
        // pointer is nulled immediately after so it is never used again.
        let res = unsafe { AAudioStream_close(stream) };
        if res != AAUDIO_OK {
            log::warn!(
                "AAudioStream_close failed: {} ({}); continuing teardown",
                result_name(res),
                res
            );
        }
        handles.stream = std::ptr::null_mut();

        // 4. Reclaim the leaked callback contexts — exactly once (pointers
        // are nulled), and only now that close has returned so no callback
        // can reference them (the module-docs ownership dance, step 3).
        if !handles.data_ctx.is_null() {
            // SAFETY: `data_ctx` came from `Box::into_raw` in
            // `create_android_capture`, has not been freed (non-null ⇒ this
            // is the first reclamation, guarded by the Mutex), and no
            // callback can still hold a reference (stream closed above).
            drop(unsafe { Box::from_raw(handles.data_ctx) });
            handles.data_ctx = std::ptr::null_mut();
        }
        if !handles.error_ctx.is_null() {
            // SAFETY: same argument as `data_ctx`.
            drop(unsafe { Box::from_raw(handles.error_ctx) });
            handles.error_ctx = std::ptr::null_mut();
        }

        self.is_active.store(false, Ordering::SeqCst);

        // 5. Producer-terminal-signal (ADR-0010): the stream is closed, so
        // no more pushes can occur — drive the bridge to the graceful
        // ending state (`signal_done` semantics via the shared handle;
        // the producer itself was just reclaimed with the data context).
        // `Running → Stopping` keeps a buffered tail drainable; the CAS
        // no-ops if the state already advanced (e.g. `BridgeStream::stop`
        // or the error callback got there first — terminal Error is sticky).
        let _ = self
            .terminal
            .state
            .transition(StreamState::Running, StreamState::Stopping);
        // Wake a parked blocking reader so it observes the ending state
        // promptly (PU-5). Non-RT stop path — the notify is allowed here
        // (ADR-0001 forbids it only on the RT callback push path).
        self.terminal.notify_wake();
        #[cfg(feature = "async-stream")]
        self.terminal.waker.wake();

        Ok(())
    }
}

impl PlatformStream for AndroidPlatformStream {
    fn stop_capture(&self) -> AudioResult<()> {
        self.stop_and_close()
    }

    fn is_active(&self) -> bool {
        self.is_active.load(Ordering::SeqCst)
    }
}

impl Drop for AndroidPlatformStream {
    /// Deterministic shutdown: stop + close the stream (and signal the
    /// bridge terminal) if not already done, so dropping the handle never
    /// leaves a running callback pushing into a bridge nobody reads, a
    /// parked reader hanging (ADR-0010), or the leaked contexts unreclaimed.
    fn drop(&mut self) {
        if let Err(e) = self.stop_and_close() {
            log::warn!("AndroidPlatformStream::drop: teardown failed: {:?}", e);
        }
    }
}

/// Bounded wait for the stream to leave the transitional
/// STARTING/STARTED/STOPPING states after a stop request, so the data
/// callback is quiescent before close + context reclamation.
///
/// Up to 5 × 100 ms slices of [`AAudioStream_waitForStateChange`]; any
/// error or a non-transitional state ends the wait early. Purely
/// best-effort: teardown proceeds regardless (a warning is logged if the
/// stream never settled).
fn wait_until_quiescent(stream: *mut AAudioStream) {
    /// One wait slice, in nanoseconds (100 ms).
    const SLICE_NANOS: i64 = 100_000_000;
    /// Maximum slices (≤ 500 ms total).
    const MAX_SLICES: usize = 5;

    // SAFETY: `stream` is live and open (caller holds the handles Mutex and
    // has not closed it yet); getState is a trivial read.
    let mut current = unsafe { AAudioStream_getState(stream) };
    for _ in 0..MAX_SLICES {
        if current != AAUDIO_STREAM_STATE_STARTING
            && current != AAUDIO_STREAM_STATE_STARTED
            && current != AAUDIO_STREAM_STATE_STOPPING
        {
            return;
        }
        let mut next = current;
        // SAFETY: live stream; `next` is a valid out-pointer to a local for
        // the duration of the call.
        let res =
            unsafe { AAudioStream_waitForStateChange(stream, current, &mut next, SLICE_NANOS) };
        if res != AAUDIO_OK {
            return;
        }
        current = next;
    }
    log::warn!(
        "AAudio stream did not leave its transitional state within \
         {} ms of requestStop; closing anyway",
        (SLICE_NANOS / 1_000_000) * MAX_SLICES as i64
    );
}

// ── Factory ──────────────────────────────────────────────────────────────

/// Reclaims the two leaked callback contexts.
///
/// # Safety
///
/// Both pointers must have come from `Box::into_raw`, must not have been
/// freed, and **no AAudio callback may still be able to run** — i.e. the
/// stream was never opened, or `AAudioStream_close` has returned.
unsafe fn reclaim_contexts(
    data_ctx: *mut DataCallbackContext,
    error_ctx: *mut ErrorCallbackContext,
) {
    // SAFETY: per the function contract — unique, live Box allocations with
    // no remaining referents.
    drop(unsafe { Box::from_raw(data_ctx) });
    // SAFETY: as above.
    drop(unsafe { Box::from_raw(error_ctx) });
}

/// Best-effort close of a just-opened stream on a factory failure path,
/// followed by context reclamation.
///
/// # Safety
///
/// `stream` must be the live stream returned by a successful
/// `AAudioStreamBuilder_openStream` and must not be closed already; the
/// context pointers follow the [`reclaim_contexts`] contract (which is
/// satisfied once the close here returns).
unsafe fn close_and_reclaim(
    stream: *mut AAudioStream,
    data_ctx: *mut DataCallbackContext,
    error_ctx: *mut ErrorCallbackContext,
) {
    // SAFETY: live stream per the function contract; the factory abandons
    // the pointer right after this call.
    let res = unsafe { AAudioStream_close(stream) };
    if res != AAUDIO_OK {
        log::warn!(
            "AAudioStream_close failed during error cleanup: {} ({})",
            result_name(res),
            res
        );
    }
    // SAFETY: close returned above, so no callback can reference the
    // contexts anymore; pointers are unique live Box allocations.
    unsafe { reclaim_contexts(data_ctx, error_ctx) };
}

/// Opens and starts an AAudio input stream for the default audio input,
/// returning the [`AndroidPlatformStream`] handle plus the **delivered**
/// [`AudioFormat`].
///
/// Steps:
///
/// 1. Validate `target` against the mic slice ([`ensure_mic_target`]).
/// 2. Leak the two callback contexts and register them on a stream builder
///    (input direction, low-latency mode, `PCM_FLOAT` at the requested
///    rate/channels).
/// 3. Open the stream — if the requested shape is rejected, retry once with
///    `AAUDIO_UNSPECIFIED` format/rate/channels so the device-native
///    configuration is used instead of failing outright.
/// 4. Read back the **actual** format/rate/channels via the stream getters,
///    publish the delivered [`AudioFormat`] on the bridge
///    (`set_negotiated_format` — before any push, so
///    `CapturingStream::format()` is authoritative from the first buffer),
///    and size the i16→f32 scratch from the delivered values (the only
///    buffer allocation, done here on the setup thread — ADR-0001).
/// 5. `AAudioStream_requestStart` — data callbacks begin.
///
/// The delivered rate/channels may differ from the requested ones; this
/// backend does not resample (the honest contract shared with the iOS mic
/// slice). `sample_format` is always reported as `F32`: the bridge payload
/// is interleaved f32 regardless of AAudio's wire format (i16 delivery is
/// converted in the callback).
///
/// # Errors
///
/// - Target errors per [`ensure_mic_target`].
/// - [`AudioError::BackendError`] if the builder cannot be created.
/// - [`AudioError::StreamCreationFailed`] if both open attempts fail
///   (commonly: the `RECORD_AUDIO` runtime permission has not been granted
///   — a host-app responsibility; rsac's `mobile/android` helpers wrap the
///   request flow) or the delivered configuration is unusable.
/// - [`AudioError::StreamStartFailed`] if `requestStart` fails.
///
/// All failure paths close the stream (when open) and reclaim the leaked
/// contexts — nothing leaks on error.
pub(crate) fn create_android_capture(
    target: &CaptureTarget,
    requested: &AudioFormat,
    producer: BridgeProducer,
    terminal: Arc<BridgeShared>,
) -> AudioResult<(AndroidPlatformStream, AudioFormat)> {
    // AAUDIO_UNSPECIFIED for the default route, or a positive enumerated
    // AudioDeviceInfo id to pin (rsac-ad8a). Invalid ids fail here.
    let device_id = resolve_mic_target(target)?;

    let is_active = Arc::new(AtomicBool::new(true));

    // Leak the callback contexts (ownership-dance step 1). The data
    // context's delivered-format fields and scratch are placeholders until
    // step 4 below — no callback can run before requestStart.
    let data_ctx: *mut DataCallbackContext = Box::into_raw(Box::new(DataCallbackContext {
        producer,
        channels: requested.channels.max(1),
        sample_rate: requested.sample_rate.max(1),
        format: AAUDIO_FORMAT_PCM_FLOAT,
        scratch: Vec::new(),
    }));
    let error_ctx: *mut ErrorCallbackContext = Box::into_raw(Box::new(ErrorCallbackContext {
        shared: Arc::clone(&terminal),
        is_active: Arc::clone(&is_active),
    }));

    // ── Builder ──────────────────────────────────────────────────────
    let mut builder: *mut AAudioStreamBuilder = std::ptr::null_mut();
    // SAFETY: `builder` is a valid out-pointer to a local.
    let res = unsafe { AAudio_createStreamBuilder(&mut builder) };
    if res != AAUDIO_OK || builder.is_null() {
        // SAFETY: no stream was opened, so no callback ever ran — the
        // contexts have no referents.
        unsafe { reclaim_contexts(data_ctx, error_ctx) };
        return Err(result_to_error("AAudio_createStreamBuilder", res));
    }

    // ── Configure: input, low-latency, FLOAT at the requested shape ──
    // SAFETY: `builder` is the live builder created above; the setters are
    // simple field writes. The callback function pointers match the
    // registered typedefs, and the user_data pointers are the leaked
    // contexts, valid for the stream's whole lifetime (ownership dance).
    unsafe {
        AAudioStreamBuilder_setDirection(builder, AAUDIO_DIRECTION_INPUT);
        // Pin the enumerated device only when one is requested, so the
        // default-route path is byte-for-byte unchanged (rsac-ad8a). The
        // device pin is intentionally left set across the format-negotiation
        // retry below: a wrong *format* must not silently re-route to a
        // different device.
        if device_id != AAUDIO_UNSPECIFIED {
            AAudioStreamBuilder_setDeviceId(builder, device_id);
        }
        AAudioStreamBuilder_setPerformanceMode(builder, AAUDIO_PERFORMANCE_MODE_LOW_LATENCY);
        AAudioStreamBuilder_setFormat(builder, AAUDIO_FORMAT_PCM_FLOAT);
        AAudioStreamBuilder_setSampleRate(
            builder,
            requested.sample_rate.min(i32::MAX as u32) as i32,
        );
        AAudioStreamBuilder_setChannelCount(builder, i32::from(requested.channels));
        AAudioStreamBuilder_setDataCallback(
            builder,
            Some(data_callback),
            data_ctx.cast::<c_void>(),
        );
        AAudioStreamBuilder_setErrorCallback(
            builder,
            Some(error_callback),
            error_ctx.cast::<c_void>(),
        );
    }

    // ── Open (attempt 1: requested shape; attempt 2: device-native) ──
    let mut stream: *mut AAudioStream = std::ptr::null_mut();
    // SAFETY: live builder; `stream` is a valid out-pointer to a local.
    let mut res = unsafe { AAudioStreamBuilder_openStream(builder, &mut stream) };
    if res != AAUDIO_OK || stream.is_null() {
        log::debug!(
            "AAudio open with requested shape ({} Hz, {} ch, PCM_FLOAT) failed: \
             {} ({}); retrying with device-native settings",
            requested.sample_rate,
            requested.channels,
            result_name(res),
            res
        );
        stream = std::ptr::null_mut();
        // SAFETY: live builder; overwriting the previous requests with the
        // UNSPECIFIED sentinels ("let AAudio choose").
        unsafe {
            AAudioStreamBuilder_setFormat(builder, AAUDIO_FORMAT_UNSPECIFIED);
            AAudioStreamBuilder_setSampleRate(builder, AAUDIO_UNSPECIFIED);
            AAudioStreamBuilder_setChannelCount(builder, AAUDIO_UNSPECIFIED);
        }
        // SAFETY: as the first attempt.
        res = unsafe { AAudioStreamBuilder_openStream(builder, &mut stream) };
    }

    // The builder is no longer needed once the stream is open (or has
    // definitively failed to open).
    // SAFETY: live builder created above; not used after this call.
    let del_res = unsafe { AAudioStreamBuilder_delete(builder) };
    if del_res != AAUDIO_OK {
        log::warn!(
            "AAudioStreamBuilder_delete failed: {} ({})",
            result_name(del_res),
            del_res
        );
    }

    if res != AAUDIO_OK || stream.is_null() {
        // SAFETY: no stream is open (both attempts failed), so no callback
        // ever ran — the contexts have no referents.
        unsafe { reclaim_contexts(data_ctx, error_ctx) };
        let device_cause = if device_id != AAUDIO_UNSPECIFIED {
            format!(
                ", the requested input device id ({}) was rejected or is no \
                 longer present (routed via AAudioStreamBuilder_setDeviceId — \
                 re-enumerate the device list)",
                device_id
            )
        } else {
            String::new()
        };
        return Err(AudioError::StreamCreationFailed {
            reason: format!(
                "AAudioStreamBuilder_openStream failed for the requested audio \
                 input: {} ({}). Common causes: the RECORD_AUDIO runtime \
                 permission has not been granted (a HOST-APP responsibility — \
                 rsac's mobile/android helpers wrap the request flow), or no \
                 audio input device is available on this device/emulator{}",
                result_name(res),
                res,
                device_cause
            ),
            context: Some(BackendContext {
                backend_name: "AAudio".to_string(),
                os_error_code: Some(i64::from(res)),
                os_error_message: Some(result_name(res).to_string()),
            }),
        });
    }

    // ── Read back the REAL delivered configuration ───────────────────
    // SAFETY: `stream` is the live stream opened above; the getters are
    // trivial reads valid on an open stream.
    let delivered_format = unsafe { AAudioStream_getFormat(stream) };
    // SAFETY: as above.
    let delivered_rate = unsafe { AAudioStream_getSampleRate(stream) };
    // SAFETY: as above.
    let delivered_channels = unsafe { AAudioStream_getChannelCount(stream) };

    let format_ok =
        delivered_format == AAUDIO_FORMAT_PCM_FLOAT || delivered_format == AAUDIO_FORMAT_PCM_I16;
    if !format_ok || delivered_rate <= 0 || !(1..=i32::from(u16::MAX)).contains(&delivered_channels)
    {
        // SAFETY: stream is live and unclosed; contexts follow the
        // ownership dance (close inside quiesces the callbacks first).
        unsafe { close_and_reclaim(stream, data_ctx, error_ctx) };
        return Err(AudioError::StreamCreationFailed {
            reason: format!(
                "AAudio delivered an unusable stream configuration \
                 (format code {}, {} Hz, {} ch); expected PCM_FLOAT or \
                 PCM_I16 with positive rate/channels",
                delivered_format, delivered_rate, delivered_channels
            ),
            context: None,
        });
    }

    let delivered = AudioFormat {
        sample_rate: delivered_rate as u32,
        // Range-checked above.
        channels: delivered_channels as u16,
        // The bridge payload is ALWAYS interleaved f32 (i16 delivery is
        // converted in the callback), so F32 is the honest report.
        sample_format: SampleFormat::F32,
    };

    // ── Complete the data context (ownership-dance step 2) ───────────
    // SAFETY: between openStream and requestStart AAudio delivers no data
    // callbacks, so this thread has exclusive access to the leaked context;
    // the error callback uses its own separate context and never touches
    // this one.
    unsafe {
        let ctx = &mut *data_ctx;
        ctx.channels = delivered.channels;
        ctx.sample_rate = delivered.sample_rate;
        ctx.format = delivered_format;
        // Pre-allocate the i16→f32 scratch from the DELIVERED shape
        // (ADR-0001: the only buffer allocation, on the setup thread). The
        // f32 path pushes the incoming slice directly and needs no scratch.
        ctx.scratch = if delivered_format == AAUDIO_FORMAT_PCM_I16 {
            Vec::with_capacity(scratch_capacity_samples(
                delivered.sample_rate,
                delivered.channels,
            ))
        } else {
            Vec::new()
        };
        // Publish the delivery format BEFORE any push so readers never see
        // the requested-format fallback once data flows (M1 pattern).
        ctx.producer.set_negotiated_format(&delivered);
    }

    // ── Start ────────────────────────────────────────────────────────
    // SAFETY: live, open stream; requestStart may be called from this
    // (setup) thread.
    let res = unsafe { AAudioStream_requestStart(stream) };
    if res != AAUDIO_OK {
        // SAFETY: stream is live and unclosed; close_and_reclaim quiesces
        // and frees everything.
        unsafe { close_and_reclaim(stream, data_ctx, error_ctx) };
        return Err(AudioError::StreamStartFailed {
            reason: format!(
                "AAudioStream_requestStart failed: {} ({})",
                result_name(res),
                res
            ),
        });
    }

    log::debug!(
        "AAudio: capture started (target={:?}, delivered {} Hz, {} ch, \
         wire format code {})",
        target,
        delivered.sample_rate,
        delivered.channels,
        delivered_format
    );

    Ok((
        AndroidPlatformStream {
            handles: Mutex::new(StreamHandles {
                stream,
                data_ctx,
                error_ctx,
            }),
            is_active,
            terminal,
        },
        delivered,
    ))
}

// ── Compile-time assertions ──────────────────────────────────────────────

/// Assert that `AndroidPlatformStream` is `Send + Sync` (required by
/// `PlatformStream` and `BridgeStream<S>`).
fn _assert_android_platform_stream_send_sync() {
    fn _assert<T: Send + Sync>() {}
    _assert::<AndroidPlatformStream>();
}

// ══════════════════════════════════════════════════════════════════════════
// Tests — pure logic only (no FFI): sample conversion, scratch sizing, and
// target classification. They compile for the Android target under `--tests`
// and will run on a future emulator job.
// ══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{ApplicationId, DeviceId, ProcessId};
    use crate::core::error::ErrorKind;

    // ── i16 → f32 conversion ─────────────────────────────────────────

    #[test]
    fn i16_conversion_maps_the_canonical_anchor_points() {
        assert_eq!(i16_sample_to_f32(0), 0.0);
        assert_eq!(i16_sample_to_f32(i16::MIN), -1.0);
        assert_eq!(i16_sample_to_f32(i16::MAX), 32_767.0 / 32_768.0);
        assert_eq!(i16_sample_to_f32(16_384), 0.5);
        assert_eq!(i16_sample_to_f32(-16_384), -0.5);
    }

    #[test]
    fn i16_conversion_output_is_always_in_unit_range() {
        for s in [i16::MIN, -1, 0, 1, i16::MAX, 12_345, -12_345] {
            let f = i16_sample_to_f32(s);
            assert!((-1.0..=1.0).contains(&f), "{s} mapped out of range: {f}");
        }
    }

    #[test]
    fn convert_into_fills_dst_in_order() {
        let src = [0i16, i16::MIN, i16::MAX, 16_384];
        let mut dst = Vec::with_capacity(4);
        assert!(convert_i16_to_f32_into(&src, &mut dst));
        assert_eq!(dst, vec![0.0, -1.0, 32_767.0 / 32_768.0, 0.5]);
    }

    #[test]
    fn convert_into_refuses_to_grow_dst() {
        // Capacity 2 but 4 samples offered → must return false and must NOT
        // reallocate (ADR-0001: drop, don't allocate).
        let src = [1i16, 2, 3, 4];
        let mut dst: Vec<f32> = Vec::with_capacity(2);
        let cap_before = dst.capacity();
        assert!(!convert_i16_to_f32_into(&src, &mut dst));
        assert_eq!(dst.capacity(), cap_before, "capacity must be untouched");
    }

    #[test]
    fn convert_into_clears_previous_contents() {
        let mut dst = Vec::with_capacity(8);
        assert!(convert_i16_to_f32_into(&[i16::MAX; 8], &mut dst));
        assert_eq!(dst.len(), 8);
        // A smaller follow-up period must fully replace, not append.
        assert!(convert_i16_to_f32_into(&[0i16, 0], &mut dst));
        assert_eq!(dst, vec![0.0, 0.0]);
    }

    #[test]
    fn convert_into_handles_empty_input() {
        let mut dst: Vec<f32> = Vec::new(); // capacity 0 is still >= 0 needed
        assert!(convert_i16_to_f32_into(&[], &mut dst));
        assert!(dst.is_empty());
    }

    // ── Scratch sizing ───────────────────────────────────────────────

    #[test]
    fn scratch_capacity_covers_generous_periods_at_common_rates() {
        for (rate, ch) in [(48_000u32, 2u16), (44_100, 1), (96_000, 2), (8_000, 1)] {
            let cap = scratch_capacity_samples(rate, ch);
            // At least the frame floor…
            assert!(cap >= SCRATCH_MIN_FRAMES * usize::from(ch));
            // …and at least 400 ms of audio at the delivered rate (we size
            // to 500 ms), far above any realistic AAudio burst.
            let worst_case_400ms = (rate as usize * 2 / 5) * usize::from(ch);
            assert!(
                cap >= worst_case_400ms,
                "scratch for {rate} Hz x {ch} ch ({cap}) must cover a 400 ms \
                 period ({worst_case_400ms})"
            );
        }
    }

    #[test]
    fn scratch_capacity_guards_degenerate_channel_count() {
        // channels == 0 is clamped to 1, never a zero-sized scratch.
        assert!(scratch_capacity_samples(48_000, 0) >= SCRATCH_MIN_FRAMES);
    }

    // ── Target classification (mic slice, ADR-0013) ──────────────────

    #[test]
    fn default_device_ids_resolve_to_unspecified() {
        for id in ["default", "DEFAULT", "Default", ""] {
            let target = CaptureTarget::Device(DeviceId(id.to_string()));
            assert_eq!(
                resolve_mic_target(&target).expect("default id must resolve"),
                AAUDIO_UNSPECIFIED,
                "id {id:?} must select the default route"
            );
        }
    }

    #[test]
    fn positive_numeric_device_id_resolves_to_that_id() {
        let target = CaptureTarget::Device(DeviceId("11".to_string()));
        assert_eq!(
            resolve_mic_target(&target).expect("positive id must resolve"),
            11
        );
    }

    #[test]
    fn non_numeric_and_non_positive_device_ids_are_device_not_found() {
        for id in ["abc", "0", "-3"] {
            let target = CaptureTarget::Device(DeviceId(id.to_string()));
            match resolve_mic_target(&target).unwrap_err() {
                AudioError::DeviceNotFound { device_id } => assert_eq!(device_id, id),
                other => panic!("expected DeviceNotFound for {id:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn system_default_is_routed_to_playback_capture_not_the_mic() {
        let err = resolve_mic_target(&CaptureTarget::SystemDefault).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Platform);
        match err {
            AudioError::PlatformNotSupported { feature, platform } => {
                assert_eq!(platform, "android");
                // The honesty pillars: what SystemDefault really means, where
                // it is served, and what this device does serve.
                assert!(
                    feature.contains("AudioPlaybackCapture"),
                    "real meaning: {feature}"
                );
                assert!(feature.contains("NOT the"), "not-the-mic: {feature}");
                assert!(
                    feature.contains("playback-capture device"),
                    "serving device: {feature}"
                );
                assert!(
                    feature.contains("Device(DeviceId(\"default\""),
                    "mic guidance: {feature}"
                );
            }
            other => panic!("expected PlatformNotSupported, got {other:?}"),
        }
    }

    #[test]
    fn per_app_targets_are_served_by_the_playback_device() {
        let targets = [
            CaptureTarget::Application(ApplicationId("1234".to_string())),
            CaptureTarget::ApplicationByName("com.example.app".to_string()),
            CaptureTarget::ProcessTree(ProcessId(1234)),
        ];
        for target in targets {
            match resolve_mic_target(&target).unwrap_err() {
                AudioError::PlatformNotSupported { feature, platform } => {
                    assert_eq!(platform, "android");
                    assert!(
                        feature.contains("playback-capture device"),
                        "must name the serving device for {target:?}: {feature}"
                    );
                    assert!(
                        feature.contains("tree ≡ app"),
                        "must document the UID equivalence for {target:?}: {feature}"
                    );
                    assert!(
                        !feature.contains("permanent"),
                        "Android playback capture is SUPPORTED, never permanent \
                         ({target:?}): {feature}"
                    );
                }
                other => panic!("expected PlatformNotSupported for {target:?}, got {other:?}"),
            }
        }
    }
}
