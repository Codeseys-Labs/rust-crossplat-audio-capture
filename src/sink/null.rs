//! NullSink — discards all audio data. Useful for testing and benchmarking.

use super::traits::AudioSink;
use crate::core::buffer::AudioBuffer;
use crate::core::error::AudioResult;

/// A sink that discards all audio data.
///
/// Useful for:
/// - Testing capture pipelines without writing to disk
/// - Benchmarking capture performance without I/O overhead
/// - Placeholder when no output is needed
#[derive(Debug, Default, Clone)]
pub struct NullSink {
    /// Number of buffers received (for diagnostics)
    buffers_received: u64,
    /// Number of frames received (for diagnostics)
    frames_received: u64,
}

impl NullSink {
    /// Create a new NullSink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the number of buffers received.
    pub fn buffers_received(&self) -> u64 {
        self.buffers_received
    }

    /// Get the number of frames received.
    pub fn frames_received(&self) -> u64 {
        self.frames_received
    }
}

impl AudioSink for NullSink {
    fn write(&mut self, buffer: &AudioBuffer) -> AudioResult<()> {
        self.buffers_received += 1;
        self.frames_received += buffer.num_frames() as u64;
        Ok(())
    }
    // flush() and close() use defaults (no-op)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_buffer() -> AudioBuffer {
        AudioBuffer::new(vec![0.5f32; 960], 2, 48000) // 10ms stereo at 48kHz
    }

    #[test]
    fn test_null_sink_write() {
        let mut sink = NullSink::new();
        assert_eq!(sink.buffers_received(), 0);
        assert_eq!(sink.frames_received(), 0);

        let buf = test_buffer();
        sink.write(&buf).unwrap();
        assert_eq!(sink.buffers_received(), 1);
        assert_eq!(sink.frames_received(), 480); // 960 samples / 2 channels

        sink.write(&buf).unwrap();
        assert_eq!(sink.buffers_received(), 2);
        assert_eq!(sink.frames_received(), 960);
    }

    #[test]
    fn test_null_sink_flush() {
        let mut sink = NullSink::new();
        assert!(sink.flush().is_ok());
    }

    #[test]
    fn test_null_sink_close() {
        let mut sink = NullSink::new();
        assert!(sink.close().is_ok());
    }

    #[test]
    fn test_null_sink_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<NullSink>();
    }

    // ===== K5.5: NullSink Edge Case Tests =====

    #[test]
    fn null_sink_write_empty_buffer() {
        let mut sink = NullSink::new();
        let buf = AudioBuffer::empty(2, 48000);
        assert!(sink.write(&buf).is_ok());
        assert_eq!(sink.buffers_received(), 1);
        assert_eq!(sink.frames_received(), 0); // empty buffer has 0 frames
    }

    #[test]
    fn null_sink_multiple_writes() {
        let mut sink = NullSink::new();
        for i in 0..100 {
            let buf = AudioBuffer::new(vec![i as f32; 4], 2, 48000);
            assert!(sink.write(&buf).is_ok());
        }
        assert_eq!(sink.buffers_received(), 100);
        assert_eq!(sink.frames_received(), 200); // 4 samples / 2 channels = 2 frames each
    }

    #[test]
    fn null_sink_counters_start_at_zero() {
        let sink = NullSink::new();
        assert_eq!(sink.buffers_received(), 0);
        assert_eq!(sink.frames_received(), 0);
    }

    #[test]
    fn null_sink_write_single_sample() {
        let mut sink = NullSink::new();
        let buf = AudioBuffer::new(vec![0.5], 1, 44100);
        assert!(sink.write(&buf).is_ok());
        assert_eq!(sink.buffers_received(), 1);
        assert_eq!(sink.frames_received(), 1);
    }
}
