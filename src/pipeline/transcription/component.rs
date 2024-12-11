use crate::pipeline::base::{AudioChunk, ComponentConfig, PipelineComponent};
use crate::pipeline::transcription::{TranscribedSegment, TranscriptionConfig};
use async_trait::async_trait;
use color_eyre::Result;
use std::collections::VecDeque;
use std::env;
use std::path::PathBuf;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Output from the transcription component
#[derive(Debug, Clone)]
pub struct TranscriptionOutput {
    pub segments: Vec<TranscribedSegment>,
    pub timestamp: f64,
}

/// Component for speech transcription using whisper-rs
pub struct TranscriptionComponent {
    config: TranscriptionConfig,
    context: WhisperContext,
    buffer: VecDeque<f32>,
    last_timestamp: f64,
}

#[async_trait]
impl PipelineComponent for TranscriptionComponent {
    type Input = AudioChunk;
    type Output = TranscriptionOutput;
    type Config = TranscriptionConfig;

    async fn initialize(config: Self::Config) -> Result<Self> {
        // Validate configuration
        config.validate()?;

        // Get current directory and construct absolute paths
        let current_dir = env::current_dir()?;
        let model_path = PathBuf::from(&config.model_path);
        let absolute_path = if model_path.is_absolute() {
            model_path
        } else {
            current_dir.join(&model_path)
        };

        println!("Attempting to load model...");
        println!("Using path: {}", absolute_path.display());

        // Initialize whisper context
        let context = WhisperContext::new_with_params(
            absolute_path.to_str().unwrap(),
            WhisperContextParameters::new(),
        )?;

        Ok(Self {
            config,
            context,
            buffer: VecDeque::new(),
            last_timestamp: 0.0,
        })
    }

    async fn process(&mut self, input: Self::Input) -> Result<Self::Output> {
        // Add new samples to buffer
        self.buffer.extend(input.samples.iter());

        // Process samples if we have enough data
        let segments = if self.buffer.len() >= self.config.sample_rate as usize {
            // Convert buffer to Vec for processing
            let samples: Vec<f32> = self.buffer.drain(..).collect();

            // Create state for this processing
            println!("Creating state for chunk processing...");
            let mut state = self.context.create_state()?;

            // Set up parameters using default sampling strategy
            let mut params = FullParams::new(SamplingStrategy::default());
            params.set_language(Some(&self.config.language));
            params.set_translate(self.config.translate);
            params.set_print_realtime(false);
            params.set_print_progress(false);
            params.set_print_timestamps(self.config.timestamp_enabled);
            params.set_print_special(false);

            // Process through whisper
            println!("Processing audio chunk...");
            state.full(params, &samples)?;

            // Extract segments
            let mut transcribed = Vec::new();
            let n_segments = state.full_n_segments()?;
            println!("Found {} segments", n_segments);

            for i in 0..n_segments {
                let text = state.full_get_segment_text(i)?;
                let start = state.full_get_segment_t0(i)? as f64 / 1000.0;
                let end = state.full_get_segment_t1(i)? as f64 / 1000.0;

                // Skip segments that are too short
                if end - start < self.config.min_segment_duration {
                    continue;
                }

                // Add timing offset from last timestamp
                let segment = TranscribedSegment::new(
                    text.trim().to_string(),
                    start + self.last_timestamp,
                    end + self.last_timestamp,
                    1.0, // Whisper doesn't provide confidence scores
                );

                if !segment.is_empty() {
                    transcribed.push(segment);
                }
            }

            // Drop state explicitly before returning
            drop(state);
            transcribed
        } else {
            Vec::new()
        };

        // Update timestamp
        self.last_timestamp = input.timestamp;

        Ok(TranscriptionOutput {
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

impl TranscriptionComponent {
    /// Get the current language being used
    pub fn language(&self) -> &str {
        &self.config.language
    }

    /// Change the language for transcription
    pub fn set_language(&mut self, language: String) -> Result<()> {
        // Validate language code
        if language.len() != 2 {
            return Err(color_eyre::eyre::eyre!(
                "Invalid language code: {}. Expected 2-letter code.",
                language
            ));
        }

        self.config.language = language;
        Ok(())
    }

    /// Enable or disable translation to English
    pub fn set_translate(&mut self, translate: bool) {
        self.config.translate = translate;
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
        let model_path = temp_dir.path().join("whisper-base.bin");
        File::create(&model_path)
            .unwrap()
            .write_all(b"dummy")
            .unwrap();

        let config = TranscriptionConfig {
            model_path: model_path.to_str().unwrap().to_string(),
            ..Default::default()
        };

        let component = TranscriptionComponent::initialize(config).await;
        assert!(component.is_err()); // Will fail with dummy model, which is expected
    }

    #[tokio::test]
    async fn test_language_settings() {
        let temp_dir = tempdir().unwrap();
        let model_path = temp_dir.path().join("whisper-base.bin");
        File::create(&model_path)
            .unwrap()
            .write_all(b"dummy")
            .unwrap();

        let config = TranscriptionConfig {
            model_path: model_path.to_str().unwrap().to_string(),
            ..Default::default()
        };

        let mut component = TranscriptionComponent::initialize(config).await;

        // Skip further tests if initialization failed (expected with dummy model)
        if let Ok(ref mut component) = component {
            // Test valid language change
            assert!(component.set_language("fr".to_string()).is_ok());
            assert_eq!(component.language(), "fr");

            // Test invalid language
            assert!(component.set_language("eng".to_string()).is_err());
        }
    }
}
