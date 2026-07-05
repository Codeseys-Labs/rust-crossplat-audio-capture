//! C FFI bindings for the rsac (Rust Cross-Platform Audio Capture) library.
//!
//! This crate exposes rsac's streaming-first audio capture API through a C-compatible
//! foreign function interface. All functions use opaque handle types, return error codes,
//! and are safe to call from C (null-checked, panic-caught).
//!
//! # Memory ownership convention
//!
//! - Functions returning `*mut T` transfer ownership to the caller.
//!   The caller **must** call the corresponding `rsac_*_free()` function.
//! - Functions taking `*const T` or `*mut T` borrow the handle; the caller retains ownership.
//! - String pointers returned by `rsac_error_message()`, `rsac_device_name()`, etc.
//!   are owned by this library and valid until the next call on the same thread.

#![allow(clippy::missing_safety_doc)]
#![allow(non_camel_case_types)]

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::panic::{self, AssertUnwindSafe};
use std::ptr;

// Pull the everyday capture surface (AudioBuffer, AudioCapture,
// AudioCaptureBuilder, CaptureTarget, PlatformCapabilities, …) from the prelude
// in one line (rsac-8e6c); the capture-target ID newtypes are not in the prelude
// (they are constructed only at the FFI boundary), so import them explicitly.
use rsac::prelude::*;
use rsac::{ApplicationId, DeviceId, ProcessId};

// Multi-source channel composition FFI (rsac_composition_* / rsac_group_*),
// behind the `compose` feature (forwards to rsac/compose). Header declarations
// are guarded by RSAC_FEATURE_COMPOSE — see cbindgen.toml [defines].
#[cfg(feature = "compose")]
pub mod compose;

// ── Error codes ──────────────────────────────────────────────────────────

/// Error codes returned by all rsac FFI functions.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum rsac_error_t {
    /// Operation succeeded.
    RSAC_OK = 0,
    /// A null pointer was passed where a valid pointer was expected.
    RSAC_ERROR_NULL_POINTER = 1,
    /// Invalid parameter value.
    RSAC_ERROR_INVALID_PARAMETER = 2,
    /// The requested device was not found.
    RSAC_ERROR_DEVICE_NOT_FOUND = 3,
    /// The requested feature is not supported on this platform.
    RSAC_ERROR_PLATFORM_NOT_SUPPORTED = 4,
    /// Failed to create or start an audio stream.
    RSAC_ERROR_STREAM_FAILED = 5,
    /// Failed to read audio data from the stream.
    RSAC_ERROR_STREAM_READ = 6,
    /// A configuration error occurred.
    RSAC_ERROR_CONFIGURATION = 7,
    /// The target application was not found.
    RSAC_ERROR_APPLICATION_NOT_FOUND = 8,
    /// A backend-specific error occurred.
    RSAC_ERROR_BACKEND = 9,
    /// Permission denied.
    RSAC_ERROR_PERMISSION_DENIED = 10,
    /// An operation timed out.
    RSAC_ERROR_TIMEOUT = 11,
    /// An internal or unknown error occurred.
    RSAC_ERROR_INTERNAL = 12,
    /// A Rust panic was caught (should not happen in normal use).
    RSAC_ERROR_PANIC = 99,
}

/// Device kind for enumeration.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum rsac_device_kind_t {
    RSAC_DEVICE_INPUT = 0,
    RSAC_DEVICE_OUTPUT = 1,
}

/// Sample wire/storage format, mirroring [`rsac::SampleFormat`].
///
/// All audio data is delivered as interleaved `f32` regardless of this value;
/// it describes the negotiated wire format the backend reports.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum rsac_sample_format_t {
    /// Signed 16-bit integer.
    RSAC_SAMPLE_FORMAT_I16 = 0,
    /// Signed 24-bit integer (packed in a 32-bit container).
    RSAC_SAMPLE_FORMAT_I24 = 1,
    /// Signed 32-bit integer.
    RSAC_SAMPLE_FORMAT_I32 = 2,
    /// 32-bit IEEE 754 floating-point (the library's internal standard).
    RSAC_SAMPLE_FORMAT_F32 = 3,
}

impl From<rsac::SampleFormat> for rsac_sample_format_t {
    fn from(f: rsac::SampleFormat) -> Self {
        match f {
            rsac::SampleFormat::I16 => rsac_sample_format_t::RSAC_SAMPLE_FORMAT_I16,
            rsac::SampleFormat::I24 => rsac_sample_format_t::RSAC_SAMPLE_FORMAT_I24,
            rsac::SampleFormat::I32 => rsac_sample_format_t::RSAC_SAMPLE_FORMAT_I32,
            rsac::SampleFormat::F32 => rsac_sample_format_t::RSAC_SAMPLE_FORMAT_F32,
        }
    }
}

/// A point-in-time snapshot of a capture's stream statistics.
///
/// Filled by [`rsac_capture_stream_stats`] from [`AudioCapture::stream_stats`].
/// This is a plain C-ABI value type (no heap, no free required). It mirrors the
/// counters in [`rsac::StreamStats`]; `is_running` is `1` when capturing, else `0`.
///
/// When no stream has been created (before start, or after stop) every counter
/// is `0`, `uptime_secs` is `0.0`, and `is_running` is `0`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RsacStreamStats {
    /// Buffers delivered to the consumer (popped off the ring) since start.
    pub buffers_captured: u64,
    /// Buffers dropped due to ring-buffer overflow since start.
    pub buffers_dropped: u64,
    /// Buffers enqueued by the producer (the OS audio callback) since start.
    pub buffers_pushed: u64,
    /// Ring-buffer overruns. Equal to `buffers_dropped` (retained alias).
    pub overruns: u64,
    /// How long the stream has been running, in seconds. `0.0` when not started.
    pub uptime_secs: f64,
    /// Fraction of accounted-for buffers lost to overflow, in `0.0..=1.0`.
    pub dropped_ratio: f64,
    /// `1` if the stream is currently capturing, `0` otherwise.
    pub is_running: i32,
}

/// A point-in-time **windowed** backpressure snapshot.
///
/// Filled by [`rsac_capture_backpressure_report`] from
/// [`AudioCapture::backpressure_report`]. Plain C-ABI value type (no heap, no
/// free required). Unlike the lifetime counters in [`RsacStreamStats`], the
/// `pushed`/`dropped` here cover a bounded recent window, so `drop_rate`
/// surfaces a sustained 1-in-N loss the consecutive-drop flag resets away.
///
/// When no stream has been created every field is `0`/`0.0`/`false`.
/// `window_secs` is `0.0` when the span cannot be attributed (unknown buffer
/// size or sample rate); the `pushed`/`dropped` tallies are still valid.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RsacBackpressureReport {
    /// Wall-clock span the tallies cover, in seconds. `0.0` when unattributed.
    pub window_secs: f64,
    /// Buffers successfully pushed by the producer within the window.
    pub pushed: u64,
    /// Buffers dropped due to ring-buffer overflow within the window.
    pub dropped: u64,
    /// Fraction of buffers lost within the window, in `0.0..=1.0`.
    pub drop_rate: f64,
    /// `1` if the legacy consecutive-drop flag is set, `0` otherwise.
    pub is_under_backpressure: i32,
}

/// A point-in-time snapshot of a capture's negotiated delivery format.
///
/// Filled by [`rsac_capture_format`] from [`AudioCapture::format`]. Plain C-ABI
/// value type (no heap, no free required).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RsacAudioFormat {
    /// Samples per second (e.g. 44100, 48000).
    pub sample_rate: u32,
    /// Number of audio channels (e.g. 1 mono, 2 stereo).
    pub channels: u16,
    /// The negotiated sample wire format.
    pub sample_format: rsac_sample_format_t,
    /// Bits per sample for `sample_format` (16, 24, or 32).
    pub bits_per_sample: u16,
}

