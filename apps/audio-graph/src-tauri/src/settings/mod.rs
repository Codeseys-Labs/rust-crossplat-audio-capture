//! Application settings — persistence layer for user configuration.
//!
//! Settings are stored as JSON in the app data directory and loaded
//! at startup. If the file is missing or unparseable, defaults are used.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::Manager;

// ---------------------------------------------------------------------------
// ASR provider
// ---------------------------------------------------------------------------

/// ASR provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AsrProvider {
    #[serde(rename = "local_whisper")]
    LocalWhisper,
    #[serde(rename = "api")]
    Api {
        endpoint: String,
        api_key: String,
        model: String,
    },
}

impl Default for AsrProvider {
    fn default() -> Self {
        Self::LocalWhisper
    }
}

// ---------------------------------------------------------------------------
// LLM API config
// ---------------------------------------------------------------------------

/// LLM API configuration (mirrors llm/api_client.rs ApiConfig for persistence)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmApiConfig {
    pub endpoint: String,
    #[serde(default)]
    pub api_key: Option<String>,
    pub model: String,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
}

fn default_max_tokens() -> u32 {
    2048
}
fn default_temperature() -> f32 {
    0.7
}

// ---------------------------------------------------------------------------
// LLM provider
// ---------------------------------------------------------------------------

/// LLM provider configuration — local LFM2-350M GGUF model vs API endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LlmProvider {
    #[serde(rename = "local_llama")]
    LocalLlama,
    #[serde(rename = "api")]
    Api {
        endpoint: String,
        api_key: String,
        model: String,
    },
}

impl Default for LlmProvider {
    fn default() -> Self {
        Self::Api {
            endpoint: "http://localhost:11434/v1".to_string(),
            api_key: String::new(),
            model: "llama3.2".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Audio settings
// ---------------------------------------------------------------------------

/// Audio processing settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioSettings {
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    #[serde(default = "default_channels")]
    pub channels: u16,
}

fn default_sample_rate() -> u32 {
    16000
}
fn default_channels() -> u16 {
    1
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            sample_rate: default_sample_rate(),
            channels: default_channels(),
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level settings
// ---------------------------------------------------------------------------

/// Top-level application settings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub asr_provider: AsrProvider,
    #[serde(default)]
    pub llm_provider: LlmProvider,
    #[serde(default)]
    pub llm_api_config: Option<LlmApiConfig>,
    #[serde(default)]
    pub audio_settings: AudioSettings,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            asr_provider: AsrProvider::default(),
            llm_provider: LlmProvider::default(),
            llm_api_config: None,
            audio_settings: AudioSettings::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Path resolution
// ---------------------------------------------------------------------------

/// Get the path to the settings JSON file
pub fn get_settings_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data directory: {}", e))?;
    Ok(data_dir.join("settings.json"))
}

// ---------------------------------------------------------------------------
// Load / Save
// ---------------------------------------------------------------------------

/// Load settings from disk, returning defaults if file doesn't exist or is invalid
pub fn load_settings(app: &tauri::AppHandle) -> AppSettings {
    match get_settings_path(app) {
        Ok(path) => {
            if path.exists() {
                match fs::read_to_string(&path) {
                    Ok(contents) => match serde_json::from_str::<AppSettings>(&contents) {
                        Ok(settings) => {
                            log::info!("Loaded settings from {}", path.display());
                            settings
                        }
                        Err(e) => {
                            log::warn!("Failed to parse settings file, using defaults: {}", e);
                            AppSettings::default()
                        }
                    },
                    Err(e) => {
                        log::warn!("Failed to read settings file, using defaults: {}", e);
                        AppSettings::default()
                    }
                }
            } else {
                log::info!("No settings file found, using defaults");
                AppSettings::default()
            }
        }
        Err(e) => {
            log::warn!("Failed to determine settings path, using defaults: {}", e);
            AppSettings::default()
        }
    }
}

/// Save settings to disk (atomic write: write to tmp then rename)
pub fn save_settings(app: &tauri::AppHandle, settings: &AppSettings) -> Result<(), String> {
    let path = get_settings_path(app)?;

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create settings directory: {}", e))?;
    }

    let json = serde_json::to_string_pretty(settings)
        .map_err(|e| format!("Failed to serialize settings: {}", e))?;

    // Atomic write: write to temp file, then rename
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, &json).map_err(|e| format!("Failed to write settings file: {}", e))?;
    fs::rename(&tmp_path, &path).map_err(|e| format!("Failed to finalize settings file: {}", e))?;

    log::info!("Settings saved to {}", path.display());
    Ok(())
}
