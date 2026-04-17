//! BridgeStream — universal CapturingStream implementation backed by the ring buffer bridge.
//!
//! `BridgeStream<S>` is the key type that all platform backends use. They create
//! a `BridgeStream` wrapping their platform-specific stream, and consumers interact
//! with it through the [`CapturingStream`] trait.
//!
//! # Architecture
//!
//! ```text
//! OS callback → BridgeProducer → [ring buffer] → BridgeConsumer → BridgeStream
//!                                                                   ↓
//!                                                            CapturingStream
//! ```
//!
//! The `BridgeStream` wraps:
//! 1. A [`BridgeConsumer`] — for reading audio data from the ring buffer
//! 2. An `Arc<BridgeShared>` — shared state with the producer for lifecycle coordination
//! 3. An [`AudioFormat`] — the format of audio data in this stream
//! 4. A platform-specific stream `S` — wrapped in `Mutex` for interior mutability

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::core::buffer::AudioBuffer;
use crate::core::config::AudioFormat;
use crate::core::error::{AudioError, AudioResult};
use crate::core::interface::CapturingStream;

use super::ring_buffer::{BridgeConsumer, BridgeShared};
use super::state::StreamState;

// ── PlatformStream Trait ─────────────────────────────────────────────────

/// Internal trait that platform-specific stream implementations must satisfy.
///
/// This trait is NOT part of the public API. Platform backends implement this
/// to provide the OS-specific stop/cleanup logic, while [`BridgeStream`] handles
/// the ring buffer and state management.
///
/// # Implementors
///
/// Each platform backend (WASAPI, PipeWire, CoreAudio) provides a type that
/// implements `PlatformStream`. The type is moved into a `BridgeStream` during
/// stream creation and is only accessed through the `stop_capture()` and
/// `is_active()` methods.
// Platform-conditional: only used when a platform backend feature is enabled.
#[allow(dead_code)]
pub(crate) trait PlatformStream: Send {
    /// Stop the OS audio capture callback.
    ///
    /// Called by [`BridgeStream::stop()`] after transitioning the shared state
    /// to `Stopping`. The implementation should signal the OS to stop delivering
    /// audio data.
    fn stop_capture(&self) -> AudioResult<()>;

    /// Check if the OS capture is still active.
    ///
    /// Returns `true` if the platform-level capture is still running.
    fn is_active(&self) -> bool;
}

// ── BridgeStream ─────────────────────────────────────────────────────────

/// Universal [`CapturingStream`] implementation backed by a ring buffer bridge.
///
/// `BridgeStream<S>` is parameterized over a platform-specific stream type `S`
/// that handles OS-level audio capture. The bridge handles all the cross-thread
/// data transfer and state management.
///
/// # Thread Safety
///
/// `BridgeStream` is `Send + Sync`. The consumer and platform stream are each
/// protected by a [`Mutex`] to allow `&self` methods as required by
/// [`CapturingStream`].
///
/// # Construction
///
/// ```rust,ignore
/// let (mut producer, consumer) = create_bridge(capacity, format.clone());
/// // Transition to Running so reads work
/// consumer.shared().state.transition(StreamState::Created, StreamState::Running).unwrap();
/// let stream = BridgeStream::new(consumer, platform_stream, format, Duration::from_secs(1));
/// ```
// Platform-conditional: only constructed when a platform backend feature is enabled.
#[allow(dead_code)]
pub(crate) struct BridgeStream<S: PlatformStream> {
    /// Consumer side of the SPSC ring buffer, protected by Mutex for &self access.
    consumer: Mutex<BridgeConsumer>,
    /// Shared state (lifecycle + diagnostics) — cloned from the consumer's Arc.
    shared: Arc<BridgeShared>,
    /// Audio format of data flowing through this stream.
    format: AudioFormat,
    /// Platform-specific stream handle, protected by Mutex for &self access.
    platform_stream: Mutex<S>,
    /// Default timeout for blocking reads.
    default_timeout: Duration,
}

