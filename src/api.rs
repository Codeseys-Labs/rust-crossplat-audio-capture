//! Public builder/handle facade: [`AudioCaptureBuilder`] → [`AudioCapture`].
//!
//! This module defines the library's primary entry points. Consumers interact
//! with rsac through [`AudioCaptureBuilder`] (configuration) and
//! [`AudioCapture`] (the lifecycle handle returned from `build()`).
//!
//! # Thread safety
//!
//! [`AudioCapture`] is `Send + Sync`. Its internal state guards the
//! platform-specific stream behind an [`Arc<Mutex<_>>`] so the handle can be
//! moved across threads or shared behind an [`Arc`]. The underlying data plane
//! (ring buffer between OS callback and consumer) is lock-free; see
//! [`crate::bridge`] for the full description.
//!
//! # Multiple concurrent captures
//!
//! Multiple [`AudioCapture`] instances can run in the same process; each has
//! its own isolated ring buffer bridge (see [`crate::bridge`]), so they
//! cannot interfere.
//!
//! [`Arc`]: std::sync::Arc
//! [`Arc<Mutex<_>>`]: std::sync::Arc

use crate::audio::get_device_enumerator;
use crate::core::buffer::AudioBuffer;
use crate::core::capabilities::PlatformCapabilities;
use crate::core::config::{CaptureTarget, SampleFormat, StreamConfig};
// `AudioFormat` is only referenced by `pick_supported_format` (and its tests),
// which is itself `cfg(not(target_os = "linux"))`; gate the import to match so
// the Linux build stays warning-clean under `-D warnings`.
#[cfg(not(target_os = "linux"))]
use crate::core::config::AudioFormat;
use crate::core::error::{AudioError, AudioResult};
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

// Re-export AudioCaptureConfig from core::config so downstream code
// that uses `crate::api::AudioCaptureConfig` still resolves.
pub use crate::core::config::AudioCaptureConfig;

/// A builder for creating [`AudioCapture`] instances.
///
/// This builder allows for a flexible and clear way to specify audio capture parameters.
/// Once all desired parameters are set, call [`build`](AudioCaptureBuilder::build)
/// to validate the configuration and create an [`AudioCapture`] instance.
///
/// ## Example (new API)
///
/// ```rust,no_run
/// # use rsac::api::AudioCaptureBuilder;
/// # use rsac::core::config::{CaptureTarget, SampleFormat};
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let capture = AudioCaptureBuilder::new()
///     .with_target(CaptureTarget::SystemDefault)
///     .sample_rate(48000)
///     .channels(2)
///     .sample_format(SampleFormat::F32)
///     .build()?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct AudioCaptureBuilder {
    target: CaptureTarget,
    config: StreamConfig,
}

impl Default for AudioCaptureBuilder {
    fn default() -> Self {
        Self {
            target: CaptureTarget::SystemDefault,
            config: StreamConfig::default(),
        }
    }
}

impl AudioCaptureBuilder {
    /// Creates a new `AudioCaptureBuilder` with default settings.
    ///
    /// Defaults: target = `CaptureTarget::SystemDefault`, config = `StreamConfig::default()`
    /// (48 kHz, 2 channels, F32, no buffer size preference).
    pub fn new() -> Self {
        Self::default()
    }

    // ── CaptureTarget-based API ──────────────────────────────────────

    /// Sets the capture target (system default, device, application, …).
    pub fn with_target(mut self, target: CaptureTarget) -> Self {
        self.target = target;
        self
    }

    /// Sets the desired stream config in one shot.
    pub fn with_config(mut self, config: StreamConfig) -> Self {
        self.config = config;
        self
    }

    // ── Individual config setters ────────────────────────────────────

    /// Sets the desired sample rate in Hz (e.g., 44100, 48000).
    pub fn sample_rate(mut self, rate: u32) -> Self {
        self.config.sample_rate = rate;
        self
    }

    /// Sets the desired number of audio channels.
    pub fn channels(mut self, channels: u16) -> Self {
        self.config.channels = channels;
        self
    }

    /// Sets the desired sample format.
    pub fn sample_format(mut self, format: SampleFormat) -> Self {
        self.config.sample_format = format;
        self
    }

    /// Sets the desired buffer size in frames.
    pub fn buffer_size(mut self, size: Option<usize>) -> Self {
        self.config.buffer_size = size;
        self
    }

    /// Kept for backward compat — alias for `buffer_size`.
    pub fn buffer_size_frames(mut self, size: Option<u32>) -> Self {
        self.config.buffer_size = size.map(|s| s as usize);
        self
    }

