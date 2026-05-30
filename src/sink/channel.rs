//! ChannelSink — sends audio buffers over a std::sync::mpsc channel.

use std::sync::mpsc;

use super::traits::AudioSink;
use crate::core::buffer::AudioBuffer;
use crate::core::error::{AudioError, AudioResult};

/// The sending half — either an unbounded or a bounded (capacity-limited) MPSC.
enum ChannelKind {
    /// Unbounded: `write()` never blocks and never drops, but a stalled receiver
    /// lets the queue grow without limit (each entry is a full `Vec<f32>` clone),
    /// so a slow/stuck consumer can exhaust memory. Prefer [`ChannelSink::bounded`]
    /// for untrusted/long-running consumers.
    Unbounded(mpsc::Sender<AudioBuffer>),
    /// Bounded: backed by [`mpsc::SyncSender`]. `write()` is non-blocking and
    /// **drops** the buffer when the channel is full (audio back-pressure), so a
    /// slow consumer cannot grow memory unboundedly or stall the writer.
    Bounded(mpsc::SyncSender<AudioBuffer>),
}

/// A sink that sends audio buffers over a standard library MPSC channel.
///
/// This is useful for:
/// - Passing audio data to another thread for processing
/// - Building custom processing pipelines
/// - Testing (receive buffers on the other end and verify)
///
/// # Bounded vs unbounded
///
/// [`ChannelSink::new`] / [`from_sender`](ChannelSink::from_sender) use an
/// **unbounded** channel: writes never block or drop, but a stalled receiver
/// can grow the queue without limit (OOM risk on a long-running capture). For
/// untrusted or potentially-slow consumers, prefer [`ChannelSink::bounded`],
/// which drops buffers when the channel is full instead of growing memory.
///
/// # Example
/// ```rust,no_run
/// use rsac::sink::ChannelSink;
/// let (sink, receiver) = ChannelSink::new();
/// // In capture thread: sink.write(&buffer)?;
/// // In processing thread: let buf = receiver.recv()?;
/// ```
pub struct ChannelSink {
    sender: ChannelKind,
}

impl ChannelSink {
    /// Create a new **unbounded** ChannelSink and its corresponding receiver.
    ///
    /// Returns `(sink, receiver)` where:
    /// - `sink` implements `AudioSink` and sends buffers
    /// - `receiver` can be used to receive buffers on another thread
    ///
    /// Note: unbounded — a stalled receiver grows the queue without limit. See
    /// [`ChannelSink::bounded`] for a drop-on-full variant.
    pub fn new() -> (Self, mpsc::Receiver<AudioBuffer>) {
        let (sender, receiver) = mpsc::channel();
        (
            Self {
                sender: ChannelKind::Unbounded(sender),
            },
            receiver,
        )
    }

    /// Create a **bounded** ChannelSink holding at most `capacity` queued buffers.
    ///
    /// `write()` drops the buffer (returning `Ok(())`) when the channel is full,
    /// providing audio-appropriate back-pressure: a slow consumer loses the
    /// oldest-uncaught frames rather than causing unbounded memory growth or
    /// stalling the capture/writer thread. Disconnection still returns an error.
    pub fn bounded(capacity: usize) -> (Self, mpsc::Receiver<AudioBuffer>) {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        (
            Self {
                sender: ChannelKind::Bounded(sender),
            },
            receiver,
        )
    }

    /// Create a ChannelSink from an existing (unbounded) sender.
    ///
    /// Useful when you want to control channel creation yourself.
    pub fn from_sender(sender: mpsc::Sender<AudioBuffer>) -> Self {
        Self {
            sender: ChannelKind::Unbounded(sender),
        }
    }
}

impl AudioSink for ChannelSink {
    fn write(&mut self, buffer: &AudioBuffer) -> AudioResult<()> {
        match &self.sender {
            ChannelKind::Unbounded(tx) => {
                tx.send(buffer.clone())
                    .map_err(|_| AudioError::InternalError {
                        message: "Channel receiver disconnected".to_string(),
                        source: None,
                    })
            }
            ChannelKind::Bounded(tx) => match tx.try_send(buffer.clone()) {
                Ok(()) => Ok(()),
                // Full → drop the buffer (back-pressure), not an error.
                Err(mpsc::TrySendError::Full(_)) => Ok(()),
                Err(mpsc::TrySendError::Disconnected(_)) => Err(AudioError::InternalError {
                    message: "Channel receiver disconnected".to_string(),
                    source: None,
                }),
            },
        }
    }
    // flush() and close() use defaults
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_buffer() -> AudioBuffer {
        AudioBuffer::new(vec![0.5f32; 960], 2, 48000) // 10ms stereo at 48kHz
    }

    #[test]
    fn test_channel_sink_write_receive() {
        let (mut sink, receiver) = ChannelSink::new();
        let buf = test_buffer();
        sink.write(&buf).unwrap();

        let received = receiver.recv().unwrap();
        assert_eq!(received.data(), buf.data());
        assert_eq!(received.channels(), buf.channels());
        assert_eq!(received.sample_rate(), buf.sample_rate());
        assert_eq!(received.num_frames(), buf.num_frames());
    }

