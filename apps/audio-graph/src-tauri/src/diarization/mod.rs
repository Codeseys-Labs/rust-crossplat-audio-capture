//! Speaker diarization module.
//!
//! Uses pyannote-rs (ONNX models) for speaker segmentation and embedding
//! extraction. Maintains a speaker registry for tracking and re-identifying
//! speakers across segments.

/// Diarization worker that assigns speaker labels to audio segments.
#[allow(dead_code)]
pub struct DiarizationWorker {
    // TODO: Fields will be added when ort is integrated:
    // segment_rx: crossbeam_channel::Receiver<AudioSegment>,
    // speaker_tx: crossbeam_channel::Sender<SpeakerAssignment>,
    // segmentation_model: ort::Session,
    // embedding_model: ort::Session,
    // speaker_registry: SpeakerRegistry,
    _placeholder: (),
}

impl DiarizationWorker {
    /// Create a new diarization worker.
    pub fn new() -> Self {
        // TODO: Load pyannote segmentation + embedding ONNX models
        // TODO: Initialize speaker registry with configured threshold
        Self { _placeholder: () }
    }

    /// Run the diarization processing loop (blocking, should be spawned in a thread).
    pub fn run(&mut self) {
        // TODO: Main loop:
        // 1. Receive AudioSegment from segment_rx
        // 2. Run segmentation model to detect speaker changes
        // 3. Extract speaker embeddings
        // 4. Match against speaker registry (cosine similarity)
        // 5. Send SpeakerAssignment to speaker_tx
        log::info!("DiarizationWorker::run() stub — not yet implemented");
    }
}

impl Default for DiarizationWorker {
    fn default() -> Self {
        Self::new()
    }
}