    /// Validates settings and constructs an [`AudioCapture`] instance.
    pub fn build(self) -> AudioResult<AudioCapture> {
        // ── Validate target against platform capabilities ────────────
        let caps = PlatformCapabilities::query();
        match &self.target {
            CaptureTarget::Application(_) | CaptureTarget::ApplicationByName(_)
                if !caps.supports_application_capture =>
            {
                return Err(AudioError::PlatformNotSupported {
                    feature: "application capture".to_string(),
                    platform: caps.backend_name.to_string(),
                });
            }
            CaptureTarget::ProcessTree(_) if !caps.supports_process_tree_capture => {
                return Err(AudioError::PlatformNotSupported {
                    feature: "process tree capture".to_string(),
                    platform: caps.backend_name.to_string(),
                });
            }
            _ => {}
        }

        // ── Validate sample rate ────────────────────────────────────
        const SUPPORTED_SAMPLE_RATES: [u32; 6] = [22050, 32000, 44100, 48000, 88200, 96000];
        if !SUPPORTED_SAMPLE_RATES.contains(&self.config.sample_rate) {
            return Err(AudioError::InvalidParameter {
                param: "sample_rate".into(),
                reason: format!(
                    "Unsupported sample rate: {} Hz. Supported: 22050, 32000, 44100, 48000, 88200, 96000",
                    self.config.sample_rate
                ),
            });
        }

        // ── Validate channels ───────────────────────────────────────
        if self.config.channels == 0 {
            return Err(AudioError::ConfigurationError {
                message: "Channels must be greater than 0.".to_string(),
            });
        }
        const MAX_CHANNELS: u16 = 32;
        if self.config.channels > MAX_CHANNELS {
            return Err(AudioError::ConfigurationError {
                message: format!(
                    "Number of channels ({}) exceeds the maximum supported ({}).",
                    self.config.channels, MAX_CHANNELS
                ),
            });
        }

        // ── Build capture config ────────────────────────────────────
        let mut stream_config = self.config;
        stream_config.capture_target = self.target.clone();
        #[allow(unused_mut)] // mutated only in the non-Linux negotiation block
        let mut capture_config = AudioCaptureConfig {
            target: self.target,
            stream_config,
        };

        // ── Resolve device from target ──────────────────────────────
        let enumerator = get_device_enumerator()?;

        let selected_device = match &capture_config.target {
            CaptureTarget::SystemDefault => {
                // All backends return the default output device (used for loopback capture).
                enumerator
                    .get_default_device()
                    .map_err(|e| AudioError::DeviceEnumerationError {
                        reason: format!("Failed to get default device: {}", e),
                        context: None,
                    })?
            }
            CaptureTarget::Device(device_id) => {
                let devices = enumerator.enumerate_devices()?;
                let device = devices
                    .into_iter()
                    .find(|d| d.id() == *device_id)
                    .ok_or_else(|| AudioError::DeviceNotFound {
                        device_id: device_id.0.clone(),
                    })?;
                // Warn users that targeting an output device for capture may
                // not produce data on all platforms.  System capture or
                // Process Tap loopback is required for output-device audio.
                log::info!(
                    "Device capture targeting '{}' (id: {}). Note: if this is \
                     an output-only device, consider using CaptureTarget::SystemDefault \
                     for loopback capture.",
                    device.name(),
                    device_id
                );
                device
            }
            CaptureTarget::Application(_)
            | CaptureTarget::ApplicationByName(_)
            | CaptureTarget::ProcessTree(_) => {
                // Application capture typically uses the default output device
                enumerator
                    .get_default_device()
                    .map_err(|e| AudioError::DeviceEnumerationError {
                        reason: format!(
                            "Failed to get default output device for app capture: {}",
                            e
                        ),
                        context: None,
                    })?
            }
        };

        // ── Format negotiation (non-Linux) ──────────────────────────
        // Devices advertise a fixed set of formats via WASAPI / CoreAudio. If
        // the exact requested format isn't on offer, negotiate to the closest
        // supported one (prefer the requested sample rate, then an F32 sample
        // type) instead of hard-failing — consumers resample/downmix
        // downstream anyway, and the alternative is that perfectly capturable
        // devices (e.g. a virtual surround endpoint that only advertises
        // 8ch/96000, or a 44.1kHz-only interface) are unusable. Only error if
        // the device advertises no formats at all.
        #[cfg(not(target_os = "linux"))]
        {
            let requested = capture_config.stream_config.to_audio_format();
            let supported = selected_device.supported_formats();
            if !supported.is_empty() && !supported.contains(&requested) {
                match pick_supported_format(&supported, &requested) {
                    Some(f) => {
                        log::warn!(
                            "Device '{}' does not support requested format {:?}; \
                             negotiated to {:?}",
                            selected_device.name(),
                            requested,
                            f
                        );
                        capture_config.stream_config.sample_rate = f.sample_rate;
                        capture_config.stream_config.channels = f.channels;
                        capture_config.stream_config.sample_format = f.sample_format;
                    }
                    None => {
                        return Err(AudioError::UnsupportedFormat {
                            format: format!(
                                "The selected device '{}' advertises no usable audio formats \
                                 (requested {:?})",
                                selected_device.name(),
                                requested
                            ),
                            context: None,
                        });
                    }
                }
            }
        }

        Ok(AudioCapture {
            config: capture_config,
            device: Some(selected_device),
            stream: None,
            callback: Mutex::new(None),
            callback_pump: None,
        })
    }
}

/// Pick a device-supported format closest to `requested`.
///
/// Used by [`AudioCaptureBuilder::build()`] to negotiate when a device does
/// not advertise the exact requested format. Preference order:
/// 1. F32 at the requested sample rate (any channel count).
/// 2. F32 at the requested channel count (any sample rate).
/// 3. Any F32 format (fewest channels first — cheapest to downmix).
/// 4. The device's first advertised format (last resort).
///
/// Returns `None` only when `supported` is empty.
#[cfg(not(target_os = "linux"))]
fn pick_supported_format(
    supported: &[AudioFormat],
    requested: &AudioFormat,
) -> Option<AudioFormat> {
    if supported.is_empty() {
        return None;
    }
    if supported.contains(requested) {
        return Some(requested.clone());
    }
    let is_f32 = |f: &&AudioFormat| f.sample_format == SampleFormat::F32;
    if let Some(f) = supported
        .iter()
        .filter(is_f32)
        .find(|f| f.sample_rate == requested.sample_rate)
    {
        return Some(f.clone());
    }
    if let Some(f) = supported
        .iter()
        .filter(is_f32)
        .find(|f| f.channels == requested.channels)
    {
        return Some(f.clone());
    }
    if let Some(f) = supported.iter().filter(is_f32).min_by_key(|f| f.channels) {
        return Some(f.clone());
    }
    supported.first().cloned()
}

#[cfg(all(test, not(target_os = "linux")))]
mod format_negotiation_tests {
    use super::*;

    fn fmt(sample_rate: u32, channels: u16, sample_format: SampleFormat) -> AudioFormat {
        AudioFormat {
            sample_rate,
            channels,
            sample_format,
        }
    }

    #[test]
    fn empty_supported_returns_none() {
        assert!(pick_supported_format(&[], &fmt(48000, 2, SampleFormat::F32)).is_none());
    }

    #[test]
    fn surround_only_device_negotiates() {
        // The exact field failure: default output is an 8ch/96000-only endpoint.
        let supported = [
            fmt(96000, 8, SampleFormat::F32),
            fmt(96000, 8, SampleFormat::I16),
        ];
        let chosen = pick_supported_format(&supported, &fmt(48000, 2, SampleFormat::F32)).unwrap();
        assert_eq!(chosen, fmt(96000, 8, SampleFormat::F32));
    }

    #[test]
    fn prefers_requested_sample_rate_f32() {
        let supported = [
            fmt(44100, 2, SampleFormat::F32),
            fmt(48000, 2, SampleFormat::F32),
        ];
        let chosen = pick_supported_format(&supported, &fmt(48000, 1, SampleFormat::F32)).unwrap();
        assert_eq!(chosen, fmt(48000, 2, SampleFormat::F32));
    }

    #[test]
    fn exact_match_passthrough() {
        let supported = [fmt(48000, 2, SampleFormat::F32)];
        let chosen = pick_supported_format(&supported, &fmt(48000, 2, SampleFormat::F32)).unwrap();
        assert_eq!(chosen, fmt(48000, 2, SampleFormat::F32));
    }
}

/// Represents an active audio capture session.
///
/// Created via [`AudioCaptureBuilder::build()`]. Provides methods to start/stop
/// audio capture and read audio data via a pull-based streaming model.
/// The user audio callback type. Boxed `FnMut` invoked once per captured buffer.
type AudioCallback = Box<dyn FnMut(&AudioBuffer) + Send + 'static>;

