//! WavFileSink — writes audio data to WAV files using the hound crate.

use std::path::Path;

use hound::{SampleFormat as HoundSampleFormat, WavSpec, WavWriter};

use super::traits::AudioSink;
use crate::core::buffer::AudioBuffer;
use crate::core::config::AudioFormat;
use crate::core::error::{AudioError, AudioResult};

/// A sink that writes audio data to a WAV file.
///
/// Uses the `hound` crate for WAV encoding. The WAV file is created
/// when the sink is constructed and finalized when `close()` is called.
///
/// # Feature Gate
/// Requires the `sink-wav` feature to be enabled.
///
/// # Example
/// ```rust,no_run
/// use rsac::sink::WavFileSink;
/// use rsac::core::config::{AudioFormat, SampleFormat};
/// let format = AudioFormat { sample_rate: 48000, channels: 2, sample_format: SampleFormat::F32 };
/// let mut sink = WavFileSink::new("output.wav", &format).unwrap();
/// // sink.write(&buffer)?;
/// // sink.close()?;
/// ```
pub struct WavFileSink {
    writer: Option<WavWriter<std::io::BufWriter<std::fs::File>>>,
    frames_written: u64,
}

impl WavFileSink {
    /// Create a new WavFileSink that writes to the specified path.
    ///
    /// The WAV file is created immediately. The format determines the
    /// WAV header parameters (sample rate, channels, bit depth).
    pub fn new<P: AsRef<Path>>(path: P, format: &AudioFormat) -> AudioResult<Self> {
        let spec = Self::format_to_spec(format)?;
        let writer = WavWriter::create(path, spec).map_err(|e| AudioError::InternalError {
            message: format!("Failed to create WAV file: {}", e),
            source: None,
        })?;
        Ok(Self {
            writer: Some(writer),
            frames_written: 0,
        })
    }

    /// Get the number of frames written so far.
    pub fn frames_written(&self) -> u64 {
        self.frames_written
    }

    /// Convert AudioFormat to hound WavSpec.
    fn format_to_spec(format: &AudioFormat) -> AudioResult<WavSpec> {
        // All internal audio is f32, so we write 32-bit float WAV
        Ok(WavSpec {
            channels: format.channels,
            sample_rate: format.sample_rate,
            bits_per_sample: 32,
            sample_format: HoundSampleFormat::Float,
        })
    }
}

impl AudioSink for WavFileSink {
    fn write(&mut self, buffer: &AudioBuffer) -> AudioResult<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| AudioError::InternalError {
                message: "WAV writer already closed".to_string(),
                source: None,
            })?;

        for &sample in buffer.data() {
            writer
                .write_sample(sample)
                .map_err(|e| AudioError::InternalError {
                    message: format!("Failed to write WAV sample: {}", e),
                    source: None,
                })?;
        }

        self.frames_written += buffer.num_frames() as u64;
        Ok(())
    }

    fn flush(&mut self) -> AudioResult<()> {
        if let Some(ref mut writer) = self.writer {
            writer.flush().map_err(|e| AudioError::InternalError {
                message: format!("Failed to flush WAV writer: {}", e),
                source: None,
            })?;
        }
        Ok(())
    }

    fn close(&mut self) -> AudioResult<()> {
        if let Some(writer) = self.writer.take() {
            writer.finalize().map_err(|e| AudioError::InternalError {
                message: format!("Failed to finalize WAV file: {}", e),
                source: None,
            })?;
        }
        Ok(())
    }
}

impl Drop for WavFileSink {
    fn drop(&mut self) {
        // Best-effort finalize on drop
        if let Some(writer) = self.writer.take() {
            let _ = writer.finalize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::SampleFormat;

    fn test_format() -> AudioFormat {
        AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
        }
    }

    fn test_buffer() -> AudioBuffer {
        AudioBuffer::new(vec![0.5f32; 960], 2, 48000) // 10ms stereo at 48kHz
    }

    #[test]
    fn test_wav_sink_create_and_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.wav");
        let format = test_format();

        let mut sink = WavFileSink::new(&path, &format).unwrap();
        let buf = test_buffer();
        sink.write(&buf).unwrap();
        sink.close().unwrap();

        assert!(path.exists());
        // Verify the file has content (WAV header + data)
        let metadata = std::fs::metadata(&path).unwrap();
        assert!(metadata.len() > 44); // WAV header is at least 44 bytes
    }

    #[test]
    fn test_wav_sink_frames_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_frames.wav");
        let format = test_format();

