//! Real-time Speech Analysis and Capture (RSAC)
//!
//! This library provides functionality for real-time audio processing,
//! including speaker diarization and speech transcription.

pub mod audio;
pub mod models;
pub mod pipeline;

// Re-export main types for easier access
pub use pipeline::{
    create_diarization_transcription_pipeline,
    // Diarization types
    diarization::{DiarizationComponent, DiarizationConfig, SpeakerSegment},

    process_audio,
    // Transcription types
    transcription::{TranscribedSegment, TranscriptionComponent, TranscriptionConfig},

    // Core pipeline types
    AudioChunk,
    // Combined types and utilities
    CombinedSegment,
    ComponentConfig,
    Pipeline,
    PipelineComponent,
};

pub use models::ModelHub;

/// Error type for the library
pub type Error = color_eyre::Report;
/// Result type for the library
pub type Result<T> = std::result::Result<T, Error>;

/// Initialize the library
pub fn init() -> Result<()> {
    color_eyre::install()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_library_initialization() {
        assert!(init().is_ok());
    }
}