/// A registered-but-not-yet-running callback, stored in [`AudioCapture`] until
/// [`start()`](AudioCapture::start) moves it into a pump thread. Held behind a
/// `Mutex<Option<...>>` only so `&self`-style set/clear can mutate it before the
/// pump owns it — the pump thread does **not** lock this while invoking the
/// callback (it takes ownership), so a callback can freely re-enter the handle.
type PendingCallback = Mutex<Option<AudioCallback>>;

/// Handle to a running callback pump thread.
///
/// The pump *owns* the callback (it was moved out of [`PendingCallback`] at
/// [`start()`](AudioCapture::start)), so no lock is held while the user closure
/// runs — a callback may call back into `AudioCapture` without deadlocking. The
/// pump exits when `stop_flag` is set or the stream errors; the [`JoinHandle`]
/// lets `stop()`/`Drop` join it deterministically rather than leaking the thread.
struct CallbackPump {
    stop_flag: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl CallbackPump {
    /// Signal the pump to stop and join it. Idempotent.
    ///
    /// If called from the pump's own thread (i.e. the user closure re-entered
    /// `AudioCapture` and triggered teardown), the join is skipped — a thread
    /// cannot join itself — and only the stop flag is set; the pump will exit at
    /// the next loop iteration. This makes "clear the callback from within the
    /// callback" safe rather than a self-join deadlock.
    fn shutdown(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            if handle.thread().id() == std::thread::current().id() {
                // Re-entrant teardown from the pump thread: don't join self.
                // The stop flag is set; the loop will break on its next pass.
                // Put the handle back so a later stop()/Drop from another thread
                // can still join it.
                self.handle = Some(handle);
            } else {
                let _ = handle.join();
            }
        }
    }
}

pub struct AudioCapture {
    config: AudioCaptureConfig,
    device: Option<Box<dyn crate::core::interface::AudioDevice>>,
    stream: Option<Arc<dyn crate::core::interface::CapturingStream + 'static>>,
    /// Callback registered via [`set_callback`](AudioCapture::set_callback)
    /// before the capture starts. Moved into the pump thread on `start()`.
    callback: PendingCallback,
    /// Active callback pump, if a callback was set when `start()` ran. `None`
    /// means no pump is running (so `start()` will never double-spawn).
    callback_pump: Option<CallbackPump>,
}

impl AudioCapture {
    /// Starts the audio capture stream.
    ///
    /// Creates the underlying OS stream (if not already created) and marks
    /// the capture as running. In the new `CapturingStream` contract, the
    /// stream starts producing data upon creation.
    pub fn start(&mut self) -> AudioResult<()> {
        // If a stream already exists and is running, this is a no-op.
        if let Some(stream) = self.stream.as_ref() {
            if stream.is_running() {
                return Ok(());
            }
        }

        if self.stream.is_none() {
            let device_ref =
                self.device
                    .as_ref()
                    .ok_or_else(|| AudioError::StreamCreationFailed {
                        reason: "Audio device not available to create stream (was None)."
                            .to_string(),
                        context: None,
                    })?;
            let capturing_stream_obj = device_ref.create_stream(&self.config.stream_config)?;
            self.stream = Some(Arc::from(capturing_stream_obj));
        }

        // Verify stream is available
        let stream_ref = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamCreationFailed {
                reason: "Stream not initialized before starting.".to_string(),
                context: None,
            })?;

        // If a callback was registered via set_callback() AND no pump is already
        // running, spawn a pump thread that delivers captured buffers to it.
        // Without this the stored closure is never invoked (the callback
        // delivery mode would silently do nothing). See
        // docs/designs/0002-callback-delivery.md.
        //
        // Guarding on `self.callback_pump.is_none()` makes a second start() a
        // no-op for the pump — two pumps must never race for the same ring. The
        // callback is *moved* into the pump (taken out of the pending slot), so
        // the pump never holds a lock while running the user closure.
        if self.callback_pump.is_none() {
            let taken = self.callback.lock().ok().and_then(|mut g| g.take());
            if let Some(callback) = taken {
                let pump = Self::spawn_callback_pump(Arc::clone(stream_ref), callback)?;
                self.callback_pump = Some(pump);
            }
        }

