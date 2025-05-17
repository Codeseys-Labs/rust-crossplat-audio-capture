// src/core/processing.rs

use crate::core::buffer::AudioBuffer;
use crate::core::error::ProcessError;

/// A trait for types that can process audio data.
///
/// Implementors of `AudioProcessor` can be used to apply various
/// effects, analyses, or transformations to an `AudioBuffer`.
pub trait AudioProcessor: Send + Sync + 'static {
    /// Processes the given audio buffer.
    ///
    /// # Arguments
    ///
    /// * `buffer` - A reference to the `AudioBuffer` to be processed.
    ///
    /// # Returns
    ///
    /// A `Result` indicating success or a `ProcessError` if processing fails.
    fn process(&mut self, buffer: &AudioBuffer) -> Result<(), ProcessError>;
}
