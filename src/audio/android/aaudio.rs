//! Minimal in-tree FFI bindings for the stable **AAudio** NDK ABI (mic
//! slice, rsac-20cd).
//!
//! AAudio is Android's native (C) audio API, available since API 26 and part
//! of the NDK's stable ABI (`libaaudio.so`). rsac binds it directly instead
//! of pulling a binding crate: the mic slice needs a handful of functions,
//! and an in-tree `extern "C"` block keeps the dependency tree unchanged
//! (see `Cargo.toml` — the Android backend deliberately adds **no** crate
//! dependencies).
//!
//! # Scope
//!
//! Only the declarations the mic slice actually uses are bound — builder
//! creation/configuration, open/start/stop/close, the delivered-format
//! getters, and the state-wait pair the teardown path uses to quiesce the
//! callback before reclaiming its context. Every type, constant, and
//! function is documented against its exact `aaudio/AAudio.h` name and value
//! so the block can be audited against the NDK header 1:1.
//!
//! # Verification status
//!
//! These declarations are checked against the NDK r27 `aaudio/AAudio.h`
//! header *by eye* and compile-checked for `aarch64-linux-android`; they are
//! **not yet exercised on a device or emulator** (no NDK link step runs in
//! this repo's Windows-host check). Values are pinned by unit tests below so
//! an accidental edit cannot silently drift.

#![cfg(all(target_os = "android", feature = "feat_android"))]
// NDK-name fidelity: the types, constants, and functions below keep their
// exact `aaudio/AAudio.h` spellings so every declaration can be diffed
// against the header. The C naming conventions (`aaudio_result_t`,
// `AAudioStream_dataCallback`) trip Rust's type-case lint, so it is allowed
// for this FFI module only.
#![allow(non_camel_case_types)]

use std::ffi::c_void;

use crate::core::error::{AudioError, BackendContext};

// ── Opaque handle types ──────────────────────────────────────────────────

/// Opaque AAudio stream handle — `AAudioStream` in `aaudio/AAudio.h`.
///
/// Zero-sized, non-constructible opaque-FFI pattern (the form recommended by
/// the Rustonomicon): rsac only ever holds `*mut AAudioStream` values
/// returned by [`AAudioStreamBuilder_openStream`] and passes them back to
/// the `AAudioStream_*` functions.
#[repr(C)]
pub struct AAudioStream {
    _private: [u8; 0],
}

/// Opaque AAudio stream-builder handle — `AAudioStreamBuilder` in
/// `aaudio/AAudio.h`.
///
/// Created by [`AAudio_createStreamBuilder`], consumed by
/// [`AAudioStreamBuilder_openStream`], and released with
/// [`AAudioStreamBuilder_delete`].
#[repr(C)]
pub struct AAudioStreamBuilder {
    _private: [u8; 0],
}

// ── Scalar typedefs (all `int32_t` in the header) ────────────────────────

/// `aaudio_result_t` — the AAudio result/error code type (`int32_t`).
/// [`AAUDIO_OK`] (0) is success; errors are negative (see the
/// `AAUDIO_ERROR_*` constants).
pub type aaudio_result_t = i32;

/// `aaudio_direction_t` — stream direction (`int32_t`).
/// See [`AAUDIO_DIRECTION_INPUT`].
pub type aaudio_direction_t = i32;

/// `aaudio_format_t` — sample data format (`int32_t`).
/// See [`AAUDIO_FORMAT_PCM_I16`] / [`AAUDIO_FORMAT_PCM_FLOAT`].
pub type aaudio_format_t = i32;

/// `aaudio_performance_mode_t` — performance-mode hint (`int32_t`).
/// See [`AAUDIO_PERFORMANCE_MODE_LOW_LATENCY`].
pub type aaudio_performance_mode_t = i32;

/// `aaudio_data_callback_result_t` — value a data callback returns to keep
/// or stop the callback stream (`int32_t`). See
/// [`AAUDIO_CALLBACK_RESULT_CONTINUE`] / [`AAUDIO_CALLBACK_RESULT_STOP`].
pub type aaudio_data_callback_result_t = i32;

/// `aaudio_stream_state_t` — stream lifecycle state (`int32_t`).
/// Only the states the teardown quiesce-loop inspects are declared (see
/// [`AAUDIO_STREAM_STATE_STOPPING`] and siblings).
pub type aaudio_stream_state_t = i32;

// ── Constants (each documented against its NDK header name) ─────────────

