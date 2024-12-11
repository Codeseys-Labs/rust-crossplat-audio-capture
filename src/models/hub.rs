use color_eyre::Result;
use dirs;
use reqwest;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs;

/// Configuration for model downloads and caching
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub cache_dir: PathBuf,
    pub models: Vec<ModelInfo>,
}

/// Information about a specific model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    pub url: String,
    pub version: String,
    pub size: u64,
    pub sha256: String,
}

/// Model hub for managing downloads and caching
pub struct ModelHub {
    config: ModelConfig,
    client: reqwest::Client,
}

impl ModelHub {
    /// Create a new model hub with default cache directory
    pub fn new() -> Result<Self> {
        let cache_dir = dirs::cache_dir()
            .ok_or_else(|| color_eyre::eyre::eyre!("Could not determine cache directory"))?
            .join("rsac")
            .join("models");

        let config = ModelConfig {
            cache_dir,
            models: Vec::new(),
        };

        Ok(Self {
            config,
            client: reqwest::Client::new(),
        })
    }

    /// Create a model hub with custom configuration
    pub fn with_config(config: ModelConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Get the path to a cached model, downloading if necessary
    pub async fn get_model(&self, name: &str) -> Result<PathBuf> {
        let model = self
            .config
            .models
            .iter()
            .find(|m| m.name == name)
            .ok_or_else(|| color_eyre::eyre::eyre!("Model {} not found in config", name))?;

        let model_path = self.config.cache_dir.join(&model.name);

        if !model_path.exists() {
            self.download_model(model).await?;
        }

        Ok(model_path)
    }

    /// Download a model to the cache directory
    async fn download_model(&self, model: &ModelInfo) -> Result<()> {
        println!("Downloading model: {}", model.name);

        // Create cache directory if it doesn't exist
        fs::create_dir_all(&self.config.cache_dir).await?;

        // Download the model
        let response = self.client.get(&model.url).send().await?;
        let bytes = response.bytes().await?;

        // Verify checksum
        let hash = sha256::digest(&bytes[..]);
        if hash != model.sha256 {
            return Err(color_eyre::eyre::eyre!(
                "Checksum mismatch for model {}",
                model.name
            ));
        }

        // Save to cache
        let model_path = self.config.cache_dir.join(&model.name);
        fs::write(&model_path, bytes).await?;

        println!("Model downloaded successfully: {}", model.name);
        Ok(())
    }

    /// Check if a model exists in the cache
    pub fn is_model_cached(&self, name: &str) -> bool {
        if let Some(model) = self.config.models.iter().find(|m| m.name == name) {
            self.config.cache_dir.join(&model.name).exists()
        } else {
            false
        }
    }

    /// Add a new model to the configuration
    pub fn add_model(&mut self, model: ModelInfo) {
        if !self.config.models.iter().any(|m| m.name == model.name) {
            self.config.models.push(model);
        }
    }

    /// Get the cache directory path
    pub fn cache_dir(&self) -> &Path {
        &self.config.cache_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_model_hub_creation() {
        let hub = ModelHub::new();
        assert!(hub.is_ok());
    }

    #[tokio::test]
    async fn test_custom_cache_dir() {
        let temp_dir = tempdir().unwrap();
        let config = ModelConfig {
            cache_dir: temp_dir.path().to_path_buf(),
            models: Vec::new(),
        };
        let hub = ModelHub::with_config(config);
        assert_eq!(hub.cache_dir(), temp_dir.path());
    }
}
