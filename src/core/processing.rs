//! [`AudioProcessor`] trait for in-flight audio transformations.
//!
//! This is an extension point: consumers that want to chain effects or
//! analysis passes on top of [`AudioBuffer`]s can implement
//! [`AudioProcessor`] and feed it from any
//! [`CapturingStream`](crate::core::interface::CapturingStream).
//!
//! rsac itself does not bundle DSP (mixing, resampling, VAD, etc.); see
//! [`VISION.md`](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/blob/master/VISION.md)
//! "What's Out of Scope" for the list of downstream crates to reach for.

use crate::core::buffer::AudioBuffer; // This will be the new struct
use crate::core::error::ProcessError;

/// A trait for types that can process audio data.
///
/// Implementors of `AudioProcessor` can be used to apply effects,
/// analyze audio, or perform other operations on audio streams.
pub trait AudioProcessor: Send + 'static {
    /// Processes the given audio buffer.
    ///
    /// # Arguments
    ///
    /// * `buffer`: A reference to the `AudioBuffer` containing the audio data to process.
    ///
    /// # Returns
    ///
    /// A `Result` indicating success or a `ProcessError` if processing fails.
    fn process(&mut self, buffer: &AudioBuffer) -> Result<(), ProcessError>;
}

// AudioFormat is defined in crate::core::config
