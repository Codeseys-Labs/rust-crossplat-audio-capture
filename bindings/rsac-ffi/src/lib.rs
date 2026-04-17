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

use rsac::{
    ApplicationId, AudioBuffer, AudioCapture, AudioCaptureBuilder, CaptureTarget, DeviceId,
    PlatformCapabilities, ProcessId,
};

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
        | AudioError::StreamStopFailed { .. } => rsac_error_t::RSAC_ERROR_STREAM_FAILED,
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
        unsafe {
            (self.cb)(
                data.as_ptr(),
                data.len(),
                buffer.channels(),
                buffer.sample_rate(),
                self.user_data,
            );
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
    capture: *mut RsacCapture,
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
        let c = unsafe { &mut *capture };
        match c.inner.read_buffer() {
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
    capture: *mut RsacCapture,
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
        let c = unsafe { &mut *capture };
        match c.inner.read_buffer_blocking() {
            Ok(buf) => {
                let handle = Box::new(RsacAudioBuffer { inner: buf });
                unsafe { *out = Box::into_raw(handle) };
                rsac_error_t::RSAC_OK
            }
            Err(e) => handle_rsac_error(e),
        }
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
    _kind: rsac_device_kind_t,
    out: *mut *mut RsacDevice,
) -> rsac_error_t {
    // NOTE: The `_kind` parameter is currently ignored. All platform
    // backends return the default *output* device (used for loopback
    // capture); kind-based selection was never implemented. Preserved in
    // the C ABI so existing consumers don't need to recompile. Future
    // major versions may remove the parameter.
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
        match e.inner.get_default_device() {
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
    static VERSION: &[u8] = b"0.1.0\0";
    VERSION.as_ptr() as *const c_char
}
