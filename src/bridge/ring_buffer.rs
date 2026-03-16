//! Ring buffer bridge: lock-free SPSC bridge between producer (OS callback) and consumer threads.
//!
//! This module provides [`BridgeProducer`] and [`BridgeConsumer`], connected by
//! an [`rtrb`] lock-free SPSC ring buffer. The producer is designed to run inside
//! the OS audio callback thread (no locks, no allocations on the hot path), while
//! the consumer runs in the user/reader thread with optional blocking reads.
//!
//! # Usage
//!
//! ```rust,ignore
//! use rsac::bridge::ring_buffer::{create_bridge, calculate_capacity};
//! use rsac::core::config::AudioFormat;
//!
//! let format = AudioFormat::default();
//! let capacity = calculate_capacity(Some(32), 4);
//! let (mut producer, mut consumer) = create_bridge(capacity, format);
//!
//! // Producer side (OS callback thread):
//! producer.push_or_drop(audio_buffer);
//!
//! // Consumer side (user thread):
//! if let Some(buf) = consumer.pop() {
//!     // process buf
//! }
//! ```

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::core::buffer::AudioBuffer;
use crate::core::config::AudioFormat;
use crate::core::error::{AudioError, AudioResult};

use super::state::{AtomicStreamState, StreamState};

// ── Shared State ─────────────────────────────────────────────────────────

/// Shared state between producer and consumer for diagnostics and coordination.
///
/// Both [`BridgeProducer`] and [`BridgeConsumer`] hold an `Arc<BridgeShared>`
/// to access stream lifecycle state and diagnostic counters without locks.
pub(crate) struct BridgeShared {
    /// Stream lifecycle state (atomic, lock-free).
    pub state: AtomicStreamState,
    /// Total buffers successfully pushed by the producer.
    pub buffers_pushed: AtomicU64,
    /// Total buffers dropped due to the ring buffer being full.
    pub buffers_dropped: AtomicU64,
    /// Total buffers successfully popped by the consumer.
    pub buffers_popped: AtomicU64,
    /// Audio format for this bridge (immutable after creation).
    /// Stored for diagnostics and future format-aware consumers.
    #[allow(dead_code)]
    pub format: AudioFormat,
    /// Waker for async stream consumers — notified when new data is pushed.
    #[cfg(feature = "async-stream")]
    pub waker: atomic_waker::AtomicWaker,
}

// ── BridgeProducer ───────────────────────────────────────────────────────

/// Producer side of the ring buffer bridge.
///
/// Runs in the OS audio callback thread. All operations are lock-free
/// and non-allocating in the hot path.
///
/// # Safety
///
/// This type is [`Send`] so it can be moved to the callback thread.
/// It is **not** [`Sync`] — only one thread should use the producer.
pub struct BridgeProducer {
    producer: rtrb::Producer<AudioBuffer>,
    shared: Arc<BridgeShared>,
    /// Reusable scratch buffer to reduce heap allocations in the real-time
    /// audio callback. When a push is rejected (ring buffer full), the Vec
    /// is reclaimed and reused on the next call, avoiding alloc+free cycles
    /// on the RT thread during back-pressure.
    scratch: Vec<f32>,
}

// BridgeProducer is Send (can be moved to the callback thread) but not necessarily Sync.
// rtrb::Producer<T> is Send when T: Send, which AudioBuffer satisfies.
// We do NOT implement Sync — only one thread should use the producer.

impl BridgeProducer {
    /// Non-blocking push of an [`AudioBuffer`] into the ring buffer.
    ///
    /// If the ring buffer is full, returns `Err(buffer)` giving back the
    /// buffer to the caller. Does **not** increment `buffers_dropped` —
    /// the caller decides what to do with the rejected buffer.
    ///
    /// Increments `buffers_pushed` on success.
    pub fn push(&mut self, buffer: AudioBuffer) -> Result<(), AudioBuffer> {
        match self.producer.push(buffer) {
            Ok(()) => {
                self.shared.buffers_pushed.fetch_add(1, Ordering::Relaxed);
                #[cfg(feature = "async-stream")]
                self.shared.waker.wake();
                Ok(())
            }
            Err(rtrb::PushError::Full(buffer)) => Err(buffer),
        }
    }

