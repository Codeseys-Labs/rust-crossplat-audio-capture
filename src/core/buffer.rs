// src/core/buffer.rs

use crate::core::config::{AudioFormat, SampleFormat};
use std::time::Duration;

/// Represents a buffer of interleaved audio data.
///
/// `AudioBuffer` holds raw audio samples (always interleaved `f32` internally)
/// along with format metadata describing the sample rate, channel count, and
/// sample format. An optional timestamp marks the buffer's position relative
/// to the stream start.
///
/// # Construction
///
/// Use one of the provided constructors:
///
/// ```rust,ignore
/// // Simple constructor (defaults to F32 sample format)
/// let buf = AudioBuffer::new(samples, 2, 48000);
///
/// // From an existing AudioFormat
/// let buf = AudioBuffer::with_format(samples, format);
///
/// // With a precise timestamp
/// let buf = AudioBuffer::with_timestamp(samples, format, Duration::from_millis(100));
///
/// // Empty buffer for pre-allocation
/// let buf = AudioBuffer::empty(2, 48000);
/// ```
///
/// # Streaming-first design
///
/// `AudioBuffer` is the primary data unit flowing through the capture pipeline:
///
/// ```text
/// OS callback → ring buffer → CapturingStream::read_chunk() → AudioBuffer
/// ```
///
/// File writing is a downstream *sink adapter*, not a core concern of this type.
#[derive(Debug, Clone)]
pub struct AudioBuffer {
    /// Interleaved audio samples in f32 format.
    data: Vec<f32>,
    /// Audio format metadata (sample_rate, channels, sample_format).
    format: AudioFormat,
    /// Timestamp of the first sample (relative to stream start).
    /// `None` if no timestamp was assigned.
    timestamp: Option<Duration>,
}

impl Default for AudioBuffer {
    /// Returns an empty stereo buffer at 48 kHz with no timestamp.
    fn default() -> Self {
        Self {
            data: Vec::new(),
            format: AudioFormat::default(),
            timestamp: None,
        }
    }
}

impl AudioBuffer {
    // ── Constructors ─────────────────────────────────────────────────

    /// Creates a new `AudioBuffer` from interleaved `f32` samples.
    ///
    /// Uses [`SampleFormat::F32`] as the sample format since the data is
    /// already in `f32` representation.
    ///
    /// # Arguments
    ///
    /// * `data` — Interleaved audio samples.
    /// * `channels` — Number of audio channels (e.g. 2 for stereo).
    /// * `sample_rate` — Sample rate in Hz (e.g. 48000).
    pub fn new(data: Vec<f32>, channels: u16, sample_rate: u32) -> Self {
        Self {
            data,
            format: AudioFormat {
                sample_rate,
                channels,
                sample_format: SampleFormat::F32,
            },
            timestamp: None,
        }
    }

    /// Creates an `AudioBuffer` with a specific [`AudioFormat`].
    ///
    /// # Arguments
    ///
    /// * `data` — Interleaved audio samples.
    /// * `format` — The [`AudioFormat`] describing the buffer's metadata.
    pub fn with_format(data: Vec<f32>, format: AudioFormat) -> Self {
        Self {
            data,
            format,
            timestamp: None,
        }
    }

    /// Creates an `AudioBuffer` with a specific [`AudioFormat`] and timestamp.
    ///
    /// # Arguments
    ///
    /// * `data` — Interleaved audio samples.
    /// * `format` — The [`AudioFormat`] describing the buffer's metadata.
    /// * `timestamp` — Position of the first sample relative to stream start.
    pub fn with_timestamp(data: Vec<f32>, format: AudioFormat, timestamp: Duration) -> Self {
        Self {
            data,
            format,
            timestamp: Some(timestamp),
        }
    }

    /// Creates an empty buffer with no samples.
    ///
    /// Useful for pre-allocating a buffer structure before filling it.
    ///
    /// # Arguments
    ///
    /// * `channels` — Number of audio channels.
    /// * `sample_rate` — Sample rate in Hz.
    pub fn empty(channels: u16, sample_rate: u32) -> Self {
        Self {
            data: Vec::new(),
            format: AudioFormat {
                sample_rate,
                channels,
                sample_format: SampleFormat::F32,
            },
            timestamp: None,
        }
    }

    /// Creates an `AudioBuffer` from interleaved samples.
    ///
    /// This is an alias for [`AudioBuffer::new`] for API clarity.
    pub fn from_interleaved(samples: Vec<f32>, channels: u16, sample_rate: u32) -> Self {
        Self::new(samples, channels, sample_rate)
    }

    // ── Accessors ────────────────────────────────────────────────────

    /// Returns a reference to the interleaved sample data.
    pub fn data(&self) -> &[f32] {
        &self.data
    }

    /// Consumes the buffer and returns the raw sample data.
    pub fn into_data(self) -> Vec<f32> {
        self.data
    }

    /// Returns a reference to the [`AudioFormat`] metadata.
    pub fn format(&self) -> &AudioFormat {
        &self.format
    }

    /// Returns the number of audio channels.
    pub fn channels(&self) -> u16 {
        self.format.channels
    }

