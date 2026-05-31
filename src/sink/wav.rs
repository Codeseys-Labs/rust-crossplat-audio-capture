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
/// # Fixed format for the file's lifetime
///
/// The WAV header (channel count and sample rate) is written **once**, at
/// construction, from the [`AudioFormat`] passed to [`new`](Self::new). A WAV
/// container has a single, fixed `channels`/`sample_rate` for its whole length,
/// so every buffer subsequently handed to [`write`](Self::write) **must** match
/// that fixed shape. A buffer whose [`channels()`](AudioBuffer::channels) or
/// [`sample_rate()`](AudioBuffer::sample_rate) differs is **rejected** with
/// [`AudioError::ConfigurationError`] rather than being appended — appending a
/// mismatched buffer would silently corrupt the file (the samples would be
/// reinterpreted against the wrong header) and miscount
/// [`frames_written`](Self::frames_written) (which floor-divides the sample
/// count by the buffer's own channel count). If you need to capture a stream
/// whose format may change, open a fresh `WavFileSink` per format.
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
    /// Channel count fixed in the WAV header at construction. Every buffer
    /// written must match this (a WAV container has a single channel count for
    /// its whole length); a mismatch is rejected by [`write`](Self::write).
    channels: u16,
    /// Sample rate fixed in the WAV header at construction. Every buffer written
    /// must match this; a mismatch is rejected by [`write`](Self::write).
    sample_rate: u32,
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
            // Capture the header's fixed shape so write() can reject any buffer
            // that does not match it (FH-3): a WAV container's channel count and
            // sample rate are written once, here, and are immutable thereafter.
            channels: format.channels,
            sample_rate: format.sample_rate,
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
        // FH-3: reject a buffer whose format differs from the header fixed at
        // construction BEFORE touching the writer. The WAV header's channel
        // count / sample rate are immutable for the file's lifetime; appending a
        // mismatched buffer would silently corrupt the file (samples
        // reinterpreted against the wrong header) and miscount frames_written
        // (which divides by the buffer's own channel count below). A configured
        // mismatch is a caller error, not a transient hiccup, so this is a fatal
        // ConfigurationError. An empty buffer with matching channels/rate is
        // still accepted (0 frames written) — only a channel/rate MISMATCH errors.
        if buffer.channels() != self.channels || buffer.sample_rate() != self.sample_rate {
            return Err(AudioError::ConfigurationError {
                message: format!(
                    "WavFileSink configured for {}ch@{}Hz but received a buffer with \
                     {}ch@{}Hz; a WAV file's format is fixed at construction. Open a \
                     new WavFileSink for a different format.",
                    self.channels,
                    self.sample_rate,
                    buffer.channels(),
                    buffer.sample_rate()
                ),
            });
        }

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
        // Best-effort finalize on drop. If the caller never called close(),
        // this is the last chance to write the WAV header/length fields — a
        // failure here leaves a corrupt/truncated file, so log it rather than
        // swallowing it silently (audit M9). We cannot return an error from
        // Drop, but a log line gives operators a signal.
        if let Some(writer) = self.writer.take() {
            if let Err(e) = writer.finalize() {
                log::error!(
                    "WavFileSink: failed to finalize WAV file on drop (file may be \
                     corrupt; call close() explicitly to surface this error): {}",
                    e
                );
            }
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

    // ===== FH-3: fixed-format validation tests =====

    /// A buffer whose channel count differs from the header is rejected with a
    /// ConfigurationError, and the rejected write does NOT advance frames_written
    /// (it must not silently corrupt the file or miscount frames).
    #[test]
    fn wav_sink_rejects_channel_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ch_mismatch.wav");
        let format = AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
        };
        let mut sink = WavFileSink::new(&path, &format).unwrap();

        // A matching 2ch buffer writes fine.
        let ok_buf = AudioBuffer::new(vec![0.1, 0.2, 0.3, 0.4], 2, 48000); // 2 frames
        assert!(sink.write(&ok_buf).is_ok());
        assert_eq!(sink.frames_written(), 2);

        // A 1ch buffer at the same rate must be rejected.
        let mono = AudioBuffer::new(vec![0.5, 0.6, 0.7], 1, 48000);
        let err = sink.write(&mono).expect_err("channel mismatch must error");
        match err {
            AudioError::ConfigurationError { message } => {
                assert!(message.contains("2ch"), "message was: {message}");
                assert!(message.contains("1ch"), "message was: {message}");
            }
            other => panic!("expected ConfigurationError, got {other:?}"),
        }
        // The rejected write must not have advanced the frame counter.
        assert_eq!(sink.frames_written(), 2);

        sink.close().unwrap();
    }

    /// A buffer whose sample rate differs from the header is rejected with a
    /// ConfigurationError, and frames_written does not advance.
    #[test]
    fn wav_sink_rejects_sample_rate_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rate_mismatch.wav");
        let format = AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
        };
        let mut sink = WavFileSink::new(&path, &format).unwrap();

        // Correct channel count but the wrong (44.1k) sample rate.
        let wrong_rate = AudioBuffer::new(vec![0.1, 0.2], 2, 44100);
        let err = sink
            .write(&wrong_rate)
            .expect_err("sample-rate mismatch must error");
        assert!(matches!(err, AudioError::ConfigurationError { .. }));
        assert_eq!(
            sink.frames_written(),
            0,
            "rejected write must not count frames"
        );

        sink.close().unwrap();
    }

    /// The rejected mismatch error is FATAL (a ConfigurationError is a caller
    /// error, not a transient hiccup a drain loop should retry forever).
    #[test]
    fn wav_sink_mismatch_error_is_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mismatch_fatal.wav");
        let format = AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
        };
        let mut sink = WavFileSink::new(&path, &format).unwrap();
        let err = sink
            .write(&AudioBuffer::new(vec![0.0], 1, 48000))
            .expect_err("mismatch must error");
        assert!(
            err.is_fatal(),
            "format mismatch should be fatal, not recoverable"
        );
        sink.close().unwrap();
    }

    /// An empty buffer with MATCHING channels/rate is still accepted (it writes
    /// zero frames) — only a channel/rate mismatch errors, never an empty buffer.
    #[test]
    fn wav_sink_accepts_matching_empty_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("matching_empty.wav");
        let format = AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
        };
        let mut sink = WavFileSink::new(&path, &format).unwrap();
        let empty = AudioBuffer::empty(2, 48000);
        assert!(sink.write(&empty).is_ok());
        assert_eq!(sink.frames_written(), 0);
        sink.close().unwrap();
    }
}
