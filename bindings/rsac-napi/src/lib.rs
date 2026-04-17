// bindings/rsac-napi/src/lib.rs
//
// Production-ready Node.js/TypeScript bindings for rsac (Rust Cross-Platform Audio Capture).
// Uses napi-rs to expose rsac's streaming-first audio capture API as native Node.js classes.

#[macro_use]
extern crate napi_derive;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ErrorStrategy, ThreadsafeFunction, ThreadsafeFunctionCallMode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

// ── Error conversion ─────────────────────────────────────────────────────

/// Convert rsac's AudioError into a napi::Error with structured code/message.
fn audio_err_to_napi(e: rsac::AudioError) -> napi::Error {
    let kind = e.kind();
    let code = match kind {
        rsac::ErrorKind::Configuration => "ERR_RSAC_CONFIGURATION",
        rsac::ErrorKind::Device => "ERR_RSAC_DEVICE",
        rsac::ErrorKind::Stream => "ERR_RSAC_STREAM",
        rsac::ErrorKind::Backend => "ERR_RSAC_BACKEND",
        rsac::ErrorKind::Application => "ERR_RSAC_APPLICATION",
        rsac::ErrorKind::Platform => "ERR_RSAC_PLATFORM",
        rsac::ErrorKind::Internal => "ERR_RSAC_INTERNAL",
    };
    napi::Error::new(napi::Status::GenericFailure, format!("[{}] {}", code, e))
}

// ── AudioChunk (JS-facing audio buffer representation) ───────────────────

/// A chunk of captured audio data exposed to JavaScript.
///
/// Contains interleaved Float32 PCM samples along with format metadata.
/// This is the primary data unit flowing through the JS capture pipeline.
#[napi(object)]
#[derive(Clone)]
pub struct AudioChunk {
    /// Interleaved Float32 PCM audio samples.
    pub data: Vec<f64>,
    /// Number of audio frames (samples per channel).
    pub num_frames: u32,
    /// Number of audio channels.
    pub channels: u32,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Total number of interleaved samples (num_frames * channels).
    pub length: u32,
    /// Duration of this chunk in seconds.
    pub duration: f64,
}

impl AudioChunk {
    fn from_rsac_buffer(buf: &rsac::AudioBuffer) -> Self {
        let data: Vec<f64> = buf.data().iter().map(|&s| s as f64).collect();
        let num_frames = buf.num_frames() as u32;
        let channels = buf.channels() as u32;
        let sample_rate = buf.sample_rate();
        let length = data.len() as u32;
        let duration = if sample_rate > 0 {
            num_frames as f64 / sample_rate as f64
        } else {
            0.0
        };
        AudioChunk {
            data,
            num_frames,
            channels,
            sample_rate,
            length,
            duration,
        }
    }
}

// ── CaptureTarget constructors ───────────────────────────────────────────

/// An opaque capture target value. Created via static factory methods on this class.
///
/// Usage from JS/TS:
/// ```js
/// CaptureTarget.systemDefault()
/// CaptureTarget.device("device-id-string")
/// CaptureTarget.application("app-session-id")
/// CaptureTarget.applicationByName("Firefox")
/// CaptureTarget.processTree(12345)
/// ```
#[napi]
pub struct CaptureTarget {
    inner: rsac::CaptureTarget,
}

#[napi]
impl CaptureTarget {
    /// Capture from the system default audio device.
    #[napi(factory)]
    pub fn system_default() -> Self {
        CaptureTarget {
            inner: rsac::CaptureTarget::SystemDefault,
        }
    }

    /// Capture from a specific audio device by ID.
    #[napi(factory)]
    pub fn device(device_id: String) -> Self {
        CaptureTarget {
            inner: rsac::CaptureTarget::Device(rsac::DeviceId(device_id)),
        }
    }

    /// Capture audio from a specific application by session ID.
    #[napi(factory)]
    pub fn application(app_id: String) -> Self {
        CaptureTarget {
            inner: rsac::CaptureTarget::Application(rsac::ApplicationId(app_id)),
        }
    }

    /// Capture audio from the first application matching the given name.
    #[napi(factory)]
    pub fn application_by_name(name: String) -> Self {
        CaptureTarget {
            inner: rsac::CaptureTarget::ApplicationByName(name),
        }
    }

