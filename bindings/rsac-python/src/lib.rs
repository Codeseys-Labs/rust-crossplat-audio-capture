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
use std::sync::Mutex;

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
///     CaptureTarget.parse("app:<pid>")
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
///     max_channels: int
///     sample_rate_range: tuple[int, int]
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

    #[getter]
    fn max_channels(&self) -> u16 {
        self.inner.max_channels
    }

    #[getter]
    fn sample_rate_range(&self) -> (u32, u32) {
        self.inner.sample_rate_range
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
#[pyclass(name = "AudioCapture", module = "rsac._rsac")]
struct PyAudioCapture {
    /// We store the AudioCapture inside a Mutex so that we can take &self
    /// in __next__ (required by PyO3 iterator protocol) while still mutating.
    inner: Mutex<Option<rsac::AudioCapture>>,
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
            inner: Mutex::new(Some(capture)),
            iterating: Mutex::new(false),
        })
    }

    /// Start audio capture.
    ///
    /// Must be called before reading audio data. Called automatically when
    /// using AudioCapture as a context manager.
    fn start(&self, py: Python<'_>) -> PyResult<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock poisoned: {}", e)))?;
        let capture = guard
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("AudioCapture has been closed"))?;

        py.allow_threads(|| capture.start())
            .map_err(audio_error_to_pyerr)
    }

    /// Stop audio capture.
    ///
    /// Stops the underlying OS audio stream and releases resources.
    /// After stopping, the capture cannot be restarted.
    fn stop(&self, py: Python<'_>) -> PyResult<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock poisoned: {}", e)))?;
        let capture = guard
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("AudioCapture has been closed"))?;

        py.allow_threads(|| capture.stop())
            .map_err(audio_error_to_pyerr)
    }

    /// Whether the capture is currently running.
    #[getter]
    fn is_running(&self) -> PyResult<bool> {
        let guard = self
            .inner
            .lock()
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
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock poisoned: {}", e)))?;
        let capture = guard
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("AudioCapture has been closed"))?;

        // We need to call read_buffer which takes &mut self.
        // Release GIL for the blocking part.
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
    /// Raises StreamError if the stream is not running or encounters an error.
    fn read(&self, py: Python<'_>) -> PyResult<PyAudioBuffer> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock poisoned: {}", e)))?;
        let capture = guard
            .as_mut()
            .ok_or_else(|| PyRuntimeError::new_err("AudioCapture has been closed"))?;

        let result = py.allow_threads(|| capture.read_buffer_blocking());

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
            .lock()
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
            .lock()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock poisoned: {}", e)))?;
        let inner = guard.as_ref().map(|c| c.stream_stats()).unwrap_or_default();
        Ok(PyStreamStats { inner })
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
            .lock()
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
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock poisoned: {}", e)))?;

        if let Some(mut capture) = guard.take() {
            py.allow_threads(|| {
                let _ = capture.stop();
                drop(capture);
            });
        }

        Ok(())
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
        if let Ok(mut guard) = self.inner.lock() {
            if let Some(mut capture) = guard.take() {
                py.allow_threads(|| {
                    let _ = capture.stop();
                    drop(capture);
                });
            }
        }
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

            // Auto-start if not already running
            let guard = self_ref
                .inner
                .lock()
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
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| PyRuntimeError::new_err(format!("Lock poisoned: {}", e)))?;
        let capture = match guard.as_mut() {
            Some(c) => c,
            None => return Err(PyStopIteration::new_err("Capture closed")),
        };

        if !capture.is_running() {
            return Err(PyStopIteration::new_err("Capture stopped"));
        }

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
        loop {
            // Release GIL for the blocking read.
            let result = py.allow_threads(|| capture.read_chunk_blocking());

            match result {
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
                // stream is not yet initialized, or a transient backend hiccup),
                // so re-check the running state to avoid spinning forever after a
                // stop — a stop mid-iteration that has not yet reached the fatal
                // terminal ends cleanly via `StopIteration`.
                Err(e) if e.is_recoverable() => {
                    if !capture.is_running() {
                        return Err(PyStopIteration::new_err("Capture stopped"));
                    }
                    continue;
                }
                // Genuinely unexpected (neither recoverable nor a fatal terminal):
                // surface it as the mapped Python exception.
                Err(e) => return Err(audio_error_to_pyerr(e)),
            }
        }
    }

    fn __repr__(&self) -> String {
        let guard = self.inner.lock();
        match guard {
            Ok(ref g) => match g.as_ref() {
                Some(c) => format!("AudioCapture(running={})", c.is_running()),
                None => "AudioCapture(closed)".to_string(),
            },
            Err(_) => "AudioCapture(error)".to_string(),
        }
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
}