    /// Tries to push an [`AudioBuffer`]. If the ring buffer is full, the
    /// buffer is dropped and `buffers_dropped` is incremented.
    ///
    /// Returns `true` if pushed successfully, `false` if dropped.
    ///
    /// This is the primary method used by audio callbacks — it never blocks
    /// and silently drops data when the consumer can't keep up.
    pub fn push_or_drop(&mut self, buffer: AudioBuffer) -> bool {
        match self.push(buffer) {
            Ok(()) => true,
            Err(_dropped) => {
                self.shared.buffers_dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Push raw audio samples into the ring buffer without requiring the
    /// caller to allocate a `Vec<f32>`.
    ///
    /// Uses an internal scratch buffer to minimize heap allocations on the
    /// real-time audio callback thread:
    ///
    /// - **Ring buffer has space (success):** Samples are copied into the
    ///   scratch buffer, which is then moved into an [`AudioBuffer`] and
    ///   pushed. The scratch buffer is consumed (left empty).
    /// - **Ring buffer is full (back-pressure):** The rejected [`AudioBuffer`]'s
    ///   `Vec` is reclaimed into the scratch buffer, so the **next** call
    ///   reuses the allocation — zero heap allocation on the RT thread when
    ///   the consumer can't keep up.
    ///
    /// This is the preferred method for OS audio callbacks where real-time
    /// safety is critical. Callers should use this instead of manually
    /// calling `data.to_vec()` + `AudioBuffer::new()` + `push_or_drop()`.
    pub fn push_samples_or_drop(&mut self, data: &[f32], channels: u16, sample_rate: u32) -> bool {
        // Fill scratch from slice (reuses allocation if capacity is sufficient)
        self.scratch.clear();
        self.scratch.extend_from_slice(data);

        // Take ownership of the filled scratch Vec
        let vec = std::mem::take(&mut self.scratch);
        let buffer = AudioBuffer::new(vec, channels, sample_rate);

        match self.producer.push(buffer) {
            Ok(()) => {
                self.shared.buffers_pushed.fetch_add(1, Ordering::Relaxed);
                #[cfg(feature = "async-stream")]
                self.shared.waker.wake();
                true
            }
            Err(rtrb::PushError::Full(rejected)) => {
                // Reclaim the Vec allocation for reuse on the next call.
                // This is the key optimization: no alloc+free cycle under
                // back-pressure on the RT thread.
                self.scratch = rejected.into_data();
                self.shared.buffers_dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Signals that the producer is done sending data.
    ///
    /// Attempts to transition the stream state from [`StreamState::Running`]
    /// to [`StreamState::Stopping`]. This is best-effort — if the transition
    /// fails (e.g., state was already changed), the failure is silently ignored.
    ///
    /// Called when the OS callback knows no more audio data will come.
    pub fn signal_done(&self) {
        // Best-effort: ignore if the CAS fails (state already changed).
        let _ = self
            .shared
            .state
            .transition(StreamState::Running, StreamState::Stopping);
        #[cfg(feature = "async-stream")]
        self.shared.waker.wake();
    }

    /// Returns the number of free slots in the ring buffer.
    pub fn available_slots(&self) -> usize {
        self.producer.slots()
    }

    /// Returns the total number of buffers dropped due to the ring buffer being full.
    pub fn buffers_dropped(&self) -> u64 {
        self.shared.buffers_dropped.load(Ordering::Relaxed)
    }

    /// Returns a reference to the shared state.
    /// Part of the bridge API surface for platform backends and diagnostics.
    #[allow(dead_code)]
    pub(crate) fn shared(&self) -> &Arc<BridgeShared> {
        &self.shared
    }
}

// ── BridgeConsumer ───────────────────────────────────────────────────────

/// Consumer side of the ring buffer bridge.
///
/// Runs in the user/consumer thread. Supports both blocking and
/// non-blocking reads.
pub struct BridgeConsumer {
    consumer: rtrb::Consumer<AudioBuffer>,
    shared: Arc<BridgeShared>,
}

// BridgeConsumer is Send (can be moved to the consumer thread).
// rtrb::Consumer<T> is Send when T: Send, which AudioBuffer satisfies.

impl BridgeConsumer {
    /// Non-blocking pop. Returns `None` if the ring buffer is empty.
    ///
    /// Increments `buffers_popped` on success.
    pub fn pop(&mut self) -> Option<AudioBuffer> {
        match self.consumer.pop() {
            Ok(buffer) => {
                self.shared.buffers_popped.fetch_add(1, Ordering::Relaxed);
                Some(buffer)
            }
            Err(rtrb::PopError::Empty) => None,
        }
    }

    /// Blocks until data is available or `timeout` expires.
    ///
    /// Uses a spin-loop with short [`std::thread::sleep`] intervals (1 ms)
    /// to wait for data without busy-spinning at 100% CPU.
    ///
    /// # Errors
    ///
    /// - [`AudioError::Timeout`] if the timeout expires before data arrives.
    /// - [`AudioError::StreamReadError`] if the stream state becomes terminal
    ///   (Stopped, Closed, or Error) during the wait.
    pub fn pop_blocking(&mut self, timeout: Duration) -> AudioResult<AudioBuffer> {
        let deadline = Instant::now() + timeout;
        let sleep_interval = Duration::from_millis(1);

        loop {
            // Try to pop data first.
            if let Some(buffer) = self.pop() {
                return Ok(buffer);
            }

            // Check if the stream is in a terminal state.
            if self.shared.state.is_terminal() {
                return Err(AudioError::StreamReadError {
                    reason: "Stream stopped".to_string(),
                });
            }

            // Check if we've exceeded the timeout.
            if Instant::now() >= deadline {
                return Err(AudioError::Timeout {
                    operation: "read_chunk".to_string(),
                    duration: timeout,
                });
            }

            // Sleep briefly to avoid busy-spinning.
            std::thread::sleep(sleep_interval);
        }
    }

    /// Returns the number of buffers ready to read.
    pub fn available_buffers(&self) -> usize {
        self.consumer.slots()
    }

    /// Returns the total number of buffers successfully popped.
    pub fn buffers_popped(&self) -> u64 {
        self.shared.buffers_popped.load(Ordering::Relaxed)
    }

    /// Returns `true` if the producer has signaled it is done.
    ///
    /// This is the case when the stream state is [`StreamState::Stopping`],
    /// [`StreamState::Stopped`], [`StreamState::Closed`], or [`StreamState::Error`].
    pub fn is_producer_done(&self) -> bool {
        matches!(
            self.shared.state.get(),
            StreamState::Stopping | StreamState::Stopped | StreamState::Closed | StreamState::Error
        )
    }

    /// Returns a reference to the shared state.
    /// Platform-conditional: called by BridgeStream::new() and used by platform backends.
    #[allow(dead_code)]
    pub(crate) fn shared(&self) -> &Arc<BridgeShared> {
        &self.shared
    }
}

// ── Factory ──────────────────────────────────────────────────────────────

/// Create a matched producer/consumer pair connected by a lock-free ring buffer.
///
/// # Arguments
///
/// * `capacity` — Number of [`AudioBuffer`] slots in the ring buffer.
///   Should be a power of 2 for optimal performance (use [`calculate_capacity`]).
/// * `format` — Audio format for this bridge (stored in shared state for reference).
///
/// # Returns
///
/// A `(BridgeProducer, BridgeConsumer)` pair. The producer should be moved to the
/// OS callback thread; the consumer stays on the reader thread.
pub fn create_bridge(capacity: usize, format: AudioFormat) -> (BridgeProducer, BridgeConsumer) {
    let (producer, consumer) = rtrb::RingBuffer::<AudioBuffer>::new(capacity);

    let shared = Arc::new(BridgeShared {
        state: AtomicStreamState::new(StreamState::Created),
        buffers_pushed: AtomicU64::new(0),
        buffers_dropped: AtomicU64::new(0),
        buffers_popped: AtomicU64::new(0),
        format,
        #[cfg(feature = "async-stream")]
        waker: atomic_waker::AtomicWaker::new(),
    });

    (
        BridgeProducer {
            producer,
            shared: Arc::clone(&shared),
            // Pre-allocate for a typical callback size:
            // 512 frames × 2 channels = 1024 samples.
            // This avoids a heap allocation on the very first callback.
            scratch: Vec::with_capacity(1024),
        },
        BridgeConsumer { consumer, shared },
    )
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Calculate an appropriate ring buffer capacity.
///
/// Uses the requested size or a sensible default (64 buffers), ensuring the
/// result is at least `min_capacity` and rounded up to the next power of two.
///
/// # Arguments
///
/// * `requested` — Desired capacity, or `None` for the default (64).
/// * `min_capacity` — Absolute minimum capacity (suggested: 4).
///
/// # Examples
///
/// ```rust,ignore
/// assert_eq!(calculate_capacity(None, 4), 64);      // default
/// assert_eq!(calculate_capacity(Some(100), 4), 128); // rounded up to next power of 2
/// assert_eq!(calculate_capacity(Some(2), 4), 4);     // clamped to min_capacity
/// ```
pub fn calculate_capacity(requested: Option<usize>, min_capacity: usize) -> usize {
    let raw = requested.unwrap_or(64).max(min_capacity);
    raw.next_power_of_two()
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::AudioFormat;

    /// Creates a test [`AudioBuffer`] filled with `value` — 10 ms of stereo 48 kHz audio.
    fn test_buffer(value: f32) -> AudioBuffer {
        AudioBuffer::new(vec![value; 960], 2, 48000)
    }

    fn test_format() -> AudioFormat {
        AudioFormat::default() // 48 kHz, 2ch, F32
    }

    // 1. Factory creates a valid pair
    #[test]
    fn test_create_bridge() {
        let (producer, consumer) = create_bridge(16, test_format());
        assert_eq!(producer.available_slots(), 16);
        assert_eq!(consumer.available_buffers(), 0);
        assert_eq!(producer.buffers_dropped(), 0);
        assert_eq!(consumer.buffers_popped(), 0);
        assert_eq!(producer.shared().state.get(), StreamState::Created);
        assert_eq!(consumer.shared().state.get(), StreamState::Created);
    }

    // 2. Push a buffer, pop it, verify data integrity
    #[test]
    fn test_push_pop() {
        let (mut producer, mut consumer) = create_bridge(16, test_format());

        let buf = test_buffer(0.5);
        assert!(producer.push(buf).is_ok());

        let popped = consumer.pop().expect("should have one buffer");
        assert_eq!(popped.data()[0], 0.5);
        assert_eq!(popped.len(), 960);
        assert_eq!(popped.channels(), 2);
        assert_eq!(popped.sample_rate(), 48000);
    }

    // 3. Push several, pop several, verify FIFO order
    #[test]
    fn test_push_pop_multiple() {
        let (mut producer, mut consumer) = create_bridge(16, test_format());

        for i in 0..5 {
            let buf = test_buffer(i as f32);
            assert!(producer.push(buf).is_ok());
        }

        for i in 0..5 {
            let popped = consumer.pop().expect("should have buffer");
            assert_eq!(
                popped.data()[0],
                i as f32,
                "FIFO order violated at index {}",
                i
            );
        }
    }

    // 4. Pop from empty returns None
    #[test]
    fn test_empty_pop() {
        let (_producer, mut consumer) = create_bridge(16, test_format());
        assert!(consumer.pop().is_none());
    }

    // 5. Fill buffer to capacity, verify push returns Err
    #[test]
    fn test_full_push() {
        let (mut producer, _consumer) = create_bridge(4, test_format());

        for _ in 0..4 {
            assert!(producer.push(test_buffer(1.0)).is_ok());
        }

        // Ring buffer is now full — push should fail.
        let result = producer.push(test_buffer(2.0));
        assert!(result.is_err());

        // Get back the rejected buffer.
        let rejected = result.unwrap_err();
        assert_eq!(rejected.data()[0], 2.0);
    }

    // 6. push_or_drop drops and increments counter
    #[test]
    fn test_push_or_drop() {
        let (mut producer, _consumer) = create_bridge(4, test_format());

        // Fill the buffer.
        for _ in 0..4 {
            assert!(producer.push_or_drop(test_buffer(1.0)));
        }

        // This one should be dropped.
        assert!(!producer.push_or_drop(test_buffer(2.0)));
        assert_eq!(producer.buffers_dropped(), 1);

        // Drop another.
        assert!(!producer.push_or_drop(test_buffer(3.0)));
        assert_eq!(producer.buffers_dropped(), 2);
    }

    // 7. pop_blocking succeeds immediately when there is already data
    #[test]
    fn test_pop_blocking_with_data() {
        let (mut producer, mut consumer) = create_bridge(16, test_format());

        producer.push(test_buffer(0.75)).unwrap();

        let result = consumer.pop_blocking(Duration::from_millis(100));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().data()[0], 0.75);
    }

    // 8. pop_blocking on empty with short timeout returns Timeout error
    #[test]
    fn test_pop_blocking_timeout() {
        let (_producer, mut consumer) = create_bridge(16, test_format());

        let start = Instant::now();
        let result = consumer.pop_blocking(Duration::from_millis(10));
        let elapsed = start.elapsed();

        assert!(result.is_err());
        match result.unwrap_err() {
            AudioError::Timeout { operation, .. } => {
                assert_eq!(operation, "read_chunk");
            }
            other => panic!("Expected Timeout error, got: {:?}", other),
        }
        // Should have waited at least ~10ms (allow some slack).
        assert!(elapsed >= Duration::from_millis(5));
    }

    // 9. pop_blocking returns StreamReadError when state becomes terminal
    #[test]
    fn test_pop_blocking_stream_stopped() {
        let (_producer, mut consumer) = create_bridge(16, test_format());

        // Force the state to Stopped.
        consumer.shared().state.force_set(StreamState::Stopped);

        let result = consumer.pop_blocking(Duration::from_secs(5));
        assert!(result.is_err());
        match result.unwrap_err() {
            AudioError::StreamReadError { reason } => {
                assert!(reason.contains("stopped") || reason.contains("Stream"));
            }
            other => panic!("Expected StreamReadError, got: {:?}", other),
        }
    }

    // 10. available_slots and available_buffers after pushes/pops
    #[test]
    fn test_available_slots_and_buffers() {
        let (mut producer, mut consumer) = create_bridge(8, test_format());

        assert_eq!(producer.available_slots(), 8);
        assert_eq!(consumer.available_buffers(), 0);

        producer.push(test_buffer(1.0)).unwrap();
        producer.push(test_buffer(2.0)).unwrap();

        assert_eq!(producer.available_slots(), 6);
        assert_eq!(consumer.available_buffers(), 2);

        consumer.pop().unwrap();

        assert_eq!(producer.available_slots(), 7);
        assert_eq!(consumer.available_buffers(), 1);
    }

    // 11. Diagnostics counters
    #[test]
    fn test_diagnostics_counters() {
        let (mut producer, mut consumer) = create_bridge(4, test_format());

        // Push 4 (fills the ring buffer).
        for _ in 0..4 {
            producer.push(test_buffer(1.0)).unwrap();
        }
        assert_eq!(producer.shared().buffers_pushed.load(Ordering::Relaxed), 4);

        // Drop 2 via push_or_drop.
        producer.push_or_drop(test_buffer(1.0));
        producer.push_or_drop(test_buffer(1.0));
        assert_eq!(producer.buffers_dropped(), 2);
        assert_eq!(producer.shared().buffers_dropped.load(Ordering::Relaxed), 2);

        // Pop 3.
        consumer.pop().unwrap();
        consumer.pop().unwrap();
        consumer.pop().unwrap();
        assert_eq!(consumer.buffers_popped(), 3);
        assert_eq!(consumer.shared().buffers_popped.load(Ordering::Relaxed), 3);
    }

    // 12. calculate_capacity: power-of-2, minimum, default
    #[test]
    fn test_calculate_capacity() {
        // Default (None) with min 4 → 64.
        assert_eq!(calculate_capacity(None, 4), 64);

        // Requested 100 → next power of 2 = 128.
        assert_eq!(calculate_capacity(Some(100), 4), 128);

        // Requested 2 with min 4 → clamped to 4 (already power of 2).
        assert_eq!(calculate_capacity(Some(2), 4), 4);

        // Requested 1 with min 1 → 1 (already power of 2).
        assert_eq!(calculate_capacity(Some(1), 1), 1);

        // Requested exact power of 2.
        assert_eq!(calculate_capacity(Some(32), 4), 32);

        // Requested 0 with min 4 → 4.
        assert_eq!(calculate_capacity(Some(0), 4), 4);

        // Large min_capacity.
        assert_eq!(calculate_capacity(Some(3), 16), 16);

        // Requested 5 with min 4 → 8 (next power of 2 above 5).
        assert_eq!(calculate_capacity(Some(5), 4), 8);
    }

    // 13. Compile-time check that BridgeProducer is Send
    #[test]
    fn test_producer_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<BridgeProducer>();
    }

    // 14. Compile-time check that BridgeConsumer is Send
    #[test]
    fn test_consumer_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<BridgeConsumer>();
    }

    // 15. signal_done transitions state
    #[test]
    fn test_signal_done() {
        let (producer, consumer) = create_bridge(8, test_format());

        // Set state to Running first (signal_done transitions Running → Stopping).
        producer
            .shared()
            .state
            .transition(StreamState::Created, StreamState::Running)
            .unwrap();
        assert!(producer.shared().state.is_running());

        producer.signal_done();

        assert_eq!(producer.shared().state.get(), StreamState::Stopping);
        assert!(consumer.is_producer_done());
    }

    // ===== K5.2: Ring Buffer Edge Case Tests =====

    #[test]
    fn signal_done_then_remaining_data_drains() {
        let (mut producer, mut consumer) = create_bridge(4, test_format());
        producer.shared().state.force_set(StreamState::Running);

        // Push some data
        let buf1 = AudioBuffer::new(vec![1.0, 2.0], 2, 48000);
        let buf2 = AudioBuffer::new(vec![3.0, 4.0], 2, 48000);
        assert!(producer.push(buf1).is_ok());
        assert!(producer.push(buf2).is_ok());

        // Signal done
        producer.signal_done();

        // Should still be able to read remaining data
        let read1 = consumer.pop();
        assert!(read1.is_some());
        assert_eq!(read1.unwrap().data(), &[1.0, 2.0]);

        let read2 = consumer.pop();
        assert!(read2.is_some());
        assert_eq!(read2.unwrap().data(), &[3.0, 4.0]);

        // Now empty
        let read3 = consumer.pop();
        assert!(read3.is_none());
    }

    #[test]
    fn push_to_full_buffer_returns_error() {
        let (mut producer, _consumer) = create_bridge(2, test_format());

        let buf1 = AudioBuffer::new(vec![1.0], 1, 48000);
        let buf2 = AudioBuffer::new(vec![2.0], 1, 48000);
        assert!(producer.push(buf1).is_ok());
        assert!(producer.push(buf2).is_ok());

        // Buffer should be full now — next push fails
        let buf3 = AudioBuffer::new(vec![3.0], 1, 48000);
        let result = producer.push(buf3);
        assert!(result.is_err());
    }

    #[test]
    fn push_or_drop_on_full_buffer_increments_dropped() {
        let (mut producer, _consumer) = create_bridge(2, test_format());

        // Fill the buffer
        for i in 0..2 {
            let buf = AudioBuffer::new(vec![i as f32], 1, 48000);
            let _ = producer.push(buf);
        }

        // push_or_drop should not panic
        let buf_extra = AudioBuffer::new(vec![99.0], 1, 48000);
        producer.push_or_drop(buf_extra);
        assert!(producer.buffers_dropped() >= 1);
    }

    #[test]
    fn consumer_pop_empty_returns_none() {
        let (_producer, mut consumer) = create_bridge(4, test_format());
        assert!(consumer.pop().is_none());
        assert_eq!(consumer.available_buffers(), 0);
    }

    #[test]
    fn buffers_popped_counter_increments() {
        let (mut producer, mut consumer) = create_bridge(4, test_format());

        let buf = AudioBuffer::new(vec![1.0], 1, 48000);
        assert!(producer.push(buf).is_ok());

        assert_eq!(consumer.buffers_popped(), 0);
        let _ = consumer.pop();
        assert_eq!(consumer.buffers_popped(), 1);
    }

    #[test]
    fn is_producer_done_after_signal() {
        let (producer, consumer) = create_bridge(4, test_format());
        producer.shared().state.force_set(StreamState::Running);

        assert!(!consumer.is_producer_done());
        producer.signal_done();
        assert!(consumer.is_producer_done());
    }
}