    /// Capture audio from a process and all its child processes.
    #[napi(factory)]
    pub fn process_tree(pid: u32) -> Self {
        CaptureTarget {
            inner: rsac::CaptureTarget::ProcessTree(rsac::ProcessId(pid)),
        }
    }

    /// Returns a string description of this capture target.
    #[napi]
    pub fn describe(&self) -> String {
        match &self.inner {
            rsac::CaptureTarget::SystemDefault => "SystemDefault".to_string(),
            rsac::CaptureTarget::Device(id) => format!("Device({})", id),
            rsac::CaptureTarget::Application(id) => format!("Application({})", id),
            rsac::CaptureTarget::ApplicationByName(name) => {
                format!("ApplicationByName({})", name)
            }
            rsac::CaptureTarget::ProcessTree(pid) => format!("ProcessTree({})", pid),
        }
    }
}

// ── AudioCapture (main JS class) ─────────────────────────────────────────

/// The primary audio capture class for Node.js.
///
/// Wraps rsac's `AudioCaptureBuilder` → `AudioCapture` pipeline and exposes
/// streaming-first methods: `onData()` for push-based callbacks via
/// ThreadsafeFunction, `read()` for async pull, and `start()`/`stop()` for
/// lifecycle control.
///
/// ## Example
///
/// ```js
/// const capture = AudioCapture.create({
///   target: CaptureTarget.systemDefault(),
///   sampleRate: 48000,
///   channels: 2,
/// });
/// capture.onData((chunk) => {
///   console.log(`Got ${chunk.numFrames} frames`);
/// });
/// capture.start();
/// // ... later ...
/// capture.stop();
/// ```
#[napi]
pub struct AudioCapture {
    inner: Arc<Mutex<rsac::AudioCapture>>,
    /// Active data callback (ThreadsafeFunction). Held here to prevent GC.
    callback: Arc<Mutex<Option<ThreadsafeFunction<AudioChunk, ErrorStrategy::Fatal>>>>,
    /// Whether the push-model data pump thread is running.
    pump_active: Arc<AtomicBool>,
}