/// `AAUDIO_OK` = 0 — the success value of [`aaudio_result_t`].
pub const AAUDIO_OK: aaudio_result_t = 0;

/// `AAUDIO_UNSPECIFIED` = 0 — "let AAudio choose" sentinel accepted by the
/// builder's sample-rate / channel-count setters (and, as
/// [`AAUDIO_FORMAT_UNSPECIFIED`], the format setter).
pub const AAUDIO_UNSPECIFIED: i32 = 0;

// aaudio_result_t error values. The header anchors the block at
// `AAUDIO_ERROR_BASE` (-900) and counts up, skipping reserved slots; the
// effective values below match NDK r27's `aaudio/AAudio.h`.

/// `AAUDIO_ERROR_DISCONNECTED` = -899 — the stream's device is gone
/// (unplugged headset, route change without automatic recovery). This is the
/// code the error callback most commonly delivers.
pub const AAUDIO_ERROR_DISCONNECTED: aaudio_result_t = -899;
/// `AAUDIO_ERROR_ILLEGAL_ARGUMENT` = -898.
pub const AAUDIO_ERROR_ILLEGAL_ARGUMENT: aaudio_result_t = -898;
/// `AAUDIO_ERROR_INTERNAL` = -896.
pub const AAUDIO_ERROR_INTERNAL: aaudio_result_t = -896;
/// `AAUDIO_ERROR_INVALID_STATE` = -895.
pub const AAUDIO_ERROR_INVALID_STATE: aaudio_result_t = -895;
/// `AAUDIO_ERROR_INVALID_HANDLE` = -892.
pub const AAUDIO_ERROR_INVALID_HANDLE: aaudio_result_t = -892;
/// `AAUDIO_ERROR_UNIMPLEMENTED` = -890.
pub const AAUDIO_ERROR_UNIMPLEMENTED: aaudio_result_t = -890;
/// `AAUDIO_ERROR_UNAVAILABLE` = -889.
pub const AAUDIO_ERROR_UNAVAILABLE: aaudio_result_t = -889;
/// `AAUDIO_ERROR_NO_FREE_HANDLES` = -888.
pub const AAUDIO_ERROR_NO_FREE_HANDLES: aaudio_result_t = -888;
/// `AAUDIO_ERROR_NO_MEMORY` = -887.
pub const AAUDIO_ERROR_NO_MEMORY: aaudio_result_t = -887;
/// `AAUDIO_ERROR_NULL` = -886.
pub const AAUDIO_ERROR_NULL: aaudio_result_t = -886;
/// `AAUDIO_ERROR_TIMEOUT` = -885.
pub const AAUDIO_ERROR_TIMEOUT: aaudio_result_t = -885;
/// `AAUDIO_ERROR_WOULD_BLOCK` = -884.
pub const AAUDIO_ERROR_WOULD_BLOCK: aaudio_result_t = -884;
/// `AAUDIO_ERROR_INVALID_FORMAT` = -883.
pub const AAUDIO_ERROR_INVALID_FORMAT: aaudio_result_t = -883;
/// `AAUDIO_ERROR_OUT_OF_RANGE` = -882.
pub const AAUDIO_ERROR_OUT_OF_RANGE: aaudio_result_t = -882;
/// `AAUDIO_ERROR_NO_SERVICE` = -881 — the AAudio service is not running.
pub const AAUDIO_ERROR_NO_SERVICE: aaudio_result_t = -881;
/// `AAUDIO_ERROR_INVALID_RATE` = -880 — the requested sample rate is not
/// supported (relevant to the open-with-requested-rate first attempt).
pub const AAUDIO_ERROR_INVALID_RATE: aaudio_result_t = -880;

/// `AAUDIO_DIRECTION_INPUT` = 1 — capture (recording) stream
/// (`AAUDIO_DIRECTION_OUTPUT` = 0 is playback; not declared — unused).
pub const AAUDIO_DIRECTION_INPUT: aaudio_direction_t = 1;

/// `AAUDIO_FORMAT_UNSPECIFIED` = 0 — let AAudio pick the device-native
/// sample format (used by the second open attempt).
pub const AAUDIO_FORMAT_UNSPECIFIED: aaudio_format_t = 0;
/// `AAUDIO_FORMAT_PCM_I16` = 1 — interleaved signed 16-bit PCM.
pub const AAUDIO_FORMAT_PCM_I16: aaudio_format_t = 1;
/// `AAUDIO_FORMAT_PCM_FLOAT` = 2 — interleaved 32-bit float PCM (rsac's
/// preferred delivery; requested on the first open attempt).
pub const AAUDIO_FORMAT_PCM_FLOAT: aaudio_format_t = 2;

