use crate::core::config::AudioFormat;
use std::time::Duration;

/// Represents a buffer of audio data.
///
/// This struct holds raw audio samples along with metadata such as the number of channels,
/// sample rate, audio format, and a timestamp indicating when the buffer was captured or created
/// (relative to an epoch).
#[derive(Debug, Clone, Send, Sync)]
pub struct AudioBuffer {
    /// The raw audio data, typically interleaved if multi-channel.
    /// Using `f32` as the default data type for samples.
    pub data: Vec<f32>,
    /// The number of audio channels (e.g., 1 for mono, 2 for stereo).
    pub channels: u16,
    /// The sample rate of the audio data in Hz (e.g., 44100, 48000).
    pub sample_rate: u32,
    /// The format of the audio samples.
    pub format: AudioFormat,
    /// The timestamp indicating when this buffer was captured or generated,
    /// relative to a consistent epoch (e.g., application start time or stream start time).
    pub timestamp: Duration,
}

impl AudioBuffer {
    /// Creates a new `AudioBuffer`.
    ///
    /// # Arguments
    ///
    /// * `data` - The vector of f32 audio samples.
    /// * `channels` - The number of audio channels.
    /// * `sample_rate` - The sample rate in Hz.
    /// * `format` - The `AudioFormat` of the samples.
    /// * `timestamp` - The `Duration` representing the capture time relative to an epoch.
    pub fn new(
        data: Vec<f32>,
        channels: u16,
        sample_rate: u32,
        format: AudioFormat,
        timestamp: Duration,
    ) -> Self {
        AudioBuffer {
            data,
            channels,
            sample_rate,
            format,
            timestamp,
        }
    }

    /// Returns a slice view of the raw audio data.
    pub fn data(&self) -> &[f32] {
        &self.data
    }

    /// Returns the number of audio channels.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Returns the sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Returns the audio format.
    pub fn format(&self) -> &AudioFormat {
        &self.format
    }

    /// Returns the timestamp of the buffer.
    pub fn timestamp(&self) -> Duration {
        self.timestamp
    }

    /// Returns the number of audio frames in the buffer.
    ///
    /// A frame consists of one sample for each channel.
    /// For example, in a stereo (2-channel) buffer, one frame contains two f32 samples.
    pub fn num_frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.data.len() / (self.channels as usize)
        }
    }

    // Placeholder for future methods if needed, e.g., to_interleaved, to_planar, etc.
}

// Comments indicate that VecAudioBuffer and the old AudioBuffer trait
// were already removed or planned for removal, aligning with this refactoring.
