//! Model management and downloading.
//!
//! Provides model listing, status checking, and HTTP-based downloading
//! with progress reporting via Tauri events. Replaces the old shell-script
//! based model setup with a cross-platform Rust implementation.

use serde::Serialize;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

const WHISPER_MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin";
const WHISPER_MODEL_FILENAME: &str = "ggml-small.en.bin";
const WHISPER_MODEL_SIZE: u64 = 487_654_400; // ~466MB

const LLM_MODEL_URL: &str = "https://huggingface.co/LiquidAI/LFM2-350M-Extract-GGUF/resolve/main/lfm2-350m-extract-q4_k_m.gguf";
const LLM_MODEL_FILENAME: &str = "lfm2-350m-extract-q4_k_m.gguf";

/// Information about a downloadable model.
#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub name: String,
    pub filename: String,
    pub url: String,
    pub size_bytes: Option<u64>,
    pub is_downloaded: bool,
    pub local_path: Option<String>,
}

/// Progress event payload emitted during model downloads.
#[derive(Debug, Clone, Serialize)]
pub struct DownloadProgress {
    pub model_name: String,
    pub bytes_downloaded: u64,
    pub total_bytes: Option<u64>,
    pub percent: f32,
    /// One of: "downloading", "complete", "error"
    pub status: String,
}

/// Return the directory where models are stored.
///
/// Uses a `models/` directory relative to the working directory. Creates it
/// if it doesn't already exist.
pub fn get_models_dir() -> PathBuf {
    let models_dir = PathBuf::from("models");
    if !models_dir.exists() {
        let _ = fs::create_dir_all(&models_dir);
    }
    models_dir
}

/// List all known models and their download status.
pub fn list_models() -> Vec<ModelInfo> {
    let models_dir = get_models_dir();
    vec![
        {
            let path = models_dir.join(WHISPER_MODEL_FILENAME);
            let exists = path.exists();
            ModelInfo {
                name: "Whisper Small (English)".to_string(),
                filename: WHISPER_MODEL_FILENAME.to_string(),
                url: WHISPER_MODEL_URL.to_string(),
                size_bytes: Some(WHISPER_MODEL_SIZE),
                is_downloaded: exists,
                local_path: if exists {
                    Some(path.to_string_lossy().to_string())
                } else {
                    None
                },
            }
        },
        {
            let path = models_dir.join(LLM_MODEL_FILENAME);
            let exists = path.exists();
            ModelInfo {
                name: "LFM2-350M Extract (Entity Extraction)".to_string(),
                filename: LLM_MODEL_FILENAME.to_string(),
                url: LLM_MODEL_URL.to_string(),
                size_bytes: None,
                is_downloaded: exists,
                local_path: if exists {
                    Some(path.to_string_lossy().to_string())
                } else {
                    None
                },
            }
        },
    ]
}

/// Download a model file with progress reporting via Tauri events.
///
/// If the file already exists, returns its path immediately. Otherwise
/// performs a blocking HTTP download, emitting `model-download-progress`
/// events approximately every 1 MB.
pub fn download_model(
    model_name: &str,
    url: &str,
    filename: &str,
    app_handle: &tauri::AppHandle,
) -> Result<PathBuf, String> {
    use tauri::Emitter;

    let models_dir = get_models_dir();
    let target_path = models_dir.join(filename);

    if target_path.exists() {
        return Ok(target_path);
    }

    // Use reqwest blocking client for download with progress
    let client = reqwest::blocking::Client::new();
    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("Download failed: {}", e))?;

    let total_size = response.content_length();
    let mut downloaded: u64 = 0;

    let mut file =
        fs::File::create(&target_path).map_err(|e| format!("Failed to create file: {}", e))?;

    let mut reader = response;
    let mut buffer = vec![0u8; 8192];

    loop {
        let bytes_read = std::io::Read::read(&mut reader, &mut buffer)
            .map_err(|e| format!("Read error: {}", e))?;
        if bytes_read == 0 {
            break;
        }

        file.write_all(&buffer[..bytes_read])
            .map_err(|e| format!("Write error: {}", e))?;

        downloaded += bytes_read as u64;

        // Emit progress event every ~1MB
        if downloaded % (1024 * 1024) < 8192 {
            let progress = DownloadProgress {
                model_name: model_name.to_string(),
                bytes_downloaded: downloaded,
                total_bytes: total_size,
                percent: total_size
                    .map(|t| (downloaded as f32 / t as f32) * 100.0)
                    .unwrap_or(0.0),
                status: "downloading".to_string(),
            };
            let _ = app_handle.emit("model-download-progress", &progress);
        }
    }

    // Emit completion
    let progress = DownloadProgress {
        model_name: model_name.to_string(),
        bytes_downloaded: downloaded,
        total_bytes: total_size,
        percent: 100.0,
        status: "complete".to_string(),
    };
    let _ = app_handle.emit("model-download-progress", &progress);

    Ok(target_path)
}
