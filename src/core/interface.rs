// src/core/interface.rs

use super::config::{AudioFormat, StreamConfig};
use super::error::{AudioError, Result as AudioResult}; // Renamed to avoid conflict

/// Represents the kind of an audio device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    /// An input device, typically used for recording audio.
    Input,
    /// An output device, typically used for playing audio.
    Output,
}

// AudioFormat struct has been moved to src/core/config.rs

/// A trait representing an audio device.
///
/// This trait provides a platform-agnostic way to query information
/// and capabilities of an audio input or output device.
pub trait AudioDevice {
    /// The type used to uniquely identify an audio device.
    ///
    /// This ID should be stable for a given device on the system,
    /// but its specific format may vary between platforms.
    type DeviceId: Clone + PartialEq + Eq + std::hash::Hash + std::fmt::Debug + Send + Sync;

    /// Returns a unique identifier for the audio device.
    ///
    /// This identifier can be used to select a specific device or
    /// to compare devices.
    fn get_id(&self) -> Self::DeviceId;

    /// Returns a human-readable name for the audio device.
    ///
    /// This name is typically provided by the operating system and
    /// can be displayed to the user.
    fn get_name(&self) -> String;

    /// Returns a list of audio formats supported by this device.
    ///
    /// The list may vary depending on whether the device is an input
    /// or output device and its specific capabilities.
    fn get_supported_formats(&self) -> AudioResult<Vec<AudioFormat>>;

    /// Returns the default audio format for this device.
    ///
    /// This is the format the system typically uses for this device
    /// if no specific format is requested.
    fn get_default_format(&self) -> AudioResult<AudioFormat>;

    /// Returns `true` if the device is an input device (e.g., microphone).
    fn is_input(&self) -> bool;

    /// Returns `true` if the device is an output device (e.g., speakers).
    fn is_output(&self) -> bool;

    /// Returns `true` if the device is currently active or in use by the system
    /// or an application.
    ///
    /// Note: The definition of "active" can vary by platform and audio backend.
    /// It might indicate if the device is the system default, currently streaming,
    /// or simply enabled.
    fn is_active(&self) -> bool;

    /// Checks if the device supports the given audio format.
    ///
    /// # Parameters
    /// * `format`: A reference to the `AudioFormat` to check.
    ///
    /// # Returns
    /// `Ok(true)` if the format is supported, `Ok(false)` if not,
    /// or an `AudioError` if the check fails or is not possible.
    fn is_format_supported(&self, format: &AudioFormat) -> AudioResult<bool>;

    /// Creates a new audio stream associated with this device.
    ///
    /// # Parameters
    /// * `config`: The desired `StreamConfig` for the new stream.
    ///
    /// # Returns
    /// A `Result` containing a boxed `AudioStream` trait object, or an `AudioError`.
    /// The `AudioStream`'s `Device` associated type will be `dyn AudioDevice`,
    /// and its `Config` associated type will be `StreamConfig`.
    fn create_stream(
        &self,
        config: StreamConfig,
    ) -> AudioResult<Box<dyn CapturingStream + 'static>>;
}

/// A simplified trait for a type-erased audio stream focused on capture lifecycle.
///
/// This trait is intended to be returned by `AudioDevice::create_stream` as a boxed trait object,
/// abstracting away the specific `AudioStream` implementation details, including its
/// associated `Device` type, which can be problematic for trait objects.
pub trait CapturingStream: Send + Sync {
    /// Starts or resumes processing audio data on the stream.
    fn start(&mut self) -> AudioResult<()>;

    /// Stops audio processing on the stream.
    fn stop(&mut self) -> AudioResult<()>;

    /// Closes the audio stream, releasing all associated system resources.
    fn close(&mut self) -> AudioResult<()>;

    /// Checks if the stream is currently running (capturing audio).
    fn is_running(&self) -> bool;

