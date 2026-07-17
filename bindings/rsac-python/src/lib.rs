//! Python bindings for rsac (Rust Cross-Platform Audio Capture).
//!
//! This module exposes rsac's streaming-first audio capture API to Python
//! via PyO3. The design philosophy: rsac is a downstream audio pipeline
//! enabler — the Python API exposes streaming as first-class (iterators,
//! callbacks, context managers), not just file capture.

// `rsac::AudioError` is intentionally large (it carries structured BackendContext
// etc.), so closures/fns returning `Result<_, AudioError>` trip
// clippy::result_large_err. The core crate allows it crate-wide for the same
// reason (src/lib.rs:1); mirror that here so the new binding-crate clippy gate
// (rsac-3e24) does not flag an upstream design choice the binding can't change.
#![allow(clippy::result_large_err)]

use pyo3::exceptions::{PyOSError, PyRuntimeError, PyStopIteration, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use std::str::FromStr;
use std::sync::{Mutex, RwLock};

// ── Error Hierarchy ──────────────────────────────────────────────────────

/// Base exception for all rsac errors.
///
/// Attributes:
///     kind: The error category (e.g., "Device", "Stream", "Configuration").
///     is_recoverable: Whether the error is recoverable or transient.
///     is_fatal: Whether the error is fatal.
fn create_exception_classes(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // We create a hierarchy of exception classes.
    // RsacError is the base; specific errors inherit from it.
    //
    // Python hierarchy:
    //   RsacError(OSError)
    //     ├── DeviceNotFoundError(RsacError)
    //     ├── DeviceNotAvailableError(RsacError)
    //     ├── PlatformNotSupportedError(RsacError)
    //     ├── StreamError(RsacError)
    //     ├── ConfigurationError(RsacError, ValueError)
    //     ├── PermissionDeniedError(RsacError, PermissionError)
    //     ├── ApplicationNotFoundError(RsacError)
    //     └── TimeoutError(RsacError, TimeoutError)

    m.add("RsacError", m.py().get_type::<RsacError>())?;
    m.add(
        "DeviceNotFoundError",
        m.py().get_type::<DeviceNotFoundError>(),
    )?;
    m.add(
        "DeviceNotAvailableError",
        m.py().get_type::<DeviceNotAvailableError>(),
    )?;
    m.add(
        "PlatformNotSupportedError",
        m.py().get_type::<PlatformNotSupportedError>(),
    )?;
    m.add("StreamError", m.py().get_type::<StreamError>())?;
    m.add(
        "ConfigurationError",
        m.py().get_type::<ConfigurationError>(),
    )?;
    m.add(
        "PermissionDeniedError",
        m.py().get_type::<PermissionDeniedError>(),
    )?;
    m.add(
        "ApplicationNotFoundError",
        m.py().get_type::<ApplicationNotFoundError>(),
    )?;
    m.add(
        "CaptureTimeoutError",
        m.py().get_type::<CaptureTimeoutError>(),
    )?;
    m.add("BackendError", m.py().get_type::<BackendError>())?;
    Ok(())
}

pyo3::create_exception!(
    _rsac,
    RsacError,
    PyOSError,
    "Base exception for all rsac errors."
);
pyo3::create_exception!(
    _rsac,
    DeviceNotFoundError,
    RsacError,
    "The requested audio device was not found."
);
pyo3::create_exception!(
    _rsac,
    DeviceNotAvailableError,
    RsacError,
    "The audio device exists but is not currently available."
);
pyo3::create_exception!(
    _rsac,
    PlatformNotSupportedError,
    RsacError,
    "The requested feature is not supported on this platform."
);
pyo3::create_exception!(
    _rsac,
    StreamError,
    RsacError,
    "An error occurred during audio stream operation."
);
pyo3::create_exception!(
    _rsac,
    ConfigurationError,
    PyValueError,
    "Invalid capture configuration."
);
pyo3::create_exception!(
    _rsac,
    PermissionDeniedError,
    RsacError,
    "Permission denied for the requested audio operation."
);
pyo3::create_exception!(
    _rsac,
    ApplicationNotFoundError,
    RsacError,
    "The target application for capture was not found."
);
pyo3::create_exception!(
    _rsac,
    CaptureTimeoutError,
    RsacError,
    "An audio capture operation timed out."
);
pyo3::create_exception!(
    _rsac,
    BackendError,
    RsacError,
    "A platform-specific audio backend error occurred."
);

/// Convert an rsac AudioError into the appropriate Python exception.
fn audio_error_to_pyerr(err: rsac::AudioError) -> PyErr {
    let msg = err.to_string();
    match &err {
        rsac::AudioError::InvalidParameter { .. }
        | rsac::AudioError::UnsupportedFormat { .. }
        | rsac::AudioError::ConfigurationError { .. } => ConfigurationError::new_err(msg),

        rsac::AudioError::DeviceNotFound { .. } => DeviceNotFoundError::new_err(msg),
        rsac::AudioError::DeviceNotAvailable { .. } => DeviceNotAvailableError::new_err(msg),
        rsac::AudioError::DeviceEnumerationError { .. } => RsacError::new_err(msg),

        rsac::AudioError::StreamCreationFailed { .. }
        | rsac::AudioError::StreamStartFailed { .. }
        | rsac::AudioError::StreamStopFailed { .. }
        | rsac::AudioError::StreamReadError { .. }
        | rsac::AudioError::StreamEnded { .. }
        | rsac::AudioError::BufferOverrun { .. }
        | rsac::AudioError::BufferUnderrun { .. } => StreamError::new_err(msg),

        rsac::AudioError::BackendError { .. }
        | rsac::AudioError::BackendNotAvailable { .. }
        | rsac::AudioError::BackendInitializationFailed { .. } => BackendError::new_err(msg),

        rsac::AudioError::ApplicationNotFound { .. }
        | rsac::AudioError::ApplicationCaptureFailed { .. } => {
            ApplicationNotFoundError::new_err(msg)
        }

        rsac::AudioError::PlatformNotSupported { .. } => PlatformNotSupportedError::new_err(msg),
        rsac::AudioError::PermissionDenied { .. } => PermissionDeniedError::new_err(msg),

        rsac::AudioError::Timeout { .. } => CaptureTimeoutError::new_err(msg),

        rsac::AudioError::InternalError { .. } => RsacError::new_err(msg),

        // `rsac::AudioError` is `#[non_exhaustive]`: a future variant added in a
        // minor release lands here rather than breaking the build. Surface it as
        // the generic `RsacError` (still carrying the upstream message) so the
        // Python exception mapping stays forward-compatible. Every variant known
        // at this version is mapped explicitly above; this arm only fires for
        // additions.
        _ => RsacError::new_err(msg),
    }
}

// ── CaptureTarget ────────────────────────────────────────────────────────

/// Specifies what audio to capture.
///
/// Use the static constructor methods to create a target:
///
///     CaptureTarget.system_default()
///     CaptureTarget.device("device-id")
///     CaptureTarget.application(app_id)
///     CaptureTarget.application_by_name("Firefox")
///     CaptureTarget.process_tree(pid)
///
/// Or parse the canonical string grammar with `CaptureTarget.parse`:
///
///     CaptureTarget.parse("system")
///     CaptureTarget.parse("device:<id>")
///     CaptureTarget.parse("app:<id>")
///     CaptureTarget.parse("name:<n>")
///     CaptureTarget.parse("tree:<pid>")
#[pyclass(name = "CaptureTarget", module = "rsac._rsac", frozen)]
#[derive(Clone, Debug)]
struct PyCaptureTarget {
    inner: rsac::CaptureTarget,
}

#[pymethods]
impl PyCaptureTarget {
    /// Capture from the system default audio device / mix.
    #[staticmethod]
    fn system_default() -> Self {
        PyCaptureTarget {
            inner: rsac::CaptureTarget::SystemDefault,
        }
    }

    /// Capture from a specific device by its platform ID string.
    #[staticmethod]
    fn device(device_id: String) -> Self {
        PyCaptureTarget {
            inner: rsac::CaptureTarget::Device(rsac::DeviceId(device_id)),
        }
    }

    /// Capture audio from an application by its session/application ID.
    #[staticmethod]
    fn application(app_id: String) -> Self {
        PyCaptureTarget {
            inner: rsac::CaptureTarget::Application(rsac::ApplicationId(app_id)),
        }
    }

    /// Capture audio from the first application whose name matches.
    #[staticmethod]
    fn application_by_name(name: String) -> Self {
        PyCaptureTarget {
            inner: rsac::CaptureTarget::ApplicationByName(name),
        }
    }

    /// Capture audio from a process and its child processes by PID.
    #[staticmethod]
    fn process_tree(pid: u32) -> Self {
        PyCaptureTarget {
            inner: rsac::CaptureTarget::ProcessTree(rsac::ProcessId(pid)),
        }
    }

    /// Parse a capture target from its canonical string grammar.
    ///
    /// The grammar (case-insensitive scheme) is:
    ///
    /// * ``"system"`` → system default
    /// * ``"device:<id>"`` → a specific device
    /// * ``"app:<id>"`` → an application by session/application id
    /// * ``"name:<n>"`` → the first application whose name matches
    /// * ``"tree:<pid>"`` → a process and its children by PID
    ///
    /// Mirrors the typed constructors (:meth:`system_default`,
    /// :meth:`device`, :meth:`application`, :meth:`application_by_name`,
    /// :meth:`process_tree`), giving downstreams a single string entry point
    /// so they no longer hand-roll target parsing.
    ///
    /// Raises:
    ///     ConfigurationError: If ``spec`` is not a valid target string.
    #[staticmethod]
    fn parse(spec: &str) -> PyResult<Self> {
        rsac::CaptureTarget::from_str(spec)
            .map(|inner| PyCaptureTarget { inner })
            .map_err(audio_error_to_pyerr)
    }

    fn __repr__(&self) -> String {
        match &self.inner {
            rsac::CaptureTarget::SystemDefault => "CaptureTarget.system_default()".to_string(),
            rsac::CaptureTarget::Device(id) => format!("CaptureTarget.device({:?})", id.0),
            rsac::CaptureTarget::Application(id) => {
                format!("CaptureTarget.application({:?})", id.0)
            }
            rsac::CaptureTarget::ApplicationByName(name) => {
                format!("CaptureTarget.application_by_name({:?})", name)
            }
            rsac::CaptureTarget::ProcessTree(pid) => {
                format!("CaptureTarget.process_tree({})", pid.0)
            }
            // `rsac::CaptureTarget` is `#[non_exhaustive]`: a future variant added
            // in a minor release lands here rather than breaking the build. Fall
            // back to the upstream canonical string form (its in-crate `Display`
            // impl is exhaustive, so it renders any variant) so the repr stays
            // forward-compatible. Every variant known at this version has a
            // dedicated arm above; this only fires for additions.
            other => format!("CaptureTarget.parse({:?})", other.to_string()),
        }
    }

    fn __str__(&self) -> String {
        self.__repr__()
    }
}

/// Serialize interleaved `f32` samples to little-endian IEEE-754 bytes.
///
/// This is the provably-sound replacement for the old `from_raw_parts`
/// reinterpret flagged by issue #30: each sample is encoded via
/// [`f32::to_le_bytes`], so the result is always exactly `samples.len() * 4`
/// bytes in little-endian order regardless of host endianness, and the
/// conversion can never alias or misread memory. The byte layout matches
/// numpy dtype `'<f4'`.
fn f32_slice_to_le_bytes(samples: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 4);
    for &sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

// ── AudioBuffer ──────────────────────────────────────────────────────────

/// A buffer of interleaved audio samples (f32).
///
/// Attributes:
///     num_frames: Number of audio frames (samples per channel).
///     channels: Number of audio channels.
///     sample_rate: Sample rate in Hz.
///     duration_secs: Duration of audio in seconds.
///
/// Methods:
///     to_list(): Returns sample data as a Python list of floats.
///     to_bytes(): Returns raw sample data as bytes (little-endian f32).
///     channel_data(ch): Extract samples for a single channel.
///     rms(): Compute the RMS (root mean square) level.
///     peak(): Return the peak absolute sample value.
///     rms_dbfs(): RMS level in dBFS (-inf at silence).
///     peak_dbfs(): Peak level in dBFS (-inf at silence).
///     channel_rms(ch): Per-channel RMS, or None if out of range.
///     channel_peak(ch): Per-channel peak, or None if out of range.
#[pyclass(name = "AudioBuffer", module = "rsac._rsac", frozen)]
struct PyAudioBuffer {
    inner: rsac::AudioBuffer,
}

#[pymethods]
impl PyAudioBuffer {
    /// Number of audio frames (samples per channel).
    #[getter]
    fn num_frames(&self) -> usize {
        self.inner.num_frames()
    }

    /// Number of audio channels.
    #[getter]
    fn channels(&self) -> u16 {
        self.inner.channels()
    }

    /// Sample rate in Hz.
    #[getter]
    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }

    /// Total number of interleaved samples.
    #[getter]
    fn sample_count(&self) -> usize {
        self.inner.len()
    }

    /// Duration of the audio in this buffer, in seconds.
    #[getter]
    fn duration_secs(&self) -> f64 {
        self.inner.duration().as_secs_f64()
    }

    /// Whether the buffer contains no samples.
    #[getter]
    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Return the interleaved sample data as a Python list of floats.
    fn to_list(&self) -> Vec<f32> {
        self.inner.data().to_vec()
    }

    /// Return the raw sample data as bytes (little-endian f32, 4 bytes per sample).
    ///
    /// The byte layout is **little-endian IEEE-754 single-precision** (numpy
    /// dtype ``'<f4'``), independent of the host's native endianness. The
    /// returned buffer round-trips with ``numpy.frombuffer(b, dtype='<f4')``,
    /// which equals :meth:`to_list`. Length is always ``len(self) * 4``.
    ///
    /// Suitable for writing to files or passing to audio processing libraries.
    fn to_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &f32_slice_to_le_bytes(self.inner.data()))
    }

    /// Extract samples for a single channel (0-indexed).
    ///
    /// Returns None if the channel index is out of range.
    #[pyo3(signature = (channel))]
    fn channel_data(&self, channel: u16) -> Option<Vec<f32>> {
        self.inner.channel_data(channel)
    }

    /// Compute the RMS (root mean square) level of all samples.
    ///
    /// Delegates to the core, NaN-safe `AudioBuffer::rms` so the value
    /// matches every other rsac binding. Returns 0.0 for an empty buffer.
    fn rms(&self) -> f32 {
        self.inner.rms()
    }

    /// Return the peak absolute sample value across all channels.
    ///
    /// Delegates to the core, NaN-safe `AudioBuffer::peak`. Returns 0.0
    /// for an empty buffer.
    fn peak(&self) -> f32 {
        self.inner.peak()
    }

    /// Full-scale RMS level in decibels (dBFS).
    ///
    /// Returns negative infinity for digital silence (an all-zero or empty
    /// buffer). 0 dBFS corresponds to a full-scale sine/RMS of 1.0.
    fn rms_dbfs(&self) -> f32 {
        self.inner.rms_dbfs()
    }

    /// Peak level in decibels relative to full scale (dBFS).
    ///
    /// Returns negative infinity for digital silence (an all-zero or empty
    /// buffer). 0 dBFS corresponds to a peak sample magnitude of 1.0.
    fn peak_dbfs(&self) -> f32 {
        self.inner.peak_dbfs()
    }

    /// RMS level of a single channel (0-indexed).
    ///
    /// Returns None if the channel index is out of range. Returns 0.0 for an
    /// empty (but in-range) channel.
    #[pyo3(signature = (channel))]
    fn channel_rms(&self, channel: u16) -> Option<f32> {
        self.inner.channel_rms(channel)
    }

    /// Peak absolute sample value of a single channel (0-indexed).
    ///
    /// Returns None if the channel index is out of range. Returns 0.0 for an
    /// empty (but in-range) channel.
    #[pyo3(signature = (channel))]
    fn channel_peak(&self, channel: u16) -> Option<f32> {
        self.inner.channel_peak(channel)
    }

    fn __repr__(&self) -> String {
        format!(
            "AudioBuffer(frames={}, channels={}, sample_rate={}, duration={:.4}s)",
            self.inner.num_frames(),
            self.inner.channels(),
            self.inner.sample_rate(),
            self.inner.duration().as_secs_f64(),
        )
    }

    fn __str__(&self) -> String {
        self.__repr__()
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }

    fn __bool__(&self) -> bool {
        !self.inner.is_empty()
    }
}

