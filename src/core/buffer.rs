use crate::core::config::AudioFormat;
// Removed: use crate::core::error::AudioResult;
// Removed: use crate::core::interface::AudioBuffer; // This trait is being replaced by the struct below
use std::time::Instant;

/// Represents a buffer of audio data.
///
/// This struct holds raw audio samples along with metadata such as the number of channels,
/// sample rate, audio format, and a timestamp indicating when the buffer was captured or created.
#[derive(Debug, Clone)]
pub struct AudioBuffer {
    /// The raw audio data, typically interleaved if multi-channel.
    pub data: Vec<f32>,
    /// The number of audio channels (e.g., 1 for mono, 2 for stereo).
    pub channels: u16,
    /// The sample rate of the audio data in Hz (e.g., 44100, 48000).
    pub sample_rate: u32,
    /// The format of the audio samples (e.g., F32, S16).
    pub format: AudioFormat,
    /// The timestamp indicating when this buffer was captured or generated.
    pub timestamp: Instant,
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
    /// * `timestamp` - The `Instant` the buffer was created/captured.
    pub fn new(
        data: Vec<f32>,
        channels: u16,
        sample_rate: u32,
        format: AudioFormat,
        timestamp: Instant,
    ) -> Self {
        AudioBuffer {
            data,
            channels,
            sample_rate,
            format,
            timestamp,
        }
    }

    /// Returns a slice view of the audio data.
    pub fn as_slice(&self) -> &[f32] {
        &self.data
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
    // Or methods to get duration, etc.
}

// VecAudioBuffer and its impl AudioBuffer for VecAudioBuffer have been removed
// as AudioBuffer is now a concrete struct.
