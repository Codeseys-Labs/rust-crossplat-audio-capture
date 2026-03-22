//! Audio capture and processing pipeline.
//!
//! This module manages audio capture via rsac and the pre-processing pipeline
//! (resampling, VAD, speech buffering) before passing utterances to ASR.

pub mod capture;
pub mod pipeline;
pub mod vad;

pub use capture::{AudioCaptureManager, AudioChunk};
pub use pipeline::ProcessedAudioChunk;
pub use vad::{SpeechSegment, VadProcessor};