        Ok(())
    }

    /// Spawns the callback pump thread and returns a [`CallbackPump`] handle.
    ///
    /// The pump **owns** `callback` (moved in), reads buffers from `stream`, and
    /// invokes the closure on this dedicated thread — **not** the OS real-time
    /// audio thread — so a slow callback only delays delivery, it never stalls
    /// the audio callback, and the closure may freely call back into
    /// `AudioCapture` (no lock is held during invocation). The thread exits when:
    /// - `stop_flag` is set (via [`stop`](Self::stop)/[`clear_callback`](Self::clear_callback)/`Drop`), or
    /// - the stream stops or errors (`try_read_chunk` returns `Err`).
    ///
    /// The pump competes with [`read_buffer`](Self::read_buffer) and
    /// [`subscribe`](Self::subscribe) for buffers from the same ring; avoid
    /// mixing a callback with manual reads.
    fn spawn_callback_pump(
        stream: Arc<dyn crate::core::interface::CapturingStream + 'static>,
        mut callback: AudioCallback,
    ) -> AudioResult<CallbackPump> {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_thread = Arc::clone(&stop_flag);
        let handle = std::thread::Builder::new()
            .name("rsac-callback".into())
            .spawn(move || loop {
                if stop_flag_thread.load(Ordering::SeqCst) {
                    break;
                }
                match stream.try_read_chunk() {
                    // No lock held: the pump owns `callback`, so the user closure
                    // can re-enter AudioCapture (e.g. clear_callback) without
                    // deadlocking, and a panic here cannot poison a shared mutex.
                    Ok(Some(buffer)) => callback(&buffer),
                    Ok(None) => {
                        // No data right now — avoid busy-spinning.
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(_) => break, // stream stopped / closed / errored
                }
            })
            .map_err(|e| AudioError::InternalError {
                message: format!("Failed to spawn callback pump thread: {}", e),
                source: None,
            })?;
        Ok(CallbackPump {
            stop_flag,
            handle: Some(handle),
        })
    }

    /// Stops the audio capture stream.
    ///
    /// Stops the underlying OS stream and releases resources. After stopping,
    /// the stream cannot be restarted — create a new `AudioCapture` instead.
    ///
    /// Any active subscriber threads will terminate once they detect the stream
    /// has stopped. The underlying stream is released when all references
    /// (including subscriber threads) are dropped.
    pub fn stop(&mut self) -> AudioResult<()> {
        // Shut down the callback pump first (signal + join) so it stops
        // consuming buffers and releases its stream clone before we drop ours.
        // Joining here makes stop() authoritative for the pump thread rather
        // than leaking it until try_read_chunk happens to observe the stop.
        if let Some(mut pump) = self.callback_pump.take() {
            pump.shutdown();
        }

        // Nothing to stop if there is no stream (idempotent).
        if self.stream.is_none() {
            return Ok(());
        }

        if let Some(stream) = self.stream.as_ref() {
            if let Err(e) = stream.stop() {
                log::warn!("Error stopping stream: {:?}", e);
            }
        }
        // Drop our Arc reference. The stream will be fully deallocated once all
        // subscriber threads also drop their clones.
        self.stream.take();

        Ok(())
    }

    /// Returns `true` if the stream is currently capturing.
    ///
    /// Delegates to the underlying stream's state machine — the single source
    /// of truth for running status. Returns `false` if no stream has been
    /// created yet.
    pub fn is_running(&self) -> bool {
        self.stream
            .as_ref()
            .map(|s| s.is_running())
            .unwrap_or(false)
    }

    /// Returns a reference to the capture configuration.
    pub fn config(&self) -> &AudioCaptureConfig {
        &self.config
    }

    /// Reads a buffer of audio data synchronously.
    ///
    /// Uses `CapturingStream::try_read_chunk` for non-blocking reads.
    /// Returns `Ok(None)` if no data is currently available.
    pub fn read_buffer(&mut self) -> AudioResult<Option<AudioBuffer>> {
        // Get the stream first — if there's no stream, we're not running.
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: "Stream is not initialized. Call start() first.".to_string(),
            })?;

        // Check running state via the stream itself — single source of truth.
        // This eliminates the TOCTOU window that existed when a separate
        // AtomicBool was consulted before touching the stream.
        if !stream.is_running() {
            return Err(AudioError::StreamReadError {
                reason: "Stream is not running".to_string(),
            });
        }

        stream.try_read_chunk()
    }

    /// Reads a buffer of audio data, blocking until data is available.
    ///
    /// Uses `CapturingStream::read_chunk` which blocks until data arrives.
    pub fn read_buffer_blocking(&mut self) -> AudioResult<AudioBuffer> {
        // Get the stream first — if there's no stream, we're not running.
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: "Stream is not initialized. Call start() first.".to_string(),
            })?;

        // Check running state via the stream itself — single source of truth.
        if !stream.is_running() {
            return Err(AudioError::StreamReadError {
                reason: "Stream is not running".to_string(),
            });
        }

        stream.read_chunk()
    }

    /// Returns an iterator over synchronously captured audio buffers.
    pub fn buffers_iter(&mut self) -> AudioBufferIterator<'_> {
        AudioBufferIterator { capture: self }
    }

    /// Returns an asynchronous stream of audio data buffers.
    ///
    /// The returned [`AsyncAudioStream`](crate::bridge::AsyncAudioStream) implements
    /// [`futures_core::Stream`] and yields [`AudioBuffer`]s as they become available
    /// from the audio capture backend.
    ///
    /// The capture must be started (via [`start()`](Self::start)) before calling this method.
    ///
    /// # Feature Flag
    ///
    /// This method is only available when the `async-stream` feature is enabled.
    ///
    /// # Errors
    ///
    /// Returns an error if the capture has not been started.
    #[cfg(feature = "async-stream")]
    pub fn audio_data_stream(&self) -> AudioResult<crate::bridge::AsyncAudioStream<'_>> {
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: "Capture not started. Call start() before audio_data_stream().".to_string(),
            })?;

        Ok(crate::bridge::AsyncAudioStream::new(stream.as_ref()))
    }

    /// Returns an asynchronous stream of audio data buffers.
    ///
    /// **Note:** The `async-stream` feature is not enabled. Enable it in `Cargo.toml`
    /// to use async audio streaming.
    #[cfg(not(feature = "async-stream"))]
    pub fn audio_data_stream(
        &mut self,
    ) -> AudioResult<impl futures_core::Stream<Item = AudioResult<AudioBuffer>> + Send + Sync + '_>
    {
        Err::<
            std::pin::Pin<
                Box<dyn futures_core::Stream<Item = AudioResult<AudioBuffer>> + Send + Sync>,
            >,
            AudioError,
        >(AudioError::PlatformNotSupported {
            feature: "async audio streaming".to_string(),
            platform: "enable the 'async-stream' feature".to_string(),
        })
    }

    /// Sets a callback function for captured audio data.
    ///
    /// The callback will be invoked with each captured audio buffer.
    /// Callbacks cannot be set while capture is running.
    pub fn set_callback<F>(&mut self, callback: F) -> AudioResult<()>
    where
        F: FnMut(&AudioBuffer) + Send + 'static,
    {
        if self.is_running() {
            return Err(AudioError::ConfigurationError {
                message: "Cannot set callback after capture has started.".into(),
            });
        }
        match self.callback.lock() {
            Ok(mut guard) => {
                *guard = Some(Box::new(callback));
                Ok(())
            }
            Err(poisoned) => Err(AudioError::InternalError {
                message: format!("Failed to lock callback mutex: {}", poisoned),
                source: None,
            }),
        }
    }

    /// Clears the registered audio callback.
    ///
    /// If a capture is running with an active callback pump, this signals the
    /// pump to stop and joins it (so delivery ceases promptly), in addition to
    /// clearing any pending (not-yet-started) callback. It is safe to call from
    /// outside the callback. Calling it *from within* the callback signals the
    /// pump but does not self-join (the pump only joins on `stop()`/`Drop`),
    /// avoiding a self-join deadlock.
    pub fn clear_callback(&mut self) -> AudioResult<()> {
        // Tear down a running pump (the callback now lives inside it).
        if let Some(mut pump) = self.callback_pump.take() {
            pump.shutdown();
        }
        // Also clear any pending callback registered before start().
        match self.callback.lock() {
            Ok(mut guard) => {
                *guard = None;
                Ok(())
            }
            Err(poisoned) => Err(AudioError::InternalError {
                message: format!("Failed to lock callback mutex for clearing: {}", poisoned),
                source: None,
            }),
        }
    }

    /// Creates a subscription channel that delivers audio buffers as they are captured.
    ///
    /// Spawns a background thread that reads from the capture stream and sends
    /// buffers over an [`mpsc`] channel. Returns the receiving
    /// end of the channel.
    ///
    /// **Important:** The background thread competes with [`read_buffer()`](Self::read_buffer)
    /// and [`read_buffer_blocking()`](Self::read_buffer_blocking) for audio data
    /// from the same ring buffer. Avoid mixing `subscribe()` with manual buffer reads.
    ///
    /// The background thread exits automatically when:
    /// - The stream is stopped or encounters an error
    /// - The returned [`Receiver`](mpsc::Receiver) is dropped
    ///
    /// Multiple subscriptions are allowed but each subscriber competes for buffers.
    ///
    /// # Errors
    ///
    /// Returns an error if the capture is not currently running.
    pub fn subscribe(&self) -> AudioResult<mpsc::Receiver<AudioBuffer>> {
        // Get the stream first — if there's no stream, we're not running.
        let stream_ref = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: "Stream is not initialized. Call start() first.".to_string(),
            })?;

        // Check running state via the stream itself — single source of truth.
        if !stream_ref.is_running() {
            return Err(AudioError::StreamReadError {
                reason: "Stream is not running".to_string(),
            });
        }

        let stream = Arc::clone(stream_ref);

        let (tx, rx) = mpsc::channel();

        std::thread::Builder::new()
            .name("rsac-subscribe".into())
            .spawn(move || loop {
                match stream.try_read_chunk() {
                    Ok(Some(buffer)) => {
                        if tx.send(buffer).is_err() {
                            break; // Receiver dropped
                        }
                    }
                    Ok(None) => {
                        // No data available, sleep briefly to avoid busy-spinning
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(_) => {
                        break; // Stream error (stopped, closed, etc.)
                    }
                }
            })
            .map_err(|e| AudioError::InternalError {
                message: format!("Failed to spawn subscribe thread: {}", e),
                source: None,
            })?;

        Ok(rx)
    }

    /// Returns the number of audio buffers dropped due to ring buffer overflow (overruns).
    ///
    /// This counter reflects how many times the OS audio callback had to discard
    /// a buffer because the consumer was not reading fast enough. A non-zero value
    /// indicates the consumer is too slow or the ring buffer capacity is too small.
    ///
    /// Returns `0` if the stream has not been created yet.
    pub fn overrun_count(&self) -> u64 {
        self.stream.as_ref().map(|s| s.overrun_count()).unwrap_or(0)
    }

    /// Returns true if the stream is experiencing sustained backpressure —
    /// the ring buffer has dropped enough consecutive frames to indicate the
    /// consumer cannot keep up with the producer. Consumers should slow down
    /// processing, warn the user, or switch to a lower-cost provider.
    ///
    /// Returns `false` if the stream has not been created yet.
    pub fn is_under_backpressure(&self) -> bool {
        self.stream
            .as_ref()
            .map(|s| s.is_under_backpressure())
            .unwrap_or(false)
    }
}

