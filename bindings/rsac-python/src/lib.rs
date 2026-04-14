//! Python bindings for rsac (Rust Cross-Platform Audio Capture).
//!
//! This module exposes rsac's streaming-first audio capture API to Python
//! via PyO3. The design philosophy: rsac is a downstream audio pipeline
//! enabler — the Python API exposes streaming as first-class (iterators,
//! callbacks, context managers), not just file capture.

use pyo3::exceptions::{PyOSError, PyRuntimeError, PyStopIteration, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
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

    m.add(
        "RsacError",
        m.py().get_type::<RsacError>(),
    )?;
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
    m.add(
        "StreamError",
        m.py().get_type::<StreamError>(),
    )?;
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
    m.add(
        "BackendError",
        m.py().get_type::<BackendError>(),
    )?;
    Ok(())
}

pyo3::create_exception!(_rsac, RsacError, PyOSError, "Base exception for all rsac errors.");
pyo3::create_exception!(_rsac, DeviceNotFoundError, RsacError, "The requested audio device was not found.");
pyo3::create_exception!(_rsac, DeviceNotAvailableError, RsacError, "The audio device exists but is not currently available.");
pyo3::create_exception!(_rsac, PlatformNotSupportedError, RsacError, "The requested feature is not supported on this platform.");
pyo3::create_exception!(_rsac, StreamError, RsacError, "An error occurred during audio stream operation.");
pyo3::create_exception!(_rsac, ConfigurationError, PyValueError, "Invalid capture configuration.");
pyo3::create_exception!(_rsac, PermissionDeniedError, RsacError, "Permission denied for the requested audio operation.");
pyo3::create_exception!(_rsac, ApplicationNotFoundError, RsacError, "The target application for capture was not found.");
pyo3::create_exception!(_rsac, CaptureTimeoutError, RsacError, "An audio capture operation timed out.");
pyo3::create_exception!(_rsac, BackendError, RsacError, "A platform-specific audio backend error occurred.");

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
        | rsac::AudioError::BufferOverrun { .. }
        | rsac::AudioError::BufferUnderrun { .. } => StreamError::new_err(msg),

        rsac::AudioError::BackendError { .. }
        | rsac::AudioError::BackendNotAvailable { .. }
        | rsac::AudioError::BackendInitializationFailed { .. } => BackendError::new_err(msg),

        rsac::AudioError::ApplicationNotFound { .. }
        | rsac::AudioError::ApplicationCaptureFailed { .. } => {
            ApplicationNotFoundError::new_err(msg)
        }

        rsac::AudioError::PlatformNotSupported { .. } => {
            PlatformNotSupportedError::new_err(msg)
        }
        rsac::AudioError::PermissionDenied { .. } => PermissionDeniedError::new_err(msg),

        rsac::AudioError::Timeout { .. } => CaptureTimeoutError::new_err(msg),

        rsac::AudioError::InternalError { .. } => RsacError::new_err(msg),
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
        }
    }

    fn __str__(&self) -> String {
        self.__repr__()
    }
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
    /// Suitable for writing to files or passing to audio processing libraries.
    fn to_bytes<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        let data = self.inner.data();
        // Safety: reinterpret &[f32] as &[u8]
        let byte_slice = unsafe {
            std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 4)
        };
        PyBytes::new(py, byte_slice)
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
    /// Returns 0.0 for an empty buffer.
    fn rms(&self) -> f32 {
        let data = self.inner.data();
        if data.is_empty() {
            return 0.0;
        }
        let sum_sq: f64 = data.iter().map(|&s| (s as f64) * (s as f64)).sum();
        (sum_sq / data.len() as f64).sqrt() as f32
    }

    /// Return the peak absolute sample value.
    ///
    /// Returns 0.0 for an empty buffer.
    fn peak(&self) -> f32 {
        self.inner
            .data()
            .iter()
            .map(|s| s.abs())
            .fold(0.0f32, f32::max)
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

        let capture = py.allow_threads(|| {
            let mut builder = rsac::AudioCaptureBuilder::new()
                .with_target(rust_target)
                .sample_rate(sample_rate)
                .channels(channels);

            if let Some(size) = buffer_size {
                builder = builder.buffer_size(Some(size));
            }

            builder.build()
        }).map_err(audio_error_to_pyerr)?;

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
        let mut guard = self.inner.lock().map_err(|e| {
            PyRuntimeError::new_err(format!("Lock poisoned: {}", e))
        })?;
        let capture = guard.as_mut().ok_or_else(|| {
            PyRuntimeError::new_err("AudioCapture has been closed")
        })?;

        py.allow_threads(|| capture.start()).map_err(audio_error_to_pyerr)
    }

    /// Stop audio capture.
    ///
    /// Stops the underlying OS audio stream and releases resources.
    /// After stopping, the capture cannot be restarted.
    fn stop(&self, py: Python<'_>) -> PyResult<()> {
        let mut guard = self.inner.lock().map_err(|e| {
            PyRuntimeError::new_err(format!("Lock poisoned: {}", e))
        })?;
        let capture = guard.as_mut().ok_or_else(|| {
            PyRuntimeError::new_err("AudioCapture has been closed")
        })?;

        py.allow_threads(|| capture.stop()).map_err(audio_error_to_pyerr)
    }

    /// Whether the capture is currently running.
    #[getter]
    fn is_running(&self) -> PyResult<bool> {
        let guard = self.inner.lock().map_err(|e| {
            PyRuntimeError::new_err(format!("Lock poisoned: {}", e))
        })?;
        Ok(guard.as_ref().map(|c| c.is_running()).unwrap_or(false))
    }

    /// Read the next audio buffer (non-blocking).
    ///
    /// Returns an AudioBuffer if data is available, or None if no data
    /// is ready yet. Raises StreamError if the stream is not running.
    ///
    /// The GIL is released during the read operation.
    fn try_read(&self, py: Python<'_>) -> PyResult<Option<PyAudioBuffer>> {
        let mut guard = self.inner.lock().map_err(|e| {
            PyRuntimeError::new_err(format!("Lock poisoned: {}", e))
        })?;
        let capture = guard.as_mut().ok_or_else(|| {
            PyRuntimeError::new_err("AudioCapture has been closed")
        })?;

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
        let mut guard = self.inner.lock().map_err(|e| {
            PyRuntimeError::new_err(format!("Lock poisoned: {}", e))
        })?;
        let capture = guard.as_mut().ok_or_else(|| {
            PyRuntimeError::new_err("AudioCapture has been closed")
        })?;

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
        let guard = self.inner.lock().map_err(|e| {
            PyRuntimeError::new_err(format!("Lock poisoned: {}", e))
        })?;
        Ok(guard.as_ref().map(|c| c.overrun_count()).unwrap_or(0))
    }

    /// Close the capture and release all resources.
    ///
    /// After closing, the capture cannot be used. This is called automatically
    /// when exiting a `with` block.
    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let mut guard = self.inner.lock().map_err(|e| {
            PyRuntimeError::new_err(format!("Lock poisoned: {}", e))
        })?;

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

    // ── Iterator Protocol ────────────────────────────────────────────

    fn __iter__(slf: Py<Self>, py: Python<'_>) -> PyResult<Py<Self>> {
        {
            let self_ref = slf.borrow(py);
            let mut iter_guard = self_ref.iterating.lock().map_err(|e| {
                PyRuntimeError::new_err(format!("Lock poisoned: {}", e))
            })?;
            *iter_guard = true;

            // Auto-start if not already running
            let guard = self_ref.inner.lock().map_err(|e| {
                PyRuntimeError::new_err(format!("Lock poisoned: {}", e))
            })?;
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
        let mut guard = self.inner.lock().map_err(|e| {
            PyRuntimeError::new_err(format!("Lock poisoned: {}", e))
        })?;
        let capture = match guard.as_mut() {
            Some(c) => c,
            None => return Err(PyStopIteration::new_err("Capture closed")),
        };

        if !capture.is_running() {
            return Err(PyStopIteration::new_err("Capture stopped"));
        }

        // Release GIL for the blocking read.
        let result = py.allow_threads(|| capture.read_buffer_blocking());

        match result {
            Ok(buf) => Ok(Some(PyAudioBuffer { inner: buf })),
            Err(rsac::AudioError::StreamReadError { .. }) => {
                // Stream ended — stop iteration
                Err(PyStopIteration::new_err("Stream ended"))
            }
            Err(e) => Err(audio_error_to_pyerr(e)),
        }
    }

    fn __repr__(&self) -> String {
        let guard = self.inner.lock();
        match guard {
            Ok(ref g) => match g.as_ref() {
                Some(c) => format!(
                    "AudioCapture(running={})",
                    c.is_running()
                ),
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
    m.add_class::<PyAudioCapture>()?;

    // Register module-level functions
    m.add_function(wrap_pyfunction!(list_devices, m)?)?;
    m.add_function(wrap_pyfunction!(platform_capabilities, m)?)?;

    // Register exception hierarchy
    create_exception_classes(m)?;

    // Module metadata
    m.add("__version__", "0.1.0")?;

    Ok(())
}