#[napi]
impl AudioCapture {
    /// Create a new AudioCapture with the system default target and default settings.
    #[napi(constructor)]
    pub fn new() -> Result<Self> {
        let capture = rsac::AudioCaptureBuilder::new()
            .with_target(rsac::CaptureTarget::SystemDefault)
            .build()
            .map_err(audio_err_to_napi)?;

        Ok(AudioCapture {
            inner: Arc::new(Mutex::new(capture)),
            callback: Arc::new(Mutex::new(None)),
            pump_active: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Create a new AudioCapture with a specific target and optional settings.
    ///
    /// @param target - The capture target (from CaptureTarget static methods).
    /// @param sampleRate - Desired sample rate in Hz (default: 48000).
    /// @param channels - Desired number of channels (default: 2).
    /// @param bufferSize - Desired buffer size in frames (optional).
    #[napi(factory)]
    pub fn create(
        target: &CaptureTarget,
        sample_rate: Option<u32>,
        channels: Option<u32>,
        buffer_size: Option<u32>,
    ) -> Result<Self> {
        let mut builder = rsac::AudioCaptureBuilder::new().with_target(target.inner.clone());

        if let Some(sr) = sample_rate {
            builder = builder.sample_rate(sr);
        }
        if let Some(ch) = channels {
            builder = builder.channels(ch as u16);
        }
        if let Some(bs) = buffer_size {
            builder = builder.buffer_size(Some(bs as usize));
        }

        let capture = builder.build().map_err(audio_err_to_napi)?;

        Ok(AudioCapture {
            inner: Arc::new(Mutex::new(capture)),
            callback: Arc::new(Mutex::new(None)),
            pump_active: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Start audio capture.
    ///
    /// If an `onData` callback is registered, a background thread is spawned
    /// that reads audio chunks and pushes them to JavaScript via a
    /// ThreadsafeFunction.
    #[napi]
    pub fn start(&self) -> Result<()> {
        {
            let mut inner = self.inner.lock().map_err(|e| {
                napi::Error::new(
                    napi::Status::GenericFailure,
                    format!("Lock poisoned: {}", e),
                )
            })?;
            inner.start().map_err(audio_err_to_napi)?;
        }

        // If a callback is registered, start the data pump thread
        let has_callback = {
            let cb = self.callback.lock().map_err(|e| {
                napi::Error::new(
                    napi::Status::GenericFailure,
                    format!("Lock poisoned: {}", e),
                )
            })?;
            cb.is_some()
        };

        if has_callback && !self.pump_active.load(Ordering::SeqCst) {
            self.start_data_pump()?;
        }

        Ok(())
    }

    /// Stop audio capture and release resources.
    ///
    /// Active data pump threads are terminated. The callback is preserved
    /// and can be reused if a new capture session is started.
    #[napi]
    pub fn stop(&self) -> Result<()> {
        // Signal the data pump to stop
        self.pump_active.store(false, Ordering::SeqCst);

        let mut inner = self.inner.lock().map_err(|e| {
            napi::Error::new(
                napi::Status::GenericFailure,
                format!("Lock poisoned: {}", e),
            )
        })?;

        inner.stop().map_err(audio_err_to_napi)?;
        Ok(())
    }

    /// Returns whether the capture is currently running.
    #[napi(getter)]
    pub fn is_running(&self) -> Result<bool> {
        let inner = self.inner.lock().map_err(|e| {
            napi::Error::new(
                napi::Status::GenericFailure,
                format!("Lock poisoned: {}", e),
            )
        })?;
        Ok(inner.is_running())
    }

    /// Read a single audio chunk (non-blocking).
    ///
    /// Returns `null` if no data is currently available.
    /// Throws if the capture is not running.
    #[napi]
    pub fn read(&self) -> Result<Option<AudioChunk>> {
        let mut inner = self.inner.lock().map_err(|e| {
            napi::Error::new(
                napi::Status::GenericFailure,
                format!("Lock poisoned: {}", e),
            )
        })?;

        let result = inner.read_buffer().map_err(audio_err_to_napi)?;
        Ok(result.map(|buf| AudioChunk::from_rsac_buffer(&buf)))
    }

    /// Read a single audio chunk, blocking until data is available.
    ///
    /// WARNING: This blocks the calling thread. In Node.js, prefer
    /// `onData()` for push-based streaming or use `read()` in a loop
    /// with appropriate yielding.
    #[napi]
    pub fn read_blocking(&self) -> Result<AudioChunk> {
        let mut inner = self.inner.lock().map_err(|e| {
            napi::Error::new(
                napi::Status::GenericFailure,
                format!("Lock poisoned: {}", e),
            )
        })?;

        let result = inner.read_buffer_blocking().map_err(audio_err_to_napi)?;
        Ok(AudioChunk::from_rsac_buffer(&result))
    }

    /// Read a single audio chunk asynchronously (non-blocking, off main thread).
    ///
    /// Returns `null` if no data is currently available.
    /// Throws if the capture is not running.
    #[napi]
    pub async fn read_async(&self) -> Result<Option<AudioChunk>> {
        let inner = self.inner.clone();

        let result =
            tokio::task::spawn_blocking(move || -> napi::Result<Option<rsac::AudioBuffer>> {
                let mut capture = inner.lock().map_err(|e| {
                    napi::Error::new(
                        napi::Status::GenericFailure,
                        format!("Lock poisoned: {}", e),
                    )
                })?;
                capture.read_buffer().map_err(audio_err_to_napi)
            })
            .await
            .map_err(|e| {
                napi::Error::new(
                    napi::Status::GenericFailure,
                    format!("Task join error: {}", e),
                )
            })??;

        Ok(result.map(|buf| AudioChunk::from_rsac_buffer(&buf)))
    }

    /// Read a single audio chunk asynchronously, blocking the worker thread
    /// until data is available.
    ///
    /// This is useful for consuming audio in an async loop without busy-spinning.
    #[napi]
    pub async fn read_blocking_async(&self) -> Result<AudioChunk> {
        let inner = self.inner.clone();

        let result = tokio::task::spawn_blocking(move || -> napi::Result<rsac::AudioBuffer> {
            let mut capture = inner.lock().map_err(|e| {
                napi::Error::new(
                    napi::Status::GenericFailure,
                    format!("Lock poisoned: {}", e),
                )
            })?;
            capture.read_buffer_blocking().map_err(audio_err_to_napi)
        })
        .await
        .map_err(|e| {
            napi::Error::new(
                napi::Status::GenericFailure,
                format!("Task join error: {}", e),
            )
        })??;

        Ok(AudioChunk::from_rsac_buffer(&result))
    }

    /// Register a callback for push-based audio data delivery.
    ///
    /// The callback receives `AudioChunk` objects as audio is captured.
    /// This is the most efficient way to consume audio data from Node.js —
    /// it uses a ThreadsafeFunction to push data directly from the Rust
    /// capture thread to JavaScript.
    ///
    /// If capture is already running, the data pump starts immediately.
    /// If not, it starts when `start()` is called.
    ///
    /// Only one callback can be active at a time. Calling `onData()` again
    /// replaces the previous callback.
    #[napi(ts_args_type = "callback: (chunk: AudioChunk) => void")]
    pub fn on_data(
        &self,
        callback: ThreadsafeFunction<AudioChunk, ErrorStrategy::Fatal>,
    ) -> Result<()> {
        // Store the callback
        {
            let mut cb_guard = self.callback.lock().map_err(|e| {
                napi::Error::new(
                    napi::Status::GenericFailure,
                    format!("Lock poisoned: {}", e),
                )
            })?;
            *cb_guard = Some(callback);
        }

        // If already running, start the data pump now
        let is_running = {
            let inner = self.inner.lock().map_err(|e| {
                napi::Error::new(
                    napi::Status::GenericFailure,
                    format!("Lock poisoned: {}", e),
                )
            })?;
            inner.is_running()
        };

        if is_running && !self.pump_active.load(Ordering::SeqCst) {
            self.start_data_pump()?;
        }

        Ok(())
    }

    /// Remove the registered data callback.
    ///
    /// Stops the data pump thread if running.
    #[napi]
    pub fn off_data(&self) -> Result<()> {
        self.pump_active.store(false, Ordering::SeqCst);

        let mut cb_guard = self.callback.lock().map_err(|e| {
            napi::Error::new(
                napi::Status::GenericFailure,
                format!("Lock poisoned: {}", e),
            )
        })?;
        *cb_guard = None;
        Ok(())
    }

    /// Returns the number of audio buffers dropped due to ring buffer overflow.
    ///
    /// A non-zero value means the JavaScript consumer is not keeping up with
    /// the audio producer.
    #[napi(getter)]
    pub fn overrun_count(&self) -> Result<u32> {
        let inner = self.inner.lock().map_err(|e| {
            napi::Error::new(
                napi::Status::GenericFailure,
                format!("Lock poisoned: {}", e),
            )
        })?;
        Ok(inner.overrun_count() as u32)
    }
}

// ── AudioCapture private helpers ─────────────────────────────────────────

impl AudioCapture {
    /// Spawn a background thread that reads audio buffers from rsac and pushes
    /// them to JavaScript via the registered ThreadsafeFunction callback.
    fn start_data_pump(&self) -> Result<()> {
        let inner = self.inner.clone();
        let callback = self.callback.clone();
        let pump_active = self.pump_active.clone();

        pump_active.store(true, Ordering::SeqCst);

        std::thread::Builder::new()
            .name("rsac-napi-pump".into())
            .spawn(move || {
                while pump_active.load(Ordering::SeqCst) {
                    // Try to read a buffer
                    let maybe_buf = {
                        let mut capture = match inner.lock() {
                            Ok(c) => c,
                            Err(_) => break, // Mutex poisoned, bail
                        };
                        capture.read_buffer()
                    };

                    match maybe_buf {
                        Ok(Some(buf)) => {
                            let chunk = AudioChunk::from_rsac_buffer(&buf);
                            let cb = match callback.lock() {
                                Ok(c) => c,
                                Err(_) => break,
                            };
                            if let Some(ref tsfn) = *cb {
                                tsfn.call(chunk, ThreadsafeFunctionCallMode::NonBlocking);
                            }
                        }
                        Ok(None) => {
                            // No data available, yield briefly to avoid busy-spinning
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                        Err(_) => {
                            // Stream error — stop pumping
                            break;
                        }
                    }
                }
                pump_active.store(false, Ordering::SeqCst);
            })
            .map_err(|e| {
                napi::Error::new(
                    napi::Status::GenericFailure,
                    format!("Failed to spawn data pump thread: {}", e),
                )
            })?;

        Ok(())
    }
}

// ── Device enumeration ───────────────────────────────────────────────────

/// Information about an audio device.
#[napi(object)]
pub struct JsAudioDevice {
    /// Unique platform-specific device identifier.
    pub id: String,
    /// Human-readable device name.
    pub name: String,
    /// Whether this is the system default device.
    pub is_default: bool,
}

/// List all available audio devices on the system.
///
/// Returns an array of device objects with id, name, and isDefault fields.
/// This is an async operation that performs device enumeration on a worker thread.
#[napi]
pub async fn list_devices() -> Result<Vec<JsAudioDevice>> {
    tokio::task::spawn_blocking(|| -> napi::Result<Vec<JsAudioDevice>> {
        let enumerator = rsac::get_device_enumerator().map_err(audio_err_to_napi)?;
        let devices = enumerator.enumerate_devices().map_err(audio_err_to_napi)?;

        let js_devices: Vec<JsAudioDevice> = devices
            .iter()
            .map(|d| JsAudioDevice {
                id: d.id().to_string(),
                name: d.name(),
                is_default: d.is_default(),
            })
            .collect();

        Ok(js_devices)
    })
    .await
    .map_err(|e| {
        napi::Error::new(
            napi::Status::GenericFailure,
            format!("Task join error: {}", e),
        )
    })?
}

/// Get the default audio device.
///
/// Returns a device object with id, name, and isDefault fields.
#[napi]
pub async fn get_default_device() -> Result<JsAudioDevice> {
    tokio::task::spawn_blocking(|| -> napi::Result<JsAudioDevice> {
        let enumerator = rsac::get_device_enumerator().map_err(audio_err_to_napi)?;
        let device = enumerator
            .get_default_device()
            .map_err(audio_err_to_napi)?;

        Ok(JsAudioDevice {
            id: device.id().to_string(),
            name: device.name(),
            is_default: device.is_default(),
        })
    })
    .await
    .map_err(|e| {
        napi::Error::new(
            napi::Status::GenericFailure,
            format!("Task join error: {}", e),
        )
    })?
}

// ── Platform capabilities ────────────────────────────────────────────────

/// Platform capability information.
#[napi(object)]
pub struct JsPlatformCapabilities {
    /// Whether system-wide audio capture is supported.
    pub supports_system_capture: bool,
    /// Whether per-application audio capture is supported.
    pub supports_application_capture: bool,
    /// Whether process-tree audio capture is supported.
    pub supports_process_tree_capture: bool,
    /// Whether device selection is supported.
    pub supports_device_selection: bool,
    /// Maximum number of channels supported.
    pub max_channels: u32,
    /// Minimum supported sample rate in Hz.
    pub min_sample_rate: u32,
    /// Maximum supported sample rate in Hz.
    pub max_sample_rate: u32,
    /// Name of the audio backend (e.g., "WASAPI", "CoreAudio", "PipeWire").
    pub backend_name: String,
}

/// Query the audio capabilities of the current platform.
///
/// Returns information about what capture modes, sample rates, and
/// channel configurations are supported.
#[napi]
pub fn platform_capabilities() -> JsPlatformCapabilities {
    let caps = rsac::PlatformCapabilities::query();
    JsPlatformCapabilities {
        supports_system_capture: caps.supports_system_capture,
        supports_application_capture: caps.supports_application_capture,
        supports_process_tree_capture: caps.supports_process_tree_capture,
        supports_device_selection: caps.supports_device_selection,
        max_channels: caps.max_channels as u32,
        min_sample_rate: caps.sample_rate_range.0,
        max_sample_rate: caps.sample_rate_range.1,
        backend_name: caps.backend_name.to_string(),
    }
}