    /// Reads a chunk of audio data from the stream synchronously.
    ///
    /// This method will block until a chunk of audio data is available,
    /// a timeout occurs, or an error happens.
    ///
    /// # Parameters
    /// * `timeout_ms`: An optional timeout in milliseconds.
    ///   - If `Some(duration)`, the method will wait for at most `duration` milliseconds.
    ///   - If `None`, the method may block indefinitely until data is available or an error occurs,
    ///     depending on the backend implementation. Some backends might have an implicit default timeout.
    ///
    /// # Returns
    /// * `Ok(Some(buffer))`: If a chunk of audio data was successfully read. The `buffer`
    ///   is a `Box<dyn AudioBuffer>` containing the audio samples.
    /// * `Ok(None)`: If the timeout occurred before any data was available. This is only
    ///   returned if `timeout_ms` was `Some`. If `timeout_ms` was `None` and the backend
    ///   waits indefinitely, this variant typically wouldn't be returned unless the stream
    ///   is stopped or closed.
    /// * `Err(AudioError)`: If an error occurred during the read operation (e.g., stream
    ///   not started, device disconnected, internal backend error).
    ///
    /// # Example
    /// ```rust,ignore
    /// // Assuming `stream` is a mutable reference to a type implementing `CapturingStream`
    /// // and `AudioBuffer` is a trait for audio data.
    /// match stream.read_chunk(Some(100)) { // Timeout after 100ms
    ///     Ok(Some(audio_buffer)) => {
    ///         println!("Read {} frames of audio.", audio_buffer.get_length_frames());
    ///         // Process the audio_buffer...
    ///     }
    ///     Ok(None) => {
    ///         println!("Timeout: No audio data received within 100ms.");
    ///     }
    ///     Err(e) => {
    ///         eprintln!("Error reading audio chunk: {:?}", e);
    ///     }
    /// }
    /// ```
    fn read_chunk(
        &mut self,
        timeout_ms: Option<u32>,
    ) -> AudioResult<Option<Box<dyn AudioBuffer<Sample = f32>>>>;

    /// Converts the synchronous capturing stream into an asynchronous stream.
    ///
    /// This method allows consuming audio data using asynchronous patterns,
    /// integrating with Rust's async ecosystem (e.g., `tokio`, `async-std`).
    /// The returned stream will yield `AudioResult<Box<dyn AudioBuffer<Sample = f32>>>` items.
    ///
    /// The lifetime `'a` ties the returned stream to the lifetime of the `CapturingStream` instance.
    /// The stream items are `AudioResult` to allow for error propagation from the underlying
    /// audio capture mechanism. Each successful item is a `Box<dyn AudioBuffer<Sample = f32>>`.
    ///
    /// # Returns
    /// An `AudioResult` containing a pinned, boxed, dynamic `futures_core::Stream`.
    /// The stream yields `AudioResult<Box<dyn AudioBuffer<Sample = f32>>>`.
    /// Returns an `AudioError` if the asynchronous stream cannot be created.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use futures_util::stream::StreamExt; // For `next()`
    ///
    /// async fn process_audio(mut capturing_stream: Box<dyn CapturingStream>) {
    ///     match capturing_stream.to_async_stream() {
    ///         Ok(mut async_stream) => {
    ///             println!("Successfully created async audio stream. Waiting for data...");
    ///             while let Some(audio_result) = async_stream.next().await {
    ///                 match audio_result {
    ///                     Ok(audio_buffer) => {
    ///                         println!(
    ///                             "Async: Received audio buffer with {} frames, format: {:?}",
    ///                             audio_buffer.get_length_frames(),
    ///                             audio_buffer.get_format()
    ///                         );
    ///                         // Process the audio_buffer...
    ///                     }
    ///                     Err(e) => {
    ///                         eprintln!("Error receiving audio data from async stream: {:?}", e);
    ///                         // Optionally, break or handle the error
    ///                     }
    ///                 }
    ///             }
    ///             println!("Async audio stream finished.");
    ///         }
    ///         Err(e) => {
    ///             eprintln!("Failed to create async audio stream: {:?}", e);
    ///         }
    ///     }
    /// }
    ///
    /// // To run this example, you would typically use an async runtime like tokio:
    /// // #[tokio::main]
    /// // async fn main() {
    /// //     // ... setup code to get a `capturing_stream` ...
    /// //     let my_capturing_stream: Box<dyn CapturingStream> = ...;
    /// //     process_audio(my_capturing_stream).await;
    /// // }
    /// ```
    fn to_async_stream<'a>(
        &'a mut self,
    ) -> AudioResult<
        std::pin::Pin<
            Box<
                dyn futures_core::Stream<Item = AudioResult<Box<dyn AudioBuffer<Sample = f32>>>>
                    + Send
                    + Sync
                    + 'a,
            >,
        >,
    >;

    // /// Sets the callback function that will be invoked with audio data.
    // /// (To be implemented/used in later subtasks)
    // fn set_callback(&mut self, callback: StreamDataCallback) -> AudioResult<()>;

    // /// Gets the actual `AudioFormat` the stream is currently using.
    // /// (To be implemented/used in later subtasks)
    // fn get_current_format(&self) -> AudioResult<AudioFormat>;

    // Note: CapturingStream intentionally does not have associated types like Config or Device
    // to make it easily usable as a `Box<dyn CapturingStream>`. The configuration is passed
    // during creation via `AudioDevice::create_stream(config)`.
}