impl<S: PlatformStream> BridgeStream<S> {
    /// Create a new `BridgeStream` from a consumer and platform stream.
    ///
    /// The stream starts in whatever state the bridge was created with
    /// (typically [`StreamState::Created`]). The caller should transition
    /// the shared state to [`StreamState::Running`] before reading data.
    ///
    /// # Arguments
    ///
    /// * `consumer` — The consumer side of the ring buffer bridge.
    /// * `platform_stream` — The platform-specific stream handle.
    /// * `format` — The audio format of data in this stream.
    /// * `default_timeout` — Default timeout for blocking `read_chunk()` calls.
    ///
    /// Platform-conditional: called by platform backends when features are enabled.
    #[allow(dead_code)]
    pub fn new(
        consumer: BridgeConsumer,
        platform_stream: S,
        format: AudioFormat,
        default_timeout: Duration,
    ) -> Self {
        let shared = consumer.shared().clone();
        Self {
            consumer: Mutex::new(consumer),
            shared,
            format,
            platform_stream: Mutex::new(platform_stream),
            default_timeout,
        }
    }

    /// Returns a reference to the shared bridge state.
    ///
    /// Useful for external code that needs to inspect or transition the
    /// stream lifecycle state.
    /// Diagnostic API — used in tests and by platform backends for state inspection.
    #[allow(dead_code)]
    pub(crate) fn shared(&self) -> &Arc<BridgeShared> {
        &self.shared
    }

    /// Returns the number of buffers dropped by the producer due to ring buffer overflow.
    /// Diagnostic counter — used in tests; part of the stream monitoring API.
    #[allow(dead_code)]
    pub fn buffers_dropped(&self) -> u64 {
        self.shared.buffers_dropped.load(Ordering::Relaxed)
    }

    /// Returns the number of buffers successfully read by the consumer.
    /// Diagnostic counter — used in tests; part of the stream monitoring API.
    #[allow(dead_code)]
    pub fn buffers_read(&self) -> u64 {
        self.shared.buffers_popped.load(Ordering::Relaxed)
    }

    /// Returns true if the producer has dropped enough consecutive buffers to
    /// indicate sustained backpressure (consumer cannot keep up with producer).
    ///
    /// Part of the public observability API — no in-tree consumer today, but
    /// downstream apps (e.g. audio-graph) are expected to poll this to decide
    /// whether to throttle expensive per-chunk work.
    #[allow(dead_code)]
    pub fn is_under_backpressure(&self) -> bool {
        self.shared.is_under_backpressure()
    }
}

// ── CapturingStream Implementation ───────────────────────────────────────

impl<S: PlatformStream + Sync + 'static> CapturingStream for BridgeStream<S> {
    fn read_chunk(&self) -> AudioResult<AudioBuffer> {
        // Check state — must be readable (Running or Stopping).
        if !self.shared.state.is_readable() {
            let state = self.shared.state.get();
            return Err(AudioError::StreamReadError {
                reason: format!("Stream is in {} state, cannot read", state),
            });
        }

        let mut consumer = self
            .consumer
            .lock()
            .map_err(|_| AudioError::InternalError {
                message: "Consumer mutex poisoned".to_string(),
                source: None,
            })?;

        consumer.pop_blocking(self.default_timeout)
    }

    fn try_read_chunk(&self) -> AudioResult<Option<AudioBuffer>> {
        // Check state — must be readable (Running or Stopping).
        if !self.shared.state.is_readable() {
            let state = self.shared.state.get();
            return Err(AudioError::StreamReadError {
                reason: format!("Stream is in {} state, cannot read", state),
            });
        }

        let mut consumer = self
            .consumer
            .lock()
            .map_err(|_| AudioError::InternalError {
                message: "Consumer mutex poisoned".to_string(),
                source: None,
            })?;

        Ok(consumer.pop())
    }

    fn stop(&self) -> AudioResult<()> {
        let current = self.shared.state.get();
        match current {
            StreamState::Running => {
                // Transition Running → Stopping.
                let _ = self
                    .shared
                    .state
                    .transition(StreamState::Running, StreamState::Stopping);
            }
            StreamState::Stopping | StreamState::Stopped | StreamState::Closed => {
                // Already stopping/stopped — idempotent, return Ok.
                return Ok(());
            }
            _ => {
                return Err(AudioError::StreamStopFailed {
                    reason: format!("Cannot stop stream in {} state", current),
                });
            }
        }

        // Tell the platform to stop OS capture.
        let platform = self
            .platform_stream
            .lock()
            .map_err(|_| AudioError::InternalError {
                message: "Platform stream mutex poisoned".to_string(),
                source: None,
            })?;

        let result = platform.stop_capture();

        // Transition Stopping → Stopped regardless of platform result.
        let _ = self
            .shared
            .state
            .transition(StreamState::Stopping, StreamState::Stopped);

        result
    }

    fn format(&self) -> AudioFormat {
        self.format.clone()
    }

    fn is_running(&self) -> bool {
        self.shared.state.is_running()
    }

    fn overrun_count(&self) -> u64 {
        self.shared.buffers_dropped.load(Ordering::Relaxed)
    }

    fn is_under_backpressure(&self) -> bool {
        self.shared.is_under_backpressure()
    }

    #[cfg(feature = "async-stream")]
    fn register_waker(&self, waker: &std::task::Waker) -> bool {
        self.shared.waker.register(waker);
        true
    }

    #[cfg(feature = "async-stream")]
    fn is_stream_producing(&self) -> bool {
        matches!(
            self.shared.state.get(),
            StreamState::Created | StreamState::Running
        )
    }
}

