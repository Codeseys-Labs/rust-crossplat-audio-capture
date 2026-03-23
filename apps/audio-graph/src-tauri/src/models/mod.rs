//! Model management and downloading.
//!
//! Provides model listing, status checking, and HTTP-based downloading
//! with progress reporting via Tauri events. Replaces the old shell-script
//! based model setup with a cross-platform Rust implementation.

use serde::Serialize;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};

// ---------------------------------------------------------------------------
// Model definitions
// ---------------------------------------------------------------------------

/// Internal model definition with expected sizes for verification.
struct ModelDef {
    name: &'static str,
    filename: &'static str,
    url: &'static str,
    expected_size: Option<u64>, // bytes, with 1% tolerance
    description: &'static str,
}

const WHISPER_MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin";
const WHISPER_MODEL_FILENAME: &str = "ggml-small.en.bin";
const WHISPER_EXPECTED_SIZE: u64 = 487_654_400; // ~466MB

const LLM_MODEL_URL: &str = "https://huggingface.co/LiquidAI/LFM2-350M-Extract-GGUF/resolve/main/lfm2-350m-extract-q4_k_m.gguf";
/// Public so that commands can reference the canonical LLM model filename.
pub const LLM_MODEL_FILENAME: &str = "lfm2-350m-extract-q4_k_m.gguf";
const LLM_EXPECTED_SIZE: u64 = 229_000_000; // ~218MB Q4_K_M

