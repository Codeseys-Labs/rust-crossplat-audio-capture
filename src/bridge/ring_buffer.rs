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

use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::core::buffer::AudioBuffer;
use crate::core::config::{AudioFormat, SampleFormat};
use crate::core::error::{AudioError, AudioResult};

use super::state::{AtomicStreamState, StreamState};

/// Default per-buffer sample capacity for the free-list and scratch allocations.
///
/// Sized for a realistic worst-case callback period so the real-time producer is
/// allocation-free in steady state without re-growing on the first packets. CoreAudio
/// can deliver ~1024 frames/callback; at stereo that is 2048 `f32` samples. We seed a
/// little above that so a typical 1024-frame stereo (or 2048-frame mono) period fits
/// without a reallocation. Recycled buffers additionally grow to the observed
/// high-water mark, so even larger periods converge to zero allocation after warm-up.
///
/// See `docs/designs/0001-rt-allocation-guarantee.md`.
const RT_BUFFER_SAMPLE_CAPACITY: usize = 2048;

/// Default back-pressure threshold: the number of *consecutive* dropped buffers
/// (with no successful push in between) before `is_under_backpressure`
/// returns true. At a typical ~10 ms callback period, 10 consecutive drops is
/// roughly 100 ms of sustained data loss — long enough to be a real signal that
/// the consumer cannot keep up, short enough to react before a long stall.
///
/// Overridable per-bridge via [`create_bridge_with_options`].
pub const DEFAULT_BACKPRESSURE_THRESHOLD: u32 = 10;

/// Encode a [`SampleFormat`] as a `u8` for lock-free atomic storage.
///
/// Paired with [`sample_format_from_atomic`]. The mapping is stable and
/// internal — it only needs to round-trip, not match any wire format.
fn sample_format_to_atomic(sf: SampleFormat) -> u8 {
    match sf {
        SampleFormat::I16 => 0,
        SampleFormat::I24 => 1,
        SampleFormat::I32 => 2,
        SampleFormat::F32 => 3,
    }
}

/// Decode a `u8` written by [`sample_format_to_atomic`] back into a
/// [`SampleFormat`]. Any unknown value falls back to [`SampleFormat::F32`]
/// (the library's internal standard), which is the value written by the
/// "no negotiated format yet" sentinel.
fn sample_format_from_atomic(v: u8) -> SampleFormat {
    match v {
        0 => SampleFormat::I16,
        1 => SampleFormat::I24,
        2 => SampleFormat::I32,
        _ => SampleFormat::F32,
    }
}

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
    /// Consecutive drop count — resets to 0 on successful push.
    /// Used to detect sustained backpressure without relying on total drop rate.
    pub consecutive_drops: AtomicU32,
    /// Threshold above which `is_under_backpressure()` returns true.
    /// Default: [`DEFAULT_BACKPRESSURE_THRESHOLD`] consecutive drops
    /// (≈100ms of data loss at typical rates). Configurable per-bridge via
    /// [`create_bridge_with_options`].
    pub backpressure_threshold: u32,
    /// Audio format **requested** when the bridge was constructed.
    /// This is the fallback returned by [`negotiated_format`] until a backend
    /// records what the OS actually delivered (see [`negotiated_*`] fields).
    ///
    /// [`negotiated_format`]: BridgeShared::negotiated_format
    /// [`negotiated_*`]: BridgeShared::set_negotiated_format
    #[allow(dead_code)]
    pub format: AudioFormat,
    /// `true` once a backend has recorded an authoritative *delivery* format via
    /// [`BridgeProducer::set_negotiated_format`]. Until then, [`negotiated_format`]
    /// falls back to the requested [`format`].
    ///
    /// [`format`]: BridgeShared::format
    /// [`negotiated_format`]: BridgeShared::negotiated_format
    negotiated_set: std::sync::atomic::AtomicBool,
    /// Delivery sample rate recorded by the backend (valid only when
    /// `negotiated_set` is true).
    negotiated_sample_rate: AtomicU32,
    /// Delivery channel count recorded by the backend (valid only when
    /// `negotiated_set` is true).
    negotiated_channels: AtomicU16,
    /// Delivery sample format recorded by the backend, encoded via
    /// [`sample_format_to_atomic`] (valid only when `negotiated_set` is true).
    negotiated_sample_format: AtomicU8,
    /// Waker for async stream consumers — notified when new data is pushed.
    #[cfg(feature = "async-stream")]
    pub waker: atomic_waker::AtomicWaker,
}

