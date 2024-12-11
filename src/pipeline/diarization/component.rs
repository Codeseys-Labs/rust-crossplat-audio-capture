use crate::pipeline::base::{AudioChunk, ComponentConfig, PipelineComponent};
use crate::pipeline::diarization::{DiarizationConfig, SpeakerSegment};
use async_trait::async_trait;
use color_eyre::Result;
use sherpa_rs::diarize::{Diarize, DiarizeConfig as SherpaDiarizeConfig};
use std::collections::VecDeque;

/// Output from the diarization component
#[derive(Debug, Clone)]
pub struct DiarizationOutput {
    pub segments: Vec<SpeakerSegment>,
    pub timestamp: f64,
}

/// Component for speaker diarization
pub struct DiarizationComponent {
    config: DiarizationConfig,
    buffer: VecDeque<f32>,
    last_timestamp: f64,
    diarizer: Diarize,
}

#[async_trait]
impl PipelineComponent for DiarizationComponent {
    type Input = AudioChunk;
    type Output = DiarizationOutput;
    type Config = DiarizationConfig;

    async fn initialize(config: Self::Config) -> Result<Self> {
        // Validate configuration
        config.validate()?;

        // Initialize sherpa diarizer
        let sherpa_config = SherpaDiarizeConfig {
            num_clusters: Some(config.max_speakers),
            ..Default::default()
        };

        let progress_callback = |n_computed_chunks: i32, n_total_chunks: i32| -> i32 {
            let progress = 100 * n_computed_chunks / n_total_chunks;
            println!("🗣️ Diarizing... {}% 🎯", progress);
            0
        };

        let diarizer = Diarize::new(
            &config.segment_model_path,
            &config.embedding_model_path,
            sherpa_config,
        )?;

        Ok(Self {
            config,
            buffer: VecDeque::new(),
            last_timestamp: 0.0,
            diarizer,
        })
    }

    async fn process(&mut self, input: Self::Input) -> Result<Self::Output> {
        // Add new samples to buffer
        self.buffer.extend(input.samples.iter());

        // Process samples if we have enough data
        let segments = if self.buffer.len() >= self.config.sample_rate as usize {
            // Convert buffer to Vec for processing
            let samples: Vec<f32> = self.buffer.drain(..).collect();

            // Process audio through sherpa diarizer
            let progress_callback = |n_computed_chunks: i32, n_total_chunks: i32| -> i32 {
                let progress = 100 * n_computed_chunks / n_total_chunks;
                println!("🗣️ Diarizing chunk... {}% 🎯", progress);
                0
            };

            let diarization_result = self
                .diarizer
                .compute(samples, Some(Box::new(progress_callback)))?;

            // Convert sherpa segments to our SpeakerSegment format
            diarization_result
                .iter()
                .map(|segment| {
                    SpeakerSegment::new(
                        segment.speaker as i32,
                        self.last_timestamp + segment.start as f64,
                        self.last_timestamp + segment.end as f64,
                        1.0, // Sherpa doesn't provide confidence scores
                    )
                })
                .collect()
        } else {
            Vec::new()
        };

        // Update timestamp
        self.last_timestamp = input.timestamp;

        Ok(DiarizationOutput {
            segments,
            timestamp: input.timestamp,
        })
    }

    async fn reset(&mut self) -> Result<()> {
        self.buffer.clear();
        self.last_timestamp = 0.0;
        Ok(())
    }
}

impl DiarizationComponent {
    /// Merge overlapping segments from the same speaker
    fn merge_segments(&self, mut segments: Vec<SpeakerSegment>) -> Vec<SpeakerSegment> {
        segments.sort_by(|a, b| a.start_time.partial_cmp(&b.start_time).unwrap());

        let mut merged = Vec::new();
        let mut current_opt = None;

        for segment in segments {
            match current_opt {
                None => {
                    current_opt = Some(segment);
                }
                Some(current) => {
                    if current.speaker_id == segment.speaker_id
                        && segment.start_time - current.end_time <= self.config.overlap_threshold
                    {
                        // Merge segments
                        current_opt = Some(SpeakerSegment::new(
                            current.speaker_id,
                            current.start_time,
                            segment.end_time,
                            (current.confidence + segment.confidence) / 2.0,
                        ));
                    } else {
                        // Add current to merged and start new segment
                        merged.push(current);
                        current_opt = Some(segment);
                    }
                }
            }
        }

        if let Some(last) = current_opt {
            merged.push(last);
        }

        merged
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_component_initialization() {
        let temp_dir = tempdir().unwrap();
        let segment_model_path = temp_dir.path().join("segment_model.onnx");
        let embedding_model_path = temp_dir.path().join("embedding_model.onnx");

        File::create(&segment_model_path)
            .unwrap()
            .write_all(b"dummy")
            .unwrap();
        File::create(&embedding_model_path)
            .unwrap()
            .write_all(b"dummy")
            .unwrap();

        let config = DiarizationConfig {
            segment_model_path: segment_model_path.to_str().unwrap().to_string(),
            embedding_model_path: embedding_model_path.to_str().unwrap().to_string(),
            ..Default::default()
        };

        let component = DiarizationComponent::initialize(config).await;
        assert!(component.is_ok());
    }

    #[tokio::test]
    async fn test_basic_processing() {
        let temp_dir = tempdir().unwrap();
        let segment_model_path = temp_dir.path().join("segment_model.onnx");
        let embedding_model_path = temp_dir.path().join("embedding_model.onnx");

        File::create(&segment_model_path)
            .unwrap()
            .write_all(b"dummy")
            .unwrap();
        File::create(&embedding_model_path)
            .unwrap()
            .write_all(b"dummy")
            .unwrap();

        let config = DiarizationConfig {
            segment_model_path: segment_model_path.to_str().unwrap().to_string(),
            embedding_model_path: embedding_model_path.to_str().unwrap().to_string(),
            max_speakers: 2,
            ..Default::default()
        };

        let mut component = DiarizationComponent::initialize(config).await.unwrap();

        // Test with enough samples for processing
        let chunk = AudioChunk::new(
            vec![0.0; 16000], // 1 second of audio at 16kHz
            0.0,
            16000,
        );

        let result = component.process(chunk).await.unwrap();
        assert!(!result.segments.is_empty());

        // Process another chunk
        let chunk = AudioChunk::new(vec![0.0; 16000], 1.0, 16000);
        let result = component.process(chunk).await.unwrap();
        assert!(!result.segments.is_empty());
    }
}