/// A trait for discovering and enumerating audio devices available on the system.
///
/// This trait provides a platform-agnostic way to list devices,
/// retrieve default devices, and find specific devices by their ID.
pub trait DeviceEnumerator {
    /// The concrete type of `AudioDevice` that this enumerator provides.
    type Device: AudioDevice;

    /// Lists all available audio devices on the system.
    ///
    /// This includes both input and output devices.
    ///
    /// # Returns
    /// A `Result` containing a vector of devices, or an `AudioError` if enumeration fails.
    fn enumerate_devices(&self) -> AudioResult<Vec<Self::Device>>;

    /// Gets the default audio device of the specified kind (input or output).
    ///
    /// # Parameters
    /// * `kind`: The [`DeviceKind`] (Input or Output) of the default device to retrieve.
    ///
    /// # Returns
    /// A `Result` containing the default device, or an `AudioError` if it cannot be determined
    /// or no default device of that kind exists.
    fn get_default_device(&self, kind: DeviceKind) -> AudioResult<Self::Device>;

    /// Lists all available audio input devices (e.g., microphones).
    ///
    /// # Returns
    /// A `Result` containing a vector of input devices, or an `AudioError` if enumeration fails.
    fn get_input_devices(&self) -> AudioResult<Vec<Self::Device>>;

    /// Lists all available audio output devices (e.g., speakers, headphones).
    ///
    /// # Returns
    /// A `Result` containing a vector of output devices, or an `AudioError` if enumeration fails.
    fn get_output_devices(&self) -> AudioResult<Vec<Self::Device>>;

    /// Retrieves a specific audio device by its unique identifier.
    ///
    /// # Parameters
    /// * `id`: A reference to the [`AudioDevice::DeviceId`] of the device to retrieve.
    ///
    /// # Returns
    /// A `Result` containing the device if found, or an `AudioError` if no device
    /// with the given ID exists or an error occurs.
    fn get_device_by_id(
        &self,
        id: &<Self::Device as AudioDevice>::DeviceId,
    ) -> AudioResult<Self::Device>;
}

// AudioError enum has been moved to src/core/error.rs
// StreamConfig struct has been moved to src/core/config.rs

/// A callback function type for processing audio data from a stream.
///
/// The callback receives a slice of raw audio data and the format of that data.
/// It should return `Ok(())` on success, or an `AudioError` if processing fails.
pub type StreamDataCallback = Box<dyn FnMut(&[u8], &AudioFormat) -> AudioResult<()> + Send + Sync>;

/// A trait representing an audio stream for capturing or playing audio.
///
/// This trait provides methods to manage the lifecycle (open, start, stop, close),
/// configure (format, callback), and inspect the status of an audio stream.
/// It is designed to work in conjunction with an `AudioDevice`.
pub trait AudioStream {
    /// The type representing the configuration for this stream.
    /// This should typically be `StreamConfig` or a compatible type.
    type Config: Clone + std::fmt::Debug + Send + Sync;

    /// The type of the audio device this stream is associated with.
    /// This ensures that the stream is opened with a compatible device.
    type Device: AudioDevice;

    /// Opens the audio stream on the specified device with the given configuration.
    ///
    /// # Parameters
    /// * `device`: A reference to the `AudioDevice` to open the stream on.
    /// * `config`: The desired configuration for the stream.
    ///
    /// # Returns
    /// `Ok(())` if the stream was opened successfully, or an `AudioError` otherwise.
    fn open(&mut self, device: &Self::Device, config: Self::Config) -> AudioResult<()>;

