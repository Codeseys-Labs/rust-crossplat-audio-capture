use crate::audio::get_device_enumerator;
use crate::core::buffer::AudioBuffer;
use crate::core::capabilities::PlatformCapabilities;
use crate::core::config::{CaptureTarget, SampleFormat, StreamConfig};
use crate::core::error::{AudioError, AudioResult};
use crate::core::interface::DeviceKind;
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
            CaptureTarget::Application(_) | CaptureTarget::ApplicationByName(_) => {
                if !caps.supports_application_capture {
                    return Err(AudioError::PlatformNotSupported {
                        feature: "application capture".to_string(),
                        platform: caps.backend_name.to_string(),
                    });
                }
            }
            CaptureTarget::ProcessTree(_) => {
                if !caps.supports_process_tree_capture {
                    return Err(AudioError::PlatformNotSupported {
                        feature: "process tree capture".to_string(),
                        platform: caps.backend_name.to_string(),
                    });
                }
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
        let capture_config = AudioCaptureConfig {
            target: self.target,
            stream_config: self.config,
        };

        // ── Resolve device from target ──────────────────────────────
        let enumerator = get_device_enumerator()?;

        let selected_device = match &capture_config.target {
            CaptureTarget::SystemDefault => {
                // Default to input device for system default
                enumerator
                    .get_default_device(DeviceKind::Input)
                    .map_err(|e| AudioError::DeviceEnumerationError {
                        reason: format!("Failed to get default input device: {}", e),
                        context: None,
                    })?
            }
            CaptureTarget::Device(device_id) => {
                let devices = enumerator.enumerate_devices()?;
                devices
                    .into_iter()
                    .find(|d| d.id() == *device_id)
                    .ok_or_else(|| AudioError::DeviceNotFound {
                        device_id: device_id.0.clone(),
                    })?
            }
            CaptureTarget::Application(_)
            | CaptureTarget::ApplicationByName(_)
            | CaptureTarget::ProcessTree(_) => {
                // Application capture typically uses the default output device
                enumerator
                    .get_default_device(DeviceKind::Output)
                    .map_err(|e| AudioError::DeviceEnumerationError {
                        reason: format!(
                            "Failed to get default output device for app capture: {}",
                            e
                        ),
                        context: None,
                    })?
            }
        };

        // ── Format validation (non-Linux) ───────────────────────────
        #[cfg(not(target_os = "linux"))]
        {
            let audio_format = capture_config.stream_config.to_audio_format();
            let supported = selected_device.supported_formats();
            if !supported.is_empty() && !supported.contains(&audio_format) {
                return Err(AudioError::UnsupportedFormat {
                    format: format!(
                        "The selected device '{}' does not support the requested audio format: {:?}",
                        selected_device.name(),
                        audio_format
                    ),
                    context: None,
                });
            }
        }

        Ok(AudioCapture {
            config: capture_config,
            device: Some(selected_device),
            stream: None,
            is_running: AtomicBool::new(false),
            callback: Arc::new(Mutex::new(None)),
        })
    }
}

/// Represents an active audio capture session.
///
/// Created via [`AudioCaptureBuilder::build()`]. Provides methods to start/stop
/// audio capture and read audio data via a pull-based streaming model.
pub struct AudioCapture {
    config: AudioCaptureConfig,
    device: Option<Box<dyn crate::core::interface::AudioDevice>>,
    stream: Option<Arc<dyn crate::core::interface::CapturingStream + 'static>>,
    is_running: AtomicBool,
    #[allow(clippy::type_complexity)]
    callback: Arc<Mutex<Option<Box<dyn FnMut(&AudioBuffer) + Send + 'static>>>>,
}

impl AudioCapture {
    /// Starts the audio capture stream.
    ///
    /// Creates the underlying OS stream (if not already created) and marks
    /// the capture as running. In the new `CapturingStream` contract, the
    /// stream starts producing data upon creation.
    pub fn start(&mut self) -> AudioResult<()> {
        if self.is_running.load(Ordering::SeqCst) {
            return Ok(());
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
        let _stream_ref = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamCreationFailed {
                reason: "Stream not initialized before starting.".to_string(),
                context: None,
            })?;

        self.is_running.store(true, Ordering::SeqCst);
        Ok(())
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
        if !self.is_running.load(Ordering::SeqCst) {
            return Ok(());
        }

        if let Some(stream) = self.stream.as_ref() {
            if let Err(e) = stream.stop() {
                eprintln!("Error stopping stream: {:?}", e);
            }
        }
        // Drop our Arc reference. The stream will be fully deallocated once all
        // subscriber threads also drop their clones.
        self.stream.take();

        self.is_running.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Returns `true` if the stream is currently capturing.
    pub fn is_running(&self) -> bool {
        self.is_running.load(Ordering::SeqCst)
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
        if !self.is_running.load(Ordering::SeqCst) {
            return Err(AudioError::StreamReadError {
                reason: "Stream is not running. Call start() first.".to_string(),
            });
        }
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: "Stream is not initialized, though is_running was true.".to_string(),
            })?;
        stream.try_read_chunk()
    }

    /// Reads a buffer of audio data, blocking until data is available.
    ///
    /// Uses `CapturingStream::read_chunk` which blocks until data arrives.
    pub fn read_buffer_blocking(&mut self) -> AudioResult<AudioBuffer> {
        if !self.is_running.load(Ordering::SeqCst) {
            return Err(AudioError::StreamReadError {
                reason: "Stream is not running. Call start() first.".to_string(),
            });
        }
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: "Stream is not initialized, though is_running was true.".to_string(),
            })?;
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
    pub fn clear_callback(&mut self) -> AudioResult<()> {
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
    /// buffers over an [`mpsc`](std::sync::mpsc) channel. Returns the receiving
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
        if !self.is_running.load(Ordering::SeqCst) {
            return Err(AudioError::StreamReadError {
                reason: "Stream is not running. Call start() first.".to_string(),
            });
        }

        let stream =
            Arc::clone(
                self.stream
                    .as_ref()
                    .ok_or_else(|| AudioError::StreamReadError {
                        reason: "Stream is not initialized.".to_string(),
                    })?,
            );

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
        if !self.capture.is_running() {
            return None;
        }
        match self.capture.read_buffer() {
            Ok(Some(buffer)) => Some(Ok(buffer)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

// ── Drop ─────────────────────────────────────────────────────────────────

impl Drop for AudioCapture {
    fn drop(&mut self) {
        if self.is_running.load(Ordering::SeqCst) {
            if let Err(e) = self.stop() {
                eprintln!("Error stopping audio stream during drop: {:?}", e);
            }
        } else if let Some(stream) = self.stream.as_ref() {
            // Best-effort stop; the Arc will be dropped when all references are gone.
            if let Err(e) = stream.stop() {
                eprintln!("Error stopping audio stream during drop: {:?}", e);
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
            .field("is_running", &self.is_running.load(Ordering::SeqCst))
            .field("callback_is_some", &self.callback.lock().unwrap().is_some())
            .finish()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{AudioFormat, SampleFormat};
    use crate::core::interface::CapturingStream;
    use std::sync::atomic::AtomicU64;

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
            is_running: AtomicBool::new(true),
            callback: Arc::new(Mutex::new(None)),
        }
    }

    // ── subscribe() tests ─────────────────────────────────────────────

    #[test]
    fn subscribe_returns_error_when_not_running() {
        let mock = Arc::new(MockCapturingStream::new());
        let capture = make_mock_capture(mock);
        capture.is_running.store(false, Ordering::SeqCst);

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
            is_running: AtomicBool::new(false),
            callback: Arc::new(Mutex::new(None)),
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
}