// AudioDataStreamWrapper has been removed — async streaming will be
// re-introduced via the BridgeStream layer in a later phase.

// ── Iterator ─────────────────────────────────────────────────────────────

/// An iterator that yields audio buffers by synchronously reading from an [`AudioCapture`].
pub struct AudioBufferIterator<'a> {
    capture: &'a mut AudioCapture,
}

impl<'a> Iterator for AudioBufferIterator<'a> {
    type Item = AudioResult<AudioBuffer>;

    fn next(&mut self) -> Option<Self::Item> {
        // `read_buffer()` returns `Ok(None)` to mean "no data *right now*" — a
        // transient condition on a live stream, NOT end-of-stream. Mapping that
        // straight to `None` (the previous behavior) ended the iterator the first
        // time the ring was momentarily empty, which on a running capture is
        // almost immediately. Instead, retry on transient empties and only end
        // the iteration when the stream actually stops.
        loop {
            if !self.capture.is_running() {
                return None;
            }
            match self.capture.read_buffer() {
                Ok(Some(buffer)) => return Some(Ok(buffer)),
                Ok(None) => {
                    // No data yet — yield the OS a moment, then re-check whether
                    // the stream is still running and try again.
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

// ── Drop ─────────────────────────────────────────────────────────────────

impl Drop for AudioCapture {
    fn drop(&mut self) {
        // Tear down the callback pump first (signal + join) so its thread stops
        // touching the stream before we drop it, and is never leaked.
        if let Some(mut pump) = self.callback_pump.take() {
            pump.shutdown();
        }
        // Best-effort stop of whatever stream we still hold. The stream's own
        // state machine decides whether this is a no-op (already stopped) or
        // a real stop; stop() is idempotent on the stream side.
        if let Some(stream) = self.stream.as_ref() {
            if stream.is_running() {
                if let Err(e) = stream.stop() {
                    log::warn!("Error stopping audio stream during drop: {:?}", e);
                }
            }
        }
        // Drop the Arc reference (stream fully deallocated when last clone is dropped).
        self.stream.take();
    }
}

// ── Debug ────────────────────────────────────────────────────────────────

impl fmt::Debug for AudioCapture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let device_name = self
            .device
            .as_ref()
            .map(|d| d.name())
            .unwrap_or_else(|| "None".to_string());

        f.debug_struct("AudioCapture")
            .field("config", &self.config)
            .field("device_name", &device_name)
            .field("stream_is_some", &self.stream.is_some())
            .field("is_running", &self.is_running())
            // Never panic inside Debug: a poisoned callback mutex must not take
            // down an infallible formatter. Fall back to reporting the poison.
            .field(
                "callback_is_some",
                &match self.callback.try_lock() {
                    Ok(guard) => guard.is_some(),
                    Err(_) => false,
                },
            )
            .finish()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{AudioFormat, SampleFormat};
    use crate::core::interface::CapturingStream;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    #[test]
    fn builder_defaults_to_system_default() {
        let builder = AudioCaptureBuilder::new();
        assert_eq!(builder.target, CaptureTarget::SystemDefault);
        assert_eq!(builder.config.sample_rate, 48000);
        assert_eq!(builder.config.channels, 2);
        assert_eq!(builder.config.sample_format, SampleFormat::F32);
        assert_eq!(builder.config.buffer_size, None);
    }

    #[test]
    fn builder_fails_if_channels_is_zero() {
        let result = AudioCaptureBuilder::new()
            .with_target(CaptureTarget::SystemDefault)
            .sample_rate(44100)
            .channels(0)
            .sample_format(SampleFormat::F32)
            .build();
        assert!(result.is_err());
        match result.err().unwrap() {
            AudioError::ConfigurationError { message: msg } => {
                assert_eq!(msg, "Channels must be greater than 0.");
            }
            other_error => panic!("Expected ConfigurationError, got {:?}", other_error),
        }
    }

    #[test]
    fn builder_fails_on_unsupported_sample_rate() {
        let result = AudioCaptureBuilder::new()
            .with_target(CaptureTarget::SystemDefault)
            .sample_rate(11025) // Not supported
            .channels(1)
            .sample_format(SampleFormat::F32)
            .build();
        assert!(result.is_err());
        match result.err().unwrap() {
            AudioError::InvalidParameter { param, reason } => {
                assert_eq!(param, "sample_rate");
                assert!(reason.contains("11025"));
            }
            other_error => panic!("Expected InvalidParameter, got {:?}", other_error),
        }
    }

    #[test]
    fn builder_with_target_overrides_default() {
        let device_id = crate::core::config::DeviceId("test-device".to_string());
        let builder =
            AudioCaptureBuilder::new().with_target(CaptureTarget::Device(device_id.clone()));
        assert_eq!(builder.target, CaptureTarget::Device(device_id));
    }

    #[test]
    fn builder_with_config_sets_all_fields() {
        let config = StreamConfig {
            sample_rate: 44100,
            channels: 1,
            sample_format: SampleFormat::I16,
            buffer_size: Some(1024),
            capture_target: CaptureTarget::SystemDefault,
        };
        let builder = AudioCaptureBuilder::new().with_config(config.clone());
        assert_eq!(builder.config, config);
    }

    // ── Builder method chainability & defaults ────────────────────────

    #[test]
    fn builder_is_chainable() {
        // Verify all builder methods return Self and can be chained
        let builder = AudioCaptureBuilder::new()
            .with_target(CaptureTarget::SystemDefault)
            .sample_rate(44100)
            .channels(2)
            .sample_format(SampleFormat::F32)
            .buffer_size(Some(1024))
            .buffer_size_frames(Some(512));
        // Just verifying compilation and chainability — no panic
        assert_eq!(builder.config.sample_rate, 44100);
        assert_eq!(builder.config.channels, 2);
    }

    #[test]
    fn builder_default_trait_matches_new() {
        let from_new = AudioCaptureBuilder::new();
        let from_default = AudioCaptureBuilder::default();
        // Both should produce identical builders
        assert_eq!(from_new.config.sample_rate, from_default.config.sample_rate);
        assert_eq!(from_new.config.channels, from_default.config.channels);
        assert_eq!(
            from_new.config.sample_format,
            from_default.config.sample_format
        );
    }

    // ── Invalid sample rate tests ────────────────────────────────────

    #[test]
    fn builder_rejects_sample_rate_zero() {
        let result = AudioCaptureBuilder::new().sample_rate(0).build();
        assert!(result.is_err());
        match result.unwrap_err() {
            AudioError::InvalidParameter { param, .. } => assert_eq!(param, "sample_rate"),
            e => panic!("Expected InvalidParameter, got: {e:?}"),
        }
    }

    #[test]
    fn builder_rejects_very_high_sample_rate() {
        let result = AudioCaptureBuilder::new().sample_rate(999999).build();
        assert!(result.is_err());
        match result.unwrap_err() {
            AudioError::InvalidParameter { param, .. } => assert_eq!(param, "sample_rate"),
            e => panic!("Expected InvalidParameter, got: {e:?}"),
        }
    }

    #[test]
    fn builder_rejects_nonstandard_sample_rate() {
        // 11025 is a valid audio rate but not in the supported list
        let result = AudioCaptureBuilder::new().sample_rate(11025).build();
        assert!(result.is_err());
    }

    #[test]
    fn builder_accepts_all_supported_sample_rates() {
        // These should NOT fail at the sample_rate validation step
        // They may fail later at device enumeration, which is fine
        for rate in [22050u32, 32000, 44100, 48000, 88200, 96000] {
            let result = AudioCaptureBuilder::new().sample_rate(rate).build();
            // Should NOT be InvalidParameter for sample_rate
            if let Err(AudioError::InvalidParameter { param, .. }) = &result {
                panic!(
                    "Rate {rate} should be valid, but got InvalidParameter {{ param: {param} }}"
                );
            }
            // Other errors (DeviceEnumeration, etc.) are expected without hardware
        }
    }

    // ── Invalid channel count tests ──────────────────────────────────

    #[test]
    fn builder_rejects_channels_above_max() {
        let result = AudioCaptureBuilder::new()
            .channels(33) // MAX_CHANNELS = 32
            .build();
        assert!(result.is_err());
        match result.unwrap_err() {
            AudioError::ConfigurationError { .. } => {} // expected
            e => panic!("Expected ConfigurationError, got: {e:?}"),
        }
    }

    #[test]
    fn builder_rejects_channels_way_above_max() {
        let result = AudioCaptureBuilder::new().channels(u16::MAX).build();
        assert!(result.is_err());
    }

    #[test]
    fn builder_accepts_max_channels() {
        // 32 channels should be accepted (it's the max, not above it)
        let result = AudioCaptureBuilder::new().channels(32).build();
        // Should NOT be ConfigurationError
        if let Err(AudioError::ConfigurationError { .. }) = &result {
            panic!("32 channels (MAX_CHANNELS) should be accepted");
        }
        // Other errors (DeviceEnumeration, etc.) are fine
    }

    #[test]
    fn builder_accepts_mono() {
        let result = AudioCaptureBuilder::new().channels(1).build();
        // Should NOT be ConfigurationError for channels
        if let Err(AudioError::ConfigurationError { message }) = &result {
            if message.contains("hannels") {
                panic!("Mono (1 channel) should be accepted, got ConfigurationError: {message}");
            }
        }
    }

    // ── Sample format tests ──────────────────────────────────────────

    #[test]
    fn builder_with_all_sample_formats() {
        // Verify all sample formats can be set without panic
        for format in [
            SampleFormat::I16,
            SampleFormat::I24,
            SampleFormat::I32,
            SampleFormat::F32,
        ] {
            let builder = AudioCaptureBuilder::new().sample_format(format);
            assert_eq!(builder.config.sample_format, format);
        }
    }

    // ── Buffer size tests ────────────────────────────────────────────

    #[test]
    fn builder_buffer_size_can_be_set_and_cleared() {
        let b1 = AudioCaptureBuilder::new().buffer_size(Some(1024));
        assert_eq!(b1.config.buffer_size, Some(1024));

        let b2 = AudioCaptureBuilder::new().buffer_size(None);
        assert_eq!(b2.config.buffer_size, None);
    }

    #[test]
    fn builder_buffer_size_frames_sets_buffer_size() {
        let builder = AudioCaptureBuilder::new().buffer_size_frames(Some(256));
        assert_eq!(builder.config.buffer_size, Some(256));
    }

    // ── With_config override test ────────────────────────────────────

    #[test]
    fn builder_with_config_overrides_individual_settings() {
        let config = StreamConfig {
            sample_rate: 96000,
            channels: 8,
            sample_format: SampleFormat::I32,
            buffer_size: Some(2048),
            capture_target: CaptureTarget::SystemDefault,
        };
        let builder = AudioCaptureBuilder::new()
            .sample_rate(44100) // This should be overridden
            .with_config(config.clone());
        assert_eq!(builder.config.sample_rate, 96000);
        assert_eq!(builder.config.channels, 8);
        assert_eq!(builder.config.sample_format, SampleFormat::I32);
    }

    // ── Mock CapturingStream for subscribe/overrun_count tests ────────

    /// A mock CapturingStream that serves buffers from an internal Mutex<VecDeque>
    /// and tracks an overrun counter via an AtomicU64.
    struct MockCapturingStream {
        buffers: Mutex<std::collections::VecDeque<AudioBuffer>>,
        running: AtomicBool,
        overruns: AtomicU64,
    }

    impl MockCapturingStream {
        fn new() -> Self {
            Self {
                buffers: Mutex::new(std::collections::VecDeque::new()),
                running: AtomicBool::new(true),
                overruns: AtomicU64::new(0),
            }
        }

        /// Push a buffer for the mock to serve on the next try_read_chunk call.
        fn push_buffer(&self, buf: AudioBuffer) {
            self.buffers.lock().unwrap().push_back(buf);
        }

        /// Simulate overruns by incrementing the counter.
        fn add_overruns(&self, count: u64) {
            self.overruns.fetch_add(count, Ordering::Relaxed);
        }

        /// Signal the mock stream is stopped.
        fn signal_stop(&self) {
            self.running.store(false, Ordering::SeqCst);
        }
    }

    impl CapturingStream for MockCapturingStream {
        fn read_chunk(&self) -> AudioResult<AudioBuffer> {
            // Blocking: spin-wait until data or stopped
            loop {
                if let Some(buf) = self.buffers.lock().unwrap().pop_front() {
                    return Ok(buf);
                }
                if !self.running.load(Ordering::SeqCst) {
                    return Err(AudioError::StreamReadError {
                        reason: "Mock stream stopped".into(),
                    });
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }

        fn try_read_chunk(&self) -> AudioResult<Option<AudioBuffer>> {
            if !self.running.load(Ordering::SeqCst) {
                return Err(AudioError::StreamReadError {
                    reason: "Mock stream stopped".into(),
                });
            }
            Ok(self.buffers.lock().unwrap().pop_front())
        }

        fn stop(&self) -> AudioResult<()> {
            self.running.store(false, Ordering::SeqCst);
            Ok(())
        }

        fn format(&self) -> AudioFormat {
            AudioFormat::default()
        }

        fn is_running(&self) -> bool {
            self.running.load(Ordering::SeqCst)
        }

        fn overrun_count(&self) -> u64 {
            self.overruns.load(Ordering::Relaxed)
        }
    }

    /// Creates an AudioCapture with a mock stream, bypassing the builder (no hardware needed).
    fn make_mock_capture(mock: Arc<MockCapturingStream>) -> AudioCapture {
        AudioCapture {
            config: AudioCaptureConfig {
                target: CaptureTarget::SystemDefault,
                stream_config: StreamConfig::default(),
            },
            device: None,
            stream: Some(mock),
            callback: Mutex::new(None),
            callback_pump: None,
        }
    }

    // ── subscribe() tests ─────────────────────────────────────────────

    #[test]
    fn subscribe_returns_error_when_not_running() {
        let mock = Arc::new(MockCapturingStream::new());
        // Signal the mock (stream-side) that it's no longer running — the
        // stream's state is now the single source of truth.
        mock.signal_stop();
        let capture = make_mock_capture(mock);

        let result = capture.subscribe();
        assert!(result.is_err());
        match result.unwrap_err() {
            AudioError::StreamReadError { reason } => {
                assert!(reason.contains("not running"));
            }
            e => panic!("Expected StreamReadError, got: {e:?}"),
        }
    }

    #[test]
    fn subscribe_receives_buffers() {
        let mock = Arc::new(MockCapturingStream::new());
        // Push some test buffers before subscribing
        mock.push_buffer(AudioBuffer::new(vec![0.1; 960], 2, 48000));
        mock.push_buffer(AudioBuffer::new(vec![0.2; 960], 2, 48000));
        mock.push_buffer(AudioBuffer::new(vec![0.3; 960], 2, 48000));

        let capture = make_mock_capture(Arc::clone(&mock));
        let rx = capture.subscribe().expect("subscribe should succeed");

        // Receive the three buffers
        let buf1 = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(buf1.data()[0], 0.1);

        let buf2 = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(buf2.data()[0], 0.2);

        let buf3 = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(buf3.data()[0], 0.3);

        // Stop the mock so the subscribe thread exits
        mock.signal_stop();
    }

    #[test]
    fn subscribe_thread_stops_when_stream_stops() {
        let mock = Arc::new(MockCapturingStream::new());
        let capture = make_mock_capture(Arc::clone(&mock));
        let rx = capture.subscribe().expect("subscribe should succeed");

        // Signal stop — the subscribe thread should exit
        mock.signal_stop();

        // After a short delay, recv should fail (channel disconnected)
        std::thread::sleep(std::time::Duration::from_millis(50));
        let result = rx.recv_timeout(std::time::Duration::from_millis(100));
        assert!(result.is_err());
    }

    #[test]
    fn subscribe_thread_stops_when_receiver_dropped() {
        let mock = Arc::new(MockCapturingStream::new());
        let capture = make_mock_capture(Arc::clone(&mock));
        let rx = capture.subscribe().expect("subscribe should succeed");

        // Drop the receiver — the subscribe thread should eventually exit
        drop(rx);

        // Push a buffer to trigger the send error in the thread
        mock.push_buffer(AudioBuffer::new(vec![1.0; 960], 2, 48000));

        // Give the thread time to realize the receiver is gone and exit
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Clean up
        mock.signal_stop();
    }

    // ── overrun_count() tests ─────────────────────────────────────────

    #[test]
    fn overrun_count_returns_zero_when_no_stream() {
        let capture = AudioCapture {
            config: AudioCaptureConfig {
                target: CaptureTarget::SystemDefault,
                stream_config: StreamConfig::default(),
            },
            device: None,
            stream: None,
            callback: Mutex::new(None),
            callback_pump: None,
        };
        assert_eq!(capture.overrun_count(), 0);
    }

    #[test]
    fn overrun_count_returns_zero_initially() {
        let mock = Arc::new(MockCapturingStream::new());
        let capture = make_mock_capture(mock);
        assert_eq!(capture.overrun_count(), 0);
    }

    #[test]
    fn overrun_count_reflects_mock_overruns() {
        let mock = Arc::new(MockCapturingStream::new());
        let capture = make_mock_capture(Arc::clone(&mock));

        assert_eq!(capture.overrun_count(), 0);

        mock.add_overruns(5);
        assert_eq!(capture.overrun_count(), 5);

        mock.add_overruns(3);
        assert_eq!(capture.overrun_count(), 8);
    }

    #[test]
    fn overrun_count_returns_zero_after_stop() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.add_overruns(10);
        let mut capture = make_mock_capture(mock);

        assert_eq!(capture.overrun_count(), 10);

        // Stop drops the stream Arc
        capture.stop().unwrap();

        // After stop, stream is None, so overrun_count returns 0
        assert_eq!(capture.overrun_count(), 0);
    }

    // ── buffers_iter() tests (H2) ─────────────────────────────────────

    /// Regression (audit H2): the iterator must NOT end on a transient empty
    /// poll. With buffers queued, `next()` yields them in order; an interleaved
    /// empty poll (Ok(None)) is retried, not treated as end-of-stream.
    #[test]
    fn buffers_iter_yields_queued_then_continues_past_empty() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.push_buffer(AudioBuffer::new(vec![0.1; 8], 2, 48000));
        mock.push_buffer(AudioBuffer::new(vec![0.2; 8], 2, 48000));
        let mut capture = make_mock_capture(Arc::clone(&mock));

        // First two next() calls must return the queued buffers, even though the
        // mock's try_read_chunk returns Ok(None) once the queue drains (the old
        // iterator would have stopped at the first None instead of these items).
        let mut it = capture.buffers_iter();
        let b1 = it.next().expect("first item").expect("ok");
        assert_eq!(b1.data()[0], 0.1);
        let b2 = it.next().expect("second item").expect("ok");
        assert_eq!(b2.data()[0], 0.2);
        // Queue now empty but stream still running → the iterator is retrying on
        // Ok(None). Stop the stream from another thread so next() observes
        // !is_running and terminates rather than spinning forever.
        let mock2 = Arc::clone(&mock);
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            mock2.signal_stop();
        });
        assert!(it.next().is_none(), "iterator must end once the stream stops");
        stopper.join().unwrap();
    }

    /// The iterator ends (returns None) when the capture is not running and there
    /// is no stream, rather than panicking or looping.
    #[test]
    fn buffers_iter_ends_when_not_running() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.signal_stop();
        let mut capture = make_mock_capture(mock);
        let mut it = capture.buffers_iter();
        assert!(it.next().is_none());
    }

    // ── callback delivery tests (H1 / ADR-0002) ──────────────────────

    /// Regression (audit H1): a registered callback must actually be invoked.
    /// We drive the pump helper directly against a mock stream and assert the
    /// closure observes the pushed buffers, then that clearing the callback
    /// stops delivery.
    #[test]
    fn callback_pump_invokes_registered_callback() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let mock = Arc::new(MockCapturingStream::new());
        mock.push_buffer(AudioBuffer::new(vec![0.5; 4], 2, 48000));
        mock.push_buffer(AudioBuffer::new(vec![0.6; 4], 2, 48000));

        let seen = Arc::new(AtomicU64::new(0));
        let seen_cb = Arc::clone(&seen);
        // The pump now OWNS the callback (moved in), so no shared mutex.
        let callback: AudioCallback = Box::new(move |buf: &AudioBuffer| {
            // Encode the first sample (scaled) so we can assert we saw real data.
            seen_cb.fetch_add((buf.data()[0] * 10.0) as u64, Ordering::SeqCst);
        });

        let stream: Arc<dyn CapturingStream> = mock.clone();
        let mut pump = AudioCapture::spawn_callback_pump(stream, callback).expect("pump spawns");

        // Wait until both buffers (0.5*10 + 0.6*10 = 5 + 6 = 11) are delivered.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while seen.load(Ordering::SeqCst) < 11 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(
            seen.load(Ordering::SeqCst),
            11,
            "callback should have been invoked with both buffers"
        );