// ── Thread-local error message storage ───────────────────────────────────

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_last_error(msg: &str) {
    LAST_ERROR.with(|e| {
        *e.borrow_mut() = CString::new(msg).unwrap_or_default();
    });
}

fn map_rsac_error(err: &rsac::AudioError) -> rsac_error_t {
    use rsac::AudioError;
    match err {
        AudioError::InvalidParameter { .. } => rsac_error_t::RSAC_ERROR_INVALID_PARAMETER,
        AudioError::UnsupportedFormat { .. } | AudioError::ConfigurationError { .. } => {
            rsac_error_t::RSAC_ERROR_CONFIGURATION
        }
        AudioError::DeviceNotFound { .. }
        | AudioError::DeviceNotAvailable { .. }
        | AudioError::DeviceEnumerationError { .. } => rsac_error_t::RSAC_ERROR_DEVICE_NOT_FOUND,
        AudioError::StreamCreationFailed { .. }
        | AudioError::StreamStartFailed { .. }
        | AudioError::StreamStopFailed { .. }
        // StreamEnded is the FATAL terminal-read signal (ADR-0003): the stream
        // is done and must not be retried. Map it to the (fatal) STREAM_FAILED
        // group, NOT the recoverable STREAM_READ group, so C callers can tell
        // "done" from "retry". Reuses the existing code — ABI-stable.
        | AudioError::StreamEnded { .. } => rsac_error_t::RSAC_ERROR_STREAM_FAILED,
        AudioError::StreamReadError { .. }
        | AudioError::BufferOverrun { .. }
        | AudioError::BufferUnderrun { .. } => rsac_error_t::RSAC_ERROR_STREAM_READ,
        AudioError::BackendError { .. }
        | AudioError::BackendNotAvailable { .. }
        | AudioError::BackendInitializationFailed { .. } => rsac_error_t::RSAC_ERROR_BACKEND,
        AudioError::ApplicationNotFound { .. } | AudioError::ApplicationCaptureFailed { .. } => {
            rsac_error_t::RSAC_ERROR_APPLICATION_NOT_FOUND
        }
        AudioError::PlatformNotSupported { .. } => rsac_error_t::RSAC_ERROR_PLATFORM_NOT_SUPPORTED,
        AudioError::PermissionDenied { .. } => rsac_error_t::RSAC_ERROR_PERMISSION_DENIED,
        AudioError::Timeout { .. } => rsac_error_t::RSAC_ERROR_TIMEOUT,
        AudioError::InternalError { .. } => rsac_error_t::RSAC_ERROR_INTERNAL,
        // `rsac::AudioError` is `#[non_exhaustive]`: a future variant added in a
        // minor release lands here rather than breaking the build. Map any such
        // unrecognized failure to the generic internal code so the C ABI stays
        // forward-compatible. (Every variant known at this version is matched
        // explicitly above; this arm only fires for additions.)
        _ => rsac_error_t::RSAC_ERROR_INTERNAL,
    }
}

fn handle_rsac_error(err: rsac::AudioError) -> rsac_error_t {
    let code = map_rsac_error(&err);
    set_last_error(&err.to_string());
    code
}

/// Wraps a closure in catch_unwind using AssertUnwindSafe.
///
/// FFI functions deal with raw pointers and trait objects that aren't
/// RefUnwindSafe. Since we control the entire call boundary and simply
/// return an error code on panic (never observing broken invariants),
/// AssertUnwindSafe is appropriate here.
fn catch<F>(f: F) -> rsac_error_t
where
    F: FnOnce() -> rsac_error_t,
{
    match panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(code) => code,
        Err(_) => {
            set_last_error("Rust panic caught in FFI boundary");
            rsac_error_t::RSAC_ERROR_PANIC
        }
    }
}

// ── Opaque handle types ──────────────────────────────────────────────────

/// Opaque handle to an `AudioCaptureBuilder`.
pub struct RsacBuilder {
    inner: AudioCaptureBuilder,
}

/// Opaque handle to an `AudioCapture` session.
pub struct RsacCapture {
    inner: AudioCapture,
}

/// Opaque handle to an `AudioBuffer`.
pub struct RsacAudioBuffer {
    inner: AudioBuffer,
}

/// Opaque handle to a device enumerator.
pub struct RsacDeviceEnumerator {
    inner: rsac::audio::CrossPlatformDeviceEnumerator,
}

/// Opaque handle to a single audio device.
pub struct RsacDevice {
    inner: Box<dyn rsac::AudioDevice>,
}

/// Opaque handle to a list of audio devices.
pub struct RsacDeviceList {
    inner: Vec<Box<dyn rsac::AudioDevice>>,
}

/// Opaque handle to platform capabilities.
pub struct RsacCapabilities {
    inner: PlatformCapabilities,
}

/// C callback type for audio data.
///
/// Called with:
/// - `buffer_data`: pointer to interleaved f32 sample data
/// - `num_samples`: total number of f32 values in buffer_data
/// - `channels`: number of audio channels
/// - `sample_rate`: sample rate in Hz
/// - `user_data`: opaque pointer passed to `rsac_capture_set_callback`
pub type rsac_audio_callback_t = Option<
    unsafe extern "C" fn(
        buffer_data: *const f32,
        num_samples: usize,
        channels: u16,
        sample_rate: u32,
        user_data: *mut c_void,
    ),
>;

/// Send-safe wrapper for a C callback function pointer + user_data.
///
/// # Safety
///
/// The caller guarantees that `user_data` is safe to use from any thread
/// and that the function pointer remains valid for the lifetime of this struct.
struct SendCallback {
    cb: unsafe extern "C" fn(*const f32, usize, u16, u32, *mut c_void),
    user_data: *mut c_void,
}

unsafe impl Send for SendCallback {}

impl SendCallback {
    fn new(
        cb: unsafe extern "C" fn(*const f32, usize, u16, u32, *mut c_void),
        user_data: *mut c_void,
    ) -> Self {
        Self { cb, user_data }
    }

    fn invoke(&self, buffer: &AudioBuffer) {
        let data = buffer.data();
        // Guard the FFI boundary: a panic unwinding out of the C callback (or
        // out of our call into it) and across `extern "C"` is undefined
        // behavior. Catch it here and swallow — there is no caller to return an
        // error code to on the delivery thread. See ADR-0002 (U3).
        let result = panic::catch_unwind(AssertUnwindSafe(|| unsafe {
            (self.cb)(
                data.as_ptr(),
                data.len(),
                buffer.channels(),
                buffer.sample_rate(),
                self.user_data,
            );
        }));
        if result.is_err() {
            // The panic was caught on the (non-C) delivery thread, so writing
            // the thread-local LAST_ERROR would be invisible to a C consumer
            // checking rsac_error_message() from another thread. Log it instead
            // — it is the only observable channel for a panic on this thread.
            log::error!("rsac FFI: user audio callback panicked; panic caught at FFI boundary");
        }
    }
}

// ── Error retrieval ──────────────────────────────────────────────────────

/// Returns a pointer to the last error message for the current thread.
///
/// The returned string is valid until the next rsac FFI call on this thread.
/// Returns an empty string if no error has occurred.
#[no_mangle]
pub unsafe extern "C" fn rsac_error_message() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ptr())
}

// ── Builder functions ────────────────────────────────────────────────────

/// Creates a new `AudioCaptureBuilder` with default settings.
///
/// Returns a handle that must be freed with `rsac_builder_free()`.
/// On failure, `*out` is set to null.
#[no_mangle]
pub unsafe extern "C" fn rsac_builder_new(out: *mut *mut RsacBuilder) -> rsac_error_t {
    catch(|| {
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let builder = Box::new(RsacBuilder {
            inner: AudioCaptureBuilder::new(),
        });
        unsafe { *out = Box::into_raw(builder) };
        rsac_error_t::RSAC_OK
    })
}