// ── AudioDevice ──────────────────────────────────────────────────────────

/// Information about an audio device on the system.
///
/// Attributes:
///     id: Platform-specific device identifier.
///     name: Human-readable device name.
///     is_default: Whether this is the system default device.
#[pyclass(name = "AudioDevice", module = "rsac._rsac", frozen)]
struct PyAudioDevice {
    id: String,
    name: String,
    is_default: bool,
}

#[pymethods]
impl PyAudioDevice {
    /// Platform-specific device identifier string.
    #[getter]
    fn id(&self) -> &str {
        &self.id
    }

    /// Human-readable device name.
    #[getter]
    fn name(&self) -> &str {
        &self.name
    }

    /// Whether this is the system default device.
    #[getter]
    fn is_default(&self) -> bool {
        self.is_default
    }

    fn __repr__(&self) -> String {
        format!(
            "AudioDevice(id={:?}, name={:?}, is_default={})",
            self.id, self.name, self.is_default
        )
    }

    fn __str__(&self) -> String {
        self.name.clone()
    }
}

// ── PlatformCapabilities ─────────────────────────────────────────────────

/// Reports what the current platform's audio backend supports.
///
/// Attributes:
///     supports_system_capture: bool
///     supports_application_capture: bool
///     supports_process_tree_capture: bool
///     supports_device_selection: bool
///     supports_device_change_notifications: bool
///     requires_user_consent: bool
///     max_channels: int
///     sample_rate_range: tuple[int, int]
///     supported_sample_formats: list[str]
///     supported_sample_rates: list[int]
///     backend_name: str
#[pyclass(name = "PlatformCapabilities", module = "rsac._rsac", frozen)]
struct PyPlatformCapabilities {
    inner: rsac::PlatformCapabilities,
}

#[pymethods]
impl PyPlatformCapabilities {
    #[getter]
    fn supports_system_capture(&self) -> bool {
        self.inner.supports_system_capture
    }