/// `AAUDIO_PERFORMANCE_MODE_LOW_LATENCY` = 12 — request the low-latency
/// (potentially real-time-thread) data path. This is why the data callback
/// obeys the full ADR-0001 rules: with this mode AAudio may invoke it on a
/// real-time thread.
pub const AAUDIO_PERFORMANCE_MODE_LOW_LATENCY: aaudio_performance_mode_t = 12;

/// `AAUDIO_CALLBACK_RESULT_CONTINUE` = 0 — keep delivering data callbacks.
pub const AAUDIO_CALLBACK_RESULT_CONTINUE: aaudio_data_callback_result_t = 0;
/// `AAUDIO_CALLBACK_RESULT_STOP` = 1 — stop delivering data callbacks (the
/// data callback returns this once the bridge has reached a terminal state).
pub const AAUDIO_CALLBACK_RESULT_STOP: aaudio_data_callback_result_t = 1;

// aaudio_stream_state_t values (header enum, counting from
// AAUDIO_STREAM_STATE_UNINITIALIZED = 0). Only the three *transitional*
// states the teardown quiesce-loop must wait out are declared.

/// `AAUDIO_STREAM_STATE_STARTING` = 3 — start requested, not yet running.
pub const AAUDIO_STREAM_STATE_STARTING: aaudio_stream_state_t = 3;
/// `AAUDIO_STREAM_STATE_STARTED` = 4 — running; data callbacks are firing.
pub const AAUDIO_STREAM_STATE_STARTED: aaudio_stream_state_t = 4;
/// `AAUDIO_STREAM_STATE_STOPPING` = 9 — stop requested, callbacks may still
/// be in flight until the state advances past this.
pub const AAUDIO_STREAM_STATE_STOPPING: aaudio_stream_state_t = 9;

// ── Callback typedefs ────────────────────────────────────────────────────

/// `AAudioStream_dataCallback` — invoked by AAudio with each period of
/// captured audio. With [`AAUDIO_PERFORMANCE_MODE_LOW_LATENCY`] this may run
/// on a **real-time** thread: the callback must not allocate, lock, or
/// panic (ADR-0001). `Option<..>` is the nullable-function-pointer FFI form;
/// rsac always passes `Some`.
///
/// C signature:
/// `aaudio_data_callback_result_t (*)(AAudioStream*, void* userData,
/// void* audioData, int32_t numFrames)`.
pub type AAudioStream_dataCallback = Option<
    unsafe extern "C" fn(
        stream: *mut AAudioStream,
        user_data: *mut c_void,
        audio_data: *mut c_void,
        num_frames: i32,
    ) -> aaudio_data_callback_result_t,
>;

/// `AAudioStream_errorCallback` — invoked by AAudio (on a dedicated
/// non-real-time thread it creates) when the stream can no longer function,
/// e.g. [`AAUDIO_ERROR_DISCONNECTED`]. The stream must not be closed from
/// inside this callback.
///
/// C signature:
/// `void (*)(AAudioStream*, void* userData, aaudio_result_t error)`.
pub type AAudioStream_errorCallback = Option<
    unsafe extern "C" fn(stream: *mut AAudioStream, user_data: *mut c_void, error: aaudio_result_t),
>;

// ── Foreign functions ────────────────────────────────────────────────────

