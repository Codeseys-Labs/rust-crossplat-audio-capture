use crate::pipeline::base::ComponentConfig;
use color_eyre::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionConfig {
    /// Path to the Whisper model
    pub model_path: String,

    /// Language code (e.g., "en", "fr", etc.)
    pub language: String,

    /// Sample rate expected by the model
    pub sample_rate: u32,

    /// Whether to translate to English
    pub translate: bool,

    /// Minimum segment duration in seconds
    pub min_segment_duration: f64,

    /// Whether to use timestamps
    pub timestamp_enabled: bool,
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            model_path: "models/whisper-base.bin".to_string(),
            language: "en".to_string(),
            sample_rate: 16000,
            translate: false,
            min_segment_duration: 0.1,
            timestamp_enabled: true,
        }
    }
}

impl ComponentConfig for TranscriptionConfig {
    fn validate(&self) -> Result<()> {
        // Validate language code (basic check)
        if self.language.len() != 2 {
            return Err(color_eyre::eyre::eyre!(
                "Invalid language code: {}. Expected 2-letter code.",
                self.language
            ));
        }

        // Validate sample rate
        if self.sample_rate == 0 {
            return Err(color_eyre::eyre::eyre!(
                "sample_rate must be greater than 0"
            ));
        }

        // Validate min_segment_duration
        if self.min_segment_duration <= 0.0 {
            return Err(color_eyre::eyre::eyre!(
                "min_segment_duration must be positive"
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
        let config = TranscriptionConfig::default();
        assert_eq!(config.language, "en");
        assert_eq!(config.sample_rate, 16000);
        assert!(!config.translate);
    }

    #[test]
    fn test_config_validation() {
        let temp_dir = tempdir().unwrap();
        let model_path = temp_dir.path().join("whisper-base.bin");
        File::create(&model_path)
            .unwrap()
            .write_all(b"dummy")
            .unwrap();

        let mut config = TranscriptionConfig {
            model_path: model_path.to_str().unwrap().to_string(),
            ..Default::default()
        };

        // Valid config
        assert!(config.validate().is_ok());

        // Invalid language code
        config.language = "eng".to_string();
        assert!(config.validate().is_err());
        config.language = "en".to_string();

        // Invalid sample rate
        config.sample_rate = 0;
        assert!(config.validate().is_err());
        config.sample_rate = 16000;

        // Invalid min_segment_duration
        config.min_segment_duration = -1.0;
        assert!(config.validate().is_err());
    }
}