    #[getter]
    fn supports_application_capture(&self) -> bool {
        self.inner.supports_application_capture
    }

    #[getter]
    fn supports_process_tree_capture(&self) -> bool {
        self.inner.supports_process_tree_capture
    }

    #[getter]
    fn supports_device_selection(&self) -> bool {
        self.inner.supports_device_selection
    }

    /// Whether the backend delivers device hot-plug / default-change
    /// notifications (the Rust ``DeviceEnumerator::watch`` surface).
    #[getter]
    fn supports_device_change_notifications(&self) -> bool {
        self.inner.supports_device_change_notifications
    }

    /// True when starting a capture requires a config-time user-consent
    /// artifact (mobile platforms; see docs/MOBILE_BACKEND_DESIGN.md);
    /// False on all desktop backends.
    #[getter]
    fn requires_user_consent(&self) -> bool {
        self.inner.requires_user_consent
    }

    #[getter]
    fn max_channels(&self) -> u16 {
        self.inner.max_channels
    }

    #[getter]
    fn sample_rate_range(&self) -> (u32, u32) {
        self.inner.sample_rate_range
    }

    /// Supported sample formats as lowercase strings (e.g. ``["i16", "f32"]``).
    #[getter]
    fn supported_sample_formats(&self) -> Vec<&'static str> {
        self.inner
            .supported_sample_formats
            .iter()
            .map(|f| sample_format_name(*f))
            .collect()
    }

    /// The config-time sample-rate whitelist the capture constructor accepts.
    ///
    /// Identical on every platform and intentionally narrower than the
    /// device-negotiable ``sample_rate_range``.
    #[getter]
    fn supported_sample_rates(&self) -> Vec<u32> {
        rsac::PlatformCapabilities::supported_sample_rates().to_vec()
    }

    #[getter]
    fn backend_name(&self) -> &str {
        self.inner.backend_name
    }

    fn __repr__(&self) -> String {
        format!(
            "PlatformCapabilities(backend={:?}, system={}, app={}, tree={}, device={}, max_ch={}, rate_range={:?})",
            self.inner.backend_name,
            self.inner.supports_system_capture,
            self.inner.supports_application_capture,
            self.inner.supports_process_tree_capture,
            self.inner.supports_device_selection,
            self.inner.max_channels,
            self.inner.sample_rate_range,
        )
    }
}

// ── StreamStats ──────────────────────────────────────────────────────────

/// Return the canonical lowercase string name for a sample format.
///
/// `SampleFormat` has no `Display`, so we map it explicitly rather than rely
/// on `Debug`, keeping the wire string stable for downstream consumers.
fn sample_format_name(fmt: rsac::SampleFormat) -> &'static str {
    match fmt {
        rsac::SampleFormat::I16 => "i16",
        rsac::SampleFormat::I24 => "i24",
        rsac::SampleFormat::I32 => "i32",
        rsac::SampleFormat::F32 => "f32",
    }
}

/// A point-in-time snapshot of stream statistics.
///
/// Returned by :meth:`AudioCapture.stream_stats`. Frozen / read-only.
///
/// Attributes:
///     overruns: Buffers dropped due to ring-buffer overflow.
///     buffers_captured: Buffers delivered to the consumer.
///     buffers_dropped: Buffers dropped due to overflow (alias of overruns).
///     buffers_pushed: Buffers enqueued by the OS audio callback.
///     uptime_secs: Seconds the stream has been running (0.0 if stopped).
///     is_running: Whether the stream is currently capturing.
///     format_description: Human-readable description of the captured format.
#[pyclass(name = "StreamStats", module = "rsac._rsac", frozen)]
struct PyStreamStats {
    inner: rsac::StreamStats,
}

#[pymethods]
impl PyStreamStats {
    /// Buffers dropped due to ring-buffer overflow.
    #[getter]
    fn overruns(&self) -> u64 {
        self.inner.overruns
    }

    /// Buffers delivered to the consumer since the stream started.
    #[getter]
    fn buffers_captured(&self) -> u64 {
        self.inner.buffers_captured
    }

    /// Buffers dropped due to ring-buffer overflow (alias of `overruns`).
    #[getter]
    fn buffers_dropped(&self) -> u64 {
        self.inner.buffers_dropped
    }

    /// Buffers enqueued by the OS audio callback since the stream started.
    #[getter]
    fn buffers_pushed(&self) -> u64 {
        self.inner.buffers_pushed
    }

    /// Seconds the stream has been running (0.0 when stopped / never started).
    #[getter]
    fn uptime_secs(&self) -> f64 {
        self.inner.uptime.as_secs_f64()
    }

    /// Whether the stream is currently capturing.
    #[getter]
    fn is_running(&self) -> bool {
        self.inner.is_running
    }

    /// Human-readable description of the audio format being captured.
    #[getter]
    fn format_description(&self) -> &str {
        &self.inner.format_description
    }

    /// Fraction of buffers lost to overflow, in 0.0..=1.0 (0.0 when none yet).
    fn dropped_ratio(&self) -> f64 {
        self.inner.dropped_ratio()
    }

    fn __repr__(&self) -> String {
        format!(
            "StreamStats(overruns={}, buffers_captured={}, buffers_dropped={}, buffers_pushed={}, uptime_secs={:.3}, is_running={})",
            self.inner.overruns,
            self.inner.buffers_captured,
            self.inner.buffers_dropped,
            self.inner.buffers_pushed,
            self.inner.uptime.as_secs_f64(),
            self.inner.is_running,
        )
    }
}

// ── BackpressureReport ─────────────────────────────────────────────────────

/// A windowed snapshot of producer backpressure.
///
/// Returned by :meth:`AudioCapture.backpressure_report`. Frozen / read-only.
///
/// Unlike the all-or-nothing :attr:`is_under_backpressure` flag (which trips
/// only on *consecutive* drops and resets on any successful push), this report
/// exposes a :attr:`drop_rate` over recent push activity, so sustained partial
/// loss (e.g. a steady 1-in-3 drop pattern) is visible.
///
/// Attributes:
///     window_secs: Wall-clock span the tallies cover, in seconds (0.0 when the
///         span is unattributed or no stream exists).
///     pushed: Buffers successfully pushed by the producer within the window.
///     dropped: Buffers dropped due to ring-buffer overflow within the window.
///     drop_rate: Fraction of buffers lost within the window, in 0.0..=1.0
///         (0.0 when nothing has been pushed or dropped).
///     is_under_backpressure: The legacy consecutive-drop backpressure flag.
#[pyclass(name = "BackpressureReport", module = "rsac._rsac", frozen)]
struct PyBackpressureReport {
    inner: rsac::BackpressureReport,
}

#[pymethods]
impl PyBackpressureReport {
    /// Wall-clock span the tallies cover, in seconds (0.0 when unattributed).
    #[getter]
    fn window_secs(&self) -> f64 {
        self.inner.window.as_secs_f64()
    }

    /// Buffers successfully pushed by the producer within the window.
    #[getter]
    fn pushed(&self) -> u64 {
        self.inner.pushed
    }

    /// Buffers dropped due to ring-buffer overflow within the window.
    #[getter]
    fn dropped(&self) -> u64 {
        self.inner.dropped
    }

    /// Fraction of buffers lost within the window, in 0.0..=1.0 (0.0 when none).
    #[getter]
    fn drop_rate(&self) -> f64 {
        self.inner.drop_rate
    }

    /// The legacy consecutive-drop backpressure flag.
    #[getter]
    fn is_under_backpressure(&self) -> bool {
        self.inner.is_under_backpressure
    }

    fn __repr__(&self) -> String {
        format!(
            "BackpressureReport(window_secs={:.3}, pushed={}, dropped={}, drop_rate={:.4}, is_under_backpressure={})",
            self.inner.window.as_secs_f64(),
            self.inner.pushed,
            self.inner.dropped,
            self.inner.drop_rate,
            self.inner.is_under_backpressure,
        )
    }
}

// ── AudioFormat ──────────────────────────────────────────────────────────

/// The negotiated audio delivery format of a running capture.
///
/// Returned by the :attr:`AudioCapture.format` getter. Frozen / read-only.
///
/// Attributes:
///     sample_rate: Samples per second (Hz).
///     channels: Number of interleaved channels.
///     sample_format: Sample type as a string ("f32", "i16", "i24", "i32").
#[pyclass(name = "AudioFormat", module = "rsac._rsac", frozen)]
struct PyAudioFormat {
    inner: rsac::AudioFormat,
}

#[pymethods]
impl PyAudioFormat {
    /// Samples per second (Hz).
    #[getter]
    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate
    }

    /// Number of interleaved channels.
    #[getter]
    fn channels(&self) -> u16 {
        self.inner.channels
    }

    /// Sample type as a string: "f32", "i16", "i24", or "i32".
    #[getter]
    fn sample_format(&self) -> &'static str {
        sample_format_name(self.inner.sample_format)
    }

    fn __repr__(&self) -> String {
        format!(
            "AudioFormat(sample_rate={}, channels={}, sample_format={:?})",
            self.inner.sample_rate,
            self.inner.channels,
            sample_format_name(self.inner.sample_format),
        )
    }
}

// ── Completed awaitable ──────────────────────────────────────────────────