const MODELS: &[ModelDef] = &[
    ModelDef {
        name: "Whisper Small (English)",
        filename: WHISPER_MODEL_FILENAME,
        url: WHISPER_MODEL_URL,
        expected_size: Some(WHISPER_EXPECTED_SIZE),
        description: "Speech recognition model for English transcription",
    },
    ModelDef {
        name: "LFM2-350M Extract (Entity Extraction)",
        filename: LLM_MODEL_FILENAME,
        url: LLM_MODEL_URL,
        expected_size: Some(LLM_EXPECTED_SIZE),
        description: "Small language model for entity and relationship extraction",
    },
];

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Information about a downloadable model.
#[derive(Debug, Clone, Serialize)]
pub struct ModelInfo {
    pub name: String,
    pub filename: String,
    pub url: String,
    pub size_bytes: Option<u64>,
    pub is_downloaded: bool,
    pub is_valid: bool,
    pub local_path: Option<String>,
    pub description: String,
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

/// Readiness state for a single model.
#[derive(Debug, Clone, Serialize)]
pub enum ModelReadiness {
    Ready,
    NotDownloaded,
    /// File exists but wrong size (possibly corrupt or incomplete).
    Invalid,
}

/// Aggregated status of all required models.
#[derive(Debug, Clone, Serialize)]
pub struct ModelStatus {
    pub whisper: ModelReadiness,
    pub llm: ModelReadiness,
    pub vad: ModelReadiness,
}

// ---------------------------------------------------------------------------
// Directory resolution (G6)
// ---------------------------------------------------------------------------

/// Return the directory where models are stored.
///
/// Resolves relative to Tauri's app data directory for a stable,
/// platform-appropriate location. Creates the directory if it doesn't exist.
pub fn get_models_dir(app: &AppHandle) -> PathBuf {
    let base = app
        .path()
        .app_data_dir()
        .unwrap_or_else(|_| PathBuf::from("."));
    let dir = base.join("models");
    if !dir.exists() {
        let _ = fs::create_dir_all(&dir);
    }
    dir
}

// ---------------------------------------------------------------------------
// Verification (G5)
// ---------------------------------------------------------------------------

/// Verify a model file exists and has approximately the expected size.
///
/// Returns `true` if the file exists, is non-empty, and (if an expected size
/// is given) is within 1% of the expected size.
fn verify_model_file(path: &Path, expected_size: Option<u64>) -> bool {
    if let Ok(metadata) = fs::metadata(path) {
        let size = metadata.len();
        if size == 0 {
            return false;
        }
        if let Some(expected) = expected_size {
            let tolerance = expected / 100; // 1%
            size >= expected.saturating_sub(tolerance) && size <= expected + tolerance
        } else {
            true // No expected size, just check non-empty
        }
    } else {
        false
    }
}

/// Check readiness of a single model file.
fn check_model_readiness(
    models_dir: &Path,
    filename: &str,
    expected_size: Option<u64>,
) -> ModelReadiness {
    let path = models_dir.join(filename);
    if !path.exists() {
        ModelReadiness::NotDownloaded
    } else if verify_model_file(&path, expected_size) {
        ModelReadiness::Ready
    } else {
        ModelReadiness::Invalid
    }
}

// ---------------------------------------------------------------------------
// Status (G1)
// ---------------------------------------------------------------------------

/// Get the readiness status of all known models.
pub fn get_model_status(app: &AppHandle) -> ModelStatus {
    let dir = get_models_dir(app);
    ModelStatus {
        whisper: check_model_readiness(&dir, WHISPER_MODEL_FILENAME, Some(WHISPER_EXPECTED_SIZE)),
        llm: check_model_readiness(&dir, LLM_MODEL_FILENAME, Some(LLM_EXPECTED_SIZE)),
        vad: ModelReadiness::Ready, // VAD model is bundled in the binary
    }
}

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

/// List all known models and their download/validation status.
pub fn list_models(app: &AppHandle) -> Vec<ModelInfo> {
    let models_dir = get_models_dir(app);

    MODELS
        .iter()
        .map(|def| {
            let path = models_dir.join(def.filename);
            let exists = path.exists();
            let valid = verify_model_file(&path, def.expected_size);
            ModelInfo {
                name: def.name.to_string(),
                filename: def.filename.to_string(),
                url: def.url.to_string(),
                size_bytes: def.expected_size,
                is_downloaded: exists,
                is_valid: valid,
                local_path: if exists {
                    Some(path.to_string_lossy().to_string())
                } else {
                    None
                },
                description: def.description.to_string(),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Download
// ---------------------------------------------------------------------------

/// Download a model file by filename with progress reporting via Tauri events.
///
/// Looks up the model definition by filename. If the file already exists and
/// is valid, returns its path immediately. Otherwise performs a blocking HTTP
/// download, emitting `model-download-progress` events approximately every
/// 1 MB. After download, verifies the file size.
pub fn download_model(app: &AppHandle, filename: &str) -> Result<String, String> {
    use tauri::Emitter;

    let def = MODELS
        .iter()
        .find(|m| m.filename == filename)
        .ok_or_else(|| format!("Unknown model filename: {}", filename))?;

    let models_dir = get_models_dir(app);
    let target_path = models_dir.join(filename);

    // If already downloaded and valid, return immediately.
    if target_path.exists() && verify_model_file(&target_path, def.expected_size) {
        return Ok(target_path.to_string_lossy().to_string());
    }

    // If file exists but is invalid (e.g. partial download), remove it.
    if target_path.exists() {
        let _ = fs::remove_file(&target_path);
    }

    // Blocking HTTP download with progress
    let client = reqwest::blocking::Client::new();
    let response = client
        .get(def.url)
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
                model_name: def.name.to_string(),
                bytes_downloaded: downloaded,
                total_bytes: total_size,
                percent: total_size
                    .map(|t| (downloaded as f32 / t as f32) * 100.0)
                    .unwrap_or(0.0),
                status: "downloading".to_string(),
            };
            let _ = app.emit("model-download-progress", &progress);
        }
    }

    // Verify downloaded file
    if !verify_model_file(&target_path, def.expected_size) {
        let actual_size = fs::metadata(&target_path).map(|m| m.len()).unwrap_or(0);
        let _ = fs::remove_file(&target_path);
        return Err(format!(
            "Download verification failed for '{}': got {} bytes, expected ~{:?} bytes",
            filename, actual_size, def.expected_size
        ));
    }

    // Emit completion
    let progress = DownloadProgress {
        model_name: def.name.to_string(),
        bytes_downloaded: downloaded,
        total_bytes: total_size,
        percent: 100.0,
        status: "complete".to_string(),
    };
    let _ = app.emit("model-download-progress", &progress);

    Ok(target_path.to_string_lossy().to_string())
}

// ---------------------------------------------------------------------------
// Deletion
// ---------------------------------------------------------------------------

/// Delete a downloaded model file
pub fn delete_model(app: &AppHandle, filename: &str) -> Result<String, String> {
    // Validate filename - prevent path traversal
    if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
        return Err("Invalid filename".to_string());
    }

    let models_dir = get_models_dir(app);
    let model_path = models_dir.join(filename);

    // Verify the file is actually in the models directory
    if !model_path.starts_with(&models_dir) {
        return Err("Invalid model path".to_string());
    }

    if !model_path.exists() {
        return Err(format!("Model file not found: {}", filename));
    }

    fs::remove_file(&model_path).map_err(|e| format!("Failed to delete model: {}", e))?;

    log::info!("Deleted model: {}", filename);
    Ok(format!("Model '{}' deleted successfully", filename))
}