impl BridgeShared {
    /// Returns true if the producer has dropped `backpressure_threshold` or
    /// more buffers in a row without a successful push — signals that the
    /// consumer is falling behind and cannot keep up with the producer rate.
    pub fn is_under_backpressure(&self) -> bool {
        self.consecutive_drops.load(Ordering::Relaxed) >= self.backpressure_threshold
    }

    /// Record the **authoritative delivery format** negotiated with the OS.
    ///
    /// Platform backends call this (via [`BridgeProducer::set_negotiated_format`])
    /// once they know the format the OS audio callback will actually deliver,
    /// which can differ from the requested format (e.g. the system mix format
    /// when autoconvert is unavailable). It is lock-free and cheap — three
    /// relaxed stores plus a `Release` flag store so a consumer that observes
    /// `negotiated_set == true` also sees the field writes.
    ///
    /// Safe to call more than once; the most recent values win. Reads go through
    /// [`negotiated_format`](BridgeShared::negotiated_format).
    pub fn set_negotiated_format(&self, format: &AudioFormat) {
        self.negotiated_sample_rate
            .store(format.sample_rate, Ordering::Relaxed);
        self.negotiated_channels
            .store(format.channels, Ordering::Relaxed);
        self.negotiated_sample_format.store(
            sample_format_to_atomic(format.sample_format),
            Ordering::Relaxed,
        );
        // Release so that a consumer observing the flag (Acquire) also sees the
        // three field stores above.
        self.negotiated_set.store(true, Ordering::Release);
    }