/// A minimal already-resolved awaitable wrapping a single result value.
///
/// `AudioCapture.__aenter__` / `__aexit__` do their (non-blocking) work
/// synchronously and hand back one of these so the `async with` protocol is
/// satisfied without pulling in an async runtime or `asyncio`. Awaiting it
/// completes immediately with the stored value: `__await__` returns an
/// iterator that raises `StopIteration(value)` on its first `__next__`, which
/// is exactly the coroutine-completion protocol the event loop expects.
#[pyclass(module = "rsac._rsac")]
struct CompletedAwaitable {
    value: Option<PyObject>,
}

#[pymethods]
impl CompletedAwaitable {
    fn __await__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    fn __iter__(slf: Py<Self>) -> Py<Self> {
        slf
    }

    fn __next__(&mut self, py: Python<'_>) -> PyResult<()> {
        // Hand the stored value back as StopIteration(value); awaiting an
        // already-complete coroutine yields nothing and stops immediately.
        let value = self.value.take().unwrap_or_else(|| py.None());
        Err(PyStopIteration::new_err(value))
    }
}

impl CompletedAwaitable {
    fn new(py: Python<'_>, value: PyObject) -> PyResult<Py<Self>> {
        Py::new(py, CompletedAwaitable { value: Some(value) })
    }
}

// ── AudioCapture ─────────────────────────────────────────────────────────

/// The main audio capture class. Supports context manager and iterator protocols.
///
/// Usage:
///
///     # As context manager (recommended):
///     with rsac.AudioCapture(target=CaptureTarget.system_default()) as cap:
///         for buffer in cap:
///             process(buffer)
///
///     # Manual lifecycle:
///     cap = rsac.AudioCapture(target=CaptureTarget.system_default())
///     cap.start()
///     buffer = cap.read()
///     cap.stop()
///
/// Args:
///     target: What to capture (default: CaptureTarget.system_default()).
///     sample_rate: Sample rate in Hz (default: 48000).
///     channels: Number of channels (default: 2).
///     buffer_size: Optional buffer size in frames.
/// Outcome of the GIL-released lock dances shared by `start()`/`stop()`/`close()`.
///
/// The teardown runs entirely inside `Python::allow_threads` (see the
/// `stop`/`close` deadlock-fix comments), so it cannot build a `PyErr` in place
/// (that needs the GIL). It returns one of these instead and the caller maps it
/// to the right Python exception *after* the GIL is re-acquired.
enum TeardownError {
    /// The wrapper `RwLock` was poisoned; carries the formatted poison detail so
    /// the surfaced message matches the pre-fix `format!("Lock poisoned: {e}")`.
    Poisoned(String),
    /// The capture was already closed (`None` slot). `stop()` and `start()`
    /// surface this; `close()` treats an already-closed capture as an
    /// idempotent success.
    Closed,
    /// The core `stop()` returned an error.
    Stop(rsac::AudioError),
}

impl TeardownError {
    fn into_pyerr(self) -> PyErr {
        match self {
            TeardownError::Poisoned(detail) => {
                PyRuntimeError::new_err(format!("Lock poisoned: {}", detail))
            }
            TeardownError::Closed => PyRuntimeError::new_err("AudioCapture has been closed"),
            TeardownError::Stop(e) => audio_error_to_pyerr(e),
        }
    }
}

#[pyclass(name = "AudioCapture", module = "rsac._rsac")]
struct PyAudioCapture {
    /// The wrapped rsac capture, behind an `RwLock` (not a `Mutex`) so a thread
    /// parked in a blocking `read()` / `__next__` holds only a **shared read
    /// guard** while the GIL is released.
    ///
    /// rsac's read paths (`read_buffer`, `read_chunk_blocking`) and
    /// `request_stop` take `&self`, while the lifecycle mutators
    /// (`start`/`stop`/`close`) take `&mut self`. Mapping reads to a shared read
    /// guard and the mutators to the exclusive write guard is what breaks the
    /// stop()-vs-parked-blocking-read deadlock (rsac-8082): `stop()`/`close()`
    /// FIRST take a *read* guard to call `request_stop()` — shared with the guard
    /// a parked reader holds, so it never blocks — which transitions the stream
    /// terminal and wakes the reader; only then do they take the write guard for
    /// the actual `stop()`. With the old `Mutex`, the reader parked (GIL released)
    /// while holding the sole lock and `stop()` blocked on it forever.
    ///
    /// The `Option` still models the closed state (`None` after `close()`); the
    /// `RwLock` only changes how the slot is guarded, not its contents.
    ///
    /// GIL INTERACTION (rsac-8082 follow-up): the parked reader releases the GIL
    /// across its blocking read (`py.allow_threads`) but holds its read guard the
    /// whole time — and `allow_threads` re-acquires the GIL *before* the guard is
    /// dropped. So a teardown that took `.write()` while still holding the GIL
    /// would deadlock: the woken reader blocks re-acquiring the GIL (held by the
    /// teardown) and never unwinds to drop its read guard, while the teardown
    /// blocks on `.write()` behind that guard. `stop()`/`close()`/`__del__`
    /// therefore run the ENTIRE lock dance (read-guard `request_stop` → `write()`
    /// → teardown) inside `py.allow_threads`, releasing the GIL while they block
    /// on `.write()` so the woken reader can re-acquire it and drop its guard.
    inner: RwLock<Option<rsac::AudioCapture>>,
    /// Track if we're acting as an iterator (started via __iter__).
    iterating: Mutex<bool>,
}

#[pymethods]
impl PyAudioCapture {
    /// Create a new AudioCapture instance.
    ///
    /// Does NOT start capturing immediately. Call `start()` or use as a
    /// context manager to begin capture.
    #[new]
    #[pyo3(signature = (target=None, sample_rate=48000, channels=2, buffer_size=None))]
    fn new(
        py: Python<'_>,
        target: Option<PyCaptureTarget>,
        sample_rate: u32,
        channels: u16,
        buffer_size: Option<usize>,
    ) -> PyResult<Self> {
        let rust_target = target
            .map(|t| t.inner.clone())
            .unwrap_or(rsac::CaptureTarget::SystemDefault);

        let capture = py
            .allow_threads(|| {
                let mut builder = rsac::AudioCaptureBuilder::new()
                    .with_target(rust_target)
                    .sample_rate(sample_rate)
                    .channels(channels);

                if let Some(size) = buffer_size {
                    builder = builder.buffer_size(Some(size));
                }

                builder.build()
            })
            .map_err(audio_error_to_pyerr)?;

        Ok(PyAudioCapture {
            inner: RwLock::new(Some(capture)),
            iterating: Mutex::new(false),
        })
    }

    /// Start audio capture.
    ///
    /// Must be called before reading audio data. Called automatically when
    /// using AudioCapture as a context manager.
    fn start(&self, py: Python<'_>) -> PyResult<()> {
        // Entirely GIL-released, like the teardown (rsac-8082 follow-up): readers
        // can only park in `read()`/`__next__` while the stream is RUNNING, and
        // core `start()` on a running stream is a documented idempotent no-op —
        // so a redundant `start()` first checks under a SHARED guard and skips
        // the exclusive write guard, which would otherwise queue forever behind
        // a parked reader's shared guard (and, GIL-held, recreate the circular
        // wait the teardown fix removed).
        py.allow_threads(|| {
            {
                let guard = self
                    .inner
                    .read()
                    .map_err(|e| TeardownError::Poisoned(e.to_string()))?;
                let capture = guard.as_ref().ok_or(TeardownError::Closed)?;
                if capture.is_running() {
                    return Ok(());
                }
            }
            // Not running → no reader can be parked (a blocking read on a
            // non-running stream returns immediately), so the write guard is only
            // briefly contended. A racing start() between the guards is absorbed
            // by core start()'s own running-check (idempotent no-op).
            let mut guard = self
                .inner
                .write()
                .map_err(|e| TeardownError::Poisoned(e.to_string()))?;
            let capture = guard.as_mut().ok_or(TeardownError::Closed)?;
            capture.start().map_err(TeardownError::Stop)
        })
        .map_err(TeardownError::into_pyerr)
    }

    /// Stop audio capture.
    ///
    /// Stops the underlying OS audio stream and releases resources.
    /// After stopping, the capture cannot be restarted.
    fn stop(&self, py: Python<'_>) -> PyResult<()> {
        // Deadlock fix (rsac-8082): a thread parked in `read()` / `__next__` holds
        // a *shared read guard* while blocked inside `read_chunk_blocking` with the
        // GIL released. Break the stop-vs-parked-read cycle like the C FFI / Go
        // binding: FIRST take a read guard (shared with the parked reader, so it
        // never blocks) and call `request_stop()` — which flips the stream terminal
        // and wakes the reader — then take the write guard for the real `stop()`.
        //
        // CRITICAL (GIL): the whole dance runs inside `py.allow_threads` so the GIL
        // is released while we block on `.write()`. If we held the GIL across
        // `.write()`, the woken reader — which must re-acquire the GIL before its
        // `allow_threads` returns and drops its read guard — would block on the GIL
        // we hold, while we block on the write lock behind its guard: a circular
        // wait. Releasing the GIL here lets the reader re-acquire it, unwind, and
        // drop its read guard so our `.write()` proceeds. The closure is pure Rust
        // (no Python), so we return a `TeardownError` and map it to a `PyErr` only
        // after the GIL is back.
        py.allow_threads(|| self.teardown_stop())
            .map_err(TeardownError::into_pyerr)
    }