// ── Send + Sync Assertion ────────────────────────────────────────────────

/// Compile-time assertion that `BridgeStream<S>` is `Send + Sync`
/// for any `S: PlatformStream + Sync`.
fn _assert_send_sync<S: PlatformStream + Sync>() {
    fn _assert<T: Send + Sync>() {}
    _assert::<BridgeStream<S>>();
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::ring_buffer::create_bridge;
    use crate::core::config::AudioFormat;
    use std::sync::atomic::AtomicBool;

    // ── Mock PlatformStream ──────────────────────────────────────────

    struct MockPlatformStream {
        active: AtomicBool,
    }

    impl MockPlatformStream {
        fn new() -> Self {
            Self {
                active: AtomicBool::new(true),
            }
        }
    }

    impl PlatformStream for MockPlatformStream {
        fn stop_capture(&self) -> AudioResult<()> {
            self.active.store(false, Ordering::Relaxed);
            Ok(())
        }

        fn is_active(&self) -> bool {
            self.active.load(Ordering::Relaxed)
        }
    }

    // ── Helpers ──────────────────────────────────────────────────────

    fn test_format() -> AudioFormat {
        AudioFormat::default() // 48 kHz, 2ch, F32
    }

    fn test_buffer(value: f32) -> AudioBuffer {
        AudioBuffer::new(vec![value; 960], 2, 48000)
    }

    /// Creates a BridgeStream in Running state, plus a producer for pushing data.
    fn create_test_stream() -> (
        crate::bridge::ring_buffer::BridgeProducer,
        BridgeStream<MockPlatformStream>,
    ) {
        let format = test_format();
        let (producer, consumer) = create_bridge(8, format.clone());
        // Transition to Running so reads work.
        consumer
            .shared()
            .state
            .transition(StreamState::Created, StreamState::Running)
            .unwrap();
        let stream = BridgeStream::new(
            consumer,
            MockPlatformStream::new(),
            format,
            Duration::from_secs(1),
        );
        (producer, stream)
    }

    // ── Tests ────────────────────────────────────────────────────────

    // 1. Create BridgeStream, verify format and is_running
    #[test]
    fn test_bridge_stream_creation() {
        let (_producer, stream) = create_test_stream();
        assert!(stream.is_running());
        assert_eq!(stream.format(), test_format());
    }

    // 2. try_read_chunk on empty stream returns Ok(None) when Running
    #[test]
    fn test_try_read_chunk_empty() {
        let (_producer, stream) = create_test_stream();
        let result = stream.try_read_chunk();
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    // 3. Push data via producer, read via BridgeStream (blocking)
    #[test]
    fn test_read_chunk_with_data() {
        let (mut producer, stream) = create_test_stream();
        producer.push(test_buffer(0.42)).unwrap();

        let result = stream.read_chunk();
        assert!(result.is_ok());
        let buf = result.unwrap();
        assert_eq!(buf.data()[0], 0.42);
        assert_eq!(buf.len(), 960);
        assert_eq!(buf.channels(), 2);
        assert_eq!(buf.sample_rate(), 48000);
    }

    // 4. Push data, try_read returns Some
    #[test]
    fn test_try_read_chunk_with_data() {
        let (mut producer, stream) = create_test_stream();
        producer.push(test_buffer(0.77)).unwrap();

        let result = stream.try_read_chunk();
        assert!(result.is_ok());
        let buf = result.unwrap().expect("should have data");
        assert_eq!(buf.data()[0], 0.77);
    }

    // 5. Call stop(), verify state transitions and platform stop called
    #[test]
    fn test_stop() {
        let (_producer, stream) = create_test_stream();
        assert!(stream.is_running());

        let result = stream.stop();
        assert!(result.is_ok());
        assert!(!stream.is_running());
        assert_eq!(stream.shared().state.get(), StreamState::Stopped);

        // Verify platform stream was told to stop.
        let platform = stream.platform_stream.lock().unwrap();
        assert!(!platform.is_active());
    }

    // 6. Calling stop() twice doesn't error (idempotent)
    #[test]
    fn test_stop_idempotent() {
        let (_producer, stream) = create_test_stream();

        let result1 = stream.stop();
        assert!(result1.is_ok());

        let result2 = stream.stop();
        assert!(result2.is_ok());

        assert_eq!(stream.shared().state.get(), StreamState::Stopped);
    }

    // 7. Reading after stop returns error
    #[test]
    fn test_read_after_stop() {
        let (_producer, stream) = create_test_stream();
        stream.stop().unwrap();

        let result = stream.read_chunk();
        assert!(result.is_err());
        match result.unwrap_err() {
            AudioError::StreamReadError { reason } => {
                assert!(reason.contains("Stopped"));
            }
            other => panic!("Expected StreamReadError, got: {:?}", other),
        }

        let try_result = stream.try_read_chunk();
        assert!(try_result.is_err());
    }

    // 8. Verify format() returns correct AudioFormat
    #[test]
    fn test_format_returns_correct_audio_format() {
        let format = AudioFormat {
            sample_rate: 44100,
            channels: 1,
            sample_format: crate::core::config::SampleFormat::I16,
        };
        let (_, consumer) = create_bridge(4, format.clone());
        consumer
            .shared()
            .state
            .transition(StreamState::Created, StreamState::Running)
            .unwrap();
        let stream = BridgeStream::new(
            consumer,
            MockPlatformStream::new(),
            format.clone(),
            Duration::from_secs(1),
        );
        assert_eq!(stream.format(), format);
    }

    // 9. Verify is_running reflects state
    #[test]
    fn test_is_running() {
        let format = test_format();
        let (_, consumer) = create_bridge(4, format.clone());
        // State is Created — not running yet.
        let stream = BridgeStream::new(
            consumer,
            MockPlatformStream::new(),
            format,
            Duration::from_secs(1),
        );
        assert!(!stream.is_running());

        // Transition to Running.
        stream
            .shared()
            .state
            .transition(StreamState::Created, StreamState::Running)
            .unwrap();
        assert!(stream.is_running());

        // Stop → no longer running.
        stream.stop().unwrap();
        assert!(!stream.is_running());
    }

    // 10. Compile-time check that BridgeStream is Send + Sync
    #[test]
    fn test_bridge_stream_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BridgeStream<MockPlatformStream>>();
    }

    // 11. Push when full, verify buffers_dropped counter via BridgeStream
    #[test]
    fn test_buffers_dropped_counter() {
        let format = test_format();
        let (mut producer, consumer) = create_bridge(4, format.clone());
        consumer
            .shared()
            .state
            .transition(StreamState::Created, StreamState::Running)
            .unwrap();
        let stream = BridgeStream::new(
            consumer,
            MockPlatformStream::new(),
            format,
            Duration::from_secs(1),
        );

        // Fill the ring buffer (capacity 4).
        for _ in 0..4 {
            assert!(producer.push_or_drop(test_buffer(1.0)));
        }
        assert_eq!(stream.buffers_dropped(), 0);

        // These should be dropped.
        assert!(!producer.push_or_drop(test_buffer(2.0)));
        assert!(!producer.push_or_drop(test_buffer(3.0)));
        assert_eq!(stream.buffers_dropped(), 2);
    }

    // 12. Verify buffers_read counter
    #[test]
    fn test_buffers_read_counter() {
        let (mut producer, stream) = create_test_stream();

        producer.push(test_buffer(1.0)).unwrap();
        producer.push(test_buffer(2.0)).unwrap();
        producer.push(test_buffer(3.0)).unwrap();

        assert_eq!(stream.buffers_read(), 0);

        stream.try_read_chunk().unwrap();
        assert_eq!(stream.buffers_read(), 1);

        stream.try_read_chunk().unwrap();
        stream.try_read_chunk().unwrap();
        assert_eq!(stream.buffers_read(), 3);
    }

    // ===== overrun_count via CapturingStream trait =====

    #[test]
    fn test_overrun_count_via_trait() {
        let format = test_format();
        let (mut producer, consumer) = create_bridge(4, format.clone());
        consumer
            .shared()
            .state
            .transition(StreamState::Created, StreamState::Running)
            .unwrap();
        let stream: Box<dyn CapturingStream> = Box::new(BridgeStream::new(
            consumer,
            MockPlatformStream::new(),
            format,
            Duration::from_secs(1),
        ));

        // Initially zero
        assert_eq!(stream.overrun_count(), 0);

        // Fill ring buffer (capacity 4)
        for _ in 0..4 {
            assert!(producer.push_or_drop(test_buffer(1.0)));
        }
        assert_eq!(stream.overrun_count(), 0);

        // Now push_or_drop should drop and increment
        assert!(!producer.push_or_drop(test_buffer(2.0)));
        assert_eq!(stream.overrun_count(), 1);

        assert!(!producer.push_or_drop(test_buffer(3.0)));
        assert!(!producer.push_or_drop(test_buffer(4.0)));
        assert_eq!(stream.overrun_count(), 3);
    }

    #[test]
    fn test_overrun_count_default_is_zero() {
        // Verify the default trait implementation returns 0
        // (MockPlatformStream doesn't override overrun_count)
        struct MinimalStream;
        impl CapturingStream for MinimalStream {
            fn read_chunk(&self) -> AudioResult<AudioBuffer> {
                Err(AudioError::StreamReadError {
                    reason: "not implemented".into(),
                })
            }
            fn try_read_chunk(&self) -> AudioResult<Option<AudioBuffer>> {
                Ok(None)
            }
            fn stop(&self) -> AudioResult<()> {
                Ok(())
            }
            fn format(&self) -> AudioFormat {
                AudioFormat::default()
            }
            fn is_running(&self) -> bool {
                false
            }
        }
        let stream: Box<dyn CapturingStream> = Box::new(MinimalStream);
        assert_eq!(stream.overrun_count(), 0);
    }

    // 13. Stop from Created state returns error
    #[test]
    fn test_stop_from_created_state() {
        let format = test_format();
        let (_, consumer) = create_bridge(4, format.clone());
        // State remains Created (not transitioned to Running).
        let stream = BridgeStream::new(
            consumer,
            MockPlatformStream::new(),
            format,
            Duration::from_secs(1),
        );

        let result = stream.stop();
        assert!(result.is_err());
        match result.unwrap_err() {
            AudioError::StreamStopFailed { reason } => {
                assert!(reason.contains("Created"));
            }
            other => panic!("Expected StreamStopFailed, got: {:?}", other),
        }
    }

    // 14. Read from non-readable states returns error
    #[test]
    fn test_read_from_non_readable_states() {
        let format = test_format();

        // Test Created state (not readable).
        let (_, consumer) = create_bridge(4, format.clone());
        let stream = BridgeStream::new(
            consumer,
            MockPlatformStream::new(),
            format.clone(),
            Duration::from_secs(1),
        );
        assert!(stream.read_chunk().is_err());
        assert!(stream.try_read_chunk().is_err());
    }

    // 15. Read during Stopping state (draining) works
    #[test]
    fn test_read_during_stopping_drains_buffer() {
        let format = test_format();
        let (mut producer, consumer) = create_bridge(8, format.clone());
        consumer
            .shared()
            .state
            .transition(StreamState::Created, StreamState::Running)
            .unwrap();
        let stream = BridgeStream::new(
            consumer,
            MockPlatformStream::new(),
            format,
            Duration::from_secs(1),
        );

        // Push some data, then signal stopping.
        producer.push(test_buffer(0.5)).unwrap();
        producer.push(test_buffer(0.6)).unwrap();
        stream
            .shared()
            .state
            .transition(StreamState::Running, StreamState::Stopping)
            .unwrap();

        // Should still be able to read buffered data during Stopping.
        let buf1 = stream.try_read_chunk().unwrap();
        assert!(buf1.is_some());
        assert_eq!(buf1.unwrap().data()[0], 0.5);

        let buf2 = stream.try_read_chunk().unwrap();
        assert!(buf2.is_some());
        assert_eq!(buf2.unwrap().data()[0], 0.6);

        // No more data.
        let buf3 = stream.try_read_chunk().unwrap();
        assert!(buf3.is_none());
    }

    // 16. close() default implementation stops the stream
    #[test]
    fn test_close_stops_stream() {
        let (_, stream) = create_test_stream();
        assert!(stream.is_running());

        // close() requires Box<Self>.
        let boxed: Box<dyn CapturingStream> = Box::new(stream);
        let result = boxed.close();
        assert!(result.is_ok());
    }

    // 17. Multiple reads maintain FIFO order
    #[test]
    fn test_fifo_order_through_bridge_stream() {
        let (mut producer, stream) = create_test_stream();

        for i in 0..5 {
            producer.push(test_buffer(i as f32)).unwrap();
        }

        for i in 0..5 {
            let buf = stream.try_read_chunk().unwrap().expect("should have data");
            assert_eq!(
                buf.data()[0],
                i as f32,
                "FIFO order violated at index {}",
                i
            );
        }
    }

    // ===== K5.2: BridgeStream Lifecycle Edge Case Tests =====

    #[test]
    fn read_from_created_stream_returns_error() {
        // A stream that was never started (state=Created) should error on read
        let format = AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: crate::core::config::SampleFormat::F32,
        };
        let (_producer, consumer) = create_bridge(4, format.clone());
        let stream = BridgeStream::new(
            consumer,
            MockPlatformStream::new(),
            format,
            Duration::from_millis(100),
        );

        // State is Created — not readable
        let result = stream.read_chunk();
        assert!(result.is_err());
    }

    #[test]
    fn try_read_from_created_stream_returns_error() {
        let format = AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: crate::core::config::SampleFormat::F32,
        };
        let (_producer, consumer) = create_bridge(4, format.clone());
        let stream = BridgeStream::new(
            consumer,
            MockPlatformStream::new(),
            format,
            Duration::from_millis(100),
        );

        let result = stream.try_read_chunk();
        assert!(result.is_err());
    }

    #[test]
    fn stop_from_created_returns_error() {
        let format = AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: crate::core::config::SampleFormat::F32,
        };
        let (_producer, consumer) = create_bridge(4, format.clone());
        let stream = BridgeStream::new(
            consumer,
            MockPlatformStream::new(),
            format,
            Duration::from_millis(100),
        );

        let result = stream.stop();
        assert!(result.is_err());
    }

    #[test]
    fn stop_is_idempotent() {
        let format = AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: crate::core::config::SampleFormat::F32,
        };
        let (_producer, consumer) = create_bridge(4, format.clone());
        consumer.shared().state.force_set(StreamState::Running);
        let stream = BridgeStream::new(
            consumer,
            MockPlatformStream::new(),
            format,
            Duration::from_millis(100),
        );

        // First stop should succeed
        assert!(stream.stop().is_ok());
        // Second stop should also succeed (idempotent)
        assert!(stream.stop().is_ok());
    }

    #[test]
    fn format_returns_correct_format() {
        let format = AudioFormat {
            sample_rate: 96000,
            channels: 4,
            sample_format: crate::core::config::SampleFormat::I16,
        };
        let (_producer, consumer) = create_bridge(4, format.clone());
        let stream = BridgeStream::new(
            consumer,
            MockPlatformStream::new(),
            format.clone(),
            Duration::from_millis(100),
        );

        assert_eq!(stream.format().sample_rate, 96000);
        assert_eq!(stream.format().channels, 4);
    }

    #[test]
    fn is_running_reflects_state() {
        let format = AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: crate::core::config::SampleFormat::F32,
        };
        let (_producer, consumer) = create_bridge(4, format.clone());
        let stream = BridgeStream::new(
            consumer,
            MockPlatformStream::new(),
            format,
            Duration::from_millis(100),
        );

        assert!(!stream.is_running()); // Created
        stream.shared().state.force_set(StreamState::Running);
        assert!(stream.is_running());
        stream.shared().state.force_set(StreamState::Stopped);
        assert!(!stream.is_running());
    }
}