    /// Starts or resumes processing audio data on the stream.
    ///
    /// If the stream was previously paused, this will resume it. If it was stopped
    /// or newly opened, this will begin capturing/playback.
    ///
    /// # Returns
    /// `Ok(())` if the stream started successfully, or an `AudioError` otherwise.
    fn start(&mut self) -> AudioResult<()>;

    /// Pauses audio processing on the stream.
    ///
    /// Data already in buffers might still be processed or played out.
    /// The stream can be resumed using `start()` or `resume()`.
    ///
    /// # Returns
    /// `Ok(())` if the stream paused successfully, or an `AudioError` otherwise.
    fn pause(&mut self) -> AudioResult<()>;

    /// Resumes a paused audio stream.
    ///
    /// This is typically an alias for `start()` if the stream supports a distinct
    /// pause/resume state, otherwise it behaves identically to `start()`.
    ///
    /// # Returns
    /// `Ok(())` if the stream resumed successfully, or an `AudioError` otherwise.
    fn resume(&mut self) -> AudioResult<()>;

    /// Stops audio processing on the stream.
    ///
    /// This typically clears any buffered data and releases resources associated
    /// with active streaming. The stream may need to be re-opened or re-configured
    /// after stopping, depending on the backend.
    ///
    /// # Returns
    /// `Ok(())` if the stream stopped successfully, or an `AudioError` otherwise.
    fn stop(&mut self) -> AudioResult<()>;

    /// Closes the audio stream, releasing all associated system resources.
    ///
    /// After closing, the stream object should generally not be used further
    /// unless re-opened.
    ///
    /// # Returns
    /// `Ok(())` if the stream closed successfully, or an `AudioError` otherwise.
    fn close(&mut self) -> AudioResult<()>;

    /// Sets the audio format for the stream.
    ///
    /// This might only be possible when the stream is not running, depending on
    /// the backend implementation.
    ///
    /// # Parameters
    /// * `format`: The desired `AudioFormat`.
    ///
    /// # Returns
    /// `Ok(())` if the format was set successfully, or an `AudioError` if the format
    /// is not supported or the stream is in an invalid state.
    fn set_format(&mut self, format: &AudioFormat) -> AudioResult<()>;

    /// Sets the callback function that will be invoked with audio data.
    ///
    /// For capture streams, the callback receives chunks of recorded audio data.
    /// For playback streams (if this trait is extended for playback), the callback
    /// would be used to request data to play.
    ///
    /// # Parameters
    /// * `callback`: A `StreamDataCallback` function.
    ///
    /// # Returns
    /// `Ok(())` if the callback was set successfully, or an `AudioError` otherwise.
    fn set_callback(&mut self, callback: StreamDataCallback) -> AudioResult<()>;

    /// Checks if the stream is currently running (capturing or playing audio).
    ///
    /// # Returns
    /// `true` if the stream is active, `false` otherwise.
    fn is_running(&self) -> bool;

    /// Gets the current latency of the stream, if available.
    ///
    /// Latency can be reported in various units (e.g., frames, milliseconds).
    /// The exact meaning and availability depend on the backend.
    ///
    /// # Returns
    /// `Ok(u64)` with the latency value (e.g. in frames or microseconds), or an `AudioError` if latency
    /// cannot be determined or is not applicable.
    fn get_latency_frames(&self) -> AudioResult<u64>;

    /// Gets the actual `AudioFormat` the stream is currently using.
    /// This may differ from a requested format if the backend had to adjust it.
    ///
    /// # Returns
    /// `Ok(AudioFormat)` with the current format, or an `AudioError` if not available.
    fn get_current_format(&self) -> AudioResult<AudioFormat>;
}

/// Represents the type of a single audio sample.
/// This enum would be expanded to include common sample types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleType {
    /// Signed 16-bit integer.
    S16,
    /// Signed 32-bit integer.
    S32,
    /// 32-bit floating point.
    F32,
    /// Unsigned 8-bit integer.
    U8,
    // Add other common formats like U16, S24, F64 etc.
}

/// A trait for handling and manipulating buffers of audio data.
///
/// This trait provides methods for accessing, modifying, and converting
/// audio data within a buffer.
pub trait AudioBuffer {
    /// The type of samples stored in this buffer (e.g., `f32`, `i16`).
    type Sample: Clone + Copy + std::fmt::Debug + Send + Sync; // Should align with SampleType