#[link(name = "aaudio")]
#[allow(non_snake_case)]
extern "C" {
    /// `AAudio_createStreamBuilder(AAudioStreamBuilder** builder)` — allocate
    /// a new stream builder. On [`AAUDIO_OK`] the out-pointer receives a
    /// non-null builder that must be released with
    /// [`AAudioStreamBuilder_delete`].
    pub fn AAudio_createStreamBuilder(builder: *mut *mut AAudioStreamBuilder) -> aaudio_result_t;

    /// `AAudioStreamBuilder_setDirection` — request capture
    /// ([`AAUDIO_DIRECTION_INPUT`]) or playback.
    pub fn AAudioStreamBuilder_setDirection(
        builder: *mut AAudioStreamBuilder,
        direction: aaudio_direction_t,
    );

    /// `AAudioStreamBuilder_setFormat` — request a sample format
    /// ([`AAUDIO_FORMAT_PCM_FLOAT`] / [`AAUDIO_FORMAT_PCM_I16`] /
    /// [`AAUDIO_FORMAT_UNSPECIFIED`]). The delivered format is read back
    /// after open via [`AAudioStream_getFormat`].
    pub fn AAudioStreamBuilder_setFormat(
        builder: *mut AAudioStreamBuilder,
        format: aaudio_format_t,
    );

    /// `AAudioStreamBuilder_setSampleRate` — request a sample rate in Hz, or
    /// [`AAUDIO_UNSPECIFIED`] for the device-native rate.
    pub fn AAudioStreamBuilder_setSampleRate(builder: *mut AAudioStreamBuilder, sample_rate: i32);

    /// `AAudioStreamBuilder_setChannelCount` — request a channel count, or
    /// [`AAUDIO_UNSPECIFIED`] for the device-native count.
    pub fn AAudioStreamBuilder_setChannelCount(
        builder: *mut AAudioStreamBuilder,
        channel_count: i32,
    );

    /// `AAudioStreamBuilder_setDeviceId` — route capture to a specific device
    /// id (the `AudioDeviceInfo.getId()` value from the AAR's
    /// `AudioManager.getDevices` list), or [`AAUDIO_UNSPECIFIED`] for the
    /// default input route. Stable AAudio ABI since API 26 (below the
    /// module's `minSdk 29`), so unconditionally linkable.
    pub fn AAudioStreamBuilder_setDeviceId(builder: *mut AAudioStreamBuilder, device_id: i32);

    /// `AAudioStreamBuilder_setPerformanceMode` — request
    /// [`AAUDIO_PERFORMANCE_MODE_LOW_LATENCY`] (or another mode hint).
    pub fn AAudioStreamBuilder_setPerformanceMode(
        builder: *mut AAudioStreamBuilder,
        mode: aaudio_performance_mode_t,
    );

    /// `AAudioStreamBuilder_setDataCallback` — register the per-period data
    /// callback and its opaque `user_data` pointer (delivered back verbatim
    /// on every invocation).
    pub fn AAudioStreamBuilder_setDataCallback(
        builder: *mut AAudioStreamBuilder,
        callback: AAudioStream_dataCallback,
        user_data: *mut c_void,
    );

    /// `AAudioStreamBuilder_setErrorCallback` — register the stream-error
    /// callback and its (independent) opaque `user_data` pointer.
    pub fn AAudioStreamBuilder_setErrorCallback(
        builder: *mut AAudioStreamBuilder,
        callback: AAudioStream_errorCallback,
        user_data: *mut c_void,
    );

    /// `AAudioStreamBuilder_openStream(AAudioStreamBuilder*, AAudioStream**)`
    /// — open a stream with the builder's current configuration. On
    /// [`AAUDIO_OK`] the out-pointer receives a non-null stream. The builder
    /// remains reusable (and must still be deleted).
    pub fn AAudioStreamBuilder_openStream(
        builder: *mut AAudioStreamBuilder,
        stream: *mut *mut AAudioStream,
    ) -> aaudio_result_t;

    /// `AAudioStreamBuilder_delete` — release the builder. Safe to call once
    /// the stream is open (or after a failed open).
    pub fn AAudioStreamBuilder_delete(builder: *mut AAudioStreamBuilder) -> aaudio_result_t;

    /// `AAudioStream_requestStart` — asynchronously start the stream; data
    /// callbacks begin after this.
    pub fn AAudioStream_requestStart(stream: *mut AAudioStream) -> aaudio_result_t;

    /// `AAudioStream_requestStop` — asynchronously stop the stream (drains,
    /// then ceases data callbacks).
    pub fn AAudioStream_requestStop(stream: *mut AAudioStream) -> aaudio_result_t;

    /// `AAudioStream_close` — stop (if needed) and delete the stream's
    /// internal data structures. The stream pointer is invalid afterwards.
    /// Must not be called from within one of the stream's own callbacks.
    pub fn AAudioStream_close(stream: *mut AAudioStream) -> aaudio_result_t;

    /// `AAudioStream_getFormat` — the **actual** sample format of the open
    /// stream (may differ from the requested one).
    pub fn AAudioStream_getFormat(stream: *mut AAudioStream) -> aaudio_format_t;

    /// `AAudioStream_getSampleRate` — the **actual** sample rate in Hz of
    /// the open stream.
    pub fn AAudioStream_getSampleRate(stream: *mut AAudioStream) -> i32;

    /// `AAudioStream_getChannelCount` — the **actual** channel count of the
    /// open stream.
    pub fn AAudioStream_getChannelCount(stream: *mut AAudioStream) -> i32;

    /// `AAudioStream_getState` — the stream's current lifecycle state.
    pub fn AAudioStream_getState(stream: *mut AAudioStream) -> aaudio_stream_state_t;

    /// `AAudioStream_waitForStateChange(AAudioStream*, aaudio_stream_state_t
    /// inputState, aaudio_stream_state_t* nextState, int64_t
    /// timeoutNanoseconds)` — block (bounded by the timeout) until the state
    /// differs from `input_state`, writing the new state to `next_state`.
    /// Used by the teardown path to quiesce callbacks before the callback
    /// context is reclaimed.
    pub fn AAudioStream_waitForStateChange(
        stream: *mut AAudioStream,
        input_state: aaudio_stream_state_t,
        next_state: *mut aaudio_stream_state_t,
        timeout_nanoseconds: i64,
    ) -> aaudio_result_t;
}