    /// Whether the capture is currently running.
    #[getter]
    fn is_running(&self) -> PyResult<bool> {
        let guard = self
            .inner
            .read()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock poisoned: {}", e)))?;
        Ok(guard.as_ref().map(|c| c.is_running()).unwrap_or(false))
    }

    /// Read the next audio buffer (non-blocking).
    ///
    /// Returns an AudioBuffer if data is available, or None if no data
    /// is ready yet. Raises StreamError if the stream is not running.
    ///
    /// The GIL is released during the read operation.
    fn try_read(&self, py: Python<'_>) -> PyResult<Option<PyAudioBuffer>> {
        // `read_buffer` takes `&self` → the shared read guard (allows concurrent
        // reads and, crucially, is compatible with the read guard a parked
        // blocking read/`__next__` holds).
        let guard = self
            .inner
            .read()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock poisoned: {}", e)))?;
        let capture = guard
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("AudioCapture has been closed"))?;

        // read_buffer() takes &self. Release GIL for the read.
        let result = py.allow_threads(|| capture.read_buffer());

        match result {
            Ok(Some(buf)) => Ok(Some(PyAudioBuffer { inner: buf })),
            Ok(None) => Ok(None),
            Err(e) => Err(audio_error_to_pyerr(e)),
        }
    }

    /// Read the next audio buffer (blocking).
    ///
    /// Blocks until audio data is available. The GIL is released during
    /// the wait, allowing other Python threads to run.
    ///
    /// Terminal-observable (rsac-477d): once the stream has ended — after
    /// `stop()` or a fatal backend error — this raises the stream's true
    /// terminal error (`StreamEndedError`) promptly, matching the iterator
    /// protocol, the C FFI, and Go. It no longer downgrades a terminal
    /// stream to a recoverable "not running" error.
    ///
    /// Raises StreamError if the stream encounters a recoverable error.
    fn read(&self, py: Python<'_>) -> PyResult<PyAudioBuffer> {
        // Deadlock fix (rsac-8082): `read_chunk_blocking` takes `&self`, so this
        // holds only a *shared read guard* while parked (GIL released). A
        // concurrent `stop()`/`close()` first takes its own read guard to call
        // `request_stop()` — compatible with this guard, so it never blocks — which
        // wakes this parked read; only then does it take the write guard. Under the
        // old `Mutex` this parked while holding the sole lock, wedging `stop()`.
        let guard = self
            .inner
            .read()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock poisoned: {}", e)))?;
        let capture = guard
            .as_ref()
            .ok_or_else(|| PyRuntimeError::new_err("AudioCapture has been closed"))?;

        let result = py.allow_threads(|| capture.read_chunk_blocking());

        match result {
            Ok(buf) => Ok(PyAudioBuffer { inner: buf }),
            Err(e) => Err(audio_error_to_pyerr(e)),
        }
    }

    /// Number of audio buffers dropped due to ring buffer overflow.
    ///
    /// A non-zero value indicates the consumer is not reading fast enough.
    #[getter]
    fn overrun_count(&self) -> PyResult<u64> {
        let guard = self
            .inner
            .read()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock poisoned: {}", e)))?;
        Ok(guard.as_ref().map(|c| c.overrun_count()).unwrap_or(0))
    }

    /// Return a point-in-time snapshot of stream statistics.
    ///
    /// Returns a frozen :class:`StreamStats` (overruns, buffers_captured,
    /// buffers_dropped, buffers_pushed, uptime_secs, is_running,
    /// format_description). On a closed capture, returns a default snapshot
    /// (all counters zero, ``is_running == False``).
    fn stream_stats(&self) -> PyResult<PyStreamStats> {
        let guard = self
            .inner
            .read()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock poisoned: {}", e)))?;
        let inner = guard.as_ref().map(|c| c.stream_stats()).unwrap_or_default();
        Ok(PyStreamStats { inner })
    }

    /// Return a windowed snapshot of producer backpressure.
    ///
    /// Returns a frozen :class:`BackpressureReport` (window_secs, pushed,
    /// dropped, drop_rate, is_under_backpressure). Unlike the all-or-nothing
    /// backpressure flag, ``drop_rate`` surfaces sustained partial loss. On a
    /// closed capture, returns a default report (all counters zero, ``drop_rate
    /// == 0.0``, ``is_under_backpressure == False``).
    fn backpressure_report(&self) -> PyResult<PyBackpressureReport> {
        let guard = self
            .inner
            .read()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock poisoned: {}", e)))?;
        let inner = guard
            .as_ref()
            .map(|c| c.backpressure_report())
            .unwrap_or_default();
        Ok(PyBackpressureReport { inner })
    }

    /// The negotiated audio delivery format, or None if not running.
    ///
    /// Returns an :class:`AudioFormat` (sample_rate, channels, sample_format)
    /// once the stream has started and negotiated a format with the backend;
    /// None before start, after close, or when the backend has not yet
    /// reported a format.
    #[getter]
    fn format(&self) -> PyResult<Option<PyAudioFormat>> {
        let guard = self
            .inner
            .read()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock poisoned: {}", e)))?;
        Ok(guard
            .as_ref()
            .and_then(|c| c.format())
            .map(|inner| PyAudioFormat { inner }))
    }

    /// Close the capture and release all resources.
    ///
    /// After closing, the capture cannot be used. This is called automatically
    /// when exiting a `with` block.
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        // Deadlock fix (rsac-8082): like `stop()`, `close()` must not block behind
        // a thread parked in `read()`/`__next__`. It runs the same read-guard
        // `request_stop()` → write-guard dance, and — crucially — runs it entirely
        // inside `py.allow_threads` so the GIL is released while it blocks on
        // `.write()`. Holding the GIL across `.write()` would deadlock against a
        // reader that must re-acquire the GIL to unwind and drop its read guard
        // (see the `inner` field's GIL-interaction note). The capture is ALWAYS
        // dropped (close is idempotent and must not leave a half-open stream), but
        // the core `stop()` error is propagated rather than swallowed so callers —
        // notably `__aexit__`, whose contract surfaces teardown failures when no
        // body exception is in flight — actually learn about a failed teardown.
        py.allow_threads(|| self.teardown_close())
            .map_err(TeardownError::into_pyerr)
    }

    // ── Context Manager Protocol ─────────────────────────────────────

    fn __enter__(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<Self>> {
        slf.borrow(py).start(py)?;
        Ok(slf)
    }

    #[pyo3(signature = (_exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __exit__(
        &self,
        py: Python<'_>,
        _exc_type: Option<Bound<'_, PyAny>>,
        _exc_val: Option<Bound<'_, PyAny>>,
        _exc_tb: Option<Bound<'_, PyAny>>,
    ) -> PyResult<bool> {
        self.close(py)?;
        Ok(false) // Don't suppress exceptions
    }

    // ── Async Context Manager Protocol ───────────────────────────────

    /// Enter as an async context manager: start capture, return ``self``.
    ///
    /// Start is non-blocking, so the returned awaitable is already complete;
    /// ``await``-ing it yields ``self`` immediately. Mirrors :meth:`__enter__`.
    fn __aenter__(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<CompletedAwaitable>> {
        slf.borrow(py).start(py)?;
        CompletedAwaitable::new(py, slf.into_any())
    }

    /// Exit as an async context manager: close the capture (best-effort).
    ///
    /// If the ``async with`` body raised (``exc_type`` is not None), any error
    /// from closing the stream is swallowed so it cannot mask the original
    /// exception; the awaitable resolves to ``False`` and the body exception
    /// propagates. With no exception in flight, a close failure is surfaced so
    /// callers still learn about teardown problems.
    #[pyo3(signature = (exc_type=None, _exc_val=None, _exc_tb=None))]
    fn __aexit__(
        &self,
        py: Python<'_>,
        exc_type: Option<Bound<'_, PyAny>>,
        _exc_val: Option<Bound<'_, PyAny>>,
        _exc_tb: Option<Bound<'_, PyAny>>,
    ) -> PyResult<Py<CompletedAwaitable>> {
        let close_result = self.close(py);
        match close_result {
            // No error closing → done; never suppress a body exception.
            Ok(()) => CompletedAwaitable::new(py, py.None()),
            // Error closing while an exception is already propagating → swallow
            // the teardown error so it does not mask the in-flight exception.
            Err(_) if exc_type.is_some() => CompletedAwaitable::new(py, py.None()),
            // Error closing with no exception in flight → surface it.
            Err(e) => Err(e),
        }
    }

    // ── Finalizer safety net ─────────────────────────────────────────

    /// Garbage-collection safety net: stop a still-running OS stream.
    ///
    /// Ensures a capture that was dropped without an explicit
    /// :meth:`close` / ``with`` / ``async with`` never leaks a running OS
    /// audio stream. Idempotent (a closed capture is a no-op) and never
    /// raises or panics — `__del__` must not propagate exceptions.
    fn __del__(&self, py: Python<'_>) {
        // Best-effort: take the capture and stop it, swallowing all errors.
        // A poisoned lock or already-closed capture simply means there is
        // nothing left to tear down.
        //
        // Same GIL discipline as stop()/close() (rsac-8082): Python GC can run
        // __del__ on a *different* thread than a parked reader, so the read-guard
        // `request_stop()` → write-guard dance must run inside `py.allow_threads`.
        // Holding the GIL across `.write()` would deadlock against a reader that
        // needs the GIL to unwind and drop its read guard. `teardown_close`
        // swallows the poisoned/closed/stop outcomes here — __del__ must never
        // raise or panic.
        let _ = py.allow_threads(|| self.teardown_close());
    }

    // ── Iterator Protocol ────────────────────────────────────────────

    fn __iter__(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<Self>> {
        {
            let self_ref = slf.borrow(py);
            let mut iter_guard = self_ref
                .iterating
                .lock()
                .map_err(|e| PyRuntimeError::new_err(format!("Lock poisoned: {}", e)))?;
            *iter_guard = true;
            // Drop the iterating guard BEFORE start(): start() releases the GIL
            // inside allow_threads, so holding `iterating` across it recreates
            // the GIL-vs-lock circular wait (rsac-8082 class) — a second thread
            // entering __iter__ would block on `iterating` while holding the
            // GIL, and this thread could never re-acquire the GIL to unwind.
            drop(iter_guard);

            // Auto-start if not already running. This is a read-only check
            // (`is_running()` takes `&self`), so a shared read guard suffices; the
            // guard is dropped before the `start()` call below, which takes its own
            // write guard.
            let guard = self_ref
                .inner
                .read()
                .map_err(|e| PyRuntimeError::new_err(format!("Lock poisoned: {}", e)))?;
            if let Some(ref capture) = *guard {
                if !capture.is_running() {
                    drop(guard);
                    self_ref.start(py)?;
                }
            }
        }
        Ok(slf)
    }

    fn __next__(&self, py: Python<'_>) -> PyResult<Option<PyAudioBuffer>> {
        // Terminal-error delivery (BP-2): this loop fixes the previously
        // INVERTED mapping. A FATAL terminal (`is_fatal()`, which covers
        // `StreamEnded` — the natural end of capture) must raise
        // `StopIteration` so `for buf in capture:` ends cleanly. A RECOVERABLE
        // hiccup (transient `StreamReadError`, `BufferOverrun`/`Underrun`) must
        // NOT end iteration — retry the blocking read instead of surfacing a
        // `StreamError`. Only a genuinely unexpected (non-recoverable,
        // non-terminal) error propagates via `audio_error_to_pyerr`.
        //
        // The old code did the opposite: it raised `StopIteration` on the
        // recoverable `StreamReadError` (cutting iteration short on a transient
        // blip) and surfaced the fatal `StreamEnded` as a `StreamError`
        // (raising on the natural end of capture).
        //
        // We read via `read_chunk_blocking` (the *terminal-observable* path),
        // NOT `read_buffer_blocking`. `read_buffer_blocking` short-circuits to a
        // RECOVERABLE `StreamReadError` the moment the stream leaves `Running`,
        // so its `is_running()` guard makes the fatal `StreamEnded` arm here
        // UNREACHABLE — iteration would always end via the "Capture stopped"
        // `StopIteration` below and never surface the true terminal reason.
        // `read_chunk_blocking` drains the buffered tail while `Stopping` and
        // returns the fatal `StreamEnded` once the ring is empty AND the stream
        // is terminal, so the real terminal cause flows into the `is_fatal()`
        // arm (mirrors the napi pump and Go `StreamWithErrors`).
        //
        // LOCK DISCIPLINE (rsac-8082): the `self.inner` RwLock is acquired *per
        // iteration* as a SHARED READ guard (`read_chunk_blocking` takes `&self`)
        // and dropped before the recoverable-retry backoff (and at every return).
        // Holding a read guard while parked is the crux of the deadlock fix: a
        // concurrent `stop()`/`close()`/`__del__()` first takes its own read guard
        // to `request_stop()` — shared with this one, so it never blocks — which
        // wakes this parked read; the teardown's write guard then proceeds once
        // this iteration drops its read guard. Holding the guard across the retry
        // loop would let an immediate recoverable error busy-spin while wedging
        // teardown, so it is scoped to a single attempt.
        loop {
            // Acquire, validate, and read inside a scope so the guard is dropped
            // the moment we leave it (before any retry sleep / between attempts).
            let outcome = {
                let guard = self
                    .inner
                    .read()
                    .map_err(|e| PyRuntimeError::new_err(format!("Lock poisoned: {}", e)))?;
                let capture = match guard.as_ref() {
                    Some(c) => c,
                    None => return Err(PyStopIteration::new_err("Capture closed")),
                };
                if !capture.is_running() {
                    return Err(PyStopIteration::new_err("Capture stopped"));
                }
                // Release GIL for the blocking read. The `self.inner` read guard is
                // still held here (it must be — `capture` borrows it), but it is a
                // SHARED guard: a concurrent `stop()`/`close()` takes a read guard
                // of its own to call `request_stop()`, which signals the stream
                // terminal so this read returns promptly rather than deadlocking.
                py.allow_threads(|| capture.read_chunk_blocking())
                // `guard` drops at the end of this block.
            };

            match outcome {
                Ok(buf) => return Ok(Some(PyAudioBuffer { inner: buf })),
                // Natural end of capture / terminal: end iteration carrying the
                // true terminal reason. `is_fatal()` is the single source of
                // truth (ADR-0003: `StreamEnded` is Fatal); we do not match a
                // specific variant so any fatal terminal ends cleanly.
                Err(e) if e.is_fatal() => {
                    return Err(PyStopIteration::new_err(e.to_string()));
                }
                // Recoverable hiccup: do NOT end iteration. `read_chunk_blocking`
                // can still surface a *recoverable* `StreamReadError` (e.g. the
                // stream is not yet initialized, or a transient backend hiccup).
                // The guard is already dropped here, so a bounded backoff yields
                // the lock + GIL to a concurrent stop()/close() rather than
                // busy-spinning while holding the mutex. The next iteration
                // re-locks and re-checks `is_running()`, so a stop that has not
                // yet reached the fatal terminal ends cleanly via `StopIteration`.
                Err(e) if e.is_recoverable() => {
                    py.allow_threads(|| {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    });
                    continue;
                }
                // Genuinely unexpected (neither recoverable nor a fatal terminal):
                // surface it as the mapped Python exception.
                Err(e) => return Err(audio_error_to_pyerr(e)),
            }
        }
    }

    fn __repr__(&self) -> String {
        let guard = self.inner.read();
        match guard {
            Ok(ref g) => match g.as_ref() {
                Some(c) => format!("AudioCapture(running={})", c.is_running()),
                None => "AudioCapture(closed)".to_string(),
            },
            Err(_) => "AudioCapture(error)".to_string(),
        }
    }
}

// ── Teardown helpers (GIL-released, rsac-8082) ─────────────────────────────
//
// These run the read-guard `request_stop()` → write-guard lock dance as PURE
// RUST, with NO `Python` token in scope, so `stop()`/`close()`/`__del__` can
// invoke them from inside `py.allow_threads` (GIL released). Keeping them out of
// the `#[pymethods]` block is deliberate: they must not touch the GIL, and the
// borrow of `&self` here is the plain Rust `&self`, not a PyO3 receiver. Errors
// come back as `TeardownError` (GIL-free) and the caller maps them to a `PyErr`
// after the GIL is re-acquired.
impl PyAudioCapture {
    /// The shared lock dance: take a read guard to `request_stop()` the stream
    /// (waking a parked reader), drop it, then take the write guard. Returns the
    /// woken/settled write guard's `Option` slot for the caller to finish with.
    ///
    /// MUST be called with the GIL released (inside `allow_threads`) — see the
    /// `inner` field's GIL-interaction note.
    fn request_stop_then_write(
        &self,
    ) -> std::result::Result<
        std::sync::RwLockWriteGuard<'_, Option<rsac::AudioCapture>>,
        TeardownError,
    > {
        {
            let guard = self
                .inner
                .read()
                .map_err(|e| TeardownError::Poisoned(e.to_string()))?;
            if let Some(capture) = guard.as_ref() {
                // &self, idempotent, does not touch the bridge consumer mutex the
                // parked read holds — safe concurrently with the in-flight read.
                capture.request_stop();
            }
        }
        self.inner
            .write()
            .map_err(|e| TeardownError::Poisoned(e.to_string()))
    }

    /// `stop()` teardown: signal + stop the stream in place, leaving the capture
    /// in the slot (a stopped capture can still be inspected). `None` slot →
    /// `Closed`.
    fn teardown_stop(&self) -> std::result::Result<(), TeardownError> {
        let mut guard = self.request_stop_then_write()?;
        let capture = guard.as_mut().ok_or(TeardownError::Closed)?;
        capture.stop().map_err(TeardownError::Stop)
    }

    /// `close()`/`__del__` teardown: signal, then TAKE the capture out and drop
    /// it (always, even on a stop error — close must not leave a half-open
    /// stream). An already-closed capture (`None` slot) is an idempotent success.
    fn teardown_close(&self) -> std::result::Result<(), TeardownError> {
        let mut guard = self.request_stop_then_write()?;
        if let Some(mut capture) = guard.take() {
            let r = capture.stop();
            drop(capture);
            r.map_err(TeardownError::Stop)?;
        }
        Ok(())
    }
}

// ── Module-level functions ───────────────────────────────────────────────

/// List all available audio devices on the system.
///
/// Returns:
///     list[AudioDevice]: A list of available audio devices.
///
/// Raises:
///     RsacError: If device enumeration fails.
///     PlatformNotSupportedError: If not supported on this platform.
#[pyfunction]
fn list_devices(py: Python<'_>) -> PyResult<Vec<PyAudioDevice>> {
    let devices = py
        .allow_threads(|| {
            let enumerator = rsac::get_device_enumerator()?;
            enumerator.enumerate_devices()
        })
        .map_err(audio_error_to_pyerr)?;

    Ok(devices
        .into_iter()
        .map(|d| PyAudioDevice {
            id: d.id().to_string(),
            name: d.name(),
            is_default: d.is_default(),
        })
        .collect())
}

/// Query the platform's audio capture capabilities.
///
/// Returns:
///     PlatformCapabilities: What this platform supports.
#[pyfunction]
fn platform_capabilities() -> PyPlatformCapabilities {
    PyPlatformCapabilities {
        inner: rsac::PlatformCapabilities::query(),
    }
}

// ── Module Definition ────────────────────────────────────────────────────

/// rsac — Rust Cross-Platform Audio Capture (Python bindings)
///
/// A streaming-first audio capture library. Captures system audio,
/// per-application audio, or process-tree audio on Windows (WASAPI),
/// Linux (PipeWire), and macOS (CoreAudio Process Tap).
///
/// Quick start:
///
///     import rsac
///
///     with rsac.AudioCapture() as cap:
///         for buffer in cap:
///             print(f"Got {buffer.num_frames} frames, RMS={buffer.rms():.4f}")
#[pymodule]
fn _rsac(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Register classes
    m.add_class::<PyCaptureTarget>()?;
    m.add_class::<PyAudioBuffer>()?;
    m.add_class::<PyAudioDevice>()?;
    m.add_class::<PyPlatformCapabilities>()?;
    m.add_class::<PyStreamStats>()?;
    m.add_class::<PyBackpressureReport>()?;
    m.add_class::<PyAudioFormat>()?;
    m.add_class::<PyAudioCapture>()?;
    // Internal awaitable returned by AudioCapture.__aenter__/__aexit__.
    m.add_class::<CompletedAwaitable>()?;

    // Register module-level functions
    m.add_function(wrap_pyfunction!(list_devices, m)?)?;
    m.add_function(wrap_pyfunction!(platform_capabilities, m)?)?;

    // Register exception hierarchy
    create_exception_classes(m)?;

    // Module metadata
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{f32_slice_to_le_bytes, sample_format_name};

    #[test]
    fn sample_format_name_maps_every_variant() {
        // Exhaustive: a new SampleFormat variant must extend the mapping.
        assert_eq!(sample_format_name(rsac::SampleFormat::I16), "i16");
        assert_eq!(sample_format_name(rsac::SampleFormat::I24), "i24");
        assert_eq!(sample_format_name(rsac::SampleFormat::I32), "i32");
        assert_eq!(sample_format_name(rsac::SampleFormat::F32), "f32");
    }

    #[test]
    fn to_le_bytes_length_is_four_per_sample() {
        assert_eq!(f32_slice_to_le_bytes(&[]).len(), 0);
        assert_eq!(f32_slice_to_le_bytes(&[0.0]).len(), 4);
        assert_eq!(f32_slice_to_le_bytes(&[1.0, -1.0, 0.5]).len(), 12);
    }

    #[test]
    fn to_le_bytes_is_little_endian_ieee754() {
        // 1.0f32 == 0x3F800000, little-endian byte order 00 00 80 3F.
        assert_eq!(f32_slice_to_le_bytes(&[1.0]), [0x00, 0x00, 0x80, 0x3F]);
        // -2.0f32 == 0xC0000000, little-endian 00 00 00 C0.
        assert_eq!(f32_slice_to_le_bytes(&[-2.0]), [0x00, 0x00, 0x00, 0xC0]);
    }

    #[test]
    fn to_le_bytes_round_trips_via_from_le_bytes() {
        let samples = [
            0.0f32,
            1.0,
            -1.5,
            std::f32::consts::PI,
            f32::MIN,
            f32::MAX,
            -0.0,
        ];
        let bytes = f32_slice_to_le_bytes(&samples);
        assert_eq!(bytes.len(), samples.len() * 4);
        let decoded: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        // Bit-exact round trip (compare bit patterns so -0.0 and NaN-free
        // values match precisely).
        for (orig, got) in samples.iter().zip(decoded.iter()) {
            assert_eq!(orig.to_bits(), got.to_bits());
        }
    }

    #[test]
    fn to_le_bytes_concatenates_samples_in_order() {
        let bytes = f32_slice_to_le_bytes(&[1.0, 2.0]);
        let mut expected = Vec::new();
        expected.extend_from_slice(&1.0f32.to_le_bytes());
        expected.extend_from_slice(&2.0f32.to_le_bytes());
        assert_eq!(bytes, expected);
    }

    // ── __next__ terminal-error classification (BP-2) ─────────────────
    //
    // `__next__` cannot be exercised here without a real device / PyO3 runtime,
    // so these tests pin the *classification contract* the rewritten loop relies
    // on: a fatal terminal → StopIteration, a recoverable hiccup → retry. They
    // guard against re-introducing the inverted mapping (where the recoverable
    // `StreamReadError` ended iteration and the fatal `StreamEnded` raised).
    //
    // `__next__` reads via `read_chunk_blocking` (the terminal-observable path),
    // NOT `read_buffer_blocking`. The latter's `is_running()` guard downgrades
    // every post-`Running` read to a *recoverable* `StreamReadError`, which made
    // the fatal `StreamEnded` arm UNREACHABLE — iteration always ended via the
    // "Capture stopped" `StopIteration` and never surfaced the true terminal
    // reason. `read_chunk_blocking` lets the fatal terminal flow through, so the
    // `is_fatal()` arm below is now reachable and carries the real cause.

    /// `StreamEnded` (the natural end of capture) is FATAL, so `__next__` raises
    /// `StopIteration` on it — NOT a `StreamError`. With `read_chunk_blocking`
    /// this arm is reachable (it was dead under `read_buffer_blocking`'s
    /// `is_running()` guard), and the `StopIteration` carries the terminal
    /// reason string so a caller can observe *why* iteration ended.
    #[test]
    fn stream_ended_is_fatal_so_next_raises_stop_iteration() {
        let e = rsac::AudioError::StreamEnded {
            reason: "capture ended".into(),
        };
        assert!(
            e.is_fatal(),
            "StreamEnded must be fatal → __next__ ends via StopIteration"
        );
        assert!(!e.is_recoverable());
        // The terminal reason is surfaced as the StopIteration payload
        // (`e.to_string()`), so it is non-empty and carries the upstream cause.
        assert!(!e.to_string().is_empty());
    }

    /// A transient `StreamReadError` is RECOVERABLE, so `__next__` must retry it
    /// rather than ending iteration (the old inverted bug raised StopIteration
    /// here, cutting the stream short on a blip).
    #[test]
    fn stream_read_error_is_recoverable_so_next_retries() {
        let e = rsac::AudioError::StreamReadError {
            reason: "transient".into(),
        };
        assert!(
            e.is_recoverable(),
            "a transient StreamReadError must be recoverable → __next__ retries"
        );
        assert!(!e.is_fatal());
    }

    /// Buffer over/underruns are recoverable too — a momentary ring hiccup must
    /// not end iteration.
    #[test]
    fn buffer_over_underrun_are_recoverable() {
        assert!(rsac::AudioError::BufferOverrun { dropped_frames: 1 }.is_recoverable());
        assert!(rsac::AudioError::BufferUnderrun {
            requested: 1,
            available: 0,
        }
        .is_recoverable());
    }

    // ── rsac-8082: stop()/close() must not deadlock a parked blocking read ──
    //
    // `PyAudioCapture` stores `RwLock<Option<rsac::AudioCapture>>`, and a real
    // `rsac::AudioCapture` can only be built via the device-backed builder — so a
    // *silent-stream* real capture is not constructible from a unit test without
    // hardware, and this crate links against libpython via `extension-module`
    // (no `auto-initialize`), so `Python::with_gil` has no interpreter to attach
    // to under `cargo test`. We therefore reproduce the exact WRAPPER LOCK
    // TOPOLOGY *and the GIL re-acquisition protocol* the fix depends on, with a
    // faithful stand-in whose receivers mirror core:
    //
    //   - `read_chunk_blocking(&self)` parks until a terminal flag flips (the
    //     silent-stream case: a running stream that never delivers data);
    //   - `request_stop(&self)` flips that flag (core's unblock primitive — `&self`,
    //     does not touch the lock the parked read holds);
    //   - `stop(&mut self)` is the lifecycle mutator needing exclusive access.
    //
    // GIL MODEL (the load-bearing part for Python — the first review's Finding 2):
    // a `gil: Mutex<()>` stands in for CPython's GIL. The reader enters "holding
    // the GIL", takes its read guard, then models `py.allow_threads` by DROPPING
    // the GIL for the blocking read and RE-ACQUIRING it (blocking) before it can
    // return and drop its read guard. That re-acquire is exactly what turns a
    // GIL-holding `write()` in the teardown into a circular wait:
    //   - reader holds read guard, blocked re-acquiring the GIL;
    //   - teardown holds the GIL, blocked acquiring `write()` behind that guard.
    // The FIXED teardown runs the whole dance inside `allow_threads` (GIL released
    // across `write()`), so the reader re-acquires the GIL, unwinds, and drops its
    // guard → `write()` proceeds. The BROKEN teardown holds the GIL across
    // `write()` → deadlock. The distinguishing, deterministic signal is whether
    // the teardown's `write()` acquires within a bounded deadline; both threads
    // always terminate (the broken teardown gives up its bounded `write()` and
    // releases the GIL, which finally lets the reader unwind), so nothing leaks.

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, RwLock};
    use std::time::{Duration, Instant};

    /// Stand-in for a *silent* running `rsac::AudioCapture`: `read_chunk_blocking`
    /// never returns data until signalled terminal via `request_stop`. Receivers
    /// mirror core exactly (`read`/`request_stop` = `&self`, `stop` = `&mut self`).
    struct SilentCapture {
        terminal: AtomicBool,
        /// Flipped once a blocking read has actually parked, so the test signals
        /// stop only when the read is genuinely in flight (deterministic).
        parked: AtomicBool,
    }

    impl SilentCapture {
        fn new() -> Self {
            Self {
                terminal: AtomicBool::new(false),
                parked: AtomicBool::new(false),
            }
        }

        /// Blocks until `request_stop` flips terminal, then returns the terminal
        /// signal — a running silent stream behaves the same way.
        fn read_chunk_blocking(&self) -> std::result::Result<(), String> {
            self.parked.store(true, Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.terminal.load(Ordering::SeqCst) {
                if Instant::now() > deadline {
                    // Safety net so a genuine hang fails the test, not the suite.
                    return Err("read timed out (deadlock)".to_string());
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err("StreamEnded".to_string())
        }

        /// Core's `request_stop(&self)`: unblock the parked reader. `&self`, so it
        /// is compatible with the shared read guard the reader holds.
        fn request_stop(&self) {
            self.terminal.store(true, Ordering::SeqCst);
        }

        /// Core's `stop(&mut self)`: exclusive lifecycle mutator.
        fn stop(&mut self) {
            self.terminal.store(true, Ordering::SeqCst);
        }
    }

    type Slot = Arc<RwLock<Option<SilentCapture>>>;
    type Gil = Arc<Mutex<()>>;

    /// Spawn the reader thread and block (bounded) until it has genuinely parked
    /// in the blocking read. Models a Python `read()`/`__next__` step by step:
    /// (1) enter the pymethod holding the GIL; (2) take a SHARED read guard (GIL
    /// held); (3) `py.allow_threads(|| read_chunk_blocking())` — DROP the GIL, run
    /// the blocking read, then RE-ACQUIRE the GIL (blocking) before returning;
    /// (4) drop the read guard (only reachable once the GIL is re-acquired).
    /// Returns the reader's join handle; it yields the terminal read result.
    fn spawn_parked_reader(
        gil: &Gil,
        inner: &Slot,
    ) -> std::thread::JoinHandle<std::result::Result<(), String>> {
        let handle = {
            let gil = Arc::clone(gil);
            let inner = Arc::clone(inner);
            std::thread::spawn(move || {
                let gil_guard = gil.lock().expect("GIL"); // pymethod entered holding GIL
                let guard = inner.read().expect("read guard"); // shared read guard (GIL held)
                let cap = guard.as_ref().expect("capture present");
                // py.allow_threads(|| read_chunk_blocking()): release the GIL for
                // the blocking read...
                drop(gil_guard);
                let read_result = cap.read_chunk_blocking();
                // ...then RE-ACQUIRE the GIL before allow_threads returns. This
                // blocks if the teardown is holding the GIL — the deadlock edge.
                let regil = gil.lock().expect("GIL re-acquire");
                // The read guard was held across the whole allow_threads; it drops
                // now (only reachable once the GIL is back).
                drop(guard);
                drop(regil);
                read_result
            })
        };
        // Wait (bounded) until the read has genuinely parked (GIL already dropped,
        // read guard held) so the stopper exercises the wake path, not a pre-park.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if inner
                .read()
                .unwrap()
                .as_ref()
                .map(|c| c.parked.load(Ordering::SeqCst))
                .unwrap_or(false)
            {
                break;
            }
            assert!(Instant::now() < deadline, "reader never parked");
            std::thread::yield_now();
        }
        handle
    }

    /// The teardown's bounded `write()` acquisition (take the capture out + stop).
    /// Returns whether the write guard was acquired within `deadline`.
    fn write_take_bounded(inner: &Slot, deadline: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < deadline {
            if let Ok(mut guard) = inner.try_write() {
                if let Some(mut cap) = guard.take() {
                    cap.stop();
                }
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    /// FIXED teardown (what Python `stop()`/`close()` now do): run the read-guard
    /// `request_stop()` → `write()` dance INSIDE `allow_threads`, i.e. with the
    /// GIL released across `write()`. Returns whether `write()` acquired promptly.
    #[test]
    fn stop_does_not_deadlock_parked_blocking_read() {
        let gil: Gil = Arc::new(Mutex::new(()));
        let inner: Slot = Arc::new(RwLock::new(Some(SilentCapture::new())));
        let reader = spawn_parked_reader(&gil, &inner);

        let stopper = {
            let (gil, inner) = (Arc::clone(&gil), Arc::clone(&inner));
            std::thread::spawn(move || {
                let gil_guard = gil.lock().expect("GIL"); // pymethod entered holding GIL
                                                          // py.allow_threads(|| teardown): release the GIL for the WHOLE
                                                          // dance so the woken reader can re-acquire it and drop its guard.
                drop(gil_guard);
                {
                    let g = inner.read().expect("read guard for request_stop");
                    if let Some(cap) = g.as_ref() {
                        cap.request_stop();
                    }
                }
                let got = write_take_bounded(&inner, Duration::from_secs(5));
                // allow_threads re-acquires the GIL before returning.
                let _regil = gil.lock().expect("GIL re-acquire");
                got
            })
        };

        // stop() must finish (and acquire write) well within the reader's park
        // safety net; a regression (GIL held across write) makes write_take_bounded
        // time out → got == false, failing the assertion below rather than hanging.
        let got_write = stopper.join().expect("stopper thread joins");
        assert!(
            got_write,
            "FIXED stop() failed to acquire the write lock within the deadline — \
             it deadlocked against the parked reader (rsac-8082 regression: the \
             GIL must be released across write())"
        );

        // The parked reader was woken by request_stop() and saw the terminal
        // signal, not its 10 s deadlock safety net.
        let read_result = reader.join().expect("reader thread joins");
        let err = read_result.expect_err("silent read ends with a terminal error");
        assert!(
            err.contains("StreamEnded"),
            "reader must wake on the terminal signal, not the deadlock safety net; got: {err}"
        );
    }

    /// BROKEN teardown: hold the GIL ACROSS `write()` (what a naive port of the
    /// old `Mutex` binding does). The woken reader cannot re-acquire the GIL to
    /// drop its read guard, so `write()` never acquires within the deadline — the
    /// circular wait. Asserting the bounded `write()` times out proves the
    /// `allow_threads` wrap in `stop_does_not_deadlock_parked_blocking_read` is
    /// load-bearing (not just the RwLock split). Both threads still terminate: the
    /// bounded `write()` gives up and releases the GIL, finally letting the reader
    /// unwind — so the test observes the deadlock without leaking threads.
    #[test]
    fn gil_held_across_write_deadlocks_parked_reader() {
        let gil: Gil = Arc::new(Mutex::new(()));
        let inner: Slot = Arc::new(RwLock::new(Some(SilentCapture::new())));
        let reader = spawn_parked_reader(&gil, &inner);

        let stopper = {
            let (gil, inner) = (Arc::clone(&gil), Arc::clone(&inner));
            std::thread::spawn(move || {
                // Enter the pymethod holding the GIL and NEVER release it across
                // the teardown — the defect.
                let _gil_guard = gil.lock().expect("GIL");
                {
                    let g = inner.read().expect("read guard for request_stop");
                    if let Some(cap) = g.as_ref() {
                        cap.request_stop();
                    }
                }
                // Attempt write() while still holding the GIL. The reader has woken
                // (request_stop fired) but is blocked re-acquiring the GIL we hold,
                // so it still holds its read guard → this never acquires. `_gil_guard`
                // is held across this call and drops at end of scope (after the
                // return value is computed), finally freeing the reader for cleanup.
                write_take_bounded(&inner, Duration::from_secs(2))
            })
        };

        let got_write = stopper.join().expect("stopper thread joins");
        assert!(
            !got_write,
            "GIL-held-across-write teardown unexpectedly acquired the write lock \
             while a reader was parked — the topology no longer reproduces the \
             deadlock, so the regression test would be vacuous"
        );

        // Once the broken stopper released the GIL, the reader re-acquired it and
        // unwound; it still observed the terminal signal from request_stop().
        let read_result = reader.join().expect("reader thread joins");
        let err = read_result.expect_err("silent read ends with a terminal error");
        assert!(
            err.contains("StreamEnded"),
            "reader eventually woke on the terminal signal; got: {err}"
        );
    }
}
