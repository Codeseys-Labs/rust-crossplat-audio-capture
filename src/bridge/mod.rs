//! Bridge module: Ring buffer bridge between OS audio callbacks and consumer threads.
//!
//! This module provides the lock-free data plane that connects platform-specific
//! audio capture backends to the consumer-facing `CapturingStream` API.

pub mod ring_buffer;
pub mod state;
pub mod stream;

#[cfg(feature = "async-stream")]
pub mod async_stream;

// Re-exports for internal use
pub use ring_buffer::{calculate_capacity, create_bridge, BridgeConsumer, BridgeProducer};
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
}
