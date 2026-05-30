//! Bridge module: Ring buffer bridge between OS audio callbacks and consumer threads.
//!
//! This module provides the lock-free data plane that connects platform-specific
//! audio capture backends to the consumer-facing `CapturingStream` API.

pub mod ring_buffer;
pub mod state;
pub mod stream;

#[cfg(feature = "async-stream")]
pub mod async_stream;

#[cfg(any(test, feature = "test-utils"))]
pub mod mock;

// Re-exports for internal use
#[allow(unused_imports)]
pub use ring_buffer::{
    calculate_capacity, create_bridge, create_bridge_with_options, BridgeConsumer, BridgeProducer,
    DEFAULT_BACKPRESSURE_THRESHOLD,
};
pub use state::{AtomicStreamState, StreamState};
// Platform-conditional: used by platform backends when features are enabled,
// and by integration tests in this module.
#[allow(unused_imports)]
pub(crate) use stream::BridgeStream;
// Platform-conditional: used by Windows WASAPI backend via this re-export path;
// Linux/macOS backends import directly from bridge::stream.
#[allow(unused_imports)]
pub(crate) use stream::PlatformStream;

#[cfg(feature = "async-stream")]
pub use async_stream::AsyncAudioStream;

// ── Integration Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::core::buffer::AudioBuffer;
    use crate::core::config::AudioFormat;
    use crate::core::error::AudioResult;
    use crate::core::interface::CapturingStream;
    use crate::sink::{AudioSink, ChannelSink, NullSink};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

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
        AudioBuffer::new(vec![value; 960], 2, 48000) // 10ms stereo 48kHz
    }

    // ── Full Pipeline Integration Test ───────────────────────────────

    /// Test the complete pipeline: create_bridge → push buffers → BridgeStream → sinks
    #[test]
    fn test_full_bridge_to_sink_pipeline() {
        let format = test_format();
        let num_buffers = 5;

        // 1. Create the bridge
        let (mut producer, consumer) = create_bridge(16, format.clone());

        // 2. Transition state to Running
        consumer
            .shared()
            .state
            .transition(StreamState::Created, StreamState::Running)
            .unwrap();
        assert_eq!(consumer.shared().state.get(), StreamState::Running);

        // 3. Create a BridgeStream with mock PlatformStream
        let stream = BridgeStream::new(
            consumer,
            MockPlatformStream::new(),
            format,
            Duration::from_secs(1),
        );
        assert!(stream.is_running());

        // 4. Create sinks
        let mut null_sink = NullSink::new();
        let (mut channel_sink, receiver) = ChannelSink::new();

        // 5. Push several AudioBuffers via the producer
        for i in 0..num_buffers {
            let buf = test_buffer(i as f32 * 0.1);
            producer.push(buf).expect("push should succeed");
        }

        // 6. Read them via BridgeStream and pipe to sinks
        for i in 0..num_buffers {
            // Use try_read_chunk for the first few, read_chunk for the last
            let buf = if i < 3 {
                stream
                    .try_read_chunk()
                    .expect("try_read should not error")
                    .expect("buffer should be available")
            } else {
                stream.read_chunk().expect("read_chunk should succeed")
            };

            // Verify buffer data integrity
            let expected_value = i as f32 * 0.1;
            assert!(
                (buf.data()[0] - expected_value).abs() < 1e-6,
                "Buffer {} data mismatch: expected {}, got {}",
                i,
                expected_value,
                buf.data()[0]
            );

            // Pipe to both sinks
            null_sink
                .write(&buf)
                .expect("NullSink write should succeed");
            channel_sink
                .write(&buf)
                .expect("ChannelSink write should succeed");
        }

        // 7. Verify all buffers arrived at the ChannelSink receiver
        for i in 0..num_buffers {
            let received = receiver
                .try_recv()
                .unwrap_or_else(|_| panic!("Should receive buffer {}", i));
            let expected_value = i as f32 * 0.1;
            assert!(
                (received.data()[0] - expected_value).abs() < 1e-6,
                "ChannelSink buffer {} data mismatch",
                i
            );
            assert_eq!(received.channels(), 2);
            assert_eq!(received.sample_rate(), 48000);
        }
        // No extra buffers
        assert!(
            receiver.try_recv().is_err(),
            "Should have no more buffers in channel"
        );

        // Verify NullSink diagnostics
        assert_eq!(null_sink.buffers_received(), num_buffers as u64);

        // 8. Stop the stream
        stream.stop().expect("stop should succeed");

        // 9. Verify state is Stopped
        assert!(!stream.is_running());
        assert_eq!(stream.shared().state.get(), StreamState::Stopped);

        // Verify reading after stop returns error
        assert!(stream.try_read_chunk().is_err());
    }

    /// Test bridge pipeline with producer overflow — dropped buffers don't reach sinks
    #[test]
    fn test_bridge_overflow_to_sink() {
        let format = test_format();
        let capacity = 4;

        let (mut producer, consumer) = create_bridge(capacity, format.clone());
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

        let mut null_sink = NullSink::new();

        // Fill the ring buffer
        for i in 0..capacity {
            producer.push(test_buffer(i as f32)).unwrap();
        }

        // These should be dropped
        assert!(!producer.push_or_drop(test_buffer(99.0)));
        assert!(!producer.push_or_drop(test_buffer(100.0)));
        assert_eq!(stream.buffers_dropped(), 2);

        // Read all available buffers and pipe to sink
        let mut read_count = 0;
        while let Ok(Some(buf)) = stream.try_read_chunk() {
            null_sink.write(&buf).unwrap();
            read_count += 1;
        }

        // Should have read exactly `capacity` buffers (not the dropped ones)
        assert_eq!(read_count, capacity);
        assert_eq!(null_sink.buffers_received(), capacity as u64);
        assert_eq!(stream.buffers_read(), capacity as u64);
    }

    /// Test state machine lifecycle through the full pipeline
    #[test]
    fn test_bridge_state_lifecycle() {
        let format = test_format();
        let (mut producer, consumer) = create_bridge(8, format.clone());

        // State: Created
        assert_eq!(consumer.shared().state.get(), StreamState::Created);

        // Transition: Created → Running
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

        // State: Running
        assert!(stream.is_running());

        // Push some data for draining
        producer.push(test_buffer(1.0)).unwrap();
        producer.push(test_buffer(2.0)).unwrap();

        // Producer signals done → Running → Stopping
        producer.signal_done();
        assert_eq!(stream.shared().state.get(), StreamState::Stopping);

        // Can still read during Stopping (draining)
        let buf1 = stream.try_read_chunk().unwrap();
        assert!(buf1.is_some());
        let buf2 = stream.try_read_chunk().unwrap();
        assert!(buf2.is_some());

        // No more data
        let buf3 = stream.try_read_chunk().unwrap();
        assert!(buf3.is_none());

        // Stop the stream — already Stopping, stop() is idempotent
        stream.stop().unwrap();
        // State remains Stopping because stop() returns early for Stopping state
        // (only Running → Stopping → Stopped transitions happen when stop() initiates)
        assert_eq!(stream.shared().state.get(), StreamState::Stopping);
        assert!(!stream.is_running());
    }
}