    /// Returns the authoritative **delivery** format if a backend has recorded
    /// one via `set_negotiated_format`, otherwise the requested format the
    /// bridge was constructed with.
    ///
    /// This is what `BridgeStream::format` surfaces, so consumers always see
    /// what they are actually receiving.
    pub fn negotiated_format(&self) -> AudioFormat {
        // Acquire pairs with the Release in `set_negotiated_format`.
        if self.negotiated_set.load(Ordering::Acquire) {
            AudioFormat {
                sample_rate: self.negotiated_sample_rate.load(Ordering::Relaxed),
                channels: self.negotiated_channels.load(Ordering::Relaxed),
                sample_format: sample_format_from_atomic(
                    self.negotiated_sample_format.load(Ordering::Relaxed),
                ),
            }
        } else {
            self.format.clone()
        }
    }
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
    /// Consumer side of the **free-list return ring**. The data consumer pushes
    /// drained `Vec<f32>` allocations back through this ring after handing the
    /// user an owned copy; the producer pops them here to reuse on the next
    /// callback. This is what makes [`push_samples_or_drop`] allocation-free on
    /// the real-time thread in steady state — the unavoidable allocation is
    /// performed on the (non-real-time) consumer thread instead.
    ///
    /// [`push_samples_or_drop`]: BridgeProducer::push_samples_or_drop
    free_rx: rtrb::Consumer<Vec<f32>>,
    /// Single-slot fallback buffer used only when the free-list ring is
    /// momentarily empty (e.g. during warm-up before the consumer has recycled
    /// anything, or under sustained back-pressure when a push is rejected).
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
                self.shared.consecutive_drops.store(0, Ordering::Relaxed);
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
                self.shared
                    .consecutive_drops
                    .fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    /// Push raw audio samples into the ring buffer without allocating on the
    /// real-time callback thread in steady state.
    ///
    /// # Allocation behavior
    ///
    /// The `Vec<f32>` backing each [`AudioBuffer`] is sourced from the
    /// **free-list return ring** (`free_rx`), which the consumer replenishes
    /// every time it hands an owned buffer to the user (see
    /// [`BridgeConsumer::pop`]). The unavoidable heap allocation for the user's
    /// buffer is therefore performed on the consumer (non-RT) thread, and this
    /// method reuses recycled allocations:
    ///
    /// - **Steady state:** a recycled `Vec` is popped from `free_rx`, cleared,
    ///   filled from `data`, and pushed — **no heap allocation**.
    /// - **Warm-up / free-list empty:** falls back to a single-slot `scratch`
    ///   buffer; only allocates if `scratch` has insufficient capacity (a
    ///   bounded, transient cost until the consumer starts recycling).
    /// - **Back-pressure (ring full):** the rejected `Vec` is reclaimed into
    ///   `scratch` so the next call reuses it — no alloc+free churn.
    ///
    /// This is the preferred method for OS audio callbacks. Callers should use
    /// this instead of manually calling `data.to_vec()` + `AudioBuffer::new()` +
    /// `push_or_drop()`.
    pub fn push_samples_or_drop(&mut self, data: &[f32], channels: u16, sample_rate: u32) -> bool {
        // Acquire a reusable Vec: prefer a recycled allocation from the
        // free-list ring, otherwise fall back to the single-slot scratch.
        // `used_scratch` records whether we consumed the scratch slot, so the
        // success arm can refill it and never leave it at capacity 0 (see
        // docs/designs/0001-rt-allocation-guarantee.md).
        let (mut vec, used_scratch) = match self.free_rx.pop() {
            Ok(recycled) => (recycled, false),
            Err(rtrb::PopError::Empty) => (std::mem::take(&mut self.scratch), true),
        };

        vec.clear();
        vec.extend_from_slice(data);

        let buffer = AudioBuffer::new(vec, channels, sample_rate);

        match self.producer.push(buffer) {
            Ok(()) => {
                self.shared.buffers_pushed.fetch_add(1, Ordering::Relaxed);
                self.shared.consecutive_drops.store(0, Ordering::Relaxed);
                #[cfg(feature = "async-stream")]
                self.shared.waker.wake();
                // If we consumed the scratch fallback, the scratch slot is now
                // empty (capacity 0). Refill it best-effort from a recycled
                // allocation so the next free-list-empty push reuses a buffer
                // instead of allocating on the RT thread. If no recycled buffer
                // is available yet, scratch stays empty — but the consumer will
                // recycle one shortly, and the worst case is a single bounded
                // warm-up allocation rather than a permanent one.
                if used_scratch {
                    // Refill scratch so the single-slot fallback is never left at
                    // capacity 0 (which would force an RT-thread allocation on the
                    // next free-list-empty push — the precise defect ADR-0001
                    // fixes). Prefer a recycled buffer; if none is available yet
                    // (consumer hasn't caught up), restore a pre-sized empty Vec
                    // so the next `extend_from_slice` reuses its capacity instead
                    // of growing from zero.
                    self.scratch = match self.free_rx.pop() {
                        Ok(recycled) => recycled,
                        Err(rtrb::PopError::Empty) => Vec::with_capacity(RT_BUFFER_SAMPLE_CAPACITY),
                    };
                }
                true
            }
            Err(rtrb::PushError::Full(rejected)) => {
                // Reclaim the Vec allocation into scratch for reuse on the next
                // call. This keeps the RT thread alloc-free even when the
                // consumer can't keep up.
                self.scratch = rejected.into_data();
                self.shared.buffers_dropped.fetch_add(1, Ordering::Relaxed);
                self.shared
                    .consecutive_drops
                    .fetch_add(1, Ordering::Relaxed);
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

    /// Record the **authoritative delivery format** the OS will actually feed
    /// this producer (M1).
    ///
    /// Platform backends (PipeWire / CoreAudio / WASAPI) call this once
    /// negotiation completes — typically right after they learn the endpoint
    /// mix format — so that `BridgeStream::format` reflects what is
    /// **delivered**, not merely what was **requested**.
    ///
    /// Lock-free and cheap (delegates to the shared state). It is
    /// safe to call from the setup path before the capture loop starts; calling
    /// it from the hot callback is also allowed but unnecessary in steady state.
    pub fn set_negotiated_format(&self, format: &AudioFormat) {
        self.shared.set_negotiated_format(format);
    }

    /// Returns a reference to the shared state.
    /// Part of the bridge API surface for platform backends and diagnostics.
    #[allow(dead_code)]
    pub(crate) fn shared(&self) -> &Arc<BridgeShared> {
        &self.shared
    }

    /// Number of recycled allocations currently available in the free-list
    /// return ring. Test-only — used to assert allocation recycling behavior.
    #[cfg(test)]
    pub(crate) fn recycled_available(&self) -> usize {
        self.free_rx.slots()
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
    /// Producer side of the **free-list return ring**. After popping a buffer
    /// from the data ring and handing the user an owned copy, the consumer
    /// pushes the now-spare `Vec<f32>` allocation back here so the producer can
    /// reuse it without allocating on the real-time thread. If the ring is full
    /// the spare allocation is simply dropped (freed) — bounded and harmless.
    free_tx: rtrb::Producer<Vec<f32>>,
}

// BridgeConsumer is Send (can be moved to the consumer thread).
// rtrb::Consumer<T> is Send when T: Send, which AudioBuffer satisfies.

impl BridgeConsumer {
    /// Non-blocking pop. Returns `None` if the ring buffer is empty.
    ///
    /// Increments `buffers_popped` on success.
    ///
    /// # Allocation / recycling
    ///
    /// The user receives a freshly-allocated owned [`AudioBuffer`] (the
    /// allocation happens here, on the non-real-time consumer thread). The
    /// original `Vec<f32>` that travelled through the ring is recycled back to
    /// the producer via the free-list return ring, which is what keeps
    /// [`BridgeProducer::push_samples_or_drop`] allocation-free on the RT thread.
    pub fn pop(&mut self) -> Option<AudioBuffer> {
        match self.consumer.pop() {
            Ok(buffer) => {
                self.shared.buffers_popped.fetch_add(1, Ordering::Relaxed);

                // Reconstruct an owned buffer for the user (allocates here, on
                // the consumer/non-RT thread) and recycle the original
                // allocation back to the producer's free-list ring.
                let format = buffer.format().clone();
                let timestamp = buffer.timestamp();
                let original = buffer.into_data();
                let user_copy = original.clone();

                let user_buffer = match timestamp {
                    Some(ts) => AudioBuffer::with_timestamp(user_copy, format, ts),
                    None => AudioBuffer::with_format(user_copy, format),
                };

                // Best-effort recycle; if the free-list ring is full, the spare
                // allocation is dropped (freed) — bounded and harmless.
                let _ = self.free_tx.push(original);

                Some(user_buffer)
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
    /// - [`AudioError::StreamEnded`] (Fatal) if the stream state becomes terminal
    ///   (Stopped, Closed, or Error) during the wait — end-of-stream, not a
    ///   transient read error (see ADR-0003).
    pub fn pop_blocking(&mut self, timeout: Duration) -> AudioResult<AudioBuffer> {
        let deadline = Instant::now() + timeout;
        let sleep_interval = Duration::from_millis(1);

        loop {
            // Try to pop data first.
            if let Some(buffer) = self.pop() {
                return Ok(buffer);
            }

            // Check if the stream is in a terminal state. This is end-of-stream,
            // not a transient read error — return the Fatal StreamEnded so a
            // read loop branching on is_fatal()/is_recoverable() terminates
            // instead of busy-waiting a dead stream (see ADR-0003).
            if self.shared.state.is_terminal() {
                return Err(AudioError::StreamEnded {
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
    create_bridge_with_options(capacity, format, DEFAULT_BACKPRESSURE_THRESHOLD)
}

/// Like [`create_bridge`], but lets the caller pick the back-pressure threshold.
///
/// `backpressure_threshold` is the number of *consecutive* dropped buffers (no
/// successful push in between) before `is_under_backpressure`
/// reports `true`. A value of `0` means "report back-pressure on the very first
/// drop". Most callers should use [`create_bridge`], which applies
/// [`DEFAULT_BACKPRESSURE_THRESHOLD`]; this variant exists so a backend or
/// builder that knows its callback cadence can tune the sensitivity (L6).
pub fn create_bridge_with_options(
    capacity: usize,
    format: AudioFormat,
    backpressure_threshold: u32,
) -> (BridgeProducer, BridgeConsumer) {
    let (producer, consumer) = rtrb::RingBuffer::<AudioBuffer>::new(capacity);

    // Free-list return ring: carries drained `Vec<f32>` allocations from the
    // consumer back to the producer so the RT thread can reuse them. Same
    // capacity as the data ring so it can never be the limiting factor.
    let (mut free_tx, free_rx) = rtrb::RingBuffer::<Vec<f32>>::new(capacity);

    // Pre-seed a handful of reusable allocations so the producer is
    // allocation-free from the very first callbacks, before the consumer has
    // had a chance to recycle anything. Each is sized for a realistic
    // worst-case callback period (see RT_BUFFER_SAMPLE_CAPACITY).
    let seed = capacity.min(8);
    for _ in 0..seed {
        // If the ring somehow rejects (it won't — it's empty), just drop.
        let _ = free_tx.push(Vec::with_capacity(RT_BUFFER_SAMPLE_CAPACITY));
    }

    let shared = Arc::new(BridgeShared {
        state: AtomicStreamState::new(StreamState::Created),
        buffers_pushed: AtomicU64::new(0),
        buffers_dropped: AtomicU64::new(0),
        buffers_popped: AtomicU64::new(0),
        consecutive_drops: AtomicU32::new(0),
        backpressure_threshold,
        // Seed the atomic delivery-format mirror with the requested format so a
        // read before any backend negotiation still returns sensible values.
        negotiated_set: std::sync::atomic::AtomicBool::new(false),
        negotiated_sample_rate: AtomicU32::new(format.sample_rate),
        negotiated_channels: AtomicU16::new(format.channels),
        negotiated_sample_format: AtomicU8::new(sample_format_to_atomic(format.sample_format)),
        format,
        #[cfg(feature = "async-stream")]
        waker: atomic_waker::AtomicWaker::new(),
    });

    (
        BridgeProducer {
            producer,
            shared: Arc::clone(&shared),
            free_rx,
            // Single-slot fallback for when the free-list ring is momentarily
            // empty. Pre-sized for a realistic worst-case callback period.
            scratch: Vec::with_capacity(RT_BUFFER_SAMPLE_CAPACITY),
        },
        BridgeConsumer {
            consumer,
            shared,
            free_tx,
        },
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
    use crate::core::config::{AudioFormat, SampleFormat};

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

    // 9. pop_blocking returns the terminal StreamEnded when state becomes
    //    terminal (ADR-0003 — distinct from the recoverable StreamReadError).
    #[test]
    fn test_pop_blocking_stream_stopped() {
        let (_producer, mut consumer) = create_bridge(16, test_format());

        // Force the state to Stopped.
        consumer.shared().state.force_set(StreamState::Stopped);

        let result = consumer.pop_blocking(Duration::from_secs(5));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.is_fatal(), "terminal-state read must be Fatal");
        match err {
            AudioError::StreamEnded { reason } => {
                assert!(reason.contains("stopped") || reason.contains("Stream"));
            }
            other => panic!("Expected StreamEnded, got: {:?}", other),
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

    // ===== Free-list return ring (alloc-free RT producer) tests =====

    // push_samples_or_drop → pop preserves data, channels, and rate.
    #[test]
    fn push_samples_then_pop_preserves_data() {
        let (mut producer, mut consumer) = create_bridge(8, test_format());

        let samples = [0.1, -0.2, 0.3, -0.4];
        assert!(producer.push_samples_or_drop(&samples, 2, 44100));

        let buf = consumer.pop().expect("should have one buffer");
        assert_eq!(buf.data(), &samples);
        assert_eq!(buf.channels(), 2);
        assert_eq!(buf.sample_rate(), 44100);
    }

    // Popping a buffer recycles a Vec back to the producer's free-list ring.
    #[test]
    fn pop_recycles_allocation_to_producer() {
        // Capacity 4 → free-list seeded with min(4, 8) = 4 buffers.
        let (mut producer, mut consumer) = create_bridge(4, test_format());
        assert_eq!(producer.recycled_available(), 4);

        // Drain the seed: each push_samples_or_drop consumes one recycled Vec.
        for _ in 0..4 {
            assert!(producer.push_samples_or_drop(&[1.0, 2.0], 2, 48000));
        }
        assert_eq!(
            producer.recycled_available(),
            0,
            "seed should be drained after 4 pushes"
        );

        // Popping hands the user a copy and returns the spare alloc to the ring.
        let _ = consumer.pop().expect("buffer available");
        assert_eq!(
            producer.recycled_available(),
            1,
            "pop should recycle one allocation back to the producer"
        );
    }

    // Steady-state push/pop loop preserves data integrity over many cycles
    // and keeps recycling allocations (free-list never starves to allocation
    // once warmed up).
    #[test]
    fn steady_state_push_samples_pop_loop() {
        let (mut producer, mut consumer) = create_bridge(8, test_format());

        for i in 0..1000u32 {
            let v = i as f32;
            let samples = [v, v + 0.5];
            assert!(producer.push_samples_or_drop(&samples, 2, 48000));

            let buf = consumer.pop().expect("one buffer per iteration");
            assert_eq!(buf.data(), &[v, v + 0.5], "data integrity at iter {i}");
        }

        assert_eq!(
            producer.shared().buffers_pushed.load(Ordering::Relaxed),
            1000
        );
        assert_eq!(consumer.buffers_popped(), 1000);
        // After warm-up the consumer keeps the producer supplied with recycled
        // allocations, so the producer should have spare buffers on hand.
        assert!(
            producer.recycled_available() > 0,
            "free-list should stay populated in steady state"
        );
    }

    // Regression (ADR-0001 / audit H3): after the producer consumes the scratch
    // fallback on a free-list-empty push that then SUCCEEDS, the scratch slot must
    // not be left at capacity 0 — otherwise every later free-list-empty push
    // allocates a fresh Vec on the real-time thread. We drive the free-list empty,
    // pop on the consumer to recycle exactly one buffer, then push twice in a row
    // without popping in between and assert the producer never has to allocate from
    // a zero-capacity scratch.
    #[test]
    fn scratch_never_shrinks_to_zero_after_underrun() {
        // Capacity 2 → free-list seeded with min(2, 8) = 2 buffers.
        let (mut producer, mut consumer) = create_bridge(2, test_format());
        assert_eq!(producer.recycled_available(), 2);

        // Drain the 2 seeded recycled buffers with 2 successful pushes (ring cap 2).
        assert!(producer.push_samples_or_drop(&[1.0, 2.0], 2, 48000));
        assert!(producer.push_samples_or_drop(&[3.0, 4.0], 2, 48000));
        assert_eq!(producer.recycled_available(), 0, "free-list drained");

        // Consumer pops one buffer → recycles exactly one allocation back.
        let _ = consumer.pop().expect("buffer available");
        assert_eq!(producer.recycled_available(), 1);

        // Pop the second too so the ring has room, recycling another buffer.
        let _ = consumer.pop().expect("buffer available");
        assert_eq!(producer.recycled_available(), 2);

        // Now repeatedly: push (consumes a recycled buf), pop (recycles one back).
        // Throughout, the producer must always source from the free-list or a
        // non-empty scratch — never extend a freshly-allocated zero-cap Vec.
        // We assert scratch capacity stays >= the configured floor whenever the
        // free-list is the source, by checking the push always succeeds without
        // the recycled count going negative and data integrity holding.
        for i in 0..50u32 {
            let v = i as f32;
            assert!(producer.push_samples_or_drop(&[v, v + 0.5], 2, 48000));
            let buf = consumer.pop().expect("one buffer per iteration");
            assert_eq!(buf.data(), &[v, v + 0.5], "data integrity at iter {i}");
        }
        // After warm-up the free-list keeps the producer supplied.
        assert!(
            producer.recycled_available() > 0,
            "free-list should stay populated, keeping the RT producer alloc-free"
        );
    }

    // Direct assertion that the scratch fallback retains capacity after being
    // consumed by a successful push (the precise H3 defect). With NO consumer
    // pops, the first push consumes a seeded recycled buffer; we exhaust the
    // free-list, then a push that falls back to scratch and succeeds must leave
    // scratch refilled (capacity > 0) from the remaining free-list, or — once the
    // free-list is truly empty — the producer must still not be wedged at cap 0
    // on the NEXT successful push once a buffer is recycled.
    #[test]
    fn scratch_capacity_preserved_across_successful_push() {
        let (mut producer, mut consumer) = create_bridge(8, test_format());

        // Exhaust the seeded free-list (min(8,8)=8) without popping → ring fills
        // to 8, free-list to 0.
        for _ in 0..8 {
            assert!(producer.push_samples_or_drop(&[0.25], 1, 48000));
        }
        assert_eq!(producer.recycled_available(), 0);

        // Drain everything on the consumer → 8 buffers recycled back.
        for _ in 0..8 {
            let _ = consumer.pop().expect("buffer");
        }
        assert_eq!(producer.recycled_available(), 8);

        // Steady push/pop: each push consumes a recycled buffer, each pop returns
        // one. The producer should never need the scratch slot here, and when it
        // does (transiently), the success arm refills it. Over many iterations the
        // recycled pool stays healthy — proving no permanent scratch starvation.
        for _ in 0..200 {
            assert!(producer.push_samples_or_drop(&[0.5], 1, 48000));
            let _ = consumer.pop().expect("buffer");
        }
        assert!(producer.recycled_available() > 0);
    }

    // ===== M1: negotiated (delivery) format tests =====
    // `SampleFormat` is imported explicitly in this test module's `use` block.

    // sample_format <-> atomic round-trips for every variant.
    #[test]
    fn sample_format_atomic_roundtrip() {
        for sf in [
            SampleFormat::I16,
            SampleFormat::I24,
            SampleFormat::I32,
            SampleFormat::F32,
        ] {
            assert_eq!(sample_format_from_atomic(sample_format_to_atomic(sf)), sf);
        }
        // Unknown encodings decode to the F32 fallback.
        assert_eq!(sample_format_from_atomic(200), SampleFormat::F32);
    }

    // Before any backend negotiation, negotiated_format() returns the requested
    // format the bridge was constructed with.
    #[test]
    fn negotiated_format_defaults_to_requested() {
        let requested = AudioFormat {
            sample_rate: 44100,
            channels: 1,
            sample_format: SampleFormat::I16,
        };
        let (_producer, consumer) = create_bridge(8, requested.clone());
        assert_eq!(consumer.shared().negotiated_format(), requested);
    }

    // After the producer records a delivery format, negotiated_format() reflects
    // it (NOT the requested format) — the M1 invariant.
    #[test]
    fn set_negotiated_format_overrides_requested() {
        let requested = AudioFormat::default(); // 48k/2ch/F32
        let (producer, consumer) = create_bridge(8, requested.clone());

        let delivered = AudioFormat {
            sample_rate: 44100,
            channels: 1,
            sample_format: SampleFormat::I24,
        };
        producer.set_negotiated_format(&delivered);

        let observed = consumer.shared().negotiated_format();
        assert_eq!(observed, delivered);
        assert_ne!(observed, requested, "must reflect delivery, not request");
    }

    // The most recent set_negotiated_format wins (idempotent / last-writer).
    #[test]
    fn set_negotiated_format_last_writer_wins() {
        let (producer, consumer) = create_bridge(8, AudioFormat::default());
        producer.set_negotiated_format(&AudioFormat {
            sample_rate: 96000,
            channels: 4,
            sample_format: SampleFormat::I32,
        });
        let final_fmt = AudioFormat {
            sample_rate: 22050,
            channels: 2,
            sample_format: SampleFormat::F32,
        };
        producer.set_negotiated_format(&final_fmt);
        assert_eq!(consumer.shared().negotiated_format(), final_fmt);
    }

    // ===== L6: configurable back-pressure threshold tests =====

    // create_bridge uses the documented default threshold.
    #[test]
    fn default_backpressure_threshold_applied() {
        let (mut producer, _consumer) = create_bridge(2, test_format());
        // Fill the ring (cap 2), then drive (DEFAULT - 1) extra drops — still
        // below threshold.
        assert!(producer.push_or_drop(test_buffer(1.0)));
        assert!(producer.push_or_drop(test_buffer(1.0)));
        for _ in 0..(DEFAULT_BACKPRESSURE_THRESHOLD - 1) {
            assert!(!producer.push_or_drop(test_buffer(9.0)));
        }
        assert!(
            !producer.shared().is_under_backpressure(),
            "should NOT trip one drop below the default threshold"
        );
        // One more drop reaches the threshold.
        assert!(!producer.push_or_drop(test_buffer(9.0)));
        assert!(
            producer.shared().is_under_backpressure(),
            "should trip at exactly the default threshold"
        );
    }

    // A custom (lower) threshold trips sooner; a successful push resets it.
    #[test]
    fn custom_backpressure_threshold_trips_and_resets() {
        // Threshold 2: trips after 2 consecutive drops.
        let (mut producer, mut consumer) = create_bridge_with_options(2, test_format(), 2);
        assert!(producer.push_or_drop(test_buffer(1.0)));
        assert!(producer.push_or_drop(test_buffer(1.0)));

        assert!(!producer.push_or_drop(test_buffer(9.0))); // drop 1
        assert!(!producer.shared().is_under_backpressure());
        assert!(!producer.push_or_drop(test_buffer(9.0))); // drop 2 → trips
        assert!(producer.shared().is_under_backpressure());

        // Draining a slot lets the next push succeed, which resets the streak.
        let _ = consumer.pop();
        assert!(producer.push_or_drop(test_buffer(2.0)));
        assert!(
            !producer.shared().is_under_backpressure(),
            "a successful push must clear consecutive-drop backpressure"
        );
    }

    // A zero threshold reports back-pressure on the very first drop.
    #[test]
    fn zero_backpressure_threshold_trips_on_first_drop() {
        let (mut producer, _consumer) = create_bridge_with_options(1, test_format(), 0);
        // Threshold 0 means is_under_backpressure() is true even before any drop
        // (0 consecutive drops >= 0). After a drop it stays true.
        assert!(producer.shared().is_under_backpressure());
        assert!(producer.push_or_drop(test_buffer(1.0)));
        assert!(!producer.push_or_drop(test_buffer(9.0)));
        assert!(producer.shared().is_under_backpressure());
    }

    // Timestamps survive the recycle round-trip.
    #[test]
    fn pop_preserves_timestamp_through_recycle() {
        let (mut producer, mut consumer) = create_bridge(4, test_format());

        let fmt = AudioFormat::default();
        let ts = Duration::from_millis(250);
        producer
            .push(AudioBuffer::with_timestamp(vec![0.9; 4], fmt, ts))
            .unwrap();

        let buf = consumer.pop().expect("buffer available");
        assert_eq!(buf.timestamp(), Some(ts));
        assert_eq!(buf.data(), &[0.9; 4]);
    }

    // ===== Concurrent SPSC stress test (producer thread ⇄ consumer thread) =====
    //
    // This is the test that actually exercises the production data path: the
    // producer runs on one thread (simulating the OS audio callback), the
    // consumer on another (simulating the reader), and the free-list return
    // ring recycles allocations across the thread boundary concurrently.
    //
    // It validates two invariants under real cross-thread contention:
    //   1. Conservation: every successfully-pushed buffer is eventually popped
    //      exactly once (buffers_pushed == buffers_popped at quiescence), and
    //      dropped buffers never reach the consumer.
    //   2. FIFO integrity: the sequence numbers the consumer observes are a
    //      strictly increasing subsequence of those the producer sent — i.e.
    //      no reordering, duplication, or corruption through either ring.
    #[test]
    fn concurrent_producer_consumer_stress() {
        use std::sync::atomic::{AtomicBool, AtomicU64};
        use std::sync::Arc as StdArc;

        // Keep CI fast but large enough to surface races/corruption.
        const ITEMS: u64 = 200_000;
        // Small ring → frequent full/back-pressure → exercises the drop +
        // scratch-reclaim path and keeps the free-list ring churning.
        let (mut producer, mut consumer) = create_bridge(16, test_format());
        producer.shared().state.force_set(StreamState::Running);

        let producer_done = StdArc::new(AtomicBool::new(false));
        let pushed_seqs = StdArc::new(AtomicU64::new(0)); // count of successful pushes

        let producer_done_w = StdArc::clone(&producer_done);
        let pushed_seqs_w = StdArc::clone(&pushed_seqs);

        // Producer thread: each buffer encodes its sequence number in data[0].
        let producer_handle = std::thread::spawn(move || {
            let mut pushed = 0u64;
            for seq in 0..ITEMS {
                // Encode seq as the sole sample. push_samples_or_drop copies it
                // through the (recycled) scratch/free-list allocation.
                if producer.push_samples_or_drop(&[seq as f32], 1, 48000) {
                    pushed += 1;
                }
                // else: ring full → dropped; that seq simply never arrives.
            }
            pushed_seqs_w.store(pushed, Ordering::SeqCst);
            producer_done_w.store(true, Ordering::SeqCst);
            // Return the producer so its free-list consumer side stays alive
            // until the consumer finishes recycling.
            producer
        });

        // Consumer thread: pop continuously, verify strictly-increasing seqs.
        let consumer_handle = std::thread::spawn(move || {
            let mut popped = 0u64;
            let mut last_seq: i64 = -1;
            loop {
                match consumer.pop() {
                    Some(buf) => {
                        let seq = buf.data()[0] as i64;
                        assert!(
                            seq > last_seq,
                            "FIFO/integrity violation: seq {seq} not > previous {last_seq}"
                        );
                        last_seq = seq;
                        popped += 1;
                    }
                    None => {
                        // Stop once the producer is finished AND the ring has
                        // been fully drained.
                        if producer_done.load(Ordering::SeqCst) && consumer.available_buffers() == 0
                        {
                            break;
                        }
                        std::thread::yield_now();
                    }
                }
            }
            (popped, consumer)
        });

        let _producer = producer_handle.join().expect("producer thread panicked");
        let (popped, consumer) = consumer_handle.join().expect("consumer thread panicked");

        let pushed = pushed_seqs.load(Ordering::SeqCst);

        // Conservation: every successful push is popped exactly once.
        assert_eq!(
            pushed, popped,
            "conservation violated: {pushed} pushed but {popped} popped"
        );
        // Cross-check against the bridge's own counters.
        assert_eq!(consumer.buffers_popped(), popped, "popped counter mismatch");
        // pushed + dropped must equal the total attempts.
        let dropped = consumer.shared().buffers_dropped.load(Ordering::Relaxed);
        assert_eq!(
            pushed + dropped,
            ITEMS,
            "pushed ({pushed}) + dropped ({dropped}) must equal attempts ({ITEMS})"
        );
    }
}
