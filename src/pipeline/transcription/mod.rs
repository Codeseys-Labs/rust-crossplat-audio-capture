mod component;
mod config;

pub use component::{TranscriptionComponent, TranscriptionOutput};
pub use config::TranscriptionConfig;

use crate::pipeline::base::AudioChunk;

/// Represents a transcribed segment with timing
#[derive(Debug, Clone)]
pub struct TranscribedSegment {
    pub text: String,
    pub start_time: f64,
    pub end_time: f64,
    pub confidence: f32,
}

impl TranscribedSegment {
    pub fn new(text: String, start_time: f64, end_time: f64, confidence: f32) -> Self {
        Self {
            text,
            start_time,
            end_time,
            confidence,
        }
    }

    pub fn duration(&self) -> f64 {
        self.end_time - self.start_time
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }
}