// ── Async Stream Tests ───────────────────────────────────────────────────

#[cfg(all(test, feature = "async-stream"))]
mod async_stream_tests {
    use super::*;
    use crate::bridge::async_stream::AsyncAudioStream;
    use crate::core::buffer::AudioBuffer;
    use crate::core::config::AudioFormat;
    use crate::core::error::AudioResult;
    use futures_core::Stream;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};
    use std::time::Duration;

    // ── Test Waker ───────────────────────────────────────────────────

    struct TestWaker {
        woken: AtomicBool,
    }

    impl Wake for TestWaker {
        fn wake(self: Arc<Self>) {
            self.woken.store(true, Ordering::SeqCst);
        }
    }

    fn make_test_waker() -> (Waker, Arc<TestWaker>) {
        let test_waker = Arc::new(TestWaker {
            woken: AtomicBool::new(false),
        });
        let waker = Waker::from(test_waker.clone());
        (waker, test_waker)
    }

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
        AudioBuffer::new(vec![value; 960], 2, 48000) // 10ms stereo 48kHz
    }

    fn create_test_stream() -> (
        crate::bridge::ring_buffer::BridgeProducer,
        BridgeStream<MockPlatformStream>,
    ) {
        let format = test_format();
        let (producer, consumer) = create_bridge(8, format.clone());
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

    /// 1. Poll an empty stream — should return Pending because no data has been pushed.
    #[test]
    fn test_async_stream_pending_when_empty() {
        let (_producer, bridge_stream) = create_test_stream();
        let (waker, _test_waker) = make_test_waker();
        let mut cx = Context::from_waker(&waker);

        let mut async_stream = AsyncAudioStream::new(&bridge_stream);
        let pinned = Pin::new(&mut async_stream);

        match pinned.poll_next(&mut cx) {
            Poll::Pending => {} // Expected — no data pushed yet
            Poll::Ready(Some(Ok(_))) => panic!("Expected Pending, got Ready with data"),
            Poll::Ready(Some(Err(e))) => panic!("Expected Pending, got error: {:?}", e),
            Poll::Ready(None) => panic!("Expected Pending, got stream end (None)"),
        }
    }

    /// 2. Push data via producer, then poll — should return Ready(Some(Ok(buffer))).
    #[test]
    fn test_async_stream_ready_after_push() {
        let (mut producer, bridge_stream) = create_test_stream();

        // Push data before polling
        producer.push(test_buffer(0.42)).unwrap();

        let (waker, _test_waker) = make_test_waker();
        let mut cx = Context::from_waker(&waker);

        let mut async_stream = AsyncAudioStream::new(&bridge_stream);
        let pinned = Pin::new(&mut async_stream);

        match pinned.poll_next(&mut cx) {
            Poll::Ready(Some(Ok(buffer))) => {
                assert_eq!(
                    buffer.data()[0],
                    0.42,
                    "Buffer data should match pushed value"
                );
                assert_eq!(buffer.len(), 960, "Buffer length should be 960 samples");
                assert_eq!(buffer.channels(), 2, "Buffer should have 2 channels");
                assert_eq!(
                    buffer.sample_rate(),
                    48000,
                    "Buffer sample rate should be 48000"
                );
            }
            Poll::Ready(Some(Err(e))) => panic!("Expected Ok buffer, got error: {:?}", e),
            Poll::Ready(None) => panic!("Expected Some(Ok(buffer)), got None (stream ended)"),
            Poll::Pending => panic!("Expected Ready with data, got Pending"),
        }
    }

    /// 3. Poll (Pending, registers waker), push data, verify waker notified, poll again to get data.
    #[test]
    fn test_async_stream_waker_notified_on_push() {
        let (mut producer, bridge_stream) = create_test_stream();
        let (waker, test_waker) = make_test_waker();
        let mut cx = Context::from_waker(&waker);

        let mut async_stream = AsyncAudioStream::new(&bridge_stream);

        // First poll — should be Pending (no data yet), registers the waker
        {
            let pinned = Pin::new(&mut async_stream);
            match pinned.poll_next(&mut cx) {
                Poll::Pending => {} // Expected — registers waker
                Poll::Ready(Some(Ok(_))) => panic!("Expected Pending on first poll, got data"),
                Poll::Ready(Some(Err(e))) => {
                    panic!("Expected Pending on first poll, got error: {:?}", e)
                }
                Poll::Ready(None) => panic!("Expected Pending on first poll, got stream end"),
            }
        }

        // Waker should not yet be woken
        assert!(
            !test_waker.woken.load(Ordering::SeqCst),
            "Waker should NOT be triggered before push"
        );

        // Push data — this should wake the registered waker
        producer.push(test_buffer(0.77)).unwrap();

        assert!(
            test_waker.woken.load(Ordering::SeqCst),
            "Waker SHOULD be triggered after push"
        );

        // Reset waker flag and poll again — should get the data
        test_waker.woken.store(false, Ordering::SeqCst);
        {
            let pinned = Pin::new(&mut async_stream);
            match pinned.poll_next(&mut cx) {
                Poll::Ready(Some(Ok(buffer))) => {
                    assert_eq!(
                        buffer.data()[0],
                        0.77,
                        "Should receive the pushed buffer data"
                    );
                }
                Poll::Ready(Some(Err(e))) => panic!("Expected Ok buffer, got error: {:?}", e),
                Poll::Ready(None) => panic!("Expected data, got stream end"),
                Poll::Pending => panic!("Expected Ready after push, got Pending"),
            }
        }
    }

    /// 4. Push data + signal_done, drain all buffers, final poll returns None.
    #[test]
    fn test_async_stream_ends_on_signal_done() {
        let (mut producer, bridge_stream) = create_test_stream();
        let (waker, _test_waker) = make_test_waker();
        let mut cx = Context::from_waker(&waker);

        // Push some data, then signal producer is done
        producer.push(test_buffer(1.0)).unwrap();
        producer.push(test_buffer(2.0)).unwrap();
        producer.signal_done();

        let mut async_stream = AsyncAudioStream::new(&bridge_stream);

        // Poll 1 — should get first buffer
        {
            let pinned = Pin::new(&mut async_stream);
            match pinned.poll_next(&mut cx) {
                Poll::Ready(Some(Ok(buf))) => {
                    assert_eq!(buf.data()[0], 1.0, "First buffer should have value 1.0");
                }
                Poll::Ready(Some(Err(e))) => panic!("Expected first buffer, got error: {:?}", e),
                Poll::Ready(None) => panic!("Expected first buffer, got stream end"),
                Poll::Pending => panic!("Expected first buffer, got Pending"),
            }
        }

        // Poll 2 — should get second buffer
        {
            let pinned = Pin::new(&mut async_stream);
            match pinned.poll_next(&mut cx) {
                Poll::Ready(Some(Ok(buf))) => {
                    assert_eq!(buf.data()[0], 2.0, "Second buffer should have value 2.0");
                }
                Poll::Ready(Some(Err(e))) => panic!("Expected second buffer, got error: {:?}", e),
                Poll::Ready(None) => panic!("Expected second buffer, got stream end"),
                Poll::Pending => panic!("Expected second buffer, got Pending"),
            }
        }

        // Poll 3 — stream should end (None) since producer signaled done and buffer is drained
        {
            let pinned = Pin::new(&mut async_stream);
            match pinned.poll_next(&mut cx) {
                Poll::Ready(None) => {} // Expected — producer done, buffer drained
                Poll::Ready(Some(Ok(_))) => panic!("Expected stream end (None), got more data"),
                Poll::Ready(Some(Err(e))) => {
                    panic!("Expected stream end (None), got error: {:?}", e)
                }
                Poll::Pending => panic!("Expected stream end (None), got Pending"),
            }
        }
    }

    /// 5. Poll (Pending), signal_done, verify waker woken, poll returns None.
    #[test]
    fn test_async_stream_waker_notified_on_signal_done() {
        let (producer, bridge_stream) = create_test_stream();
        let (waker, test_waker) = make_test_waker();
        let mut cx = Context::from_waker(&waker);

        let mut async_stream = AsyncAudioStream::new(&bridge_stream);

        // Poll — should be Pending (no data, producer still active)
        {
            let pinned = Pin::new(&mut async_stream);
            match pinned.poll_next(&mut cx) {
                Poll::Pending => {} // Expected — registers waker
                Poll::Ready(Some(Ok(_))) => panic!("Expected Pending, got data"),
                Poll::Ready(Some(Err(e))) => panic!("Expected Pending, got error: {:?}", e),
                Poll::Ready(None) => panic!("Expected Pending, got stream end"),
            }
        }

        assert!(
            !test_waker.woken.load(Ordering::SeqCst),
            "Waker should NOT be triggered before signal_done"
        );

        // Signal done — should wake the registered waker
        producer.signal_done();

        assert!(
            test_waker.woken.load(Ordering::SeqCst),
            "Waker SHOULD be triggered after signal_done"
        );

        // Poll again — should return None (stream ended, no data was pushed)
        {
            let pinned = Pin::new(&mut async_stream);
            match pinned.poll_next(&mut cx) {
                Poll::Ready(None) => {} // Expected — producer done, no data
                Poll::Ready(Some(Ok(_))) => panic!("Expected None (stream end), got data"),
                Poll::Ready(Some(Err(e))) => {
                    panic!("Expected None (stream end), got error: {:?}", e)
                }
                Poll::Pending => panic!("Expected None (stream end), got Pending"),
            }
        }
    }

    /// 6. signal_error (FATAL producer death, FH-1 / ADR-0010) ends the async
    ///    stream with None. This is the async EOS counterpart of
    ///    test_async_stream_ends_on_signal_done, but driven by the fatal sibling.
    ///
    ///    NOTE the deliberate behavioral difference from signal_done: the terminal
    ///    `Error` state is NOT readable (only Running/Stopping are), so
    ///    `try_read_chunk` returns the Fatal StreamEnded rather than draining, and
    ///    `poll_next` maps that to `Poll::Ready(None)` (because the stream is no
    ///    longer producing). A dead producer ends the stream promptly; any
    ///    still-buffered tail is moot once the producer has died. Contrast with
    ///    test_async_stream_ends_on_signal_done, where graceful Stopping IS
    ///    readable and drains the buffered tail before ending.
    #[test]
    fn test_signal_error_ends_async_stream() {
        let (mut producer, bridge_stream) = create_test_stream();
        let (waker, _test_waker) = make_test_waker();
        let mut cx = Context::from_waker(&waker);

        // Push some data, then the producer DIES (e.g. device unplug).
        producer.push(test_buffer(3.0)).unwrap();
        producer.push(test_buffer(4.0)).unwrap();
        producer.signal_error();

        let mut async_stream = AsyncAudioStream::new(&bridge_stream);

        // Terminal Error → not readable → stream ends with None (no hang, no
        // spurious error item leaking past the producer's death).
        {
            let pinned = Pin::new(&mut async_stream);
            match pinned.poll_next(&mut cx) {
                Poll::Ready(None) => {}
                other => panic!(
                    "Expected stream end (None) after signal_error, got {:?}",
                    other
                ),
            }
        }
    }

    /// 7. A pending async poll is woken by signal_error (the fatal sibling wakes
    ///    the waker just like signal_done), and the next poll yields None.
    #[test]
    fn test_signal_error_wakes_waker() {
        let (producer, bridge_stream) = create_test_stream();
        let (waker, test_waker) = make_test_waker();
        let mut cx = Context::from_waker(&waker);

        let mut async_stream = AsyncAudioStream::new(&bridge_stream);

        // Poll — Pending (no data, producer still alive), registers the waker.
        {
            let pinned = Pin::new(&mut async_stream);
            match pinned.poll_next(&mut cx) {
                Poll::Pending => {}
                other => panic!("Expected Pending, got {:?}", other),
            }
        }
        assert!(
            !test_waker.woken.load(Ordering::SeqCst),
            "Waker should NOT be triggered before signal_error"
        );

        // Producer dies → must wake the registered waker.
        producer.signal_error();
        assert!(
            test_waker.woken.load(Ordering::SeqCst),
            "Waker SHOULD be triggered after signal_error"
        );

        // Next poll — terminal Error, no data → None.
        {
            let pinned = Pin::new(&mut async_stream);
            match pinned.poll_next(&mut cx) {
                Poll::Ready(None) => {}
                other => panic!(
                    "Expected None (stream end) after signal_error, got {:?}",
                    other
                ),
            }
        }
    }

    // ── FH-5: non-waking-backend waker contract ──────────────────────
    //
    // These tests exercise the contract that a backend whose
    // `register_waker()` returns `false` must NEVER cause `AsyncAudioStream`
    // to park forever on a waker that will never fire. They use a mock that
    // implements `CapturingStream` DIRECTLY (not via `BridgeStream`, which
    // always returns `true`) so we can drive the `register_waker == false`
    // path with no audio device.

    use crate::core::error::AudioError;
    use crate::core::interface::CapturingStream;
    use std::sync::atomic::AtomicU32;

    /// A `CapturingStream` that deliberately violates the wake promise:
    /// `register_waker()` always returns `false` (it never stores or wakes the
    /// waker). It models an alternate backend that has no async-wake support.
    /// It can be configured with a finite supply of buffers to hand out and a
    /// flag controlling whether it claims to still be producing.
    struct NonWakingStream {
        /// Buffers remaining to hand out from `try_read_chunk`.
        remaining: AtomicU32,
        /// Value carried by each yielded buffer.
        value: f32,
        /// Whether `is_stream_producing()` reports the producer as alive.
        producing: AtomicBool,
        /// Counts `register_waker` invocations — proves the consumer keeps
        /// polling (forward progress) rather than parking forever.
        register_calls: AtomicU32,
    }

    impl NonWakingStream {
        fn new(buffers: u32, value: f32, producing: bool) -> Self {
            Self {
                remaining: AtomicU32::new(buffers),
                value,
                producing: AtomicBool::new(producing),
                register_calls: AtomicU32::new(0),
            }
        }
    }

    impl CapturingStream for NonWakingStream {
        fn read_chunk(&self) -> AudioResult<AudioBuffer> {
            // Not exercised by the async path (which uses try_read_chunk).
            Err(AudioError::StreamReadError {
                reason: "blocking read not supported by NonWakingStream".into(),
            })
        }

        fn try_read_chunk(&self) -> AudioResult<Option<AudioBuffer>> {
            // Atomically decrement the supply if any buffers remain.
            loop {
                let cur = self.remaining.load(Ordering::SeqCst);
                if cur == 0 {
                    return Ok(None);
                }
                if self
                    .remaining
                    .compare_exchange(cur, cur - 1, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    return Ok(Some(test_buffer(self.value)));
                }
            }
        }

        fn stop(&self) -> AudioResult<()> {
            self.producing.store(false, Ordering::SeqCst);
            Ok(())
        }

        fn format(&self) -> AudioFormat {
            test_format()
        }

        fn is_running(&self) -> bool {
            self.producing.load(Ordering::SeqCst)
        }

        // FH-5: the deliberate contract violation — never registers/wakes.
        fn register_waker(&self, _waker: &Waker) -> bool {
            self.register_calls.fetch_add(1, Ordering::SeqCst);
            false
        }

        fn is_stream_producing(&self) -> bool {
            self.producing.load(Ordering::SeqCst)
        }
    }

    /// FH-5: a non-waking backend with buffered data must yield that data
    /// without ever returning a bare `Pending` that would park forever.
    /// The fast path drains the supply, then the stream ends once the
    /// producer reports done.
    #[test]
    fn test_non_waking_backend_yields_data_without_hanging() {
        let backend = NonWakingStream::new(2, 0.5, true);
        let (waker, _test_waker) = make_test_waker();
        let mut cx = Context::from_waker(&waker);

        let mut async_stream = AsyncAudioStream::new(&backend);

        // Two buffers come out via the fast path (no waker reliance).
        for expected in [0.5, 0.5] {
            let pinned = Pin::new(&mut async_stream);
            match pinned.poll_next(&mut cx) {
                Poll::Ready(Some(Ok(buf))) => assert_eq!(buf.data()[0], expected),
                other => panic!("Expected buffered data, got {:?}", other),
            }
        }

        // Supply exhausted; producer signals done → stream ends with None
        // (NOT a hang, NOT a spurious error).
        backend.stop().unwrap();
        let pinned = Pin::new(&mut async_stream);
        match pinned.poll_next(&mut cx) {
            Poll::Ready(None) => {}
            other => panic!("Expected stream end (None), got {:?}", other),
        }
    }

    /// FH-5: an EMPTY non-waking backend that still claims to be producing must
    /// NOT park on a waker that will never fire. `poll_next` must self-wake so
    /// the executor re-polls — proven here by the registered waker actually
    /// being woken and `register_waker` being re-invoked on the next poll.
    #[test]
    fn test_non_waking_empty_backend_self_wakes_not_parks() {
        let backend = NonWakingStream::new(0, 0.0, true);
        let (waker, test_waker) = make_test_waker();
        let mut cx = Context::from_waker(&waker);

        let mut async_stream = AsyncAudioStream::new(&backend);

        // First poll: empty + non-waking + still producing → the stream must
        // self-wake. The result is Pending (it yields control) BUT it has
        // arranged its own wakeup, so the task is rescheduled rather than
        // parked forever.
        {
            let pinned = Pin::new(&mut async_stream);
            assert!(
                matches!(pinned.poll_next(&mut cx), Poll::Pending),
                "empty non-waking backend should yield Pending after self-waking"
            );
        }
        assert!(
            test_waker.woken.load(Ordering::SeqCst),
            "FH-5: poll_next must self-wake (wake_by_ref) a non-waking backend so \
             the task is re-polled instead of parking forever"
        );
        assert_eq!(
            backend.register_calls.load(Ordering::SeqCst),
            1,
            "register_waker should have been consulted exactly once so far"
        );

        // The executor would re-poll because of the self-wake. Simulate that:
        // the second poll consults register_waker again (forward progress),
        // confirming the consumer did not silently park.
        test_waker.woken.store(false, Ordering::SeqCst);
        {
            let pinned = Pin::new(&mut async_stream);
            let _ = pinned.poll_next(&mut cx);
        }
        assert_eq!(
            backend.register_calls.load(Ordering::SeqCst),
            2,
            "a re-poll must re-consult register_waker, proving progress is made"
        );
    }

    /// FH-5: a non-waking backend that never produces and never wakes must
    /// eventually FAIL FAST rather than self-wake (busy-spin) forever. Polling
    /// in a bounded loop must terminate with an error within a finite number of
    /// iterations — this is the test that would HANG (time out) under the
    /// pre-fix code that returned a bare `Poll::Pending`.
    #[test]
    fn test_non_waking_empty_backend_fails_fast_eventually() {
        let backend = NonWakingStream::new(0, 0.0, true);
        let (waker, _test_waker) = make_test_waker();
        let mut cx = Context::from_waker(&waker);

        let mut async_stream = AsyncAudioStream::new(&backend);

        // Drive the stream the way an executor would: re-poll on each self-wake.
        // The self-wake budget bounds this to a finite number of iterations, so
        // a generous cap that EXCEEDS the budget must observe the fail-fast
        // error. If the stream parked forever (pre-fix bug) this loop would
        // only ever see Pending and the assertion at the end would fire.
        let mut saw_error = false;
        // Budget is 1024; poll comfortably more than that.
        for _ in 0..4096 {
            let pinned = Pin::new(&mut async_stream);
            match pinned.poll_next(&mut cx) {
                Poll::Pending => continue,
                Poll::Ready(Some(Err(e))) => {
                    // Fatal so a pump-style consumer stops cleanly rather than
                    // retrying the unproductive poll forever.
                    assert!(
                        e.is_fatal(),
                        "fail-fast error must be fatal (contract violation), got {e:?}"
                    );
                    assert!(
                        matches!(e, AudioError::InternalError { .. }),
                        "fail-fast error should be an InternalError, got {e:?}"
                    );
                    saw_error = true;
                    break;
                }
                other => panic!("Expected fail-fast error, got {:?}", other),
            }
        }
        assert!(
            saw_error,
            "FH-5: a non-waking, never-producing backend must fail fast within \
             the bounded self-wake budget, never hang on a bare Pending"
        );
    }

    /// FH-5: the self-wake budget must RESET when progress is made, so a
    /// non-waking backend that produces data slowly (interleaved empties) is
    /// never starved by the fail-fast cap. We hand out one buffer after a burst
    /// of empties shorter than the budget and confirm the stream keeps serving
    /// data rather than erroring.
    #[test]
    fn test_non_waking_budget_resets_on_progress() {
        // Backend starts empty but is flipped to "has one buffer" after a few
        // empty self-wakes — well under the 1024 budget.
        let backend = NonWakingStream::new(0, 0.9, true);
        let (waker, _test_waker) = make_test_waker();
        let mut cx = Context::from_waker(&waker);

        let mut async_stream = AsyncAudioStream::new(&backend);

        // A handful of empty polls (consume part of the budget).
        for _ in 0..8 {
            let pinned = Pin::new(&mut async_stream);
            assert!(matches!(pinned.poll_next(&mut cx), Poll::Pending));
        }

        // Now make a buffer available; the next poll yields it (fast path) and
        // resets the budget.
        backend.remaining.store(1, Ordering::SeqCst);
        {
            let pinned = Pin::new(&mut async_stream);
            match pinned.poll_next(&mut cx) {
                Poll::Ready(Some(Ok(buf))) => assert_eq!(buf.data()[0], 0.9),
                other => panic!("Expected the produced buffer, got {:?}", other),
            }
        }

        // After the reset, a fresh burst of empties (again under budget) still
        // does not error — proving the budget was refilled by the yield.
        for _ in 0..8 {
            let pinned = Pin::new(&mut async_stream);
            assert!(
                matches!(pinned.poll_next(&mut cx), Poll::Pending),
                "budget should have reset after yielding a buffer; no premature error"
            );
        }
    }
}