// ── Result helpers ───────────────────────────────────────────────────────

/// Returns the `aaudio/AAudio.h` constant name for a known
/// [`aaudio_result_t`], or a fixed placeholder for unknown codes.
///
/// Pure lookup — no allocation — so it is also safe to call when formatting
/// diagnostics from the (non-real-time) error-callback thread.
pub(crate) fn result_name(code: aaudio_result_t) -> &'static str {
    match code {
        AAUDIO_OK => "AAUDIO_OK",
        AAUDIO_ERROR_DISCONNECTED => "AAUDIO_ERROR_DISCONNECTED",
        AAUDIO_ERROR_ILLEGAL_ARGUMENT => "AAUDIO_ERROR_ILLEGAL_ARGUMENT",
        AAUDIO_ERROR_INTERNAL => "AAUDIO_ERROR_INTERNAL",
        AAUDIO_ERROR_INVALID_STATE => "AAUDIO_ERROR_INVALID_STATE",
        AAUDIO_ERROR_INVALID_HANDLE => "AAUDIO_ERROR_INVALID_HANDLE",
        AAUDIO_ERROR_UNIMPLEMENTED => "AAUDIO_ERROR_UNIMPLEMENTED",
        AAUDIO_ERROR_UNAVAILABLE => "AAUDIO_ERROR_UNAVAILABLE",
        AAUDIO_ERROR_NO_FREE_HANDLES => "AAUDIO_ERROR_NO_FREE_HANDLES",
        AAUDIO_ERROR_NO_MEMORY => "AAUDIO_ERROR_NO_MEMORY",
        AAUDIO_ERROR_NULL => "AAUDIO_ERROR_NULL",
        AAUDIO_ERROR_TIMEOUT => "AAUDIO_ERROR_TIMEOUT",
        AAUDIO_ERROR_WOULD_BLOCK => "AAUDIO_ERROR_WOULD_BLOCK",
        AAUDIO_ERROR_INVALID_FORMAT => "AAUDIO_ERROR_INVALID_FORMAT",
        AAUDIO_ERROR_OUT_OF_RANGE => "AAUDIO_ERROR_OUT_OF_RANGE",
        AAUDIO_ERROR_NO_SERVICE => "AAUDIO_ERROR_NO_SERVICE",
        AAUDIO_ERROR_INVALID_RATE => "AAUDIO_ERROR_INVALID_RATE",
        _ => "unknown aaudio_result_t",
    }
}

/// Maps a failed AAudio call to a categorized [`AudioError`].
///
/// `operation` is the NDK function that failed (e.g.
/// `"AAudioStream_requestStart"`); `code` is the returned
/// [`aaudio_result_t`]. Produces [`AudioError::BackendError`] (kind
/// `Backend`, recoverability `TransientRetry` per the existing taxonomy)
/// with a [`BackendContext`] carrying the raw code and its header name, so
/// diagnostics survive across the API boundary.
pub(crate) fn result_to_error(operation: &str, code: aaudio_result_t) -> AudioError {
    AudioError::BackendError {
        backend: "aaudio".to_string(),
        operation: operation.to_string(),
        message: format!("{} ({})", result_name(code), code),
        context: Some(BackendContext {
            backend_name: "AAudio".to_string(),
            os_error_code: Some(i64::from(code)),
            os_error_message: Some(result_name(code).to_string()),
        }),
    }
}

