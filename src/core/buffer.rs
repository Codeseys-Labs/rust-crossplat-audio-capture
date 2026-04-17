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
        self.data
            .len()
            .checked_div(self.format.channels as usize)
            .unwrap_or(0)
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

    // ── Additional tests ────────────────────────────────────────────

    #[test]
    fn from_interleaved_alias() {
        let data = vec![1.0, 2.0, 3.0, 4.0];
        let buf_new = AudioBuffer::new(data.clone(), 2, 48000);
        let buf_alias = AudioBuffer::from_interleaved(data, 2, 48000);
        assert_eq!(buf_new.data(), buf_alias.data());
        assert_eq!(buf_new.channels(), buf_alias.channels());
        assert_eq!(buf_new.sample_rate(), buf_alias.sample_rate());
        assert_eq!(buf_new.format(), buf_alias.format());
    }

    #[test]
    fn with_format_constructor() {
        let fmt = AudioFormat {
            sample_rate: 96000,
            channels: 6,
            sample_format: SampleFormat::I32,
        };
        let buf = AudioBuffer::with_format(vec![0.0; 12], fmt.clone());
        assert_eq!(buf.format(), &fmt);
        assert_eq!(buf.channels(), 6);
        assert_eq!(buf.sample_rate(), 96000);
        assert!(buf.timestamp().is_none());
    }

    #[test]
    fn interleaved_returns_data_ref() {
        let buf = AudioBuffer::new(vec![1.0, 2.0, 3.0], 1, 44100);
        assert_eq!(
            buf.interleaved() as *const [f32],
            buf.data() as *const [f32],
        );
    }

    #[test]
    fn as_slice_returns_data_ref() {
        let buf = AudioBuffer::new(vec![5.0, 6.0], 1, 44100);
        assert_eq!(buf.as_slice() as *const [f32], buf.data() as *const [f32],);
    }

    #[test]
    fn as_mut_slice_allows_modification() {
        let mut buf = AudioBuffer::new(vec![1.0, 2.0, 3.0, 4.0], 2, 48000);
        let slice = buf.as_mut_slice();
        slice[0] = 10.0;
        slice[3] = 40.0;
        assert_eq!(buf.data()[0], 10.0);
        assert_eq!(buf.data()[3], 40.0);
        assert_eq!(buf.data()[1], 2.0);
    }

    #[test]
    fn format_accessor() {
        let fmt = AudioFormat {
            sample_rate: 22050,
            channels: 1,
            sample_format: SampleFormat::I16,
        };
        let buf = AudioBuffer::with_format(vec![0.0; 100], fmt.clone());
        assert_eq!(*buf.format(), fmt);
    }

    #[test]
    fn zero_channels_samples_per_channel() {
        let buf = AudioBuffer::new(vec![1.0, 2.0, 3.0], 0, 48000);
        assert_eq!(buf.samples_per_channel(), 0);
    }

    #[test]
    fn zero_sample_rate_duration() {
        let buf = AudioBuffer::new(vec![0.0; 100], 1, 0);
        assert_eq!(buf.duration(), Duration::ZERO);
    }

    #[test]
    fn channel_data_zero_channels() {
        let buf = AudioBuffer::new(vec![1.0, 2.0], 0, 48000);
        assert_eq!(buf.channel_data(0), None);
    }

    #[test]
    fn send_sync_traits() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AudioBuffer>();
    }

    #[test]
    fn clone_produces_independent_copy() {
        let original = AudioBuffer::new(vec![1.0, 2.0, 3.0, 4.0], 2, 48000);
        let mut cloned = original.clone();
        cloned.as_mut_slice()[0] = 99.0;
        assert_eq!(original.data()[0], 1.0);
        assert_eq!(cloned.data()[0], 99.0);
    }

    #[test]
    fn large_buffer_duration() {
        // 10 seconds of 48 kHz stereo audio
        let num_samples = 48000 * 2 * 10;
        let buf = AudioBuffer::new(vec![0.0; num_samples], 2, 48000);
        assert_eq!(buf.duration(), Duration::from_secs(10));
        assert_eq!(buf.samples_per_channel(), 480000);
        assert_eq!(buf.num_frames(), 480000);
    }

    #[test]
    fn mono_buffer_operations() {
        let buf = AudioBuffer::new(vec![0.1, 0.2, 0.3, 0.4, 0.5], 1, 44100);
        assert_eq!(buf.channels(), 1);
        assert_eq!(buf.samples_per_channel(), 5);
        let ch0 = buf.channel_data(0).expect("channel 0 should exist");
        assert_eq!(ch0, vec![0.1, 0.2, 0.3, 0.4, 0.5]);
        assert_eq!(buf.channel_data(1), None);
    }

    #[test]
    fn timestamp_none_by_default() {
        let buf_new = AudioBuffer::new(vec![1.0], 1, 48000);
        assert!(buf_new.timestamp().is_none());

        let buf_empty = AudioBuffer::empty(2, 44100);
        assert!(buf_empty.timestamp().is_none());
    }

    // ===== K5.4: AudioBuffer Edge Case Tests =====

    #[test]
    fn single_sample_buffer() {
        let buf = AudioBuffer::new(vec![0.5], 1, 44100);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf.num_frames(), 1);
        assert!(!buf.is_empty());
        assert_eq!(buf.data(), &[0.5]);
        assert_eq!(buf.channels(), 1);
    }

    #[test]
    fn single_sample_stereo_buffer() {
        // 1 sample with 2 channels = 0 complete frames (integer division)
        let buf = AudioBuffer::new(vec![0.5], 2, 44100);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf.num_frames(), 0); // 1 / 2 = 0
        assert!(!buf.is_empty()); // has data, even if incomplete frame
    }

    #[test]
    fn odd_sample_count_not_divisible_by_channels() {
        // 7 samples with 2 channels = 3 complete frames (7 / 2 = 3)
        let buf = AudioBuffer::new(vec![1.0; 7], 2, 48000);
        assert_eq!(buf.len(), 7);
        assert_eq!(buf.num_frames(), 3); // integer division
        assert_eq!(buf.samples_per_channel(), 3);
    }

    #[test]
    fn high_channel_count_buffer() {
        // 32 channels with 1 frame
        let data: Vec<f32> = (0..32).map(|i| i as f32 / 32.0).collect();
        let buf = AudioBuffer::new(data.clone(), 32, 48000);
        assert_eq!(buf.channels(), 32);
        assert_eq!(buf.num_frames(), 1);
        assert_eq!(buf.len(), 32);
        // Extract each channel
        for ch in 0..32u16 {
            let ch_data = buf.channel_data(ch);
            assert!(ch_data.is_some(), "Channel {ch} should exist");
            assert_eq!(ch_data.unwrap().len(), 1);
        }
        // Channel 32 should be out of range (0-indexed, so 32 channels = 0..31)
        assert!(buf.channel_data(32).is_none());
    }

    #[test]
    fn duration_precision_for_standard_rates() {
        // 48000 samples at 48kHz = exactly 1 second
        let buf = AudioBuffer::new(vec![0.0; 48000], 1, 48000);
        assert_eq!(buf.duration(), std::time::Duration::from_secs(1));

        // 44100 samples at 44100Hz = exactly 1 second
        let buf2 = AudioBuffer::new(vec![0.0; 44100], 1, 44100);
        assert_eq!(buf2.duration(), std::time::Duration::from_secs(1));
    }

    #[test]
    fn duration_for_stereo_buffer() {
        // 96000 samples, 2 channels, 48kHz = 1 second (96000 / 2 / 48000)
        let buf = AudioBuffer::new(vec![0.0; 96000], 2, 48000);
        assert_eq!(buf.num_frames(), 48000);
        assert_eq!(buf.duration(), std::time::Duration::from_secs(1));
    }

    #[test]
    fn empty_buffer_all_methods_safe() {
        let buf = AudioBuffer::empty(2, 48000);
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.num_frames(), 0);
        assert_eq!(buf.samples_per_channel(), 0);
        assert_eq!(buf.duration(), std::time::Duration::ZERO);
        assert_eq!(buf.data(), &[] as &[f32]);
        assert_eq!(buf.as_slice(), &[] as &[f32]);
        assert_eq!(buf.interleaved(), &[] as &[f32]);
        assert!(buf.channel_data(0).is_some()); // channel exists but empty
        assert!(buf.channel_data(2).is_none()); // out of range
        assert_eq!(buf.channels(), 2);
        assert_eq!(buf.sample_rate(), 48000);
        assert!(buf.timestamp().is_none());
    }

    #[test]
    fn default_buffer_is_empty() {
        let buf = AudioBuffer::default();
        assert!(buf.is_empty());
        assert_eq!(buf.channels(), 2);
        assert_eq!(buf.sample_rate(), 48000);
        assert_eq!(buf.format().sample_format, SampleFormat::F32);
    }

    #[test]
    fn with_timestamp_preserves_all_fields() {
        let format = AudioFormat {
            sample_rate: 96000,
            channels: 4,
            sample_format: SampleFormat::I24,
        };
        let ts = std::time::Duration::from_millis(1500);
        let buf = AudioBuffer::with_timestamp(vec![1.0, 2.0, 3.0, 4.0], format.clone(), ts);
        assert_eq!(buf.sample_rate(), 96000);
        assert_eq!(buf.channels(), 4);
        assert_eq!(buf.format().sample_format, SampleFormat::I24);
        assert_eq!(buf.timestamp(), Some(ts));
        assert_eq!(buf.data(), &[1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn as_mut_slice_full_modification() {
        let mut buf = AudioBuffer::new(vec![0.0, 0.0, 0.0, 0.0], 2, 48000);
        let slice = buf.as_mut_slice();
        slice[0] = 1.0;
        slice[1] = -1.0;
        slice[2] = 0.5;
        slice[3] = -0.5;
        assert_eq!(buf.data(), &[1.0, -1.0, 0.5, -0.5]);
    }

    #[test]
    fn into_data_consumes_buffer() {
        let original_data = vec![0.1, 0.2, 0.3];
        let buf = AudioBuffer::new(original_data.clone(), 1, 44100);
        let recovered = buf.into_data();
        assert_eq!(recovered, original_data);
        // buf is consumed — can't use it anymore (compile-time guarantee)
    }

    #[test]
    fn channel_data_extracts_correct_interleaved_samples() {
        // Stereo: L R L R L R
        let data = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let buf = AudioBuffer::new(data, 2, 48000);

        let left = buf.channel_data(0).unwrap();
        assert_eq!(left, vec![1.0, 3.0, 5.0]);

        let right = buf.channel_data(1).unwrap();
        assert_eq!(right, vec![2.0, 4.0, 6.0]);
    }

    #[test]
    fn channel_data_for_quad_channel() {
        // 4 channels: C0 C1 C2 C3 C0 C1 C2 C3
        let data = vec![10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0];
        let buf = AudioBuffer::new(data, 4, 48000);
        assert_eq!(buf.num_frames(), 2);

        assert_eq!(buf.channel_data(0).unwrap(), vec![10.0, 50.0]);
        assert_eq!(buf.channel_data(1).unwrap(), vec![20.0, 60.0]);
        assert_eq!(buf.channel_data(2).unwrap(), vec![30.0, 70.0]);
        assert_eq!(buf.channel_data(3).unwrap(), vec![40.0, 80.0]);
    }

    #[test]
    fn buffer_with_nan_and_infinity() {
        // AudioBuffer should handle any f32 value without panicking
        let data = vec![f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0];
        let buf = AudioBuffer::new(data, 2, 48000);
        assert_eq!(buf.len(), 4);
        assert_eq!(buf.num_frames(), 2);
        assert!(buf.data()[0].is_nan());
        assert!(buf.data()[1].is_infinite());
        // Duration should still compute without panic
        let _ = buf.duration();
    }

    #[test]
    fn buffer_with_extreme_values() {
        let data = vec![f32::MAX, f32::MIN, f32::MIN_POSITIVE, f32::EPSILON];
        let buf = AudioBuffer::new(data.clone(), 1, 48000);
        assert_eq!(buf.data(), &data[..]);
    }

    #[test]
    fn clone_is_independent() {
        let mut buf1 = AudioBuffer::new(vec![1.0, 2.0], 1, 48000);
        let buf2 = buf1.clone();
        buf1.as_mut_slice()[0] = 99.0;
        assert_eq!(buf1.data()[0], 99.0);
        assert_eq!(buf2.data()[0], 1.0); // clone is independent
    }

    #[test]
    fn debug_format_is_nonempty() {
        let buf = AudioBuffer::new(vec![1.0], 1, 48000);
        let debug = format!("{buf:?}");
        assert!(!debug.is_empty());
        assert!(debug.contains("AudioBuffer"));
    }

    #[test]
    fn with_format_non_f32_sample_format() {
        // Buffer stores f32 internally regardless of declared format
        let format = AudioFormat {
            sample_rate: 44100,
            channels: 1,
            sample_format: SampleFormat::I16,
        };
        let buf = AudioBuffer::with_format(vec![0.5, -0.5], format);
        assert_eq!(buf.format().sample_format, SampleFormat::I16);
        assert_eq!(buf.data(), &[0.5, -0.5]); // data is still f32
    }
}
