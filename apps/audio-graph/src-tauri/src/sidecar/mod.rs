//! LLM sidecar management module.
//!
//! Manages the llama-server sidecar process for entity extraction.
//! The sidecar runs as a separate process, serving an API that the
//! EntityExtractor calls to extract entities and relations from transcript text.

/// Manages the llama-server sidecar process lifecycle.
#[allow(dead_code)]
pub struct SidecarManager {
    // TODO: Fields will be added when implementing:
    // process: Option<tauri_plugin_shell::process::CommandChild>,
    // endpoint: String,
    // model_path: std::path::PathBuf,
    // health_interval: std::time::Duration,
    _placeholder: (),
}

impl SidecarManager {
    /// Create a new sidecar manager.
    pub fn new() -> Self {
        // TODO: Initialize with config (port, model path, etc.)
        Self { _placeholder: () }
    }

    /// Start the llama-server sidecar process.
    pub fn start(&mut self) -> Result<(), String> {
        // TODO: Use tauri_plugin_shell to spawn the sidecar binary
        // TODO: Wait for health check endpoint to respond
        log::info!("SidecarManager::start() stub — not yet implemented");
        Ok(())
    }

    /// Stop the llama-server sidecar process.
    pub fn stop(&mut self) -> Result<(), String> {
        // TODO: Send kill signal to sidecar process
        // TODO: Wait for clean shutdown
        log::info!("SidecarManager::stop() stub — not yet implemented");
        Ok(())
    }

    /// Check if the sidecar is healthy.
    pub fn is_healthy(&self) -> bool {
        // TODO: HTTP GET to health endpoint
        false
    }
}

impl Default for SidecarManager {
    fn default() -> Self {
        Self::new()
    }
}