    #[test]
    fn test_channel_sink_multiple_buffers() {
        let (mut sink, receiver) = ChannelSink::new();

        let buf1 = AudioBuffer::new(vec![0.1f32; 960], 2, 48000);
        let buf2 = AudioBuffer::new(vec![0.2f32; 960], 2, 48000);
        let buf3 = AudioBuffer::new(vec![0.3f32; 960], 2, 48000);

        sink.write(&buf1).unwrap();
        sink.write(&buf2).unwrap();
        sink.write(&buf3).unwrap();

        let r1 = receiver.recv().unwrap();
        let r2 = receiver.recv().unwrap();
        let r3 = receiver.recv().unwrap();

        assert_eq!(r1.data()[0], 0.1f32);
        assert_eq!(r2.data()[0], 0.2f32);
        assert_eq!(r3.data()[0], 0.3f32);
    }

    #[test]
    fn test_channel_sink_disconnected() {
        let (mut sink, receiver) = ChannelSink::new();
        drop(receiver); // Disconnect the receiver

        let buf = test_buffer();
        let result = sink.write(&buf);
        assert!(result.is_err());

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Channel receiver disconnected"));
    }

    #[test]
    fn test_channel_sink_from_sender() {
        let (sender, receiver) = mpsc::channel();
        let mut sink = ChannelSink::from_sender(sender);

        let buf = test_buffer();
        sink.write(&buf).unwrap();

        let received = receiver.recv().unwrap();
        assert_eq!(received.data(), buf.data());
    }

    #[test]
    fn test_channel_sink_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<ChannelSink>();
    }

    // ===== K5.5: ChannelSink Edge Case Tests =====

    #[test]
    fn channel_sink_write_empty_buffer() {
        let (mut sink, rx) = ChannelSink::new();
        let buf = AudioBuffer::empty(2, 48000);
        assert!(sink.write(&buf).is_ok());
        let received = rx.try_recv().unwrap();
        assert!(received.is_empty());
    }

    #[test]
    fn channel_sink_flush_is_noop() {
        let (mut sink, _rx) = ChannelSink::new();
        assert!(sink.flush().is_ok());
    }

    #[test]
    fn channel_sink_close_is_noop() {
        let (mut sink, _rx) = ChannelSink::new();
        assert!(sink.close().is_ok());
    }

    #[test]
    fn channel_sink_multiple_writes_then_reads() {
        let (mut sink, rx) = ChannelSink::new();
        let bufs: Vec<AudioBuffer> = (0..10)
            .map(|i| AudioBuffer::new(vec![i as f32], 1, 48000))
            .collect();

        for buf in &bufs {
            assert!(sink.write(buf).is_ok());
        }

        for i in 0..10 {
            let received = rx.try_recv().unwrap();
            assert_eq!(received.data(), &[i as f32]);
        }

        // No more data
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn channel_sink_dropped_receiver_returns_error() {
        let (mut sink, rx) = ChannelSink::new();
        drop(rx); // Drop receiver

        let buf = AudioBuffer::new(vec![1.0], 1, 48000);
        let result = sink.write(&buf);
        assert!(result.is_err());
    }

    // ── bounded ChannelSink (M9) ──────────────────────────────────────

    #[test]
    fn bounded_channel_sink_delivers_within_capacity() {
        let (mut sink, rx) = ChannelSink::bounded(4);
        for i in 0..4 {
            assert!(sink.write(&AudioBuffer::new(vec![i as f32], 1, 48000)).is_ok());
        }
        for i in 0..4 {
            assert_eq!(rx.try_recv().unwrap().data(), &[i as f32]);
        }
    }

    #[test]
    fn bounded_channel_sink_drops_when_full_without_error() {
        // Capacity 2, never drain → 3rd+ writes drop but still return Ok.
        let (mut sink, _rx) = ChannelSink::bounded(2);
        assert!(sink.write(&AudioBuffer::new(vec![1.0], 1, 48000)).is_ok());
        assert!(sink.write(&AudioBuffer::new(vec![2.0], 1, 48000)).is_ok());
        // Channel now full; further writes are dropped (back-pressure), not errors.
        for _ in 0..100 {
            assert!(
                sink.write(&AudioBuffer::new(vec![9.0], 1, 48000)).is_ok(),
                "bounded sink must drop-on-full, not error"
            );
        }
    }

    #[test]
    fn bounded_channel_sink_errors_on_disconnect() {
        let (mut sink, rx) = ChannelSink::bounded(2);
        drop(rx);
        let result = sink.write(&AudioBuffer::new(vec![1.0], 1, 48000));
        assert!(result.is_err(), "disconnected bounded sink must error");
    }

    #[test]
    fn channel_sink_write_preserves_buffer_data() {
        let (mut sink, rx) = ChannelSink::new();
        let data = vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
        let buf = AudioBuffer::new(data.clone(), 2, 48000);
        assert!(sink.write(&buf).is_ok());

        let received = rx.try_recv().unwrap();
        assert_eq!(received.data(), &data[..]);
        assert_eq!(received.channels(), 2);
        assert_eq!(received.sample_rate(), 48000);
    }
}
