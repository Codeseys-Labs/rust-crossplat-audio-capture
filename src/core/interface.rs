// src/core/interface.rs

//! Core traits: [`AudioDevice`], [`DeviceEnumerator`], [`CapturingStream`].
//!
//! These three traits are the platform-agnostic contract every backend
//! implements. Consumers of the library rarely call them directly — the
//! public [`AudioCapture`](crate::api::AudioCapture) facade already wires
//! them together — but they are part of the public surface for advanced
//! integrations (e.g., alternative builders, custom device filters).
//!
//! All implementations must be `Send + Sync`.

use super::config::{AudioFormat, DeviceId, StreamConfig};
use super::error::{AudioError, AudioResult};
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

    /// Returns whether this device is an [`Input`](DeviceKind::Input) or
    /// [`Output`](DeviceKind::Output) endpoint.
    ///
    /// # Platform behaviour
    ///
    /// - **Windows (WASAPI):** resolved from `IMMEndpoint::GetDataFlow`
    ///   (`eRender` → [`Output`](DeviceKind::Output),
    ///   `eCapture` → [`Input`](DeviceKind::Input)).
    /// - **Linux (PipeWire):** maps the node's source/sink role. A device that
    ///   is both a source and a sink (e.g. a monitor) reports
    ///   [`Input`](DeviceKind::Input).
    /// - **macOS (CoreAudio):** probed from the device's stream scope
    ///   (`scopeInput` vs `scopeOutput`).
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::PlatformNotSupported`] from the default
    /// implementation. Backends that cannot determine a definite kind (for
    /// example a CoreAudio device exposing no streams on either scope) return
    /// an error rather than guessing.
    ///
    /// This is a **provided** method so external `AudioDevice` implementations
    /// keep compiling without change; they inherit the
    /// `PlatformNotSupported` default until they choose to override it.
    fn kind(&self) -> AudioResult<DeviceKind> {
        Err(AudioError::PlatformNotSupported {
            feature: "device kind".to_string(),
            platform: std::env::consts::OS.to_string(),
        })
    }

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
/// // Blocking read loop. read_chunk() returns AudioError::StreamEnded (Fatal)
/// // once the stream is terminal, so break on a fatal error.
/// loop {
///     match stream.read_chunk() {
///         Ok(buffer) => process_audio(&buffer),
///         Err(e) if e.is_fatal() => break, // StreamEnded: producer is done
///         Err(_) => continue,              // transient read hiccup; retry
///     }
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
    /// * `Err(`[`AudioError::StreamEnded`]`)`
    ///   — The stream has reached a terminal state (`Stopped` / `Closed` / `Error`)
    ///   and will produce no more data. This is **fatal** for the read loop
    ///   (`is_fatal() == true`); break out of it. As of
    ///   [ADR-0003](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/blob/master/docs/designs/0003-terminal-stream-error.md)
    ///   this — not [`AudioError::StreamReadError`]
    ///   — is the clean end-of-stream signal.
    /// * `Err(`[`AudioError::StreamReadError`]`)`
    ///   — A genuinely transient read failure (recoverable; retrying may succeed).
    ///
    /// # Dropped buffers
    ///
    /// Ring-buffer overflow does **not** surface as an error from this method.
    /// When the consumer cannot keep up, the producer drops buffers and bumps the
    /// [`overrun_count()`](Self::overrun_count) counter; poll that counter (or
    /// [`is_under_backpressure()`](Self::is_under_backpressure)) to detect loss.
    /// (The [`BufferOverrun`](crate::core::error::AudioError::BufferOverrun) and
    /// [`BufferUnderrun`](crate::core::error::AudioError::BufferUnderrun) variants
    /// exist in the taxonomy but are not constructed by the production read path.)
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

    /// Returns the number of audio buffers dropped due to ring buffer overflow.
    ///
    /// A non-zero value indicates the consumer could not keep up with the
    /// producer (OS audio callback). The default implementation returns 0.
    fn overrun_count(&self) -> u64 {
        0
    }

    /// Returns the cumulative number of audio buffers **delivered to the
    /// consumer** (i.e. popped off the ring buffer by `read_chunk()` /
    /// `try_read_chunk()`) since the stream started.
    ///
    /// This is the "successfully captured and handed to the caller" tally,
    /// distinct from [`buffers_pushed()`](Self::buffers_pushed) (what the OS
    /// callback enqueued) and [`buffers_dropped()`](Self::buffers_dropped)
    /// (what was lost to overflow). The default implementation returns 0 for
    /// backends that do not track this counter.
    fn buffers_captured(&self) -> u64 {
        0
    }

    /// Returns the cumulative number of audio buffers **enqueued by the
    /// producer** (the OS audio callback) since the stream started.
    ///
    /// Together with [`buffers_dropped()`](Self::buffers_dropped) this accounts
    /// for everything the OS produced: `pushed + dropped` is the total the
    /// callback attempted to deliver. The default implementation returns 0 for
    /// backends that do not track this counter.
    fn buffers_pushed(&self) -> u64 {
        0
    }

    /// Returns the cumulative number of audio buffers **dropped due to ring
    /// buffer overflow** since the stream started.
    ///
    /// This is an alias of [`overrun_count()`](Self::overrun_count), provided so
    /// the three bridge counters (`buffers_pushed` / `buffers_dropped` /
    /// `buffers_captured`) read uniformly. The default implementation returns 0
    /// for backends that do not track buffer loss.
    fn buffers_dropped(&self) -> u64 {
        0
    }

    /// Returns `true` if the stream's producer is actively producing data.
    ///
    /// This is a convenience alias of [`is_running()`](Self::is_running) for
    /// callers reasoning about producer activity. The default implementation
    /// delegates to `is_running()`.
    fn is_producing(&self) -> bool {
        self.is_running()
    }

    /// Returns true if the stream's producer is experiencing sustained
    /// backpressure (consecutive ring buffer overflows above a threshold).
    ///
    /// Consumers should slow down, warn the user, or switch providers when
    /// this returns true. Default implementation returns false for backends
    /// that don't track backpressure.
    fn is_under_backpressure(&self) -> bool {
        false
    }

    /// Closes the stream and releases all OS resources.
    ///
    /// **Deprecated.** All real cleanup happens in the stream's `Drop` impl
    /// (which itself invokes `stop()` when still running). Call
    /// [`stop()`](Self::stop) explicitly to halt capture early and let the
    /// stream drop normally to release resources.
    ///
    /// The default implementation is a no-op; backends do not need to
    /// override it.
    #[deprecated(
        since = "0.1.0",
        note = "use stop() for explicit shutdown and rely on Drop for resource release"
    )]
    fn close(self: Box<Self>) -> AudioResult<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::error::ErrorKind;

    /// A minimal `AudioDevice` that overrides nothing beyond the required
    /// methods. It must inherit the provided `kind()` default unchanged,
    /// proving the addition is additive/non-breaking for external impls.
    struct MinimalDevice;

    impl AudioDevice for MinimalDevice {
        fn id(&self) -> DeviceId {
            DeviceId("minimal".to_string())
        }
        fn name(&self) -> String {
            "Minimal".to_string()
        }
        fn is_default(&self) -> bool {
            false
        }
        fn supported_formats(&self) -> Vec<AudioFormat> {
            Vec::new()
        }
        fn create_stream(&self, _config: &StreamConfig) -> AudioResult<Box<dyn CapturingStream>> {
            Err(AudioError::PlatformNotSupported {
                feature: "create_stream".to_string(),
                platform: "test".to_string(),
            })
        }
    }

    /// The default `kind()` reports `PlatformNotSupported` (fatal, Platform
    /// kind) without requiring the impl to override it.
    #[test]
    fn default_kind_is_platform_not_supported() {
        let device = MinimalDevice;
        let err = device.kind().expect_err("default kind() must be Err");
        assert_eq!(err.kind(), ErrorKind::Platform);
        match err {
            AudioError::PlatformNotSupported { feature, .. } => {
                assert_eq!(feature, "device kind");
            }
            other => panic!("expected PlatformNotSupported, got {other:?}"),
        }
    }
}
