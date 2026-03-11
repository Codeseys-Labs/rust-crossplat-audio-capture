//! ChannelSink — sends audio buffers over a std::sync::mpsc channel.

use std::sync::mpsc;

use super::traits::AudioSink;
use crate::core::buffer::AudioBuffer;
use crate::core::error::{AudioError, AudioResult};

/// A sink that sends audio buffers over a standard library MPSC channel.
///
/// This is useful for:
/// - Passing audio data to another thread for processing
/// - Building custom processing pipelines
/// - Testing (receive buffers on the other end and verify)
///
/// # Example
/// ```rust,no_run
/// use rsac::sink::ChannelSink;
/// let (sink, receiver) = ChannelSink::new();
/// // In capture thread: sink.write(&buffer)?;
/// // In processing thread: let buf = receiver.recv()?;
/// ```
pub struct ChannelSink {
    sender: mpsc::Sender<AudioBuffer>,
}

impl ChannelSink {
    /// Create a new ChannelSink and its corresponding receiver.
    ///
    /// Returns `(sink, receiver)` where:
    /// - `sink` implements `AudioSink` and sends buffers
    /// - `receiver` can be used to receive buffers on another thread
    pub fn new() -> (Self, mpsc::Receiver<AudioBuffer>) {
        let (sender, receiver) = mpsc::channel();
        (Self { sender }, receiver)
    }

    /// Create a ChannelSink from an existing sender.
    ///
    /// Useful when you want to control channel creation yourself.
    pub fn from_sender(sender: mpsc::Sender<AudioBuffer>) -> Self {
        Self { sender }
    }
}

impl AudioSink for ChannelSink {
    fn write(&mut self, buffer: &AudioBuffer) -> AudioResult<()> {
        self.sender
            .send(buffer.clone())
            .map_err(|_| AudioError::InternalError {
                message: "Channel receiver disconnected".to_string(),
                source: None,
            })
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
}
