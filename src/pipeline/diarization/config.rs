use crate::pipeline::base::ComponentConfig;
use color_eyre::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiarizationConfig {
    /// Path to the segmentation model (pyannote-audio)
    pub segment_model_path: String,

    /// Path to the embedding model (3dspeaker)
    pub embedding_model_path: String,

    /// Maximum number of speakers to detect (used for clustering)
    pub max_speakers: i32,

    /// Minimum segment duration in seconds
    pub min_segment_duration: f64,

    /// Overlap threshold for merging segments
    pub overlap_threshold: f64,

    /// Sample rate expected by the model
    pub sample_rate: u32,
}

impl Default for DiarizationConfig {
    fn default() -> Self {
        Self {
            segment_model_path: "models/sherpa-onnx-pyannote-segmentation-3-0/model.onnx"
                .to_string(),
            embedding_model_path:
                "models/3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx".to_string(),
            max_speakers: 2,
            min_segment_duration: 0.5,
            overlap_threshold: 0.5,
            sample_rate: 16000,
        }
    }
}

impl ComponentConfig for DiarizationConfig {
    fn validate(&self) -> Result<()> {
        // Validate max_speakers
        if self.max_speakers < 1 {
            return Err(color_eyre::eyre::eyre!("max_speakers must be at least 1"));
        }

        // Validate durations
        if self.min_segment_duration <= 0.0 {
            return Err(color_eyre::eyre::eyre!(
                "min_segment_duration must be positive"
            ));
        }

        // Validate overlap threshold
        if !(0.0..=1.0).contains(&self.overlap_threshold) {
            return Err(color_eyre::eyre::eyre!(
                "overlap_threshold must be between 0 and 1"
            ));
        }

        // Validate sample rate
        if self.sample_rate == 0 {
            return Err(color_eyre::eyre::eyre!(
                "sample_rate must be greater than 0"
            ));
        }

        // Check if model files exist
        if !std::path::Path::new(&self.segment_model_path).exists() {
            return Err(color_eyre::eyre::eyre!(
                "Segmentation model file not found: {}",
                self.segment_model_path
            ));
        }

        if !std::path::Path::new(&self.embedding_model_path).exists() {
            return Err(color_eyre::eyre::eyre!(
                "Embedding model file not found: {}",
                self.embedding_model_path
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn test_default_config() {
        let config = DiarizationConfig::default();
        assert_eq!(config.max_speakers, 2);
        assert_eq!(config.sample_rate, 16000);
    }

    #[test]
    fn test_config_validation() {
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

        let mut config = DiarizationConfig {
            segment_model_path: segment_model_path.to_str().unwrap().to_string(),
            embedding_model_path: embedding_model_path.to_str().unwrap().to_string(),
            ..Default::default()
        };

        // Valid config
        assert!(config.validate().is_ok());

        // Invalid max_speakers
        config.max_speakers = 0;
        assert!(config.validate().is_err());
        config.max_speakers = 2;

        // Invalid min_segment_duration
        config.min_segment_duration = -1.0;
        assert!(config.validate().is_err());
        config.min_segment_duration = 0.5;

        // Invalid overlap_threshold
        config.overlap_threshold = 1.5;
        assert!(config.validate().is_err());
        config.overlap_threshold = 0.5;

        // Invalid sample_rate
        config.sample_rate = 0;
        assert!(config.validate().is_err());
    }
}