        // Shut the pump down → it stops consuming; further pushes are not seen.
        pump.shutdown();
        mock.push_buffer(AudioBuffer::new(vec![9.9; 4], 2, 48000));
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(
            seen.load(Ordering::SeqCst),
            11,
            "no further delivery after pump shutdown"
        );
        mock.signal_stop();
    }

    /// The pump thread exits when the stream stops (try_read_chunk → Err), and
    /// shutdown() is safe to call afterwards (idempotent join).
    #[test]
    fn callback_pump_exits_when_stream_stops() {
        let mock = Arc::new(MockCapturingStream::new());
        let callback: AudioCallback = Box::new(|_: &AudioBuffer| {});
        let stream: Arc<dyn CapturingStream> = mock.clone();
        let mut pump = AudioCapture::spawn_callback_pump(stream, callback).expect("pump spawns");
        // Stopping the mock makes try_read_chunk return Err → pump breaks.
        mock.signal_stop();
        std::thread::sleep(std::time::Duration::from_millis(20));
        // Joining a pump whose thread already exited must not hang or panic.
        pump.shutdown();
    }

    /// Regression (wave-1 review R1-#3): a callback that re-enters the capture
    /// handle must not deadlock. Here the callback increments a counter and, on
    /// the first invocation, flips a flag — proving the pump holds no lock across
    /// the user closure (the closure could otherwise not run arbitrary code).
    #[test]
    fn callback_pump_holds_no_lock_during_invocation() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let mock = Arc::new(MockCapturingStream::new());
        for _ in 0..3 {
            mock.push_buffer(AudioBuffer::new(vec![1.0; 2], 2, 48000));
        }
        let count = Arc::new(AtomicU64::new(0));
        let count_cb = Arc::clone(&count);
        // The closure does real work (sleep) to widen any lock-held window; if
        // the pump held a lock across this, a concurrent shutdown join would
        // stall. We assert delivery proceeds and shutdown completes promptly.
        let callback: AudioCallback = Box::new(move |_buf: &AudioBuffer| {
            count_cb.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(1));
        });
        let stream: Arc<dyn CapturingStream> = mock.clone();
        let mut pump = AudioCapture::spawn_callback_pump(stream, callback).expect("pump");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while count.load(Ordering::SeqCst) < 3 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(count.load(Ordering::SeqCst), 3, "all three buffers delivered");
        pump.shutdown();
        mock.signal_stop();
    }
}