// ══════════════════════════════════════════════════════════════════════════
// Tests — pure logic only (no FFI calls): constant values pinned against the
// NDK header and the error-mapping helpers. They compile for the Android
// target under `--tests` and will run on a future emulator job.
// ══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins every declared constant to its `aaudio/AAudio.h` value so an
    /// accidental edit cannot silently drift from the stable ABI.
    #[test]
    fn constants_match_ndk_header_values() {
        assert_eq!(AAUDIO_OK, 0);
        assert_eq!(AAUDIO_UNSPECIFIED, 0);

        assert_eq!(AAUDIO_ERROR_DISCONNECTED, -899);
        assert_eq!(AAUDIO_ERROR_ILLEGAL_ARGUMENT, -898);
        assert_eq!(AAUDIO_ERROR_INTERNAL, -896);
        assert_eq!(AAUDIO_ERROR_INVALID_STATE, -895);
        assert_eq!(AAUDIO_ERROR_INVALID_HANDLE, -892);
        assert_eq!(AAUDIO_ERROR_UNIMPLEMENTED, -890);
        assert_eq!(AAUDIO_ERROR_UNAVAILABLE, -889);
        assert_eq!(AAUDIO_ERROR_NO_FREE_HANDLES, -888);
        assert_eq!(AAUDIO_ERROR_NO_MEMORY, -887);
        assert_eq!(AAUDIO_ERROR_NULL, -886);
        assert_eq!(AAUDIO_ERROR_TIMEOUT, -885);
        assert_eq!(AAUDIO_ERROR_WOULD_BLOCK, -884);
        assert_eq!(AAUDIO_ERROR_INVALID_FORMAT, -883);
        assert_eq!(AAUDIO_ERROR_OUT_OF_RANGE, -882);
        assert_eq!(AAUDIO_ERROR_NO_SERVICE, -881);
        assert_eq!(AAUDIO_ERROR_INVALID_RATE, -880);

        assert_eq!(AAUDIO_DIRECTION_INPUT, 1);

        assert_eq!(AAUDIO_FORMAT_UNSPECIFIED, 0);
        assert_eq!(AAUDIO_FORMAT_PCM_I16, 1);
        assert_eq!(AAUDIO_FORMAT_PCM_FLOAT, 2);

        assert_eq!(AAUDIO_PERFORMANCE_MODE_LOW_LATENCY, 12);

        assert_eq!(AAUDIO_CALLBACK_RESULT_CONTINUE, 0);
        assert_eq!(AAUDIO_CALLBACK_RESULT_STOP, 1);

        assert_eq!(AAUDIO_STREAM_STATE_STARTING, 3);
        assert_eq!(AAUDIO_STREAM_STATE_STARTED, 4);
        assert_eq!(AAUDIO_STREAM_STATE_STOPPING, 9);
    }

    #[test]
    fn result_name_maps_known_and_unknown_codes() {
        assert_eq!(result_name(AAUDIO_OK), "AAUDIO_OK");
        assert_eq!(
            result_name(AAUDIO_ERROR_DISCONNECTED),
            "AAUDIO_ERROR_DISCONNECTED"
        );
        assert_eq!(
            result_name(AAUDIO_ERROR_NO_SERVICE),
            "AAUDIO_ERROR_NO_SERVICE"
        );
        assert_eq!(result_name(12345), "unknown aaudio_result_t");
        assert_eq!(result_name(-1), "unknown aaudio_result_t");
    }

    #[test]
    fn result_to_error_carries_operation_code_and_context() {
        let err = result_to_error("AAudioStream_requestStart", AAUDIO_ERROR_DISCONNECTED);
        match err {
            AudioError::BackendError {
                backend,
                operation,
                message,
                context,
            } => {
                assert_eq!(backend, "aaudio");
                assert_eq!(operation, "AAudioStream_requestStart");
                assert!(message.contains("AAUDIO_ERROR_DISCONNECTED"), "{message}");
                assert!(message.contains("-899"), "{message}");
                let ctx = context.expect("BackendContext must be populated");
                assert_eq!(ctx.backend_name, "AAudio");
                assert_eq!(ctx.os_error_code, Some(-899));
                assert_eq!(
                    ctx.os_error_message.as_deref(),
                    Some("AAUDIO_ERROR_DISCONNECTED")
                );
            }
            other => panic!("expected BackendError, got {other:?}"),
        }
    }
}