/// Frees a builder handle. No-op if null.
#[no_mangle]
pub unsafe extern "C" fn rsac_builder_free(builder: *mut RsacBuilder) {
    if !builder.is_null() {
        let _ = unsafe { Box::from_raw(builder) };
    }
}

/// Sets the capture target to system default audio.
#[no_mangle]
pub unsafe extern "C" fn rsac_builder_set_target_system(builder: *mut RsacBuilder) -> rsac_error_t {
    catch(|| {
        if builder.is_null() {
            set_last_error("builder is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let b = unsafe { &mut *builder };
        b.inner = b.inner.clone().with_target(CaptureTarget::SystemDefault);
        rsac_error_t::RSAC_OK
    })
}

/// Sets the capture target to a specific device by ID.
#[no_mangle]
pub unsafe extern "C" fn rsac_builder_set_target_device(
    builder: *mut RsacBuilder,
    device_id: *const c_char,
) -> rsac_error_t {
    catch(|| {
        if builder.is_null() {
            set_last_error("builder is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if device_id.is_null() {
            set_last_error("device_id is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let id_str = match unsafe { CStr::from_ptr(device_id) }.to_str() {
            Ok(s) => s,
            Err(_) => {
                set_last_error("device_id is not valid UTF-8");
                return rsac_error_t::RSAC_ERROR_INVALID_PARAMETER;
            }
        };
        let b = unsafe { &mut *builder };
        b.inner = b
            .inner
            .clone()
            .with_target(CaptureTarget::Device(DeviceId(id_str.to_string())));
        rsac_error_t::RSAC_OK
    })
}

/// Sets the capture target to an application by name.
#[no_mangle]
pub unsafe extern "C" fn rsac_builder_set_target_app_by_name(
    builder: *mut RsacBuilder,
    app_name: *const c_char,
) -> rsac_error_t {
    catch(|| {
        if builder.is_null() {
            set_last_error("builder is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if app_name.is_null() {
            set_last_error("app_name is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let name = match unsafe { CStr::from_ptr(app_name) }.to_str() {
            Ok(s) => s,
            Err(_) => {
                set_last_error("app_name is not valid UTF-8");
                return rsac_error_t::RSAC_ERROR_INVALID_PARAMETER;
            }
        };
        let b = unsafe { &mut *builder };
        b.inner = b
            .inner
            .clone()
            .with_target(CaptureTarget::ApplicationByName(name.to_string()));
        rsac_error_t::RSAC_OK
    })
}

/// Sets the capture target to an application by ID.
#[no_mangle]
pub unsafe extern "C" fn rsac_builder_set_target_app_by_id(
    builder: *mut RsacBuilder,
    app_id: *const c_char,
) -> rsac_error_t {
    catch(|| {
        if builder.is_null() {
            set_last_error("builder is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if app_id.is_null() {
            set_last_error("app_id is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let id_str = match unsafe { CStr::from_ptr(app_id) }.to_str() {
            Ok(s) => s,
            Err(_) => {
                set_last_error("app_id is not valid UTF-8");
                return rsac_error_t::RSAC_ERROR_INVALID_PARAMETER;
            }
        };
        let b = unsafe { &mut *builder };
        b.inner = b
            .inner
            .clone()
            .with_target(CaptureTarget::Application(ApplicationId(
                id_str.to_string(),
            )));
        rsac_error_t::RSAC_OK
    })
}

/// Sets the capture target to a process tree by PID.
#[no_mangle]
pub unsafe extern "C" fn rsac_builder_set_target_process_tree(
    builder: *mut RsacBuilder,
    pid: u32,
) -> rsac_error_t {
    catch(|| {
        if builder.is_null() {
            set_last_error("builder is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let b = unsafe { &mut *builder };
        b.inner = b
            .inner
            .clone()
            .with_target(CaptureTarget::ProcessTree(ProcessId(pid)));
        rsac_error_t::RSAC_OK
    })
}

/// Sets the capture target by parsing a canonical target string.
///
/// `spec` uses the [`CaptureTarget`] string grammar (case-insensitive scheme):
/// `system`, `device:<id>`, `app:<pid-or-id>`, `name:<name>`, or `tree:<pid>`.
/// Parsing goes through `CaptureTarget::from_str` (the same path
/// [`AudioCaptureBuilder::target_str`] uses), so it round-trips with
/// [`rsac::CaptureTarget`]'s `Display`.
///
/// A malformed string is reported as `RSAC_ERROR_INVALID_PARAMETER` and the
/// builder's existing target is left unchanged (parse-then-commit). This is a
/// convenience over the typed `rsac_builder_set_target_*` setters, which remain
/// available.
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `builder` or `spec` is null, and
/// `RSAC_ERROR_INVALID_PARAMETER` if `spec` is not valid UTF-8 or not a valid
/// target string.
#[no_mangle]
pub unsafe extern "C" fn rsac_builder_set_target_str(
    builder: *mut RsacBuilder,
    spec: *const c_char,
) -> rsac_error_t {
    catch(|| {
        if builder.is_null() {
            set_last_error("builder is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if spec.is_null() {
            set_last_error("spec is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let spec_str = match unsafe { CStr::from_ptr(spec) }.to_str() {
            Ok(s) => s,
            Err(_) => {
                set_last_error("spec is not valid UTF-8");
                return rsac_error_t::RSAC_ERROR_INVALID_PARAMETER;
            }
        };
        let b = unsafe { &mut *builder };
        // target_str() parses via CaptureTarget::from_str and only commits the
        // new target on success (the builder is unchanged on a parse error). It
        // returns AudioError::InvalidParameter on a bad string, which
        // map_rsac_error maps to RSAC_ERROR_INVALID_PARAMETER.
        match b.inner.clone().target_str(spec_str) {
            Ok(updated) => {
                b.inner = updated;
                rsac_error_t::RSAC_OK
            }
            Err(e) => handle_rsac_error(e),
        }
    })
}

/// Sets the desired sample rate in Hz.
#[no_mangle]
pub unsafe extern "C" fn rsac_builder_set_sample_rate(
    builder: *mut RsacBuilder,
    sample_rate: u32,
) -> rsac_error_t {
    catch(|| {
        if builder.is_null() {
            set_last_error("builder is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let b = unsafe { &mut *builder };
        b.inner = b.inner.clone().sample_rate(sample_rate);
        rsac_error_t::RSAC_OK
    })
}

/// Sets the desired number of audio channels.
#[no_mangle]
pub unsafe extern "C" fn rsac_builder_set_channels(
    builder: *mut RsacBuilder,
    channels: u16,
) -> rsac_error_t {
    catch(|| {
        if builder.is_null() {
            set_last_error("builder is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let b = unsafe { &mut *builder };
        b.inner = b.inner.clone().channels(channels);
        rsac_error_t::RSAC_OK
    })
}

/// Builds an `AudioCapture` from the builder configuration.
///
/// On success, `*out` receives the capture handle. The builder is consumed
/// and freed. On failure, `*out` is null and the builder is also consumed
/// (Rust ownership semantics — create a new builder to retry).
#[no_mangle]
pub unsafe extern "C" fn rsac_builder_build(
    builder: *mut RsacBuilder,
    out: *mut *mut RsacCapture,
) -> rsac_error_t {
    catch(|| {
        if builder.is_null() {
            set_last_error("builder is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        unsafe { *out = ptr::null_mut() };
        let b = unsafe { Box::from_raw(builder) };
        match b.inner.build() {
            Ok(capture) => {
                let handle = Box::new(RsacCapture { inner: capture });
                unsafe { *out = Box::into_raw(handle) };
                rsac_error_t::RSAC_OK
            }
            Err(e) => handle_rsac_error(e),
        }
    })
}

// ── Capture lifecycle ────────────────────────────────────────────────────

/// Starts the audio capture stream.
#[no_mangle]
pub unsafe extern "C" fn rsac_capture_start(capture: *mut RsacCapture) -> rsac_error_t {
    catch(|| {
        if capture.is_null() {
            set_last_error("capture is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let c = unsafe { &mut *capture };
        match c.inner.start() {
            Ok(()) => rsac_error_t::RSAC_OK,
            Err(e) => handle_rsac_error(e),
        }
    })
}

/// Stops the audio capture stream.
#[no_mangle]
pub unsafe extern "C" fn rsac_capture_stop(capture: *mut RsacCapture) -> rsac_error_t {
    catch(|| {
        if capture.is_null() {
            set_last_error("capture is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let c = unsafe { &mut *capture };
        match c.inner.stop() {
            Ok(()) => rsac_error_t::RSAC_OK,
            Err(e) => handle_rsac_error(e),
        }
    })
}

/// Returns 1 if the capture stream is currently running, 0 otherwise.
/// Returns -1 if the capture handle is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_capture_is_running(capture: *const RsacCapture) -> i32 {
    if capture.is_null() {
        return -1;
    }
    let c = unsafe { &*capture };
    if c.inner.is_running() {
        1
    } else {
        0
    }
}

/// Returns the number of ring buffer overruns (dropped buffers).
/// Returns 0 if the capture handle is null or no stream exists.
#[no_mangle]
pub unsafe extern "C" fn rsac_capture_overrun_count(capture: *const RsacCapture) -> u64 {
    if capture.is_null() {
        return 0;
    }
    let c = unsafe { &*capture };
    c.inner.overrun_count()
}

/// Fills `*out` with a point-in-time [`RsacStreamStats`] snapshot of the capture.
///
/// The snapshot bundles the bridge's diagnostic counters with the running state,
/// uptime, and overflow ratio. Reading it never allocates on or blocks the OS
/// audio callback thread.
///
/// When no stream has been created (before start, or after stop), `*out` is
/// filled with an all-zero snapshot (`is_running == 0`).
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `capture` or `out` is null; otherwise
/// `RSAC_OK`. `out` is an out-parameter, not a handle: there is nothing to free.
#[no_mangle]
pub unsafe extern "C" fn rsac_capture_stream_stats(
    capture: *const RsacCapture,
    out: *mut RsacStreamStats,
) -> rsac_error_t {
    catch(|| {
        if capture.is_null() {
            set_last_error("capture is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let c = unsafe { &*capture };
        let stats = c.inner.stream_stats();
        let c_stats = RsacStreamStats {
            buffers_captured: stats.buffers_captured,
            buffers_dropped: stats.buffers_dropped,
            buffers_pushed: stats.buffers_pushed,
            overruns: stats.overruns,
            uptime_secs: stats.uptime.as_secs_f64(),
            dropped_ratio: stats.dropped_ratio(),
            is_running: i32::from(stats.is_running),
        };
        unsafe { *out = c_stats };
        rsac_error_t::RSAC_OK
    })
}

/// Fills `*out` with a point-in-time **windowed** [`RsacBackpressureReport`].
///
/// Unlike [`rsac_capture_stream_stats`]' lifetime counters, this reports a
/// bounded recent window, so `drop_rate` reflects a sustained 1-in-N loss that
/// the consecutive-drop flag resets away. Reading it never allocates on or
/// blocks the OS audio callback thread.
///
/// When no stream has been created, `*out` is filled with an all-zero report.
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `capture` or `out` is null; otherwise
/// `RSAC_OK`. `out` is an out-parameter, not a handle: there is nothing to free.
#[no_mangle]
pub unsafe extern "C" fn rsac_capture_backpressure_report(
    capture: *const RsacCapture,
    out: *mut RsacBackpressureReport,
) -> rsac_error_t {
    catch(|| {
        if capture.is_null() {
            set_last_error("capture is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let c = unsafe { &*capture };
        let report = c.inner.backpressure_report();
        let c_report = RsacBackpressureReport {
            window_secs: report.window.as_secs_f64(),
            pushed: report.pushed,
            dropped: report.dropped,
            drop_rate: report.drop_rate,
            is_under_backpressure: i32::from(report.is_under_backpressure),
        };
        unsafe { *out = c_report };
        rsac_error_t::RSAC_OK
    })
}

/// Fills `*out` with the negotiated delivery [`RsacAudioFormat`] of the capture.
///
/// This is the format the backend actually produces, atomically published by the
/// bridge once a stream is created. Returns `RSAC_ERROR_STREAM_FAILED` when no
/// stream has been created yet (before start, or after stop), leaving `*out`
/// untouched — call this only on a started capture, or after checking
/// [`rsac_capture_is_running`].
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `capture` or `out` is null. `out` is an
/// out-parameter, not a handle: there is nothing to free.
#[no_mangle]
pub unsafe extern "C" fn rsac_capture_format(
    capture: *const RsacCapture,
    out: *mut RsacAudioFormat,
) -> rsac_error_t {
    catch(|| {
        if capture.is_null() {
            set_last_error("capture is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let c = unsafe { &*capture };
        match c.inner.format() {
            Some(fmt) => {
                let c_fmt = RsacAudioFormat {
                    sample_rate: fmt.sample_rate,
                    channels: fmt.channels,
                    sample_format: rsac_sample_format_t::from(fmt.sample_format),
                    bits_per_sample: fmt.sample_format.bits_per_sample(),
                };
                unsafe { *out = c_fmt };
                rsac_error_t::RSAC_OK
            }
            None => {
                set_last_error(
                    "no negotiated format available (stream not started, or already stopped)",
                );
                rsac_error_t::RSAC_ERROR_STREAM_FAILED
            }
        }
    })
}

/// Frees a capture handle. Stops the stream if running. No-op if null.
#[no_mangle]
pub unsafe extern "C" fn rsac_capture_free(capture: *mut RsacCapture) {
    if !capture.is_null() {
        let _ = unsafe { Box::from_raw(capture) };
    }
}

// ── Reading audio data ───────────────────────────────────────────────────

/// Attempts a non-blocking read of audio data.
///
/// On success with data available, `*out` receives a buffer handle.
/// On success with no data available, `*out` is set to null and RSAC_OK is returned.
/// The buffer must be freed with `rsac_audio_buffer_free()`.
#[no_mangle]
pub unsafe extern "C" fn rsac_capture_try_read(
    capture: *const RsacCapture,
    out: *mut *mut RsacAudioBuffer,
) -> rsac_error_t {
    catch(|| {
        if capture.is_null() {
            set_last_error("capture is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        unsafe { *out = ptr::null_mut() };
        // Use the TERMINAL-OBSERVABLE read path (read_chunk_nonblocking), NOT
        // read_buffer(): read_buffer() short-circuits to a *recoverable*
        // StreamReadError as soon as the stream leaves Running, so the fatal
        // StreamEnded could never cross the C ABI — and a binding pump that
        // (correctly) retries recoverable errors would spin forever after a
        // stop instead of ending. read_chunk_nonblocking drains the Stopping
        // tail and yields Err(StreamEnded) -> RSAC_ERROR_STREAM_FAILED (fatal)
        // once the ring is empty and the stream is terminal, so Go/Node pumps
        // end cleanly. `&self` shares the capture (no `&mut` alias — fixes #28).
        let c = unsafe { &*capture };
        match c.inner.read_chunk_nonblocking() {
            Ok(Some(buf)) => {
                let handle = Box::new(RsacAudioBuffer { inner: buf });
                unsafe { *out = Box::into_raw(handle) };
                rsac_error_t::RSAC_OK
            }
            Ok(None) => rsac_error_t::RSAC_OK,
            Err(e) => handle_rsac_error(e),
        }
    })
}

/// Reads audio data, blocking until data is available.
///
/// On success, `*out` receives a buffer handle that must be freed
/// with `rsac_audio_buffer_free()`.
#[no_mangle]
pub unsafe extern "C" fn rsac_capture_read(
    capture: *const RsacCapture,
    out: *mut *mut RsacAudioBuffer,
) -> rsac_error_t {
    catch(|| {
        if capture.is_null() {
            set_last_error("capture is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        unsafe { *out = ptr::null_mut() };
        // Use the TERMINAL-OBSERVABLE blocking read (read_chunk_blocking), NOT
        // read_buffer_blocking(): the latter's is_running() guard downgrades the
        // terminal StreamEnded to a recoverable StreamReadError, so a Go/Node
        // pump that retries recoverable errors would spin forever after a stop.
        // read_chunk_blocking blocks until data OR a terminal state, then yields
        // Err(StreamEnded) -> RSAC_ERROR_STREAM_FAILED (fatal). A concurrent
        // `rsac_capture_request_stop` unblocks this parked read without forming a
        // `&mut` alias to the same capture (fixes #28).
        let c = unsafe { &*capture };
        match c.inner.read_chunk_blocking() {
            Ok(buf) => {
                let handle = Box::new(RsacAudioBuffer { inner: buf });
                unsafe { *out = Box::into_raw(handle) };
                rsac_error_t::RSAC_OK
            }
            Err(e) => handle_rsac_error(e),
        }
    })
}

/// Best-effort request to stop the capture, used to **unblock a parked
/// [`rsac_capture_read`]**.
///
/// Transitions the underlying stream toward its terminal state so a thread
/// blocked in `rsac_capture_read` returns promptly (with a terminal stream
/// error) instead of waiting out the blocking-read timeout. It is idempotent
/// and a no-op when no stream has been created (or it is already stopped).
///
/// # Safety / ordering
///
/// - Takes `*const RsacCapture`: it is **safe to call concurrently with an
///   in-flight `rsac_capture_read` / `rsac_capture_try_read`** to unblock it
///   (it forms no `&mut` alias to the capture).
/// - It is **NOT** safe to call concurrently with `rsac_capture_free`. The
///   caller must order `request_stop` + a drain of in-flight reads **before**
///   freeing the handle (the sqlite3_interrupt contract).
///
/// Returns `RSAC_ERROR_NULL_POINTER` if `capture` is null, else `RSAC_OK`.
#[no_mangle]
pub unsafe extern "C" fn rsac_capture_request_stop(capture: *const RsacCapture) -> rsac_error_t {
    catch(|| {
        if capture.is_null() {
            set_last_error("capture is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let c = unsafe { &*capture };
        c.inner.request_stop();
        rsac_error_t::RSAC_OK
    })
}

// ── Callback-based capture ───────────────────────────────────────────────

/// Sets a callback for push-based audio delivery.
///
/// The callback is invoked on a background thread for each captured audio buffer.
/// Must be called before `rsac_capture_start()`.
///
/// Pass `callback = NULL` to clear the callback.
///
/// # Safety
///
/// - `user_data` is passed through to the callback unchanged; the caller is
///   responsible for its lifetime and thread safety.
/// - The callback must not call any rsac functions (to avoid deadlocks).
#[no_mangle]
pub unsafe extern "C" fn rsac_capture_set_callback(
    capture: *mut RsacCapture,
    callback: rsac_audio_callback_t,
    user_data: *mut c_void,
) -> rsac_error_t {
    catch(|| {
        if capture.is_null() {
            set_last_error("capture is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let c = unsafe { &mut *capture };

        match callback {
            Some(cb) => {
                // Build a Send-safe wrapper that owns both the fn pointer
                // and the user_data, so the closure captures only this struct.
                let wrapper = SendCallback::new(cb, user_data);
                let result = c.inner.set_callback(move |buffer: &AudioBuffer| {
                    wrapper.invoke(buffer);
                });
                match result {
                    Ok(()) => rsac_error_t::RSAC_OK,
                    Err(e) => handle_rsac_error(e),
                }
            }
            None => match c.inner.clear_callback() {
                Ok(()) => rsac_error_t::RSAC_OK,
                Err(e) => handle_rsac_error(e),
            },
        }
    })
}

// ── AudioBuffer accessors ────────────────────────────────────────────────

/// Returns a pointer to the interleaved f32 sample data.
///
/// The pointer is valid until the buffer is freed. Returns null if the buffer is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_audio_buffer_data(buffer: *const RsacAudioBuffer) -> *const f32 {
    if buffer.is_null() {
        return ptr::null();
    }
    let b = unsafe { &*buffer };
    b.inner.data().as_ptr()
}

/// Returns the total number of f32 samples in the buffer (all channels).
/// Returns 0 if the buffer is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_audio_buffer_len(buffer: *const RsacAudioBuffer) -> usize {
    if buffer.is_null() {
        return 0;
    }
    let b = unsafe { &*buffer };
    b.inner.len()
}

/// Returns the number of audio frames in the buffer.
/// Returns 0 if the buffer is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_audio_buffer_num_frames(buffer: *const RsacAudioBuffer) -> usize {
    if buffer.is_null() {
        return 0;
    }
    let b = unsafe { &*buffer };
    b.inner.num_frames()
}

/// Returns the number of channels in the buffer.
/// Returns 0 if the buffer is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_audio_buffer_channels(buffer: *const RsacAudioBuffer) -> u16 {
    if buffer.is_null() {
        return 0;
    }
    let b = unsafe { &*buffer };
    b.inner.channels()
}

/// Returns the sample rate of the buffer in Hz.
/// Returns 0 if the buffer is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_audio_buffer_sample_rate(buffer: *const RsacAudioBuffer) -> u32 {
    if buffer.is_null() {
        return 0;
    }
    let b = unsafe { &*buffer };
    b.inner.sample_rate()
}

/// Returns the root-mean-square (RMS) level across all samples/channels.
///
/// Wraps [`rsac::AudioBuffer::rms`]: `sqrt(mean(xᵢ²))` over the interleaved
/// data. Non-finite samples are skipped; a silent or empty buffer yields `0.0`
/// (never `NaN`). Read-only measurement — no allocation, RT-callback safe.
/// Returns `0.0` if the buffer is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_audio_buffer_rms(buffer: *const RsacAudioBuffer) -> f32 {
    if buffer.is_null() {
        return 0.0;
    }
    let b = unsafe { &*buffer };
    b.inner.rms()
}

/// Returns the peak (maximum absolute) level across all samples/channels.
///
/// Wraps [`rsac::AudioBuffer::peak`]: `max(|xᵢ|)`. Non-finite samples are
/// skipped; a silent or empty buffer yields `0.0` (never `NaN`). Read-only
/// measurement — no allocation. Returns `0.0` if the buffer is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_audio_buffer_peak(buffer: *const RsacAudioBuffer) -> f32 {
    if buffer.is_null() {
        return 0.0;
    }
    let b = unsafe { &*buffer };
    b.inner.peak()
}

/// Returns the RMS level in dBFS: `20 · log10(rms())`.
///
/// Wraps [`rsac::AudioBuffer::rms_dbfs`]. Returns negative infinity for silence
/// or an empty buffer, and **also** negative infinity if the buffer is null
/// (there is no level to report). Full scale (RMS `1.0`) maps to `0.0` dBFS.
#[no_mangle]
pub unsafe extern "C" fn rsac_audio_buffer_rms_dbfs(buffer: *const RsacAudioBuffer) -> f32 {
    if buffer.is_null() {
        return f32::NEG_INFINITY;
    }
    let b = unsafe { &*buffer };
    b.inner.rms_dbfs()
}

/// Returns the peak level in dBFS: `20 · log10(peak())`.
///
/// Wraps [`rsac::AudioBuffer::peak_dbfs`]. Returns negative infinity for silence
/// or an empty buffer, and **also** negative infinity if the buffer is null.
/// A full-scale signal (peak `1.0`) maps to `0.0` dBFS.
#[no_mangle]
pub unsafe extern "C" fn rsac_audio_buffer_peak_dbfs(buffer: *const RsacAudioBuffer) -> f32 {
    if buffer.is_null() {
        return f32::NEG_INFINITY;
    }
    let b = unsafe { &*buffer };
    b.inner.peak_dbfs()
}

/// Frees an audio buffer handle. No-op if null.
#[no_mangle]
pub unsafe extern "C" fn rsac_audio_buffer_free(buffer: *mut RsacAudioBuffer) {
    if !buffer.is_null() {
        let _ = unsafe { Box::from_raw(buffer) };
    }
}

// ── Device enumeration ───────────────────────────────────────────────────

/// Creates a new device enumerator.
///
/// On success, `*out` receives the enumerator handle. Must be freed with
/// `rsac_device_enumerator_free()`.
#[no_mangle]
pub unsafe extern "C" fn rsac_device_enumerator_new(
    out: *mut *mut RsacDeviceEnumerator,
) -> rsac_error_t {
    catch(|| {
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        unsafe { *out = ptr::null_mut() };
        match rsac::get_device_enumerator() {
            Ok(enumerator) => {
                let handle = Box::new(RsacDeviceEnumerator { inner: enumerator });
                unsafe { *out = Box::into_raw(handle) };
                rsac_error_t::RSAC_OK
            }
            Err(e) => handle_rsac_error(e),
        }
    })
}

/// Frees a device enumerator handle. No-op if null.
#[no_mangle]
pub unsafe extern "C" fn rsac_device_enumerator_free(enumerator: *mut RsacDeviceEnumerator) {
    if !enumerator.is_null() {
        let _ = unsafe { Box::from_raw(enumerator) };
    }
}

/// Enumerates all audio devices into a device list.
///
/// On success, `*out` receives the device list handle. Must be freed with
/// `rsac_device_list_free()`.
#[no_mangle]
pub unsafe extern "C" fn rsac_device_list_new(
    enumerator: *const RsacDeviceEnumerator,
    out: *mut *mut RsacDeviceList,
) -> rsac_error_t {
    catch(|| {
        if enumerator.is_null() {
            set_last_error("enumerator is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        unsafe { *out = ptr::null_mut() };
        let e = unsafe { &*enumerator };
        match e.inner.enumerate_devices() {
            Ok(devices) => {
                let handle = Box::new(RsacDeviceList { inner: devices });
                unsafe { *out = Box::into_raw(handle) };
                rsac_error_t::RSAC_OK
            }
            Err(e) => handle_rsac_error(e),
        }
    })
}

/// Returns the number of devices in the list.
/// Returns 0 if the list is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_device_list_count(list: *const RsacDeviceList) -> usize {
    if list.is_null() {
        return 0;
    }
    let l = unsafe { &*list };
    l.inner.len()
}

/// Gets a device from the list by index.
///
/// On success, `*out` receives a device handle that must be freed with
/// `rsac_device_free()`. Returns an error if the index is out of bounds.
///
/// The returned device is a snapshot — it carries the device's name, ID,
/// and default status, but cannot be used to create streams directly.
#[no_mangle]
pub unsafe extern "C" fn rsac_device_list_get(
    list: *const RsacDeviceList,
    index: usize,
    out: *mut *mut RsacDevice,
) -> rsac_error_t {
    catch(|| {
        if list.is_null() {
            set_last_error("list is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        unsafe { *out = ptr::null_mut() };
        let l = unsafe { &*list };
        if index >= l.inner.len() {
            set_last_error(&format!(
                "index {} out of bounds (list has {} devices)",
                index,
                l.inner.len()
            ));
            return rsac_error_t::RSAC_ERROR_INVALID_PARAMETER;
        }
        let device = &l.inner[index];
        let snapshot = Box::new(DeviceSnapshot {
            id: device.id(),
            name: device.name(),
            is_default: device.is_default(),
            supported_formats: device.supported_formats(),
        });
        let handle = Box::new(RsacDevice {
            inner: snapshot as Box<dyn rsac::AudioDevice>,
        });
        unsafe { *out = Box::into_raw(handle) };
        rsac_error_t::RSAC_OK
    })
}

/// Frees a device list handle. No-op if null.
#[no_mangle]
pub unsafe extern "C" fn rsac_device_list_free(list: *mut RsacDeviceList) {
    if !list.is_null() {
        let _ = unsafe { Box::from_raw(list) };
    }
}

/// Gets the default audio device.
///
/// On success, `*out` receives a device handle that must be freed with
/// `rsac_device_free()`.
#[no_mangle]
pub unsafe extern "C" fn rsac_default_device(
    enumerator: *const RsacDeviceEnumerator,
    kind: rsac_device_kind_t,
    out: *mut *mut RsacDevice,
) -> rsac_error_t {
    // rsac is a loopback (output) capture library: only the default OUTPUT
    // device is meaningful. Rather than silently ignoring `kind` and returning
    // the output device for an INPUT request (a lying ABI), reject any non-
    // output kind explicitly so callers get an honest error.
    catch(|| {
        if enumerator.is_null() {
            set_last_error("enumerator is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        if kind != rsac_device_kind_t::RSAC_DEVICE_OUTPUT {
            set_last_error(
                "rsac_default_device: only RSAC_DEVICE_OUTPUT is supported \
                 (rsac is a loopback capture library)",
            );
            unsafe { *out = ptr::null_mut() };
            return rsac_error_t::RSAC_ERROR_INVALID_PARAMETER;
        }
        unsafe { *out = ptr::null_mut() };
        let e = unsafe { &*enumerator };
        match e.inner.default_device() {
            Ok(device) => {
                let handle = Box::new(RsacDevice { inner: device });
                unsafe { *out = Box::into_raw(handle) };
                rsac_error_t::RSAC_OK
            }
            Err(e) => handle_rsac_error(e),
        }
    })
}

// ── Device accessors ─────────────────────────────────────────────────────

thread_local! {
    static DEVICE_STRING_BUF: RefCell<CString> = RefCell::new(CString::default());
}

/// Returns the device name as a C string.
///
/// The returned pointer is valid until the next call to `rsac_device_name()` or
/// `rsac_device_id()` on the same thread. Returns null if the device handle is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_device_name(device: *const RsacDevice) -> *const c_char {
    if device.is_null() {
        return ptr::null();
    }
    let d = unsafe { &*device };
    let name = d.inner.name();
    DEVICE_STRING_BUF.with(|buf| {
        *buf.borrow_mut() = CString::new(name).unwrap_or_default();
        buf.borrow().as_ptr()
    })
}

/// Returns the device ID as a C string.
///
/// The returned pointer is valid until the next call to `rsac_device_name()` or
/// `rsac_device_id()` on the same thread. Returns null if the device handle is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_device_id(device: *const RsacDevice) -> *const c_char {
    if device.is_null() {
        return ptr::null();
    }
    let d = unsafe { &*device };
    let id = d.inner.id().0;
    DEVICE_STRING_BUF.with(|buf| {
        *buf.borrow_mut() = CString::new(id).unwrap_or_default();
        buf.borrow().as_ptr()
    })
}

/// Returns 1 if the device is the system default, 0 otherwise.
/// Returns -1 if the device handle is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_device_is_default(device: *const RsacDevice) -> i32 {
    if device.is_null() {
        return -1;
    }
    let d = unsafe { &*device };
    if d.inner.is_default() {
        1
    } else {
        0
    }
}

/// Frees a device handle. No-op if null.
#[no_mangle]
pub unsafe extern "C" fn rsac_device_free(device: *mut RsacDevice) {
    if !device.is_null() {
        let _ = unsafe { Box::from_raw(device) };
    }
}

// ── Device snapshot (for extracting from list) ───────────────────────────

/// A snapshot of device info that implements AudioDevice for FFI extraction.
struct DeviceSnapshot {
    id: DeviceId,
    name: String,
    is_default: bool,
    supported_formats: Vec<rsac::AudioFormat>,
}

impl rsac::AudioDevice for DeviceSnapshot {
    fn id(&self) -> DeviceId {
        self.id.clone()
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn is_default(&self) -> bool {
        self.is_default
    }

    fn supported_formats(&self) -> Vec<rsac::AudioFormat> {
        self.supported_formats.clone()
    }

    fn create_stream(
        &self,
        _config: &rsac::StreamConfig,
    ) -> rsac::AudioResult<Box<dyn rsac::CapturingStream>> {
        Err(rsac::AudioError::InternalError {
            message: "Cannot create stream from a device snapshot (obtained from device list)"
                .to_string(),
            source: None,
        })
    }
}

// ── Platform capabilities ────────────────────────────────────────────────

/// Queries platform capabilities.
///
/// On success, `*out` receives a capabilities handle. Must be freed with
/// `rsac_capabilities_free()`.
#[no_mangle]
pub unsafe extern "C" fn rsac_capabilities_query(out: *mut *mut RsacCapabilities) -> rsac_error_t {
    catch(|| {
        if out.is_null() {
            set_last_error("out pointer is null");
            return rsac_error_t::RSAC_ERROR_NULL_POINTER;
        }
        let caps = PlatformCapabilities::query();
        let handle = Box::new(RsacCapabilities { inner: caps });
        unsafe { *out = Box::into_raw(handle) };
        rsac_error_t::RSAC_OK
    })
}

/// Frees a capabilities handle. No-op if null.
#[no_mangle]
pub unsafe extern "C" fn rsac_capabilities_free(caps: *mut RsacCapabilities) {
    if !caps.is_null() {
        let _ = unsafe { Box::from_raw(caps) };
    }
}

/// Returns 1 if system capture is supported, 0 otherwise.
/// Returns -1 if the handle is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_capabilities_supports_system_capture(
    caps: *const RsacCapabilities,
) -> i32 {
    if caps.is_null() {
        return -1;
    }
    let c = unsafe { &*caps };
    if c.inner.supports_system_capture {
        1
    } else {
        0
    }
}

/// Returns 1 if application capture is supported, 0 otherwise.
/// Returns -1 if the handle is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_capabilities_supports_app_capture(
    caps: *const RsacCapabilities,
) -> i32 {
    if caps.is_null() {
        return -1;
    }
    let c = unsafe { &*caps };
    if c.inner.supports_application_capture {
        1
    } else {
        0
    }
}

/// Returns 1 if process tree capture is supported, 0 otherwise.
/// Returns -1 if the handle is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_capabilities_supports_process_tree(
    caps: *const RsacCapabilities,
) -> i32 {
    if caps.is_null() {
        return -1;
    }
    let c = unsafe { &*caps };
    if c.inner.supports_process_tree_capture {
        1
    } else {
        0
    }
}

/// Returns 1 if device selection is supported, 0 otherwise.
/// Returns -1 if the handle is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_capabilities_supports_device_selection(
    caps: *const RsacCapabilities,
) -> i32 {
    if caps.is_null() {
        return -1;
    }
    let c = unsafe { &*caps };
    if c.inner.supports_device_selection {
        1
    } else {
        0
    }
}

/// Returns the maximum number of channels supported.
/// Returns 0 if the handle is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_capabilities_max_channels(caps: *const RsacCapabilities) -> u16 {
    if caps.is_null() {
        return 0;
    }
    let c = unsafe { &*caps };
    c.inner.max_channels
}

thread_local! {
    static CAPS_STRING_BUF: RefCell<CString> = RefCell::new(CString::default());
}

/// Returns the backend name (e.g., "WASAPI", "CoreAudio", "PipeWire") as a C string.
///
/// The returned pointer is valid until the next call to `rsac_capabilities_backend_name()`
/// on the same thread. Returns null if the handle is null.
#[no_mangle]
pub unsafe extern "C" fn rsac_capabilities_backend_name(
    caps: *const RsacCapabilities,
) -> *const c_char {
    if caps.is_null() {
        return ptr::null();
    }
    let c = unsafe { &*caps };
    CAPS_STRING_BUF.with(|buf| {
        *buf.borrow_mut() = CString::new(c.inner.backend_name).unwrap_or_default();
        buf.borrow().as_ptr()
    })
}

// ── Version info ─────────────────────────────────────────────────────────

/// Returns the rsac-ffi version string.
///
/// The returned pointer is a static string valid for the lifetime of the library.
#[no_mangle]
pub extern "C" fn rsac_version() -> *const c_char {
    // Use the crate's own version (kept in lockstep with the workspace by
    // scripts/bump-version.sh + ci.yml's version-lockstep gate). `concat!` keeps
    // the NUL-terminated &[u8] a valid C string with no runtime allocation.
    const VERSION: &[u8] = concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes();
    VERSION.as_ptr() as *const c_char
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::MaybeUninit;
    use std::time::Duration;

    // ── Null-pointer contract: stats/format reject null capture & out ──────

    #[test]
    fn stream_stats_rejects_null_capture() {
        let mut out = MaybeUninit::<RsacStreamStats>::uninit();
        let code = unsafe { rsac_capture_stream_stats(ptr::null(), out.as_mut_ptr()) };
        assert_eq!(code, rsac_error_t::RSAC_ERROR_NULL_POINTER);
    }

    #[test]
    fn stream_stats_rejects_null_out() {
        // A null capture is checked first, so pass a (dangling-but-non-null)
        // capture pointer to exercise the null-`out` branch specifically. The
        // pointer is never dereferenced (the null-`out` check returns first).
        let cap = ptr::dangling::<RsacCapture>();
        let code = unsafe { rsac_capture_stream_stats(cap, ptr::null_mut()) };
        assert_eq!(code, rsac_error_t::RSAC_ERROR_NULL_POINTER);
    }

    #[test]
    fn format_rejects_null_capture() {
        let mut out = MaybeUninit::<RsacAudioFormat>::uninit();
        let code = unsafe { rsac_capture_format(ptr::null(), out.as_mut_ptr()) };
        assert_eq!(code, rsac_error_t::RSAC_ERROR_NULL_POINTER);
    }

    #[test]
    fn format_rejects_null_out() {
        // Dangling-but-non-null capture: never dereferenced (null-`out` returns first).
        let cap = ptr::dangling::<RsacCapture>();
        let code = unsafe { rsac_capture_format(cap, ptr::null_mut()) };
        assert_eq!(code, rsac_error_t::RSAC_ERROR_NULL_POINTER);
    }

    // ── request_stop null contract (H2 / #28) ──────────────────────────────

    #[test]
    fn request_stop_rejects_null_capture() {
        // The unblock primitive must null-check like every other capture fn.
        let code = unsafe { rsac_capture_request_stop(ptr::null()) };
        assert_eq!(code, rsac_error_t::RSAC_ERROR_NULL_POINTER);
    }

    // ── Sample-format mapping round-trip ───────────────────────────────────

    #[test]
    fn sample_format_maps_every_variant() {
        let cases = [
            (
                rsac::SampleFormat::I16,
                rsac_sample_format_t::RSAC_SAMPLE_FORMAT_I16,
                16u16,
            ),
            (
                rsac::SampleFormat::I24,
                rsac_sample_format_t::RSAC_SAMPLE_FORMAT_I24,
                24,
            ),
            (
                rsac::SampleFormat::I32,
                rsac_sample_format_t::RSAC_SAMPLE_FORMAT_I32,
                32,
            ),
            (
                rsac::SampleFormat::F32,
                rsac_sample_format_t::RSAC_SAMPLE_FORMAT_F32,
                32,
            ),
        ];
        for (rust_fmt, c_fmt, bits) in cases {
            assert_eq!(rsac_sample_format_t::from(rust_fmt), c_fmt);
            assert_eq!(rust_fmt.bits_per_sample(), bits);
        }
    }

    // ── StreamStats → RsacStreamStats field round-trip ─────────────────────
    //
    // Mirrors the mapping inside `rsac_capture_stream_stats` without needing a
    // live audio device (which CI lacks). Constructing the capture is what
    // requires a device; the value-level translation is what this asserts.

    #[test]
    fn stream_stats_struct_round_trip() {
        let mut stats = rsac::StreamStats::default();
        stats.buffers_captured = 30;
        stats.buffers_dropped = 10;
        stats.buffers_pushed = 100;
        stats.overruns = 10;
        stats.uptime = Duration::from_millis(2500);
        stats.is_running = true;

        let c_stats = RsacStreamStats {
            buffers_captured: stats.buffers_captured,
            buffers_dropped: stats.buffers_dropped,
            buffers_pushed: stats.buffers_pushed,
            overruns: stats.overruns,
            uptime_secs: stats.uptime.as_secs_f64(),
            dropped_ratio: stats.dropped_ratio(),
            is_running: i32::from(stats.is_running),
        };

        assert_eq!(c_stats.buffers_captured, 30);
        assert_eq!(c_stats.buffers_dropped, 10);
        assert_eq!(c_stats.buffers_pushed, 100);
        assert_eq!(c_stats.overruns, 10);
        assert!((c_stats.uptime_secs - 2.5).abs() < 1e-9);
        // 10 dropped of (30 captured + 10 dropped) accounted-for == 0.25.
        assert!((c_stats.dropped_ratio - 0.25).abs() < 1e-9);
        assert_eq!(c_stats.is_running, 1);
    }

    // ── set_target_str: null/invalid-utf8/parse contract ──────────────────

    #[test]
    fn set_target_str_rejects_null_builder() {
        let spec = CString::new("system").unwrap();
        let code = unsafe { rsac_builder_set_target_str(ptr::null_mut(), spec.as_ptr()) };
        assert_eq!(code, rsac_error_t::RSAC_ERROR_NULL_POINTER);
    }

    #[test]
    fn set_target_str_rejects_null_spec() {
        // A null builder is checked first, so pass a dangling-but-non-null
        // builder to reach the null-`spec` branch (the builder is never
        // dereferenced before that check returns).
        let b = ptr::dangling_mut::<RsacBuilder>();
        let code = unsafe { rsac_builder_set_target_str(b, ptr::null()) };
        assert_eq!(code, rsac_error_t::RSAC_ERROR_NULL_POINTER);
    }

    #[test]
    fn set_target_str_accepts_valid_spec() {
        let mut builder: *mut RsacBuilder = ptr::null_mut();
        assert_eq!(
            unsafe { rsac_builder_new(&mut builder) },
            rsac_error_t::RSAC_OK
        );
        assert!(!builder.is_null());
        let spec = CString::new("name:Firefox").unwrap();
        let code = unsafe { rsac_builder_set_target_str(builder, spec.as_ptr()) };
        assert_eq!(code, rsac_error_t::RSAC_OK);
        unsafe { rsac_builder_free(builder) };
    }

    #[test]
    fn set_target_str_rejects_garbage_spec() {
        let mut builder: *mut RsacBuilder = ptr::null_mut();
        assert_eq!(
            unsafe { rsac_builder_new(&mut builder) },
            rsac_error_t::RSAC_OK
        );
        // An unknown scheme is not a valid target string; map to INVALID_PARAMETER.
        let spec = CString::new("not-a-real-scheme:whatever").unwrap();
        let code = unsafe { rsac_builder_set_target_str(builder, spec.as_ptr()) };
        assert_eq!(code, rsac_error_t::RSAC_ERROR_INVALID_PARAMETER);
        unsafe { rsac_builder_free(builder) };
    }

    // ── AudioBuffer metering accessors: null + synthetic-signal values ─────

    #[test]
    fn buffer_metering_rejects_null() {
        assert_eq!(unsafe { rsac_audio_buffer_rms(ptr::null()) }, 0.0);
        assert_eq!(unsafe { rsac_audio_buffer_peak(ptr::null()) }, 0.0);
        assert_eq!(
            unsafe { rsac_audio_buffer_rms_dbfs(ptr::null()) },
            f32::NEG_INFINITY
        );
        assert_eq!(
            unsafe { rsac_audio_buffer_peak_dbfs(ptr::null()) },
            f32::NEG_INFINITY
        );
    }

    #[test]
    fn buffer_metering_full_scale_signal() {
        // A constant ±1.0 signal: RMS == 1.0, peak == 1.0, both 0.0 dBFS.
        let buf = RsacAudioBuffer {
            inner: rsac::AudioBuffer::new(vec![1.0, -1.0, 1.0, -1.0], 2, 48_000),
        };
        let p: *const RsacAudioBuffer = &buf;
        assert!((unsafe { rsac_audio_buffer_rms(p) } - 1.0).abs() < 1e-6);
        assert!((unsafe { rsac_audio_buffer_peak(p) } - 1.0).abs() < 1e-6);
        assert!(unsafe { rsac_audio_buffer_rms_dbfs(p) }.abs() < 1e-4);
        assert!(unsafe { rsac_audio_buffer_peak_dbfs(p) }.abs() < 1e-4);
    }

    #[test]
    fn buffer_metering_silence_is_neg_infinity_dbfs() {
        let buf = RsacAudioBuffer {
            inner: rsac::AudioBuffer::new(vec![0.0; 8], 2, 48_000),
        };
        let p: *const RsacAudioBuffer = &buf;
        assert_eq!(unsafe { rsac_audio_buffer_rms(p) }, 0.0);
        assert_eq!(unsafe { rsac_audio_buffer_peak(p) }, 0.0);
        assert_eq!(unsafe { rsac_audio_buffer_rms_dbfs(p) }, f32::NEG_INFINITY);
        assert_eq!(unsafe { rsac_audio_buffer_peak_dbfs(p) }, f32::NEG_INFINITY);
    }

    #[test]
    fn stream_stats_default_is_zeroed() {
        let stats = rsac::StreamStats::default();
        let c_stats = RsacStreamStats {
            buffers_captured: stats.buffers_captured,
            buffers_dropped: stats.buffers_dropped,
            buffers_pushed: stats.buffers_pushed,
            overruns: stats.overruns,
            uptime_secs: stats.uptime.as_secs_f64(),
            dropped_ratio: stats.dropped_ratio(),
            is_running: i32::from(stats.is_running),
        };
        assert_eq!(c_stats.buffers_captured, 0);
        assert_eq!(c_stats.buffers_dropped, 0);
        assert_eq!(c_stats.buffers_pushed, 0);
        assert_eq!(c_stats.overruns, 0);
        assert_eq!(c_stats.uptime_secs, 0.0);
        assert_eq!(c_stats.dropped_ratio, 0.0);
        assert_eq!(c_stats.is_running, 0);
    }
}
