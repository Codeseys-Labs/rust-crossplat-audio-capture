//! Bridge module: Ring buffer bridge between OS audio callbacks and consumer threads.
//!
//! This module provides the lock-free data plane that connects platform-specific
//! audio capture backends to the consumer-facing `CapturingStream` API.

pub mod ring_buffer;
pub mod state;
pub mod stream;

// Re-exports for internal use
pub(crate) use ring_buffer::BridgeShared;
pub use ring_buffer::{calculate_capacity, create_bridge, BridgeConsumer, BridgeProducer};
pub use state::{AtomicStreamState, StreamState};
pub(crate) use stream::BridgeStream;
pub(crate) use stream::PlatformStream;

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
