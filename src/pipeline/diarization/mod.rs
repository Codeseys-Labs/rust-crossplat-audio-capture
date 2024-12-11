mod component;
mod config;

pub use component::{DiarizationComponent, DiarizationOutput};
pub use config::DiarizationConfig;

use crate::pipeline::base::AudioChunk;

/// Represents a speaker segment from diarization
#[derive(Debug, Clone)]
pub struct SpeakerSegment {
    pub speaker_id: i32,
    pub start_time: f64,
    pub end_time: f64,
    pub confidence: f32,
}

impl SpeakerSegment {
    pub fn new(speaker_id: i32, start_time: f64, end_time: f64, confidence: f32) -> Self {
        Self {
            speaker_id,
            start_time,
            end_time,
            confidence,
        }
    }

    pub fn duration(&self) -> f64 {
        self.end_time - self.start_time
    }

    pub fn overlaps_with(&self, other: &SpeakerSegment) -> bool {
        self.start_time < other.end_time && self.end_time > other.start_time
    }
}
