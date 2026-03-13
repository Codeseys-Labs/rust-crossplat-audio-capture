// src/core/interface.rs

use super::config::{AudioFormat, DeviceId, StreamConfig};
use super::error::AudioResult;
use crate::core::buffer::AudioBuffer;

/// Represents the kind of an audio device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    /// An input device, typically used for recording audio.
    Input,
    /// An output device, typically used for playing audio.
    Output,
}

/// A trait representing an audio device.
///
/// Provides a platform-agnostic interface to query device information
/// and create audio capture streams from the device.
///
/// All implementations must be `Send + Sync`.
///
/// # Example
///
/// ```rust,ignore
/// let device = enumerator.default_device()?;
/// println!("Device: {} (ID: {})", device.name(), device.id());
///
/// let stream = device.create_stream(&StreamConfig::default())?;
/// ```
pub trait AudioDevice: Send + Sync {
    /// Returns the unique platform-specific identifier for this device.
    fn id(&self) -> DeviceId;

    /// Returns the human-readable name for this device.
    fn name(&self) -> String;

    /// Returns `true` if this device is the system default.
    fn is_default(&self) -> bool;

    /// Returns the audio formats supported by this device.
    fn supported_formats(&self) -> Vec<AudioFormat>;

    /// Creates a new capturing stream from this device using the given configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the stream cannot be created with the given configuration,
    /// for example if the device does not support the requested format or if the
    /// device is busy.
    fn create_stream(&self, config: &StreamConfig) -> AudioResult<Box<dyn CapturingStream>>;
}

/// The core trait for reading captured audio data.
///
/// `CapturingStream` is the bridge between OS audio callbacks and the user.
/// It is implemented by platform-specific backends and exposed via `AudioCapture`.
///
/// All implementations must be `Send + Sync`.
///
/// # Consumption Model
///
/// The stream operates on a **pull model**: the consumer calls [`read_chunk()`](Self::read_chunk)
/// or [`try_read_chunk()`](Self::try_read_chunk) to retrieve audio data. Internally, the OS
/// pushes audio into a lock-free SPSC ring buffer; these methods read from the consumer side.
///
/// # Example
///
/// ```rust,ignore
/// // Blocking read loop
/// while stream.is_running() {
///     let buffer = stream.read_chunk()?;
///     process_audio(&buffer);
/// }
/// stream.stop()?;
/// ```
pub trait CapturingStream: Send + Sync {
    /// Reads the next chunk of audio data, blocking until data is available.
    ///
    /// This is the primary method for consuming audio. It blocks the calling
    /// thread until at least one buffer of audio data is available from the
    /// ring buffer.
    ///
    /// # Returns
    ///
    /// * `Ok(buffer)` — Audio data is available.
    /// * `Err(AudioError::StreamClosed)` — Stream was closed.
    /// * `Err(AudioError::BufferOverrun { .. })` — Data was lost due to slow consumption.
    fn read_chunk(&self) -> AudioResult<AudioBuffer>;

    /// Attempts to read audio data without blocking.
    ///
    /// Returns immediately with whatever data is available, or `None` if
    /// no data is currently buffered in the ring buffer.
    ///
    /// # Returns
    ///
    /// * `Ok(Some(buffer))` — Data was available.
    /// * `Ok(None)` — No data currently available (try again later).
    /// * `Err(...)` — Stream error.
    fn try_read_chunk(&self) -> AudioResult<Option<AudioBuffer>>;

    /// Stops the audio stream. OS audio callbacks are halted.
    ///
    /// The ring buffer retains any unread data. After stopping, the stream
    /// cannot be restarted — create a new stream instead.
    fn stop(&self) -> AudioResult<()>;

    /// Returns the actual audio format being produced by the stream.
    ///
    /// This may differ from the requested format if the backend negotiated
    /// a different format with the OS.
    fn format(&self) -> AudioFormat;

    /// Returns `true` if the stream is currently capturing audio.
    fn is_running(&self) -> bool;

    /// Closes the stream and releases all OS resources.
    ///
    /// After `close()`, the stream cannot be restarted. Any subsequent
    /// method calls should return `AudioError::StreamClosed`.
    ///
    /// The default implementation calls [`stop()`](Self::stop) and returns `Ok(())`.
    fn close(self: Box<Self>) -> AudioResult<()> {
        self.stop()?;
        Ok(())
    }

    /// Register an async waker to be notified when new audio data is available.
    ///
    /// Returns `true` if the stream supports async notification, `false` otherwise.
    /// Used internally by `AsyncAudioStream`.
    #[cfg(feature = "async-stream")]
    fn register_waker(&self, waker: &std::task::Waker) -> bool {
        let _ = waker;
        false
    }

    /// Returns `true` if the stream's producer is still active and may produce more data.
    ///
    /// Returns `false` once the producer has signaled completion.
    /// Used internally by `AsyncAudioStream` to determine when to return `None`.
    #[cfg(feature = "async-stream")]
    fn is_stream_producing(&self) -> bool {
        true
    }
}

/// A trait for discovering and enumerating audio devices on the system.
///
/// Platform backends implement this trait to provide device discovery.
/// The user obtains an implementation via a platform-specific factory
/// function or [`get_device_enumerator()`](crate::audio::get_device_enumerator).
///
/// All implementations must be `Send + Sync`.
///
/// # Example
///
/// ```rust,ignore
/// let enumerator = get_device_enumerator()?;
/// for device in enumerator.enumerate_devices()? {
///     println!("{}: {}", device.id(), device.name());
/// }
/// let default = enumerator.default_device()?;
/// ```
pub trait DeviceEnumerator: Send + Sync {
    /// Lists all available audio devices on the system.
    ///
    /// This includes both input and output devices.
    ///
    /// # Returns
    ///
    /// A `Result` containing a vector of boxed audio devices, or an error
    /// if enumeration fails.
    fn enumerate_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice>>>;

    /// Returns the default audio device for the system.
    ///
    /// The choice of which device to return (input vs output) is
    /// platform-specific. For audio capture scenarios, this typically
    /// returns the default output/loopback device.
    ///
    /// # Returns
    ///
    /// A `Result` containing the default device, or an error if no
    /// default device exists or it cannot be determined.
    fn default_device(&self) -> AudioResult<Box<dyn AudioDevice>>;
}

// AudioError enum has been moved to src/core/error.rs
// StreamConfig struct has been moved to src/core/config.rs

// The AudioBuffer trait has been removed from this file.
// It is now a concrete struct defined in src/core/buffer.rs.