    /// Returns a slice providing read-only access to the raw sample data.
    ///
    /// # Returns
    /// A slice of `Self::Sample` representing the buffer's content.
    fn as_slice(&self) -> &[Self::Sample];

    /// Returns a mutable slice providing read-write access to the raw sample data.
    ///
    /// # Returns
    /// A mutable slice of `Self::Sample` representing the buffer's content.
    fn as_mut_slice(&mut self) -> &mut [Self::Sample];

    /// Reads data from the buffer into a provided slice.
    ///
    /// # Parameters
    /// * `offset_frames`: The starting frame offset within this `AudioBuffer` to read from.
    /// * `destination`: The slice to read the audio frames into.
    /// * `frames_to_read`: The number of frames to read.
    ///
    /// # Returns
    /// The number of frames actually read, or an `AudioError` if the read operation failed
    /// (e.g., out of bounds). The number of frames read can be less than `frames_to_read`
    /// if the end of the buffer is reached.
    fn read_frames(
        &self,
        offset_frames: usize,
        destination: &mut [Self::Sample],
        frames_to_read: usize,
    ) -> AudioResult<usize>;

    /// Writes data from a slice into the buffer.
    ///
    /// # Parameters
    /// * `offset_frames`: The starting frame offset within this `AudioBuffer` to write to.
    /// * `source`: The slice containing audio frames to write into the buffer.
    /// * `frames_to_write`: The number of frames to write from the source.
    ///
    /// # Returns
    /// The number of frames actually written, or an `AudioError` if the write operation failed
    /// (e.g., buffer overflow, out of bounds). The number of frames written can be less than
    /// `frames_to_write` if the buffer's capacity is reached.
    fn write_frames(
        &mut self,
        offset_frames: usize,
        source: &[Self::Sample],
        frames_to_write: usize,
    ) -> AudioResult<usize>;

    /// Returns the current number of valid audio frames in the buffer.
    /// This is the amount of data that has been written or is considered "filled".
    fn get_length_frames(&self) -> usize;

    /// Returns the total capacity of the buffer in audio frames.
    /// This is the maximum amount of data the buffer can hold.
    fn get_capacity_frames(&self) -> usize;

    /// Returns the `AudioFormat` of the data stored in this buffer.
    fn get_format(&self) -> AudioFormat;

    /// Converts the audio data in this buffer to a different `AudioFormat`.
    ///
    /// This operation might create a new buffer or modify the existing one in place,
    /// depending on the implementation.
    ///
    /// # Parameters
    /// * `target_format`: The `AudioFormat` to convert the data to.
    ///
    /// # Returns
    /// A new `AudioBuffer` (or a modified self, TBD by implementor choice) containing
    /// the converted data, or an `AudioError` if conversion failed.
    /// For simplicity, let's assume it returns a new buffer for now.
    fn convert_to_format(
        &self,
        target_format: &AudioFormat,
    ) -> AudioResult<Box<dyn AudioBuffer<Sample = Self::Sample>>>;
    // TODO: The Sample type might need to change based on target_format, making this more complex.
    // For now, Self::Sample is kept, implying conversion between sample rates/channels but not bit depth/type.
    // A more robust solution might involve an associated type for the converted buffer or generic parameters.

    /// Clears the buffer, effectively setting its length to zero.
    /// The capacity remains unchanged. Data may or may not be zeroed out.
    fn clear(&mut self);

    /// Resizes the buffer's current length.
    ///
    /// If the new length is greater than the current length, the new elements
    /// are uninitialized (or filled with a default value like zero, depending on implementation).
    /// If the new length is greater than capacity, this might reallocate or return an error.
    /// For simplicity, let's assume it can fail if new_length_frames > capacity.
    ///
    /// # Parameters
    /// * `new_length_frames`: The new length of the buffer in frames.
    ///
    /// # Returns
    /// `Ok(())` if successful, or an `AudioError` if resizing failed (e.g. new length exceeds capacity).
    fn resize_length(&mut self, new_length_frames: usize) -> AudioResult<()>;

    // Potentially add methods for changing capacity if buffers are dynamically sizable.
    // fn set_capacity_frames(&mut self, new_capacity_frames: usize) -> Result<(), AudioError>;
}
