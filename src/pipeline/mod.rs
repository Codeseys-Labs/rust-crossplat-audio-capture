pub mod base;
pub mod diarization;
pub mod transcription;

// Re-export main types
pub use base::{AudioChunk, ComponentConfig, Pipeline, PipelineComponent};
pub use diarization::{DiarizationComponent, DiarizationConfig, SpeakerSegment};
pub use transcription::{TranscribedSegment, TranscriptionComponent, TranscriptionConfig};

/// Combined output from diarization and transcription
#[derive(Debug, Clone)]
pub struct CombinedSegment {
    pub text: String,
    pub speaker_id: i32,
    pub start_time: f64,
    pub end_time: f64,
    pub confidence: f32,
}

impl CombinedSegment {
    pub fn new(
        text: String,
        speaker_id: i32,
        start_time: f64,
        end_time: f64,
        confidence: f32,
    ) -> Self {
        Self {
            text,
            speaker_id,
            start_time,
            end_time,
            confidence,
        }
    }
}

/// Helper to build a pipeline with diarization and transcription
pub async fn create_diarization_transcription_pipeline(
    diarization_config: DiarizationConfig,
    transcription_config: TranscriptionConfig,
    buffer_size: usize,
) -> Result<Pipeline, color_eyre::Report> {
    Pipeline::builder()
        .add_stage::<DiarizationComponent>(diarization_config)?
        .add_stage::<TranscriptionComponent>(transcription_config)?
        .build()
        .await
}

/// Helper to process audio through both diarization and transcription
pub async fn process_audio(
    audio: AudioChunk,
    diarizer: &mut DiarizationComponent,
    transcriber: &mut TranscriptionComponent,
) -> Result<Vec<CombinedSegment>, color_eyre::Report> {
    // Process through both components
    let diarization = diarizer.process(audio.clone()).await?;
    let transcription = transcriber.process(audio).await?;

    // Combine results
    let mut combined = Vec::new();

    for trans_seg in transcription.segments {
        // Find overlapping diarization segment
        if let Some(diar_seg) = diarization.segments.iter().find(|d_seg| {
            trans_seg.start_time >= d_seg.start_time && trans_seg.end_time <= d_seg.end_time
        }) {
            combined.push(CombinedSegment::new(
                trans_seg.text,
                diar_seg.speaker_id,
                trans_seg.start_time,
                trans_seg.end_time,
                (trans_seg.confidence + diar_seg.confidence) / 2.0,
            ));
        } else {
            // No speaker found, use default
            combined.push(CombinedSegment::new(
                trans_seg.text,
                -1,
                trans_seg.start_time,
                trans_seg.end_time,
                trans_seg.confidence,
            ));
        }
    }

    Ok(combined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_pipeline_creation() {
        let temp_dir = tempdir().unwrap();

        // Create dummy model files
        let segment_model = temp_dir.path().join("segment_model.onnx");
        let embedding_model = temp_dir.path().join("embedding_model.onnx");
        let whisper_model = temp_dir.path().join("whisper-base.bin");

        File::create(&segment_model)
            .unwrap()
            .write_all(b"dummy")
            .unwrap();
        File::create(&embedding_model)
            .unwrap()
            .write_all(b"dummy")
            .unwrap();
        File::create(&whisper_model)
            .unwrap()
            .write_all(b"dummy")
            .unwrap();

        let diar_config = DiarizationConfig {
            segment_model_path: segment_model.to_str().unwrap().to_string(),
            embedding_model_path: embedding_model.to_str().unwrap().to_string(),
            ..Default::default()
        };

        let trans_config = TranscriptionConfig {
            model_path: whisper_model.to_str().unwrap().to_string(),
            ..Default::default()
        };

        let pipeline =
            create_diarization_transcription_pipeline(diar_config, trans_config, 32).await;

        assert!(pipeline.is_ok());
    }
}
