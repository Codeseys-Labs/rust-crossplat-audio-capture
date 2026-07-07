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
///
/// `data` is a native JS `Float32Array` carrying the captured `f32` samples
/// directly — there is no `f32` -> `f64` widening on the delivery path, so the
/// values JS observes are bit-for-bit identical to the source samples.
#[napi(object)]
pub struct AudioChunk {
    /// Interleaved Float32 PCM audio samples (native `Float32Array`, no
    /// precision widening).
    pub data: Float32Array,
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
    /// Root-mean-square (RMS) level across **all** samples/channels, in linear
    /// `0.0..=1.0`. Computed once (alloc-free) by rsac core; `0.0` for silence
    /// or an empty chunk, never `NaN`.
    pub rms: f64,
    /// Peak (maximum absolute) level across all samples/channels, in linear
    /// `0.0..=1.0`. `0.0` for an empty chunk, never `NaN`.
    pub peak: f64,
    /// RMS level in dBFS (`20·log10(rms)`). `-Infinity` at silence; `0.0` dBFS
    /// is full scale. Computed by rsac core.
    pub rms_dbfs: f64,
    /// Peak level in dBFS (`20·log10(peak)`). `-Infinity` at silence; `0.0` dBFS
    /// is full scale. Computed by rsac core.
    pub peak_dbfs: f64,
    /// Per-channel RMS levels, one entry per channel in channel order, linear
    /// `0.0..=1.0`. `channelRms[ch]` is the RMS of channel `ch`. Empty when the
    /// chunk reports `0` channels. Computed once by rsac core's strided,
    /// alloc-free `channel_rms` (a channel with no finite samples is `0.0`).
    pub channel_rms: Vec<f64>,
    /// Per-channel peak levels, one entry per channel in channel order, linear
    /// `0.0..=1.0`. `channelPeak[ch]` is the peak of channel `ch`. Empty when
    /// the chunk reports `0` channels. Computed once by rsac core's strided,
    /// alloc-free `channel_peak`.
    pub channel_peak: Vec<f64>,
}

