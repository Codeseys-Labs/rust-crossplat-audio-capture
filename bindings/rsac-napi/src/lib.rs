// bindings/rsac-napi/src/lib.rs
//
// Production-ready Node.js/TypeScript bindings for rsac (Rust Cross-Platform Audio Capture).
// Uses napi-rs to expose rsac's streaming-first audio capture API as native Node.js classes.

#[macro_use]
extern crate napi_derive;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ErrorStrategy, ThreadsafeFunction, ThreadsafeFunctionCallMode};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

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
    /// The wrapped rsac capture, behind an `RwLock` (not a `Mutex`) so a blocking
    /// reader can park while holding only a **shared read guard**.
    ///
    /// rsac's read paths (`read_buffer`, `read_chunk_blocking`, …) and
    /// `request_stop` all take `&self`, while the lifecycle mutators
    /// (`start`/`stop`) take `&mut self`. Mapping reads to a shared read guard and
    /// `start`/`stop` to the exclusive write guard is what breaks the
    /// stop()-vs-parked-blocking-read deadlock (rsac-8082): `stop()` first takes a
    /// *read* guard to call `request_stop()` — compatible with the guard a reader
    /// parked in `readBlocking` holds, so it never blocks — which transitions the
    /// stream terminal and wakes the reader; only then does `stop()` take the write
    /// guard for the actual `stop()`. With the old `Mutex`, the reader parked while
    /// holding the sole lock and `stop()` blocked on it forever.
    inner: Arc<RwLock<rsac::AudioCapture>>,
    /// Active data callback (ThreadsafeFunction). Held here to prevent GC.
    callback: DataCallback,
    /// Optional terminal-observability callback (ThreadsafeFunction). Fires once
    /// when the data pump ends, carrying the terminal cause so a JS `onData`
    /// consumer can observe *why* delivery stopped — parity with Rust
    /// `subscribe_with_errors` / Go `StreamWithErrors`. Held here to prevent GC.
    end_callback: EndCallback,
    /// Whether the push-model data pump thread is running.
    pump_active: Arc<AtomicBool>,
    /// Monotonic pump generation (rsac-1a34, mirrors `Composition`'s PR #59
    /// fix): each spawned pump owns the generation current at spawn time and
    /// loops only while it still owns it. Cancellation (`stop`/`offData`)
    /// bumps the generation, so a stale pump can NEVER be resurrected by a
    /// rapid `offData()`→`onData()` flip of the bool alone (the old pump
    /// observes the generation change and exits; only the new generation's
    /// pump delivers).
    pump_generation: Arc<AtomicU64>,
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
            inner: Arc::new(RwLock::new(capture)),
            callback: Arc::new(Mutex::new(None)),
            end_callback: Arc::new(Mutex::new(None)),
            pump_active: Arc::new(AtomicBool::new(false)),
            pump_generation: Arc::new(AtomicU64::new(0)),
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
            inner: Arc::new(RwLock::new(capture)),
            callback: Arc::new(Mutex::new(None)),
            end_callback: Arc::new(Mutex::new(None)),
            pump_active: Arc::new(AtomicBool::new(false)),
            pump_generation: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Start audio capture.
    ///
    /// If an `onData` callback is registered, a background thread is spawned
    /// that reads audio chunks and pushes them to JavaScript via a
    /// ThreadsafeFunction.
    #[napi]
    pub fn start(&self) -> Result<()> {
        // Fast path under a SHARED guard (rsac-8082 follow-up): readers can only
        // park in `readBlocking` while the stream is RUNNING, and core `start()`
        // on a running stream is a documented idempotent no-op — so a redundant
        // `start()` must not request the exclusive write guard, which would queue
        // forever behind a parked reader's shared guard.
        let already_running = {
            let inner = self.inner.read().map_err(|e| {
                napi::Error::new(
                    napi::Status::GenericFailure,
                    format!("Lock poisoned: {}", e),
                )
            })?;
            inner.is_running()
        };

        if !already_running {
            // Not running → no reader can be parked (a blocking read on a
            // non-running stream returns immediately), so the write guard is
            // only ever briefly contended. If another thread starts the capture
            // between the guards, core `start()`'s own running-check makes this
            // a no-op — the recheck is delegated to core.
            let mut inner = self.inner.write().map_err(|e| {
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
        // Bump the generation FIRST: any live pump loses ownership immediately
        // and can never be resurrected by a later flag flip (rsac-1a34, PR #59
        // pattern).
        self.pump_generation.fetch_add(1, Ordering::SeqCst);
        self.pump_active.store(false, Ordering::SeqCst);

        // Deadlock fix (rsac-8082): a thread parked in `readBlocking` /
        // `readBlockingAsync` holds a *shared read guard* while blocked inside
        // `read_chunk_blocking`. If we went straight for the exclusive write
        // guard here, `stop()` would block forever waiting for that reader to
        // release — but the reader only releases once the stream goes terminal,
        // which is exactly what `stop()` is trying to do. Break the cycle the
        // same way the C FFI / Go binding do: FIRST take a *read* guard (shared
        // with the parked reader, so it never blocks) and call `request_stop()`,
        // which flips the stream terminal and wakes the reader within ~1 ms.
        // `request_stop()` takes `&self`, needs no `&mut`, and does not touch the
        // bridge consumer mutex the parked read holds, so it is safe to run
        // concurrently with the in-flight read. Drop the read guard, then take the
        // write guard for the real `stop()`; by now the woken reader has returned
        // its terminal error and released its guard, so this proceeds promptly.
        {
            let inner = self.inner.read().map_err(|e| {
                napi::Error::new(
                    napi::Status::GenericFailure,
                    format!("Lock poisoned: {}", e),
                )
            })?;
            inner.request_stop();
        }

        let mut inner = self.inner.write().map_err(|e| {
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
        let inner = self.inner.read().map_err(|e| {
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
    /// Not terminal-observable: this and `readAsync` route through rsac's
    /// `read_buffer`, which short-circuits to a *recoverable*
    /// `StreamReadError` the moment the stream leaves `Running` and never
    /// surfaces the terminal `StreamEnded`. The BLOCKING readers
    /// (`readBlocking`/`readBlockingAsync`) ARE terminal-observable as of
    /// rsac-477d, as is the push pump started by `onData` (terminal reason
    /// via `onEnd`). A non-blocking pull consumer should treat
    /// `stop`/`isRunning` as the end-of-stream signal and a thrown read
    /// error as retryable.
    #[napi]
    pub fn read(&self) -> Result<Option<AudioChunk>> {
        // `read_buffer` takes `&self` now, so the guard does not need `mut`.
        let inner = self.inner.read().map_err(|e| {
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
    /// Terminal-observable (rsac-477d): once the stream has ended — after
    /// `stop()` or a fatal backend error — this throws the stream's true
    /// terminal error (`StreamEnded`) promptly instead of downgrading it to
    /// a recoverable "not running" error, matching the C FFI and Go.
    #[napi]
    pub fn read_blocking(&self) -> Result<AudioChunk> {
        // `read_chunk_blocking` takes `&self`, so the guard does not need `mut`.
        let inner = self.inner.read().map_err(|e| {
            napi::Error::new(
                napi::Status::GenericFailure,
                format!("Lock poisoned: {}", e),
            )
        })?;

        let result = inner.read_chunk_blocking().map_err(audio_err_to_napi)?;
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
                let capture = inner.read().map_err(|e| {
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
    ///
    /// Terminal-observable (rsac-477d): once the stream has ended this
    /// rejects with the stream's true terminal error (`StreamEnded`) promptly
    /// instead of a recoverable "not running" error — see `readBlocking`.
    #[napi]
    pub async fn read_blocking_async(&self) -> Result<AudioChunk> {
        let inner = self.inner.clone();

        let result = tokio::task::spawn_blocking(move || -> napi::Result<rsac::AudioBuffer> {
            // `read_chunk_blocking` takes `&self`, so the guard does not need `mut`.
            let capture = inner.read().map_err(|e| {
                napi::Error::new(
                    napi::Status::GenericFailure,
                    format!("Lock poisoned: {}", e),
                )
            })?;
            capture.read_chunk_blocking().map_err(audio_err_to_napi)
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
            let inner = self.inner.read().map_err(|e| {
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
        // Generation bump makes cancellation immune to a rapid re-onData()
        // setting pump_active back to true before the old pump observes
        // false: the old pump checks its OWN generation and exits.
        self.pump_generation.fetch_add(1, Ordering::SeqCst);
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
        let inner = self.inner.read().map_err(|e| {
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
        let inner = self.inner.read().map_err(|e| {
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
        let inner = self.inner.read().map_err(|e| {
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
        let inner = self.inner.read().map_err(|e| {
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
        let pump_generation = self.pump_generation.clone();

        // This pump owns the generation value current at spawn time; any
        // cancellation (stop/offData) bumps it, ending ownership even if
        // pump_active is set true again by a rapid re-onData.
        let my_generation = pump_generation.fetch_add(1, Ordering::SeqCst) + 1;
        pump_active.store(true, Ordering::SeqCst);

        let spawn_result = std::thread::Builder::new()
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
                while pump_active.load(Ordering::SeqCst)
                    && pump_generation.load(Ordering::SeqCst) == my_generation
                {
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
                        let capture = match inner.read() {
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
                // Clear the running flag only if this pump still OWNS the
                // generation — a stale pump exiting after a rapid re-onData must
                // not kill the successor pump's flag.
                if pump_generation.load(Ordering::SeqCst) == my_generation {
                    pump_active.store(false, Ordering::SeqCst);
                }

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
            });
        match spawn_result {
            Ok(_) => Ok(()),
            Err(e) => {
                // Roll the flag back so a later onData()/start() can retry —
                // leaving it true would permanently wedge pump spawning.
                self.pump_active.store(false, Ordering::SeqCst);
                Err(napi::Error::new(
                    napi::Status::GenericFailure,
                    format!("Failed to spawn data pump thread: {}", e),
                ))
            }
        }
    }
}

// ── Composition (multi-source channel composition, ADR-0011) ───────────────
//
// These classes wrap `rsac::compose::{Group, CompositionBuilder, Composition}`
// directly (like `AudioCapture` wraps `rsac::AudioCapture`), NOT the C FFI. The
// `compose` cargo feature is enabled unconditionally in the addon (Cargo.toml),
// so they are always present. `Composition` reuses `AudioCapture`'s exact
// `Arc<RwLock<>>` + pump topology and the stop-vs-parked-read fix (rsac-8082):
// reads and the `CapturingStream::stop(&self)` signal take a shared read guard,
// the joining inherent `Composition::stop(&mut self)` takes the write guard.

/// A composition group: a named set of capture sources sharing a mixdown
/// layout. Built with `source`/`sourceWithGain` and a layout
/// (`mixdownMono`/`mixdownStereo`/`keepChannels`), then handed to
/// `CompositionBuilder.addGroup`.
#[napi]
pub struct Group {
    inner: Mutex<rsac::compose::Group>,
}

#[napi]
impl Group {
    /// Create a group with the given name and the default stereo layout. The
    /// name must be non-empty and unique within a composition (both enforced at
    /// `build()`).
    #[napi(constructor)]
    pub fn new(name: String) -> Self {
        Group {
            inner: Mutex::new(rsac::compose::Group::new(name)),
        }
    }

    /// Add a capture source with unit gain (1.0). `spec` uses the canonical
    /// target grammar (`"system"`, `"device:<id>"`, `"app:<id>"`, `"name:<n>"`,
    /// `"tree:<pid>"`). Throws on an invalid spec.
    #[napi]
    pub fn source(&self, spec: String) -> Result<()> {
        self.source_with_gain(spec, 1.0)
    }

    /// Add a capture source with an explicit linear mixdown gain (1.0 = unity).
    /// The gain must be finite and >= 0; an invalid gain throws eagerly.
    #[napi]
    pub fn source_with_gain(&self, spec: String, gain: f64) -> Result<()> {
        // Validate AFTER narrowing to f32: a finite f64 above f32::MAX (e.g.
        // f64::MAX) becomes +inf in the cast, which the f64 check would let
        // through (PR #59 review).
        let gain32 = gain as f32;
        if !gain32.is_finite() || gain32 < 0.0 {
            return Err(napi::Error::new(
                napi::Status::InvalidArg,
                format!(
                    "[ERR_RSAC_CONFIGURATION] gain {gain} is invalid (must be finite and >= 0 \
                     after f32 narrowing)"
                ),
            ));
        }
        let target: rsac::CaptureTarget = spec.parse().map_err(audio_err_to_napi)?;
        let mut g = self.inner.lock().map_err(lock_poisoned)?;
        *g = g.clone().source_with_gain(target, gain32);
        Ok(())
    }

    /// Fold every source to mono and sum into one output channel.
    #[napi]
    pub fn mixdown_mono(&self) -> Result<()> {
        self.set_layout_js(rsac::compose::GroupLayout::Mono)
    }

    /// Fold every source to stereo and sum into two output channels.
    #[napi]
    pub fn mixdown_stereo(&self) -> Result<()> {
        self.set_layout_js(rsac::compose::GroupLayout::Stereo)
    }

    /// Pass the group's single source through with its native channel count.
    #[napi]
    pub fn keep_channels(&self) -> Result<()> {
        let mut g = self.inner.lock().map_err(lock_poisoned)?;
        *g = g.clone().keep_channels();
        Ok(())
    }
}

impl Group {
    fn set_layout_js(&self, layout: rsac::compose::GroupLayout) -> Result<()> {
        let mut g = self.inner.lock().map_err(lock_poisoned)?;
        *g = g.clone().mixdown(layout);
        Ok(())
    }

    /// Clone the inner `Group` (for handing to a builder).
    fn snapshot(&self) -> Result<rsac::compose::Group> {
        self.inner.lock().map(|g| g.clone()).map_err(lock_poisoned)
    }
}

/// Options accepted by `CompositionBuilder.create`.
#[napi(object)]
pub struct CompositionBuilderOptions {
    /// Session sample rate in Hz (default 48000).
    pub sample_rate: Option<u32>,
    /// Saturating clamp of the summed output to [-1.0, 1.0] (default false).
    pub clamp_output: Option<bool>,
    /// Composed tick (output buffer) duration in ms (default 10).
    pub quantum_ms: Option<u32>,
    /// How long to wait for the master source before a fallback tick, ms (default 250).
    pub stall_timeout_ms: Option<u32>,
    /// Per-source buffering bound in ms (default 1000).
    pub max_buffer_ms: Option<u32>,
}

/// Builder for a multi-source `Composition` (ADR-0011).
#[napi]
pub struct CompositionBuilder {
    inner: Mutex<Option<rsac::compose::CompositionBuilder>>,
}

#[napi]
impl CompositionBuilder {
    /// Create a composition builder with optional session settings.
    #[napi(factory)]
    pub fn create(opts: Option<CompositionBuilderOptions>) -> Self {
        let opts = opts.unwrap_or(CompositionBuilderOptions {
            sample_rate: None,
            clamp_output: None,
            quantum_ms: None,
            stall_timeout_ms: None,
            max_buffer_ms: None,
        });
        let builder = rsac::compose::CompositionBuilder::new()
            .sample_rate(opts.sample_rate.unwrap_or(48_000))
            .clamp_output(opts.clamp_output.unwrap_or(false))
            .quantum(std::time::Duration::from_millis(u64::from(
                opts.quantum_ms.unwrap_or(10),
            )))
            .stall_timeout(std::time::Duration::from_millis(u64::from(
                opts.stall_timeout_ms.unwrap_or(250),
            )))
            .max_buffer(std::time::Duration::from_millis(u64::from(
                opts.max_buffer_ms.unwrap_or(1000),
            )));
        CompositionBuilder {
            inner: Mutex::new(Some(builder)),
        }
    }

    /// Append a group (cloned into the builder).
    #[napi]
    pub fn add_group(&self, group: &Group) -> Result<()> {
        let g = group.snapshot()?;
        let mut guard = self.inner.lock().map_err(lock_poisoned)?;
        let b = guard.as_ref().ok_or_else(builder_consumed)?;
        *guard = Some(b.clone().group(g));
        Ok(())
    }

    /// Run every device-independent validation `build()` performs, without
    /// building. Throws on an invalid configuration.
    #[napi]
    pub fn preflight(&self) -> Result<()> {
        let guard = self.inner.lock().map_err(lock_poisoned)?;
        let b = guard.as_ref().ok_or_else(builder_consumed)?;
        b.preflight().map_err(audio_err_to_napi)
    }

    /// Validate and build a (not-yet-started) `Composition`. No devices are
    /// touched here. Throws on an invalid configuration.
    #[napi]
    pub fn build(&self) -> Result<Composition> {
        let guard = self.inner.lock().map_err(lock_poisoned)?;
        let b = guard.as_ref().ok_or_else(builder_consumed)?;
        // Clone-then-build so this JS builder object stays reusable (the core
        // build() consumes the builder value; here a JS object outlives one
        // build call, so leave the builder in place).
        let composition = b.clone().build().map_err(audio_err_to_napi)?;
        Ok(Composition {
            inner: Arc::new(RwLock::new(composition)),
            callback: Arc::new(Mutex::new(None)),
            end_callback: Arc::new(Mutex::new(None)),
            pump_active: Arc::new(AtomicBool::new(false)),
            pump_generation: Arc::new(AtomicU64::new(0)),
        })
    }
}

/// A multi-source composed capture session (ADR-0011). Created via
/// `CompositionBuilder.build`; inert until `start()`.
///
/// An explicit `stop()` discards the buffered composed tail; read until the
/// terminal error before stopping to capture everything (the natural end
/// drains the tail first).
#[napi]
pub struct Composition {
    /// The wrapped `rsac::Composition` behind an `RwLock` so a blocking reader
    /// can park under a shared read guard — same topology + rsac-8082 fix as
    /// `AudioCapture`. Reads and the `CapturingStream::stop(&self)` signal take
    /// the shared read guard; the joining inherent `Composition::stop(&mut self)`
    /// takes the write guard.
    inner: Arc<RwLock<rsac::Composition>>,
    /// Active data callback (ThreadsafeFunction). Held to prevent GC.
    callback: DataCallback,
    /// Optional terminal-observability callback, fired once when the pump ends.
    end_callback: EndCallback,
    /// Whether the push-model data pump thread is running (spawn guard).
    pump_active: Arc<AtomicBool>,
    /// Monotonic pump generation (PR #59 review): each spawned pump owns the
    /// generation current at spawn time and loops only while it still owns it.
    /// Cancellation bumps the generation, so a stale pump can NEVER be
    /// resurrected by a rapid `offData()`→`onData()` flip of the bool alone
    /// (the old pump observes the generation change and exits; only the new
    /// generation's pump delivers).
    pump_generation: Arc<AtomicU64>,
}

#[napi]
impl Composition {
    /// Start the composition (build + start one capture per source). If an
    /// `onData` callback is registered, a background pump thread is spawned.
    #[napi]
    pub fn start(&self) -> Result<()> {
        // Fast path under a shared guard (rsac-8082): a redundant start on a
        // running composition must not request the exclusive write guard, which
        // would queue behind a parked reader's shared guard.
        let already_running = {
            let inner = self.inner.read().map_err(lock_poisoned)?;
            inner.is_running()
        };
        if !already_running {
            let mut inner = self.inner.write().map_err(lock_poisoned)?;
            inner.start().map_err(audio_err_to_napi)?;
        }

        let has_callback = {
            let cb = self.callback.lock().map_err(lock_poisoned)?;
            cb.is_some()
        };
        if has_callback && !self.pump_active.load(Ordering::SeqCst) {
            self.start_data_pump()?;
        }
        Ok(())
    }

    /// Stop the composition: signal the ring + engine (waking a parked reader)
    /// and join the compositor thread. Discards the buffered tail.
    #[napi]
    pub fn stop(&self) -> Result<()> {
        // Bump the generation FIRST: any live pump loses ownership immediately
        // and can never be resurrected by a later flag flip (PR #59 review).
        self.pump_generation.fetch_add(1, Ordering::SeqCst);
        self.pump_active.store(false, Ordering::SeqCst);

        // rsac-8082: FIRST take a read guard to signal via
        // CapturingStream::stop(&self) — shared with a parked reader, so it never
        // blocks — which ends the ring + flags the engine and wakes the reader;
        // then take the write guard for the joining inherent stop.
        {
            let inner = self.inner.read().map_err(lock_poisoned)?;
            let _ = rsac::CapturingStream::stop(&*inner);
        }
        let mut inner = self.inner.write().map_err(lock_poisoned)?;
        // Fully-qualified inherent stop: `Composition` also implements the trait
        // `CapturingStream::stop(&self)` (signal-only, no join), and bare
        // `inner.stop()` would resolve to THAT by autoref — leaving the engine
        // thread unjoined. Call the joining lifecycle stop explicitly.
        rsac::Composition::stop(&mut inner).map_err(audio_err_to_napi)?;
        Ok(())
    }

    /// Whether the composed stream is currently running.
    #[napi(getter)]
    pub fn is_running(&self) -> Result<bool> {
        let inner = self.inner.read().map_err(lock_poisoned)?;
        Ok(inner.is_running())
    }

    /// Read the next composed buffer (non-blocking). Returns `null` if no data
    /// is available yet; terminal-observable (throws the fatal terminal error
    /// once the composition ends and drains).
    #[napi]
    pub fn read(&self) -> Result<Option<AudioChunk>> {
        let inner = self.inner.read().map_err(lock_poisoned)?;
        let result = inner.read_chunk_nonblocking().map_err(audio_err_to_napi)?;
        Ok(result.map(|buf| AudioChunk::from_rsac_buffer(&buf)))
    }

    /// Read the next composed buffer, blocking until data is available.
    /// Terminal-observable. WARNING: blocks the calling thread.
    #[napi]
    pub fn read_blocking(&self) -> Result<AudioChunk> {
        let inner = self.inner.read().map_err(lock_poisoned)?;
        let result = inner.read_chunk_blocking().map_err(audio_err_to_napi)?;
        Ok(AudioChunk::from_rsac_buffer(&result))
    }

    /// Read the next composed buffer asynchronously (non-blocking, off the main
    /// thread). Returns `null` if no data is available yet.
    #[napi]
    pub async fn read_async(&self) -> Result<Option<AudioChunk>> {
        let inner = self.inner.clone();
        let result =
            tokio::task::spawn_blocking(move || -> napi::Result<Option<rsac::AudioBuffer>> {
                let comp = inner.read().map_err(lock_poisoned)?;
                comp.read_chunk_nonblocking().map_err(audio_err_to_napi)
            })
            .await
            .map_err(join_err)??;
        Ok(result.map(|buf| AudioChunk::from_rsac_buffer(&buf)))
    }

    /// Read the next composed buffer asynchronously, blocking the worker thread
    /// until data is available (does not block the event loop).
    /// Terminal-observable.
    #[napi]
    pub async fn read_blocking_async(&self) -> Result<AudioChunk> {
        let inner = self.inner.clone();
        let result = tokio::task::spawn_blocking(move || -> napi::Result<rsac::AudioBuffer> {
            let comp = inner.read().map_err(lock_poisoned)?;
            comp.read_chunk_blocking().map_err(audio_err_to_napi)
        })
        .await
        .map_err(join_err)??;
        Ok(AudioChunk::from_rsac_buffer(&result))
    }

    /// Register a callback for push-based composed-audio delivery. Only one
    /// callback is active at a time; calling again replaces it.
    #[napi(ts_args_type = "callback: (chunk: AudioChunk) => void")]
    pub fn on_data(
        &self,
        callback: ThreadsafeFunction<AudioChunk, ErrorStrategy::Fatal>,
    ) -> Result<()> {
        {
            let mut cb_guard = self.callback.lock().map_err(lock_poisoned)?;
            *cb_guard = Some(callback);
        }
        let is_running = {
            let inner = self.inner.read().map_err(lock_poisoned)?;
            inner.is_running()
        };
        if is_running && !self.pump_active.load(Ordering::SeqCst) {
            self.start_data_pump()?;
        }
        Ok(())
    }

    /// Register a callback that fires exactly once when push-based delivery ends,
    /// carrying *why* it ended (a non-null message on a fatal terminal, `null`
    /// on a clean stop). Parity with the `AudioCapture` `onEnd`.
    #[napi(ts_args_type = "callback: (error: string | null) => void")]
    pub fn on_end(
        &self,
        callback: ThreadsafeFunction<Option<String>, ErrorStrategy::Fatal>,
    ) -> Result<()> {
        let mut cb_guard = self.end_callback.lock().map_err(lock_poisoned)?;
        *cb_guard = Some(callback);
        Ok(())
    }

    /// Remove the registered data callback and stop the pump. The `onEnd`
    /// callback is left registered for a later session; use `offEnd` to clear it.
    #[napi]
    pub fn off_data(&self) -> Result<()> {
        // Generation bump makes cancellation immune to a rapid re-`onData()`
        // setting pump_active back to true before the old pump observes false:
        // the old pump checks its OWN generation and exits (PR #59 review).
        self.pump_generation.fetch_add(1, Ordering::SeqCst);
        self.pump_active.store(false, Ordering::SeqCst);
        let mut cb_guard = self.callback.lock().map_err(lock_poisoned)?;
        *cb_guard = None;
        Ok(())
    }

    /// Remove the registered terminal-observability callback (see `onEnd`).
    #[napi]
    pub fn off_end(&self) -> Result<()> {
        let mut cb_guard = self.end_callback.lock().map_err(lock_poisoned)?;
        *cb_guard = None;
        Ok(())
    }

    /// Number of composed-ring overruns (composed buffers dropped because the
    /// consumer read slower than the compositor produced). 0 before start.
    #[napi(getter)]
    pub fn overrun_count(&self) -> Result<u32> {
        let inner = self.inner.read().map_err(lock_poisoned)?;
        Ok(rsac::CapturingStream::overrun_count(&*inner) as u32)
    }

    /// Number of composed output channels (0 before a successful start).
    #[napi(getter)]
    pub fn channel_count(&self) -> Result<u16> {
        let inner = self.inner.read().map_err(lock_poisoned)?;
        Ok(inner.channel_map().map(|m| m.channels()).unwrap_or(0))
    }

    /// Name of the group producing composed output channel `channel` (0-based),
    /// or `null` if not started or out of bounds.
    #[napi]
    pub fn channel_group(&self, channel: u32) -> Result<Option<String>> {
        let inner = self.inner.read().map_err(lock_poisoned)?;
        Ok(inner
            .channel_map()
            .and_then(|m| m.entries().get(channel as usize).map(|e| e.group.clone())))
    }

    /// Index of composed output channel `channel` within its group (0-based;
    /// e.g. 0 = L, 1 = R for a stereo group), or `null` if not started or out of
    /// bounds.
    #[napi]
    pub fn channel_in_group(&self, channel: u32) -> Result<Option<i32>> {
        let inner = self.inner.read().map_err(lock_poisoned)?;
        Ok(inner.channel_map().and_then(|m| {
            m.entries()
                .get(channel as usize)
                .map(|e| i32::from(e.channel_in_group))
        }))
    }

    /// Point-in-time composition counters, or `null` if not started.
    #[napi]
    pub fn stats(&self) -> Result<Option<JsCompositionStats>> {
        let inner = self.inner.read().map_err(lock_poisoned)?;
        Ok(inner.stats().map(|s| JsCompositionStats {
            ticks: bigint_from_u64(s.ticks),
            fallback_ticks: bigint_from_u64(s.fallback_ticks),
            num_sources: bigint_from_u64(s.sources.len() as u64),
        }))
    }

    /// Per-source counters for the source at `index` (flat declaration order),
    /// or `null` if not started or `index` is out of bounds.
    #[napi]
    pub fn source_stats(&self, index: u32) -> Result<Option<JsSourceStats>> {
        let inner = self.inner.read().map_err(lock_poisoned)?;
        Ok(inner
            .stats()
            .and_then(|s| s.sources.into_iter().nth(index as usize))
            .map(|src| JsSourceStats {
                group: src.group,
                target: src.target,
                buffers_received: bigint_from_u64(src.buffers_received),
                padded_frames: bigint_from_u64(src.padded_frames),
                trimmed_frames: bigint_from_u64(src.trimmed_frames),
                gap_padded_frames: bigint_from_u64(src.gap_padded_frames),
                inner_dropped: bigint_from_u64(src.inner_dropped),
                resampling: src.resampling,
                ended: src.ended,
            }))
    }

    /// Total composed buffers dropped by this composition's subscribe pumps
    /// because a subscriber's bounded channel was full. 0 before start.
    #[napi]
    pub fn subscriber_dropped_count(&self) -> Result<BigInt> {
        let inner = self.inner.read().map_err(lock_poisoned)?;
        Ok(bigint_from_u64(inner.subscriber_dropped_count()))
    }
}

impl Composition {
    /// Spawn the composed-audio data pump — a copy of `AudioCapture`'s
    /// `start_data_pump` reading `read_chunk_nonblocking`, with the same
    /// recoverable-error log throttle and terminal `onEnd` firing.
    fn start_data_pump(&self) -> Result<()> {
        let inner = self.inner.clone();
        let callback = self.callback.clone();
        let end_callback = self.end_callback.clone();
        let pump_active = self.pump_active.clone();
        let pump_generation = self.pump_generation.clone();

        // This pump owns the generation value current at spawn time; any
        // cancellation (stop/offData) bumps it, ending ownership even if
        // pump_active is set true again by a rapid re-onData (PR #59 review).
        let my_generation = pump_generation.fetch_add(1, Ordering::SeqCst) + 1;
        pump_active.store(true, Ordering::SeqCst);

        let spawn_result = std::thread::Builder::new()
            .name("rsac-napi-compose-pump".into())
            .spawn(move || {
                let mut end_reason: Option<String> = None;
                let mut recoverable_errors: u64 = 0;
                while pump_active.load(Ordering::SeqCst)
                    && pump_generation.load(Ordering::SeqCst) == my_generation
                {
                    let maybe_buf = {
                        let comp = match inner.read() {
                            Ok(c) => c,
                            Err(_) => break,
                        };
                        comp.read_chunk_nonblocking()
                    };
                    match maybe_buf {
                        Ok(Some(buf)) => {
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
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                        Err(ref e) if e.is_fatal() => {
                            eprintln!("rsac-napi compose pump ended (terminal): {}", e);
                            end_reason = Some(e.to_string());
                            break;
                        }
                        Err(e) => {
                            if should_log_recoverable(recoverable_errors) {
                                if recoverable_errors == 0 {
                                    eprintln!(
                                        "rsac-napi compose pump read error (retrying): {}",
                                        e
                                    );
                                } else {
                                    eprintln!(
                                        "rsac-napi compose pump read error (retrying; \
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
                // Clear the running flag only if this pump still OWNS the
                // generation — a stale pump exiting after a rapid re-onData
                // must not kill the successor pump's flag (PR #59 review).
                if pump_generation.load(Ordering::SeqCst) == my_generation {
                    pump_active.store(false, Ordering::SeqCst);
                }

                let end_tsfn = match end_callback.lock() {
                    Ok(guard) => guard.clone(),
                    Err(_) => None,
                };
                if let Some(tsfn) = end_tsfn {
                    tsfn.call(end_reason, ThreadsafeFunctionCallMode::NonBlocking);
                }
            });
        match spawn_result {
            Ok(_) => Ok(()),
            Err(e) => {
                // Roll the flag back so a later onData()/start() can retry —
                // leaving it true would permanently wedge pump spawning
                // (PR #59 review).
                self.pump_active.store(false, Ordering::SeqCst);
                Err(napi::Error::new(
                    napi::Status::GenericFailure,
                    format!("Failed to spawn compose data pump thread: {}", e),
                ))
            }
        }
    }
}

/// A point-in-time snapshot of a running composition's counters. Counters are
/// `BigInt` (u64) to avoid silent precision loss on long-running sessions.
#[napi(object)]
pub struct JsCompositionStats {
    /// Composed buffers (ticks) emitted so far.
    pub ticks: BigInt,
    /// Ticks emitted by the wall-clock stall fallback (master had no data).
    pub fallback_ticks: BigInt,
    /// Number of composed sources, in flat declaration order.
    pub num_sources: BigInt,
}

/// A point-in-time snapshot of one composed source's counters. Exposes the full
/// Rust `SourceStats` set (including `gapPaddedFrames` / `innerDropped`, which
/// the C FFI struct omits). u64 counters are `BigInt`.
#[napi(object)]
pub struct JsSourceStats {
    /// Name of the group the source belongs to.
    pub group: String,
    /// The source's capture target in canonical grammar (e.g. `"system"`).
    pub target: String,
    /// Buffers received from the inner capture so far.
    pub buffers_received: BigInt,
    /// Frames of silence inserted because the source was behind at tick time.
    pub padded_frames: BigInt,
    /// Frames trimmed because the source drifted past the buffering bound.
    pub trimmed_frames: BigInt,
    /// Frames of silence inserted to compensate intra-source timestamp gaps.
    pub gap_padded_frames: BigInt,
    /// Ring-overflow drops inside the source's own capture (upstream loss).
    pub inner_dropped: BigInt,
    /// Whether this source is being resampled to the session rate.
    pub resampling: bool,
    /// Whether the source's stream has ended.
    pub ended: bool,
}

// ── napi error helpers (shared by the compose surface) ─────────────────────

/// Map a poisoned lock to a napi error (deduped across the compose methods).
fn lock_poisoned<E: std::fmt::Display>(e: E) -> napi::Error {
    napi::Error::new(
        napi::Status::GenericFailure,
        format!("Lock poisoned: {}", e),
    )
}

/// Map a tokio join error to a napi error.
fn join_err<E: std::fmt::Display>(e: E) -> napi::Error {
    napi::Error::new(
        napi::Status::GenericFailure,
        format!("Task join error: {}", e),
    )
}

/// The error for operating on a `CompositionBuilder` whose value was taken.
fn builder_consumed() -> napi::Error {
    napi::Error::new(
        napi::Status::GenericFailure,
        "CompositionBuilder has been consumed".to_string(),
    )
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

    // ── rsac-8082: stop() must not deadlock a parked blocking read ───────
    //
    // The real `AudioCapture` wrapper stores `Arc<RwLock<rsac::AudioCapture>>`,
    // and `rsac::AudioCapture` can only be built via the device-backed builder —
    // so we cannot construct a *silent-stream* real capture from a unit test
    // without hardware. Instead we reproduce the exact WRAPPER LOCK TOPOLOGY the
    // fix depends on with a faithful stand-in whose method split mirrors core:
    //
    //   - `read_chunk_blocking(&self)` parks until a terminal flag flips (the
    //     silent-stream case: a running stream that never delivers data);
    //   - `request_stop(&self)` flips that flag (core's unblock primitive — it
    //     needs only `&self` and does not touch the lock the parked read holds);
    //   - `stop(&mut self)` is the lifecycle mutator that needs exclusive access.
    //
    // This is the binding-side analogue of core's
    // `request_stop_unblocks_parked_blocking_read` (src/api.rs). It deadlocks on
    // the OLD shape (a single `Mutex` held across the park, `stop()` taking that
    // same lock) and passes on the NEW shape (`RwLock`: reader parks under a
    // shared read guard; `stop()` takes a read guard to `request_stop()` first,
    // then the write guard), which is exactly what the napi `stop()` now does.

    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    /// A stand-in for `rsac::AudioCapture` that models a *silent* running stream:
    /// `read_chunk_blocking` never returns data until the stream is signalled
    /// terminal via `request_stop`. Mirrors the core method receivers exactly
    /// (`read`/`request_stop` = `&self`, `stop` = `&mut self`).
    struct SilentCapture {
        terminal: AtomicBool,
        /// Set once a blocking read has actually parked, so the test signals stop
        /// only when the read is genuinely in flight (deterministic, no fixed
        /// sleep) — same barrier idea as core's parked-read test.
        parked: AtomicBool,
    }

    impl SilentCapture {
        fn new() -> Self {
            Self {
                terminal: AtomicBool::new(false),
                parked: AtomicBool::new(false),
            }
        }

        /// Blocks until `request_stop` flips the terminal flag, then returns the
        /// terminal signal (`Err`). A real silent stream behaves the same: the
        /// blocking read parks until the stream goes terminal.
        fn read_chunk_blocking(&self) -> Result<()> {
            self.parked.store(true, Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.terminal.load(Ordering::SeqCst) {
                if Instant::now() > deadline {
                    // Safety net so a genuine hang fails the test rather than
                    // wedging the whole suite forever.
                    return Err(napi::Error::from_reason("read timed out (deadlock)"));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(napi::Error::from_reason("StreamEnded"))
        }

        /// Core's `request_stop(&self)`: flips the stream terminal to unblock a
        /// parked reader. Needs only `&self`, so it is compatible with the shared
        /// read guard the parked reader holds.
        fn request_stop(&self) {
            self.terminal.store(true, Ordering::SeqCst);
        }

        /// Core's `stop(&mut self)`: the lifecycle mutator needing exclusive
        /// access. (Body is irrelevant to the topology under test.)
        fn stop(&mut self) {
            self.terminal.store(true, Ordering::SeqCst);
        }
    }

    /// The FIXED `stop()` topology (what napi `stop()` now does): take a *read*
    /// guard to `request_stop()` (shared with the parked reader → never blocks),
    /// drop it, then take the *write* guard for `stop()`. With a reader parked
    /// under a read guard, this must complete promptly.
    #[test]
    fn stop_does_not_deadlock_parked_blocking_read() {
        let inner = Arc::new(RwLock::new(SilentCapture::new()));

        // Reader thread: parks in a blocking read while holding a SHARED READ
        // guard, exactly like napi `read_blocking`.
        let reader = {
            let inner = Arc::clone(&inner);
            std::thread::spawn(move || {
                let guard = inner.read().expect("read guard");
                guard.read_chunk_blocking()
            })
        };

        // Wait (bounded) until the read has genuinely parked, so we exercise the
        // stop-vs-in-flight-read race rather than a pre-park stop.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if inner.read().unwrap().parked.load(Ordering::SeqCst) {
                break;
            }
            assert!(Instant::now() < deadline, "reader never parked");
            std::thread::yield_now();
        }

        // The FIXED stop(): read-guard → request_stop() → drop → write-guard.
        // Run it on another thread bounded by a generous timeout so a regression
        // (e.g. reverting to a write-first stop) surfaces as a test failure
        // instead of hanging the suite.
        let stopper = {
            let inner = Arc::clone(&inner);
            std::thread::spawn(move || {
                {
                    let guard = inner.read().expect("read guard for request_stop");
                    guard.request_stop();
                }
                let mut guard = inner.write().expect("write guard for stop");
                guard.stop();
            })
        };

        // stop() must finish well within the reader's 10 s park safety net.
        let stop_deadline = Instant::now() + Duration::from_secs(5);
        while !stopper.is_finished() {
            assert!(
                Instant::now() < stop_deadline,
                "stop() deadlocked against a parked blocking read (rsac-8082 regression)"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        stopper.join().expect("stopper thread joins");

        // The parked reader was woken by request_stop() and observed the terminal
        // signal (an Err — the analogue of StreamEnded), not a timeout.
        let read_result = reader.join().expect("reader thread joins");
        let err = read_result.expect_err("silent read ends with a terminal error");
        assert!(
            err.reason.contains("StreamEnded"),
            "reader must wake on the terminal signal, not the deadlock safety net; got: {}",
            err.reason
        );
    }

    /// Documents WHY the old single-lock shape deadlocked: if `stop()` took the
    /// exclusive guard *first* (as the old `Mutex`-based binding effectively did
    /// by locking before signalling), it could never acquire it while a reader is
    /// parked holding a (shared) guard — because the parked reader only releases
    /// once signalled, and the signal is unreachable behind the exclusive
    /// acquire. We assert the deadlock shape times out, proving the ordering in
    /// `stop_does_not_deadlock_parked_blocking_read` is load-bearing.
    #[test]
    fn write_first_stop_would_deadlock_against_parked_reader() {
        let inner = Arc::new(RwLock::new(SilentCapture::new()));

        let reader = {
            let inner = Arc::clone(&inner);
            std::thread::spawn(move || {
                let guard = inner.read().expect("read guard");
                guard.read_chunk_blocking()
            })
        };

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if inner.read().unwrap().parked.load(Ordering::SeqCst) {
                break;
            }
            assert!(Instant::now() < deadline, "reader never parked");
            std::thread::yield_now();
        }

        // The BROKEN ordering: try to take the write guard BEFORE signalling. A
        // reader is parked under a read guard, so this write acquire cannot
        // succeed until the reader releases — which it never will without a
        // signal. Bounded try so the test itself cannot hang.
        let acquired_write = {
            let inner = Arc::clone(&inner);
            std::thread::spawn(move || {
                let start = Instant::now();
                // Poll try_write to emulate a bounded wait for the exclusive lock.
                while start.elapsed() < Duration::from_millis(500) {
                    if inner.try_write().is_ok() {
                        return true; // got the exclusive lock → would NOT deadlock
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                false // never acquired within the window → the deadlock shape
            })
            .join()
            .expect("write-attempt thread joins")
        };

        assert!(
            !acquired_write,
            "write-first stop unexpectedly acquired the exclusive lock while a \
             reader was parked — the topology no longer reproduces the deadlock, \
             so the regression test would be vacuous"
        );

        // Clean up: signal so the parked reader (and its read guard) is released.
        inner.read().unwrap().request_stop();
        let _ = reader.join().expect("reader thread joins");
    }

    // ── rsac-fba7: compose classification + gain validation + lock topology ──
    //
    // A real `rsac::Composition` needs devices, and the `#[napi]` surface needs a
    // node runtime — so, like the capture tests above, these pin the
    // napi-environment-independent logic: (1) the terminal-error classification
    // the compose read path + pump rely on, (2) the eager gain rejection
    // `Group.source_with_gain` performs, and (3) the stop-vs-parked-read RwLock
    // topology `Composition` shares with `AudioCapture`, via a compose-flavoured
    // `SilentComposition` stand-in whose receivers mirror `Composition`.

    /// The compose read path + pump end on, and only on, a FATAL terminal
    /// (`StreamEnded`); a not-started read is a RECOVERABLE `StreamReadError`.
    /// This is the same classification the napi pump's `is_fatal()` branch uses.
    #[test]
    fn compose_read_terminal_classification() {
        let ended = rsac::AudioError::StreamEnded {
            reason: "Composition ended (all sources terminal, ring drained)".into(),
        };
        assert!(ended.is_fatal());
        assert!(!ended.is_recoverable());

        let not_started = rsac::AudioError::StreamReadError {
            reason: "not started".into(),
        };
        assert!(not_started.is_recoverable());
        assert!(!not_started.is_fatal());
    }

    /// `Group.sourceWithGain` rejects a non-finite or negative gain eagerly. Pin
    /// the exact predicate the method uses — validation happens AFTER the f32
    /// narrowing, so a finite f64 above f32::MAX (which casts to +inf) is
    /// rejected too (PR #59 review) — matching the C FFI
    /// `invalid_gain_rejected_eagerly` and the Python binding.
    #[test]
    fn compose_gain_validation_predicate() {
        let reject = |g: f64| {
            let g32 = g as f32;
            !g32.is_finite() || g32 < 0.0
        };
        for bad in [
            -0.5f64,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::MAX, // finite as f64, +inf after f32 narrowing
        ] {
            assert!(reject(bad), "gain {bad} must be rejected");
        }
        for ok in [0.0f64, 1.0, 0.8, 4.0] {
            assert!(!reject(ok), "gain {ok} must be accepted");
        }
    }

    /// A stand-in for a *silent* running `rsac::Composition`: `read_chunk_blocking`
    /// parks until signalled terminal via `request_stop` (the
    /// `CapturingStream::stop(&self)` analogue), and `stop(&mut self)` is the
    /// joining lifecycle mutator. Copy of the capture `SilentCapture` renamed for
    /// compose (same receivers as core).
    struct SilentComposition {
        terminal: AtomicBool,
        parked: AtomicBool,
    }

    impl SilentComposition {
        fn new() -> Self {
            Self {
                terminal: AtomicBool::new(false),
                parked: AtomicBool::new(false),
            }
        }

        fn read_chunk_blocking(&self) -> Result<()> {
            self.parked.store(true, Ordering::SeqCst);
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.terminal.load(Ordering::SeqCst) {
                if Instant::now() > deadline {
                    return Err(napi::Error::from_reason("read timed out (deadlock)"));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(napi::Error::from_reason("StreamEnded"))
        }

        fn request_stop(&self) {
            self.terminal.store(true, Ordering::SeqCst);
        }

        fn stop(&mut self) {
            self.terminal.store(true, Ordering::SeqCst);
        }
    }

    /// The FIXED `Composition.stop()` topology (what the napi `stop()` now does):
    /// read-guard `request_stop()` (shared with the parked reader → never blocks)
    /// → drop → write-guard `stop()`. With a reader parked under a read guard,
    /// this must complete promptly (the napi side needs no GIL dance — a woken
    /// Rust reader unwinds and drops its guard directly).
    #[test]
    fn composition_stop_does_not_deadlock_parked_blocking_read() {
        let inner = Arc::new(RwLock::new(SilentComposition::new()));

        let reader = {
            let inner = Arc::clone(&inner);
            std::thread::spawn(move || {
                let guard = inner.read().expect("read guard");
                guard.read_chunk_blocking()
            })
        };

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if inner.read().unwrap().parked.load(Ordering::SeqCst) {
                break;
            }
            assert!(Instant::now() < deadline, "reader never parked");
            std::thread::yield_now();
        }

        let stopper = {
            let inner = Arc::clone(&inner);
            std::thread::spawn(move || {
                {
                    let guard = inner.read().expect("read guard for request_stop");
                    guard.request_stop();
                }
                let mut guard = inner.write().expect("write guard for stop");
                guard.stop();
            })
        };

        let stop_deadline = Instant::now() + Duration::from_secs(5);
        while !stopper.is_finished() {
            assert!(
                Instant::now() < stop_deadline,
                "Composition.stop() deadlocked against a parked blocking read (rsac-8082 regression)"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        stopper.join().expect("stopper thread joins");

        let read_result = reader.join().expect("reader thread joins");
        let err = read_result.expect_err("silent composition read ends terminal");
        assert!(
            err.reason.contains("StreamEnded"),
            "reader must wake on the terminal signal, not the deadlock safety net; got: {}",
            err.reason
        );
    }
}