    /// Returns the sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.format.sample_rate
    }

    /// Returns the timestamp of the first sample, if set.
    pub fn timestamp(&self) -> Option<Duration> {
        self.timestamp
    }

    /// Returns a reference to the interleaved data.
    ///
    /// This is an alias for [`data()`](Self::data) for API clarity.
    pub fn interleaved(&self) -> &[f32] {
        &self.data
    }

    // ── Derived metrics ──────────────────────────────────────────────

    /// Returns the total number of interleaved samples (all channels).
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Returns `true` if the buffer contains no samples.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Returns the number of samples per channel (i.e. number of frames).
    ///
    /// For a stereo buffer with 960 total samples the result is 480.
    /// Returns 0 if `channels` is 0.
    pub fn samples_per_channel(&self) -> usize {
        let ch = self.format.channels as usize;
        if ch == 0 {
            0
        } else {
            self.data.len() / ch
        }
    }

    /// Returns the number of audio frames in the buffer.
    ///
    /// A frame consists of one sample for each channel.
    /// This is equivalent to [`samples_per_channel`](Self::samples_per_channel).
    pub fn num_frames(&self) -> usize {
        self.samples_per_channel()
    }

    /// Returns the duration of audio contained in this buffer.
    ///
    /// Calculated from the frame count and sample rate:
    /// `duration = frames / sample_rate`.
    ///
    /// Returns [`Duration::ZERO`] if the sample rate is 0 or the buffer is empty.
    pub fn duration(&self) -> Duration {
        let sr = self.format.sample_rate;
        if sr == 0 {
            return Duration::ZERO;
        }
        let frames = self.samples_per_channel() as u64;
        let nanos = frames * 1_000_000_000 / sr as u64;
        Duration::from_nanos(nanos)
    }

    // ── Channel extraction ───────────────────────────────────────────

    /// Extracts the samples for a single channel (0-indexed).
    ///
    /// Returns `None` if `channel` is out of range.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// // Get left channel from stereo buffer
    /// let left = buffer.channel_data(0).unwrap();
    /// ```
    pub fn channel_data(&self, channel: u16) -> Option<Vec<f32>> {
        let ch = self.format.channels as usize;
        if ch == 0 || channel as usize >= ch {
            return None;
        }
        let channel_idx = channel as usize;
        let samples: Vec<f32> = self
            .data
            .iter()
            .skip(channel_idx)
            .step_by(ch)
            .copied()
            .collect();
        Some(samples)
    }

    // ── Slice compatibility (kept for backward compat) ───────────────

    /// Returns a slice view of the audio data.
    ///
    /// This is an alias for [`data()`](Self::data) kept for backward
    /// compatibility.
    pub fn as_slice(&self) -> &[f32] {
        &self.data
    }

    /// Returns a mutable slice view of the audio data.
    pub fn as_mut_slice(&mut self) -> &mut [f32] {
        &mut self.data
    }
}

// AudioBuffer only contains Vec<f32>, AudioFormat, and Option<Duration>,
// all of which are Send + Sync, so auto-derive is sufficient.
// Explicit impls below kept for clarity and to match the old code.
unsafe impl Send for AudioBuffer {}
unsafe impl Sync for AudioBuffer {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_basic() {
        let buf = AudioBuffer::new(vec![1.0, 2.0, 3.0, 4.0], 2, 48000);
        assert_eq!(buf.channels(), 2);
        assert_eq!(buf.sample_rate(), 48000);
        assert_eq!(buf.len(), 4);
        assert_eq!(buf.samples_per_channel(), 2);
        assert!(!buf.is_empty());
        assert!(buf.timestamp().is_none());
    }

    #[test]
    fn empty_buffer() {
        let buf = AudioBuffer::empty(2, 44100);
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.samples_per_channel(), 0);
        assert_eq!(buf.channels(), 2);
        assert_eq!(buf.sample_rate(), 44100);
    }

    #[test]
    fn with_format_and_timestamp() {
        let fmt = AudioFormat {
            sample_rate: 44100,
            channels: 1,
            sample_format: SampleFormat::F32,
        };
        let ts = Duration::from_millis(500);
        let buf = AudioBuffer::with_timestamp(vec![0.5; 44100], fmt, ts);
        assert_eq!(buf.timestamp(), Some(ts));
        assert_eq!(buf.samples_per_channel(), 44100);
        assert_eq!(buf.channels(), 1);
    }

    #[test]
    fn duration_calculation() {
        // 48000 frames at 48000 Hz = exactly 1 second
        let buf = AudioBuffer::new(vec![0.0; 48000 * 2], 2, 48000);
        assert_eq!(buf.duration(), Duration::from_secs(1));
    }

    #[test]
    fn channel_data_extraction() {
        // Interleaved stereo: L R L R
        let buf = AudioBuffer::new(vec![1.0, 2.0, 3.0, 4.0], 2, 48000);
        assert_eq!(buf.channel_data(0), Some(vec![1.0, 3.0])); // Left
        assert_eq!(buf.channel_data(1), Some(vec![2.0, 4.0])); // Right
        assert_eq!(buf.channel_data(2), None); // Out of range
    }

    #[test]
    fn default_buffer() {
        let buf = AudioBuffer::default();
        assert!(buf.is_empty());
        assert_eq!(buf.channels(), 2);
        assert_eq!(buf.sample_rate(), 48000);
    }

    #[test]
    fn into_data_consumes() {
        let buf = AudioBuffer::new(vec![1.0, 2.0], 1, 48000);
        let data = buf.into_data();
        assert_eq!(data, vec![1.0, 2.0]);
    }

    #[test]
    fn num_frames_matches_samples_per_channel() {
        let buf = AudioBuffer::new(vec![0.0; 960], 2, 48000);
        assert_eq!(buf.num_frames(), 480);
        assert_eq!(buf.num_frames(), buf.samples_per_channel());
    }
}