impl AudioChunk {
    fn from_rsac_buffer(buf: &rsac::AudioBuffer) -> Self {
        // Pre-compute the whole-buffer metering scalars via rsac core's
        // alloc-free, NaN-safe meters *before* we move the samples out — the
        // values JS sees are exactly what core measured (no re-derivation in JS,
        // no precision loss beyond the f32 -> f64 widen of a single scalar).
        let rms = buf.rms() as f64;
        let peak = buf.peak() as f64;
        let rms_dbfs = buf.rms_dbfs() as f64;
        let peak_dbfs = buf.peak_dbfs() as f64;

        // Per-channel meters. rsac core's `channel_rms(ch)`/`channel_peak(ch)`
        // are parameterized, and a `#[napi(object)]` is a plain JS object that
        // cannot carry methods — and the package entry point re-exports a fixed
        // symbol set, so free functions would be unreachable. So materialize a
        // small per-channel array (one f64 per channel) here. Core returns
        // `None` only for an out-of-range index, which cannot happen for
        // `0..channels`, so `unwrap_or(0.0)` is a defensive no-op.
        let channels = buf.channels() as u32;
        let mut channel_rms = Vec::with_capacity(channels as usize);
        let mut channel_peak = Vec::with_capacity(channels as usize);
        for ch in 0..channels {
            channel_rms.push(buf.channel_rms(ch as u16).unwrap_or(0.0) as f64);
            channel_peak.push(buf.channel_peak(ch as u16).unwrap_or(0.0) as f64);
        }

        // Carry the interleaved f32 samples straight through to a native JS
        // Float32Array. We take a single owned Vec<f32> (one allocation, same
        // cost as the previous Vec<f64> collect but half the width) and hand
        // it to napi's Float32Array, which adopts the buffer without any
        // per-sample f32 -> f64 conversion.
        let samples = buf.data().to_vec();
        let length = samples.len() as u32;
        let num_frames = buf.num_frames() as u32;
        let sample_rate = buf.sample_rate();
        let duration = if sample_rate > 0 {
            num_frames as f64 / sample_rate as f64
        } else {
            0.0
        };
        AudioChunk {
            data: Float32Array::new(samples),
            num_frames,
            channels,
            sample_rate,
            length,
            duration,
            rms,
            peak,
            rms_dbfs,
            peak_dbfs,
            channel_rms,
            channel_peak,
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

    /// Parse a capture target from its canonical string form.
    ///
    /// Accepts the cross-binding grammar (the scheme is case-insensitive):
    /// - `"system"` / `"default"` → system default mix
    /// - `"device:<id>"` → a specific device (the id may itself contain colons,
    ///   e.g. `"device:hw:0,0"`)
    /// - `"app:<id>"` → a specific application session
    /// - `"name:<name>"` → the first application matching `<name>`
    /// - `"tree:<pid>"` / `"pid:<pid>"` → a process and its children
    ///
    /// Throws a JS `Error` (code `ERR_RSAC_CONFIGURATION`) on an unknown scheme
    /// or a non-numeric / out-of-range pid. Never panics.
    ///
    /// ```js
    /// CaptureTarget.parse("system")
    /// CaptureTarget.parse("name:Firefox")
    /// CaptureTarget.parse("pid:1234")
    /// ```
    #[napi(factory)]
    pub fn parse(spec: String) -> Result<Self> {
        // Parses via rsac's CaptureTarget::FromStr; an invalid spec surfaces as
        // AudioError::InvalidParameter, mapped to a thrown JS Error.
        let inner: rsac::CaptureTarget = spec.parse().map_err(audio_err_to_napi)?;
        Ok(CaptureTarget { inner })
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
            // `rsac::CaptureTarget` is `#[non_exhaustive]`: a future variant added
            // in a minor release lands here rather than breaking the build. Fall
            // back to the upstream canonical string form (its in-crate `Display`
            // impl is exhaustive, so it renders any variant) so describe() stays
            // forward-compatible. Every variant known at this version has a
            // dedicated arm above; this only fires for additions.
            other => other.to_string(),
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
/// The per-buffer data callback held behind an `Arc<Mutex<Option<…>>>` (to
/// prevent GC + allow re-registration). Aliased to keep the struct field within
/// clippy's `type_complexity` bar.
type DataCallback = Arc<Mutex<Option<ThreadsafeFunction<AudioChunk, ErrorStrategy::Fatal>>>>;
/// The terminal-observability callback, carrying the optional terminal cause
/// string. Same storage shape as [`DataCallback`].
type EndCallback = Arc<Mutex<Option<ThreadsafeFunction<Option<String>, ErrorStrategy::Fatal>>>>;

#[napi]
pub struct AudioCapture {
    inner: Arc<Mutex<rsac::AudioCapture>>,
    /// Active data callback (ThreadsafeFunction). Held here to prevent GC.
    callback: DataCallback,
    /// Optional terminal-observability callback (ThreadsafeFunction). Fires once
    /// when the data pump ends, carrying the terminal cause so a JS `onData`
    /// consumer can observe *why* delivery stopped — parity with Rust
    /// `subscribe_with_errors` / Go `StreamWithErrors`. Held here to prevent GC.
    end_callback: EndCallback,
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
            end_callback: Arc::new(Mutex::new(None)),
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
            end_callback: Arc::new(Mutex::new(None)),
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
    ///
    /// Not terminal-observable: this (and `read_blocking`/`read_async`/
    /// `read_blocking_async`) routes through rsac's `read_buffer`, which
    /// short-circuits to a *recoverable* `StreamReadError` the moment the stream
    /// leaves `Running` and never surfaces the terminal `StreamEnded`. Only the
    /// push pump started by `on_data` drains the terminal state and reports it via
    /// `on_end`. A pull consumer should treat `stop`/`is_running` as the
    /// end-of-stream signal and a thrown read error as retryable.
    #[napi]
    pub fn read(&self) -> Result<Option<AudioChunk>> {
        // `read_buffer` takes `&self` now, so the guard does not need `mut`.
        let inner = self.inner.lock().map_err(|e| {
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
    ///
    /// Like `read`, this is not terminal-observable: it routes through
    /// `read_buffer_blocking` and surfaces only recoverable errors, never the
    /// terminal `StreamEnded`. Use `on_data` + `on_end` to observe the terminal
    /// reason.
    #[napi]
    pub fn read_blocking(&self) -> Result<AudioChunk> {
        // `read_buffer_blocking` takes `&self` now, so the guard does not need `mut`.
        let inner = self.inner.lock().map_err(|e| {
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
                // `read_buffer` takes `&self`, so the guard does not need `mut`.
                let capture = inner.lock().map_err(|e| {
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
            // `read_buffer_blocking` takes `&self`, so the guard does not need `mut`.
            let capture = inner.lock().map_err(|e| {
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

    /// Register a callback that fires exactly once when push-based delivery ends,
    /// carrying *why* it ended.
    ///
    /// The data pump started by `onData` ends in one of two ways, and a plain
    /// `onData` consumer cannot otherwise distinguish them:
    ///
    /// - **Terminal (fatal):** the backend stream reached its terminal state
    ///   (e.g. `StreamEnded`). The callback's `error` argument is the formatted
    ///   terminal `AudioError` message (a non-null string).
    /// - **Clean stop:** the pump was torn down by `stop` / `offData`, or the
    ///   read mutex was poisoned. The callback's `error` argument is `null`.
    ///
    /// This is the Node parity for Rust's `subscribe_with_errors` and Go's
    /// `StreamWithErrors`: an `onData` consumer registers `onEnd` to learn the
    /// terminal reason instead of the pump silently logging it. Registering an
    /// `onEnd` callback is optional and independent of `onData`.
    ///
    /// The registration persists across multiple `start`/`stop` sessions on the
    /// same `AudioCapture`, exactly like `onData`: it fires once per session (at
    /// each pump run's end) and stays armed for the next session. The pump fires a
    /// *clone* and leaves the registered callback in place; it is cleared only by
    /// `offEnd` or replaced by a later `onEnd` call. A recoverable hiccup never
    /// fires it — only the single terminal end (fatal) or a clean stop does.
    #[napi(ts_args_type = "callback: (error: string | null) => void")]
    pub fn on_end(
        &self,
        callback: ThreadsafeFunction<Option<String>, ErrorStrategy::Fatal>,
    ) -> Result<()> {
        let mut cb_guard = self.end_callback.lock().map_err(|e| {
            napi::Error::new(
                napi::Status::GenericFailure,
                format!("Lock poisoned: {}", e),
            )
        })?;
        *cb_guard = Some(callback);
        Ok(())
    }

    /// Remove the registered data callback.
    ///
    /// Stops the data pump thread if running. The terminal-observability
    /// callback registered via `onEnd` is left registered for a later session;
    /// use `offEnd` to clear it.
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

    /// Remove the registered terminal-observability callback (see `onEnd`).
    ///
    /// This is the *only* way to clear an `onEnd` registration — unlike a
    /// one-shot, the pump leaves it armed across sessions. After this, the pump's
    /// end is no longer reported to JS; a *fatal* terminal still falls back to a
    /// (throttled) stderr log line, while a clean stop ends silently. Does not
    /// stop the pump.
    #[napi]
    pub fn off_end(&self) -> Result<()> {
        let mut cb_guard = self.end_callback.lock().map_err(|e| {
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

    /// Returns a point-in-time snapshot of the stream's diagnostic counters.
    ///
    /// Maps rsac's `#[non_exhaustive]` `StreamStats` field-by-field into a plain
    /// JS object (see `StreamStats` in the type definitions). All counters are
    /// read with cheap relaxed loads on this non-real-time query path — calling
    /// this never allocates on or blocks the OS audio callback thread.
    ///
    /// Before `start()` (or after `stop()`) this returns a zeroed snapshot with
    /// `isRunning === false` and `uptimeSecs === 0`.
    #[napi]
    pub fn stream_stats(&self) -> Result<JsStreamStats> {
        let inner = self.inner.lock().map_err(|e| {
            napi::Error::new(
                napi::Status::GenericFailure,
                format!("Lock poisoned: {}", e),
            )
        })?;
        let s = inner.stream_stats();
        // Map field-by-field; `dropped_ratio()` is a derived accessor on the
        // Rust side, surfaced here as a precomputed field for JS convenience.
        Ok(JsStreamStats {
            overruns: bigint_from_u64(s.overruns),
            buffers_captured: bigint_from_u64(s.buffers_captured),
            buffers_dropped: bigint_from_u64(s.buffers_dropped),
            buffers_pushed: bigint_from_u64(s.buffers_pushed),
            uptime_secs: s.uptime.as_secs_f64(),
            dropped_ratio: s.dropped_ratio(),
            is_running: s.is_running,
            format_description: s.format_description.clone(),
        })
    }

    /// Returns a windowed snapshot of the stream's recent backpressure.
    ///
    /// Maps rsac's `#[non_exhaustive]` `BackpressureReport` field-by-field into a
    /// plain JS object (see `BackpressureReport` in the type definitions). Unlike
    /// the all-or-nothing `isUnderBackpressure` flag — which trips only on a run of
    /// *consecutive* drops and resets on any successful push — `dropRate` is
    /// computed over a recent window of push activity, so a sustained partial loss
    /// (e.g. a steady 1-in-3 drop pattern) is visible. The legacy bool is carried
    /// through unchanged. Reading these counters never allocates on or blocks the
    /// OS audio callback thread.
    ///
    /// Before `start()` (or after `stop()`) this returns the all-zero default with
    /// `windowSecs === 0`, `dropRate === 0.0`, and `isUnderBackpressure === false`.
    #[napi]
    pub fn backpressure_report(&self) -> Result<JsBackpressureReport> {
        let inner = self.inner.lock().map_err(|e| {
            napi::Error::new(
                napi::Status::GenericFailure,
                format!("Lock poisoned: {}", e),
            )
        })?;
        let r = inner.backpressure_report();
        // Map field-by-field; `window` is a Rust `Duration` surfaced here as a
        // f64 of seconds (`windowSecs`), matching how `stream_stats` surfaces
        // `uptime`. The `pushed`/`dropped` u64 tallies widen to BigInt for the
        // same precision reason as the `StreamStats` counters.
        Ok(JsBackpressureReport {
            window_secs: r.window.as_secs_f64(),
            pushed: bigint_from_u64(r.pushed),
            dropped: bigint_from_u64(r.dropped),
            drop_rate: r.drop_rate,
            is_under_backpressure: r.is_under_backpressure,
        })
    }

    /// The negotiated *delivery* format the backend actually produces, or `null`
    /// before `start()` creates a stream.
    ///
    /// This is the authoritative format published by the bridge once a backend
    /// records it; it may differ from the requested settings when the device
    /// forced a negotiation. Reading it does not allocate or lock the data plane.
    #[napi(getter)]
    pub fn format(&self) -> Result<Option<JsAudioFormat>> {
        let inner = self.inner.lock().map_err(|e| {
            napi::Error::new(
                napi::Status::GenericFailure,
                format!("Lock poisoned: {}", e),
            )
        })?;
        Ok(inner.format().map(|f| JsAudioFormat {
            sample_rate: f.sample_rate,
            channels: f.channels as u32,
            sample_format: sample_format_name(f.sample_format).to_string(),
        }))
    }
}

// ── Stream statistics + format (read-side observability) ─────────────────

/// Widen a Rust `u64` counter to a JS `BigInt`.
///
/// rsac's stream counters are `u64`; JS `number` is an IEEE-754 double that
/// loses integer precision past 2^53. Carrying them as `BigInt` is the honest
/// typing — a long-running capture can legitimately exceed `Number.MAX_SAFE_INTEGER`.
#[inline]
fn bigint_from_u64(v: u64) -> BigInt {
    BigInt::from(v)
}

/// Map an rsac `SampleFormat` to its short uppercase name (matches core's
/// `format_description_string`).
#[inline]
fn sample_format_name(fmt: rsac::SampleFormat) -> &'static str {
    match fmt {
        rsac::SampleFormat::I16 => "I16",
        rsac::SampleFormat::I24 => "I24",
        rsac::SampleFormat::I32 => "I32",
        rsac::SampleFormat::F32 => "F32",
    }
}

/// A point-in-time snapshot of an [`AudioCapture`]'s diagnostic counters.
///
/// Field-by-field mirror of rsac's `#[non_exhaustive]` `StreamStats`. Counters
/// are `BigInt` (u64) to avoid silent precision loss on long-running captures;
/// `uptimeSecs` and `droppedRatio` are doubles.
#[napi(object)]
pub struct JsStreamStats {
    /// Buffers dropped due to ring-buffer overflow (alias of `buffersDropped`,
    /// kept for backward compatibility with `overrunCount`).
    pub overruns: BigInt,
    /// Cumulative buffers delivered to the consumer (popped off the ring).
    pub buffers_captured: BigInt,
    /// Cumulative buffers dropped due to ring-buffer overflow.
    pub buffers_dropped: BigInt,
    /// Cumulative buffers enqueued by the producer (the OS audio callback).
    pub buffers_pushed: BigInt,
    /// How long the stream has been running, in seconds. `0` when not started.
    pub uptime_secs: f64,
    /// Fraction of accounted-for buffers lost to overflow, in `0.0..=1.0`
    /// (`buffersDropped / (buffersCaptured + buffersDropped)`; `0.0` when none).
    pub dropped_ratio: f64,
    /// Whether the stream is currently capturing.
    pub is_running: bool,
    /// Compact human-readable description of the negotiated format
    /// (e.g. `"2ch 48000Hz F32"`); empty before the stream starts.
    pub format_description: String,
}

/// A windowed snapshot of an [`AudioCapture`]'s recent backpressure.
///
/// Field-by-field mirror of rsac's `#[non_exhaustive]` `BackpressureReport`. The
/// `pushed`/`dropped` tallies are `BigInt` (u64) to avoid silent precision loss on
/// long-running captures; `windowSecs` and `dropRate` are doubles, and
/// `isUnderBackpressure` is the legacy consecutive-drop flag carried unchanged.
#[napi(object)]
pub struct JsBackpressureReport {
    /// The wall-clock span the `pushed`/`dropped` tallies cover, in seconds. `0`
    /// when the span cannot be attributed (no stream / not yet negotiated).
    pub window_secs: f64,
    /// Buffers successfully pushed by the producer within the window.
    pub pushed: BigInt,
    /// Buffers dropped due to ring-buffer overflow within the window.
    pub dropped: BigInt,
    /// Fraction of buffers lost within the window, in `0.0..=1.0`
    /// (`dropped / (pushed + dropped)`; `0.0` when nothing has been pushed or
    /// dropped). Surfaces sustained partial loss the legacy bool misses.
    pub drop_rate: f64,
    /// The legacy consecutive-drop backpressure flag, carried unchanged: trips only
    /// on a run of consecutive drops and resets on any successful push.
    pub is_under_backpressure: bool,
}

/// The negotiated audio delivery format.
#[napi(object)]
pub struct JsAudioFormat {
    /// Samples per second (e.g. 48000).
    pub sample_rate: u32,
    /// Number of interleaved channels (e.g. 2 for stereo).
    pub channels: u32,
    /// Sample format name: one of `"I16"`, `"I24"`, `"I32"`, `"F32"`.
    pub sample_format: String,
}

// ── Data-pump recoverable-error log throttle ─────────────────────────────

/// How often the data pump logs a *sustained* run of recoverable read errors.
///
/// The pump logs the first error in a streak eagerly, then only every
/// `RECOVERABLE_LOG_EVERY`th after that, so a backend stuck returning a
/// recoverable error on every ~1 ms poll cannot flood stderr at ~1000 lines/sec.
const RECOVERABLE_LOG_EVERY: u64 = 1000;

/// Whether the `count`-th (0-based) consecutive recoverable read error should be
/// logged, given [`RECOVERABLE_LOG_EVERY`].
///
/// Returns `true` for the first error in a streak (`count == 0`) and then once
/// per `RECOVERABLE_LOG_EVERY` errors. Pure arithmetic so the throttle policy is
/// unit-testable without a node runtime; the pump increments the count on each
/// recoverable error and resets it to `0` on a successful read.
#[inline]
fn should_log_recoverable(count: u64) -> bool {
    // is_multiple_of (stable 1.87 = pinned MSRV) — clippy::manual_is_multiple_of
    // rejects the `% N == 0` form under -D warnings.
    count.is_multiple_of(RECOVERABLE_LOG_EVERY)
}

// ── AudioCapture private helpers ─────────────────────────────────────────

impl AudioCapture {
    /// Spawn a background thread that reads audio buffers from rsac and pushes
    /// them to JavaScript via the registered ThreadsafeFunction callback.
    fn start_data_pump(&self) -> Result<()> {
        let inner = self.inner.clone();
        let callback = self.callback.clone();
        let end_callback = self.end_callback.clone();
        let pump_active = self.pump_active.clone();

        pump_active.store(true, Ordering::SeqCst);

        std::thread::Builder::new()
            .name("rsac-napi-pump".into())
            .spawn(move || {
                // The terminal reason this pump ended with: `Some(msg)` on a fatal
                // terminal (the formatted `AudioError`), `None` on a clean stop /
                // poisoned mutex. Fired to the JS `onEnd` callback exactly once
                // after the loop exits (see the `end_callback` notification below).
                let mut end_reason: Option<String> = None;
                // Recoverable-read-error log throttle. A sustained transient (e.g. a
                // backend that returns a recoverable `StreamReadError` on every poll)
                // would otherwise spam stderr ~1000 lines/sec (one per 1 ms retry).
                // We log the FIRST occurrence eagerly, then only every
                // `RECOVERABLE_LOG_EVERY`th thereafter (see `should_log_recoverable`)
                // — annotated with how many were suppressed since the last line — so a
                // stuck stream leaves a bounded, legible trail instead of a flood.
                // Counting is a plain `u64` on this thread (no alloc, no lock), and the
                // counter resets whenever a read succeeds so an isolated blip logs.
                let mut recoverable_errors: u64 = 0;
                while pump_active.load(Ordering::SeqCst) {
                    // Read via `read_chunk_nonblocking` (NOT `read_buffer`):
                    // `read_buffer` short-circuits to a RECOVERABLE
                    // `StreamReadError` the moment the stream leaves `Running`,
                    // so it can NEVER surface the fatal `StreamEnded` — the pump
                    // would loop on a recoverable error forever after stop.
                    // `read_chunk_nonblocking` drains the buffered tail during
                    // `Stopping` and yields the terminal `StreamEnded` (fatal)
                    // once the ring is empty AND the stream is terminal, so we
                    // can end cleanly on a real terminal (BP-3).
                    let maybe_buf = {
                        let capture = match inner.lock() {
                            Ok(c) => c,
                            Err(_) => break, // Mutex poisoned, bail (clean: None)
                        };
                        capture.read_chunk_nonblocking()
                    };

                    match maybe_buf {
                        Ok(Some(buf)) => {
                            // A successful read clears the transient-error streak so a
                            // later isolated blip is logged eagerly again (the throttle
                            // only suppresses a *sustained* run of recoverable errors).
                            recoverable_errors = 0;
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
                        // FATAL terminal (e.g. StreamEnded): the stream is done —
                        // stop pumping cleanly. Capture the terminal cause so it is
                        // surfaced to a registered `onEnd` callback (and logged), so
                        // the end is never fully silent. (A consumer that needs the
                        // terminal AudioError surfaced to JS registers `onEnd`; the
                        // data pump itself ends here without erroring.)
                        Err(ref e) if e.is_fatal() => {
                            eprintln!("rsac-napi data pump ended (terminal): {}", e);
                            end_reason = Some(e.to_string());
                            break;
                        }
                        // RECOVERABLE hiccup (transient StreamReadError,
                        // BufferOverrun/Underrun): a transient error must NOT end
                        // delivery. Log and retry after a brief pause, mirroring
                        // the in-process callback pump and the subscribe loop. The
                        // log is throttled (first occurrence, then every
                        // `RECOVERABLE_LOG_EVERY`th) so a stuck stream cannot flood
                        // stderr at ~1000 lines/sec.
                        Err(e) => {
                            if should_log_recoverable(recoverable_errors) {
                                if recoverable_errors == 0 {
                                    eprintln!("rsac-napi data pump read error (retrying): {}", e);
                                } else {
                                    eprintln!(
                                        "rsac-napi data pump read error (retrying; \
                                         {} more suppressed since last line): {}",
                                        RECOVERABLE_LOG_EVERY - 1,
                                        e
                                    );
                                }
                            }
                            recoverable_errors = recoverable_errors.saturating_add(1);
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                    }
                }
                pump_active.store(false, Ordering::SeqCst);

                // Notify the JS consumer (if it registered `onEnd`) *why* delivery
                // stopped — the terminal `AudioError` message on a fatal terminal,
                // or `null` on a clean stop. This is the Node parity for
                // `subscribe_with_errors` / `StreamWithErrors`.
                //
                // The callback is *cloned* (not taken) so it survives across
                // multiple start/stop sessions on the same `AudioCapture`, exactly
                // like the `onData` callback. A `ThreadsafeFunction` is internally
                // ref-counted, so the clone shares the same underlying JS function;
                // firing this run's clone leaves the canonical copy registered in
                // `end_callback` for the next pump run to re-arm automatically. The
                // registration is cleared only by an explicit `offEnd` (or replaced
                // by a later `onEnd`), so `onEnd` and `onData` are now symmetric:
                // both fire once per session and both persist until explicitly
                // cleared. (Fires at most once per pump run.)
                let end_tsfn = match end_callback.lock() {
                    Ok(guard) => guard.clone(),
                    Err(_) => None,
                };
                if let Some(tsfn) = end_tsfn {
                    tsfn.call(end_reason, ThreadsafeFunctionCallMode::NonBlocking);
                }
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
        let device = enumerator.default_device().map_err(audio_err_to_napi)?;

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
    /// Whether the backend delivers device hot-plug / default-change
    /// notifications.
    pub supports_device_change_notifications: bool,
    /// True when starting a capture requires a config-time user-consent
    /// artifact (mobile platforms; see docs/MOBILE_BACKEND_DESIGN.md);
    /// false on all desktop backends.
    pub requires_user_consent: bool,
    /// Maximum number of channels supported.
    pub max_channels: u32,
    /// Minimum supported sample rate in Hz.
    pub min_sample_rate: u32,
    /// Maximum supported sample rate in Hz.
    pub max_sample_rate: u32,
    /// Supported sample formats (short names, e.g. "I16", "F32").
    pub supported_sample_formats: Vec<String>,
    /// The config-time sample-rate whitelist the capture constructor accepts —
    /// identical on every platform and intentionally narrower than the
    /// device-negotiable min/max sample-rate range.
    pub supported_sample_rates: Vec<u32>,
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
        supports_device_change_notifications: caps.supports_device_change_notifications,
        requires_user_consent: caps.requires_user_consent,
        max_channels: caps.max_channels as u32,
        min_sample_rate: caps.sample_rate_range.0,
        max_sample_rate: caps.sample_rate_range.1,
        supported_sample_formats: caps
            .supported_sample_formats
            .iter()
            .map(|f| sample_format_name(*f).to_string())
            .collect(),
        supported_sample_rates: rsac::PlatformCapabilities::supported_sample_rates().to_vec(),
        backend_name: caps.backend_name.to_string(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────
//
// These exercise the napi-environment-independent logic only (string-target
// parsing via rsac, the strided per-channel metering math, and the field-map
// helpers). The JS-facing `#[napi]` surface (Float32Array I/O, ThreadsafeFn) is
// validated at the node-build / inspection tier.
#[cfg(test)]
mod tests {
    use super::*;

    // ── rsac-28ee: CaptureTarget.parse round-trip ────────────────────────

    /// Valid specs round-trip through rsac's FromStr into the right variant —
    /// the same parse `CaptureTarget::parse(spec)` performs. Covers the bare
    /// scheme, a colon-bearing device id, name, app, and both pid spellings.
    #[test]
    fn parse_valid_specs_round_trip() {
        let cases: &[(&str, rsac::CaptureTarget)] = &[
            ("system", rsac::CaptureTarget::SystemDefault),
            ("DEFAULT", rsac::CaptureTarget::SystemDefault), // case-insensitive
            (
                "device:hw:0,0",
                rsac::CaptureTarget::Device(rsac::DeviceId("hw:0,0".to_string())),
            ),
            (
                "app:session-7",
                rsac::CaptureTarget::Application(rsac::ApplicationId("session-7".to_string())),
            ),
            (
                "name:Firefox",
                rsac::CaptureTarget::ApplicationByName("Firefox".to_string()),
            ),
            (
                "pid:1234",
                rsac::CaptureTarget::ProcessTree(rsac::ProcessId(1234)),
            ),
            (
                "tree:1234",
                rsac::CaptureTarget::ProcessTree(rsac::ProcessId(1234)),
            ),
        ];
        for (spec, expected) in cases {
            let parsed: rsac::CaptureTarget = spec
                .parse()
                .unwrap_or_else(|e| panic!("'{spec}' should parse: {e}"));
            assert_eq!(&parsed, expected, "spec '{spec}'");
        }
    }

    /// Invalid specs surface an `AudioError` (mapped to a thrown JS Error by
    /// `CaptureTarget::parse`) without panicking.
    #[test]
    fn parse_invalid_specs_error_not_panic() {
        for bad in ["bogus", "pid:not-a-number", "tree:-1", "device"] {
            // "device" has no colon → unknown target; the others are bad scheme/pid.
            let res: std::result::Result<rsac::CaptureTarget, _> = bad.parse();
            assert!(res.is_err(), "'{bad}' should be rejected");
        }
    }

    // ── rsac-fd16: metering exposed via AudioChunk fields ────────────────

    /// The eager scalar + per-channel meters on `AudioChunk` come straight from
    /// rsac core's `AudioBuffer` meters. Assert the source methods agree with
    /// hand-computed values for a known interleaved stereo buffer:
    /// L = [1.0, -1.0] → rms 1.0, peak 1.0; R = [0.5, -0.5] → rms 0.5, peak 0.5.
    #[test]
    fn channel_metering_matches_known_values() {
        // interleaved L,R,L,R
        let buf = rsac::AudioBuffer::new(vec![1.0f32, 0.5, -1.0, -0.5], 2, 48_000);
        let rms_l = buf.channel_rms(0).unwrap();
        let rms_r = buf.channel_rms(1).unwrap();
        let peak_l = buf.channel_peak(0).unwrap();
        let peak_r = buf.channel_peak(1).unwrap();
        assert!((rms_l - 1.0).abs() < 1e-6, "L rms {rms_l}");
        assert!((rms_r - 0.5).abs() < 1e-6, "R rms {rms_r}");
        assert!((peak_l - 1.0).abs() < 1e-6, "L peak {peak_l}");
        assert!((peak_r - 0.5).abs() < 1e-6, "R peak {peak_r}");
    }

    /// Out-of-range channel → None in core (the napi field path materializes
    /// exactly `0..channels`, so that index is never produced).
    #[test]
    fn channel_metering_edge_cases() {
        let buf = rsac::AudioBuffer::new(vec![1.0f32, 0.5, -1.0, -0.5], 2, 48_000);
        assert_eq!(buf.channel_rms(2), None); // out of range
        assert_eq!(buf.channel_peak(5), None);
    }

    /// The eager whole-buffer scalars on `AudioChunk` come straight from rsac
    /// core's `AudioBuffer` meters — assert the source methods agree with the
    /// expected values for a known buffer (the napi `from_rsac_buffer` copy is
    /// exercised at the node tier).
    #[test]
    fn audio_buffer_scalar_meters_agree_with_core() {
        let buf = rsac::AudioBuffer::new(vec![1.0f32, -1.0, 1.0, -1.0], 2, 48_000);
        assert!((buf.rms() - 1.0).abs() < 1e-6);
        assert!((buf.peak() - 1.0).abs() < 1e-6);
        assert!((buf.peak_dbfs() - 0.0).abs() < 1e-6); // full scale → 0 dBFS
                                                       // Silence → NEG_INFINITY dBFS (carried to JS as -Infinity).
        let silent = rsac::AudioBuffer::new(vec![0.0f32; 4], 2, 48_000);
        assert_eq!(silent.rms(), 0.0);
        assert!(silent.rms_dbfs().is_infinite() && silent.rms_dbfs() < 0.0);
    }

    // ── rsac-fe6e: stats/format field mapping ────────────────────────────

    /// `sample_format_name` maps every rsac SampleFormat variant to the short
    /// uppercase name used in the JS `AudioFormat.sampleFormat` field.
    #[test]
    fn sample_format_name_maps_all_variants() {
        assert_eq!(sample_format_name(rsac::SampleFormat::I16), "I16");
        assert_eq!(sample_format_name(rsac::SampleFormat::I24), "I24");
        assert_eq!(sample_format_name(rsac::SampleFormat::I32), "I32");
        assert_eq!(sample_format_name(rsac::SampleFormat::F32), "F32");
    }

    /// A zeroed `StreamStats` (the pre-start snapshot) maps to a JS object with
    /// `isRunning == false`, zero counters/uptime, and `droppedRatio == 0.0` —
    /// i.e. every field the JS `StreamStats` interface declares is populated.
    #[test]
    fn stream_stats_default_maps_field_by_field() {
        let s = rsac::StreamStats::default();
        assert_eq!(s.overruns, 0);
        assert_eq!(s.buffers_captured, 0);
        assert_eq!(s.buffers_dropped, 0);
        assert_eq!(s.buffers_pushed, 0);
        assert_eq!(s.uptime, std::time::Duration::ZERO);
        assert!(!s.is_running);
        assert_eq!(s.dropped_ratio(), 0.0);
        assert!(s.format_description.is_empty());
        // The counters widen losslessly to BigInt (u64 -> BigInt) for JS.
        // get_u64() -> (sign_negative, value, lossless).
        let (neg, val, lossless) = bigint_from_u64(u64::MAX).get_u64();
        assert!(!neg && lossless);
        assert_eq!(val, u64::MAX);
    }

    /// `overruns` is the documented alias of `buffers_dropped`, so a JS caller's
    /// `streamStats().overruns` and `overrunCount` agree (Go test parity).
    ///
    /// `StreamStats` is `#[non_exhaustive]`, so it can only be obtained via
    /// `Default` outside its defining crate — which is exactly how the JS path
    /// reads it (the live snapshot comes from `AudioCapture::stream_stats`).
    #[test]
    fn stream_stats_overruns_aliases_buffers_dropped() {
        let s = rsac::StreamStats::default();
        assert_eq!(s.overruns, s.buffers_dropped);
    }

    // ── rsac-cbda: onEnd terminal-observability classification ───────────
    //
    // The `onEnd` ThreadsafeFunction path cannot be driven without a node
    // runtime + a real device, so these pin the *classification contract* the
    // data pump's `end_reason` computation relies on: only a FATAL terminal
    // produces a non-null reason (the formatted `AudioError`), and the formatted
    // message is what gets surfaced to JS; a RECOVERABLE hiccup never ends the
    // pump and so never fires `onEnd`. This mirrors the Python `__next__` and Go
    // `StreamWithErrors` terminal-classification tests.

    /// A fatal terminal (`StreamEnded`, ADR-0003) is what the pump surfaces to
    /// `onEnd` as a non-null reason — and the reason is exactly `e.to_string()`,
    /// the same formatted message the pump logs.
    #[test]
    fn on_end_fatal_terminal_surfaces_formatted_reason() {
        let e = rsac::AudioError::StreamEnded {
            reason: "capture ended".into(),
        };
        assert!(
            e.is_fatal(),
            "StreamEnded must be fatal → pump ends and fires onEnd with Some(reason)"
        );
        // The pump stores `Some(e.to_string())` as the end reason; the JS callback
        // receives exactly this string (non-empty, carries the upstream cause).
        let surfaced = e.to_string();
        assert!(!surfaced.is_empty());
        assert_eq!(surfaced, e.to_string());
    }

    /// A recoverable hiccup (transient `StreamReadError`, `BufferOverrun`/
    /// `Underrun`) is NOT fatal, so the pump retries instead of ending — it never
    /// fires `onEnd`. Pin the classification so a future change can't silently
    /// turn a transient blip into a terminal `onEnd`.
    #[test]
    fn on_end_recoverable_does_not_terminate_pump() {
        for e in [
            rsac::AudioError::StreamReadError {
                reason: "transient".into(),
            },
            rsac::AudioError::BufferOverrun { dropped_frames: 1 },
            rsac::AudioError::BufferUnderrun {
                requested: 1,
                available: 0,
            },
        ] {
            assert!(
                !e.is_fatal(),
                "{e} is recoverable → pump retries, onEnd not fired"
            );
            assert!(e.is_recoverable());
        }
    }

    // ── rsac-2587: data-pump recoverable-error log throttle ──────────────
    //
    // The pump cannot be driven without a node runtime + a real device, so
    // these pin the *throttle policy* the pump's logging decision relies on:
    // the first error in a streak logs, then only every `RECOVERABLE_LOG_EVERY`th
    // thereafter, so a sustained transient cannot flood stderr at ~1000 lines/sec.

    /// The first error in a streak (`count == 0`) always logs, so an isolated
    /// blip is never silently swallowed.
    #[test]
    fn recoverable_throttle_logs_first_occurrence() {
        assert!(should_log_recoverable(0));
    }

    /// Within the first window, errors `1..RECOVERABLE_LOG_EVERY` are suppressed
    /// (only the boundary multiples log), so a sustained transient at the pump's
    /// ~1 ms retry cadence produces at most ~1 line per `RECOVERABLE_LOG_EVERY`
    /// errors instead of one per error.
    #[test]
    fn recoverable_throttle_suppresses_within_window() {
        for count in 1..RECOVERABLE_LOG_EVERY {
            assert!(
                !should_log_recoverable(count),
                "count {count} should be suppressed (not a multiple of {RECOVERABLE_LOG_EVERY})"
            );
        }
    }

    /// Logging recurs exactly on each `RECOVERABLE_LOG_EVERY` boundary, so a
    /// stuck stream still leaves a bounded, periodic trail (a flat-zero rate
    /// would hide a genuinely wedged backend).
    #[test]
    fn recoverable_throttle_logs_on_each_boundary() {
        assert!(should_log_recoverable(RECOVERABLE_LOG_EVERY));
        assert!(should_log_recoverable(RECOVERABLE_LOG_EVERY * 2));
        assert!(should_log_recoverable(RECOVERABLE_LOG_EVERY * 7));
        // ...and the off-by-one neighbours of a boundary are still suppressed.
        assert!(!should_log_recoverable(RECOVERABLE_LOG_EVERY - 1));
        assert!(!should_log_recoverable(RECOVERABLE_LOG_EVERY + 1));
    }

    /// Over a long sustained streak, the number of *logged* lines is bounded by
    /// `streak / RECOVERABLE_LOG_EVERY + 1` — the property that makes the flood
    /// impossible. Assert it directly for a streak far larger than one window.
    #[test]
    fn recoverable_throttle_bounds_logged_lines() {
        let streak = RECOVERABLE_LOG_EVERY * 5 + 123; // not a clean multiple
        let logged = (0..streak).filter(|&c| should_log_recoverable(c)).count() as u64;
        let upper_bound = streak / RECOVERABLE_LOG_EVERY + 1;
        assert!(
            logged <= upper_bound,
            "logged {logged} lines over a {streak}-error streak exceeds the \
             {upper_bound}-line bound"
        );
        // Concretely: 5 full windows + a partial → exactly 6 logged lines
        // (counts 0, EVERY, 2·EVERY, 3·EVERY, 4·EVERY, 5·EVERY).
        assert_eq!(logged, 6);
    }
}