        let mut sink = WavFileSink::new(&path, &format).unwrap();
        assert_eq!(sink.frames_written(), 0);

        let buf = test_buffer();
        sink.write(&buf).unwrap();
        assert_eq!(sink.frames_written(), 480); // 960 samples / 2 channels

        sink.write(&buf).unwrap();
        assert_eq!(sink.frames_written(), 960);

        sink.close().unwrap();
    }

    #[test]
    fn test_wav_sink_close_finalizes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_finalize.wav");
        let format = test_format();

        let mut sink = WavFileSink::new(&path, &format).unwrap();
        let buf = test_buffer();
        sink.write(&buf).unwrap();
        sink.close().unwrap();

        // Read back with hound to verify valid WAV
        let reader = hound::WavReader::open(&path).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.channels, 2);
        assert_eq!(spec.sample_rate, 48000);
        assert_eq!(spec.bits_per_sample, 32);
        assert_eq!(spec.sample_format, HoundSampleFormat::Float);

        let samples: Vec<f32> = reader.into_samples::<f32>().map(|s| s.unwrap()).collect();
        assert_eq!(samples.len(), 960);
        for s in &samples {
            assert!((s - 0.5f32).abs() < 1e-6);
        }
    }

    #[test]
    fn test_wav_sink_double_close() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_double_close.wav");
        let format = test_format();

        let mut sink = WavFileSink::new(&path, &format).unwrap();
        sink.close().unwrap();
        // Second close should be a no-op (writer already taken)
        sink.close().unwrap();
    }

    #[test]
    fn test_wav_sink_write_after_close() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_write_after_close.wav");
        let format = test_format();

        let mut sink = WavFileSink::new(&path, &format).unwrap();
        sink.close().unwrap();

        let buf = test_buffer();
        let result = sink.write(&buf);
        assert!(result.is_err());

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("WAV writer already closed"));
    }

    // ===== K5.5: WavFileSink Edge Case Tests =====

    #[test]
    fn wav_sink_write_empty_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.wav");
        let format = AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
        };
        let mut sink = WavFileSink::new(&path, &format).unwrap();
        let buf = AudioBuffer::empty(2, 48000);
        assert!(sink.write(&buf).is_ok());
        assert_eq!(sink.frames_written(), 0);
        sink.close().unwrap();
    }

    #[test]
    fn wav_sink_mono_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mono.wav");
        let format = AudioFormat {
            sample_rate: 44100,
            channels: 1,
            sample_format: SampleFormat::F32,
        };
        let mut sink = WavFileSink::new(&path, &format).unwrap();
        let buf = AudioBuffer::new(vec![0.5, -0.5, 0.25], 1, 44100);
        assert!(sink.write(&buf).is_ok());
        assert_eq!(sink.frames_written(), 3);
        sink.close().unwrap();

        // Verify the WAV file is readable
        let reader = hound::WavReader::open(&path).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.channels, 1);
        assert_eq!(spec.sample_rate, 44100);
    }

    #[test]
    fn wav_sink_flush_then_continue_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flush_continue.wav");
        let format = AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
        };
        let mut sink = WavFileSink::new(&path, &format).unwrap();

        let buf1 = AudioBuffer::new(vec![1.0, 2.0], 2, 48000);
        assert!(sink.write(&buf1).is_ok());
        assert!(sink.flush().is_ok());

        // Should be able to continue writing after flush
        let buf2 = AudioBuffer::new(vec![3.0, 4.0], 2, 48000);
        assert!(sink.write(&buf2).is_ok());
        assert_eq!(sink.frames_written(), 2); // 2 frames total (each buf has 1 frame of stereo)

        sink.close().unwrap();
    }

    #[test]
    fn wav_sink_invalid_path() {
        let format = AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
        };
        // Try to create in a non-existent directory
        let result = WavFileSink::new("/nonexistent/directory/test.wav", &format);
        assert!(result.is_err());
    }

    #[test]
    fn wav_sink_multiple_writes_accumulate_frames() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("multi.wav");
        let format = AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
        };
        let mut sink = WavFileSink::new(&path, &format).unwrap();

        for _ in 0..10 {
            let buf = AudioBuffer::new(vec![0.0; 4], 2, 48000); // 2 frames each
            assert!(sink.write(&buf).is_ok());
        }
        assert_eq!(sink.frames_written(), 20); // 10 writes × 2 frames
        sink.close().unwrap();
    }
}
