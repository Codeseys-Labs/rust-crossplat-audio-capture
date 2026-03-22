//! LLM Sidecar management.
//!
//! Manages a llama-server subprocess for entity extraction via HTTP.
//! Falls back to rule-based extraction if no sidecar is available.
//!
//! ## Design note: blocking HTTP (I10)
//!
//! This module intentionally uses `reqwest::blocking::Client` rather than an
//! async client.  The sidecar is only called from the speech-processor thread
//! (a dedicated OS thread, not a Tokio task), so blocking I/O is appropriate
//! and avoids pulling the async runtime into the hot audio path.  If the
//! sidecar is ever called from an async context, switch to `reqwest::Client`.

use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::graph::entities::ExtractionResult;

/// Configuration for the sidecar process.
#[derive(Debug, Clone)]
pub struct SidecarConfig {
    /// Path to the llama-server binary.
    pub binary_path: String,
    /// Path to the model file (e.g., GGUF model for entity extraction).
    pub model_path: String,
    /// Host to bind llama-server to.
    pub host: String,
    /// Port to bind llama-server to.
    pub port: u16,
    /// Number of GPU layers (-1 = all).
    pub n_gpu_layers: i32,
    /// Context size.
    pub context_size: u32,
}

impl Default for SidecarConfig {
    fn default() -> Self {
        Self {
            #[cfg(target_os = "windows")]
            binary_path: "llama-server.exe".to_string(),
            #[cfg(not(target_os = "windows"))]
            binary_path: "llama-server".to_string(),
            model_path: String::new(),
            host: "127.0.0.1".to_string(),
            port: 8089,
            n_gpu_layers: -1,
            context_size: 2048,
        }
    }
}

/// Manages the llama-server sidecar process and provides entity extraction via HTTP.
pub struct SidecarManager {
    config: SidecarConfig,
    child: Arc<Mutex<Option<Child>>>,
    http_client: reqwest::blocking::Client,
    healthy: Arc<Mutex<bool>>,
}

impl SidecarManager {
    /// Create a new sidecar manager with the given configuration.
    pub fn new(config: SidecarConfig) -> Self {
        let http_client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());

        Self {
            config,
            child: Arc::new(Mutex::new(None)),
            http_client,
            healthy: Arc::new(Mutex::new(false)),
        }
    }

    /// Start the llama-server subprocess.
    pub fn start(&self) -> Result<(), String> {
        let mut child_lock = self
            .child
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;

        if child_lock.is_some() {
            log::warn!("Sidecar already running");
            return Ok(());
        }

        if self.config.model_path.is_empty() {
            return Err("No model path configured".to_string());
        }

        log::info!(
            "Starting llama-server: {} --model {} --host {} --port {}",
            self.config.binary_path,
            self.config.model_path,
            self.config.host,
            self.config.port
        );

        let child = Command::new(&self.config.binary_path)
            .arg("--model")
            .arg(&self.config.model_path)
            .arg("--host")
            .arg(&self.config.host)
            .arg("--port")
            .arg(self.config.port.to_string())
            .arg("--n-gpu-layers")
            .arg(self.config.n_gpu_layers.to_string())
            .arg("--ctx-size")
            .arg(self.config.context_size.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start llama-server: {}", e))?;

        *child_lock = Some(child);

        // Wait for health check
        drop(child_lock);
        self.wait_for_healthy(Duration::from_secs(30))?;

        log::info!("Sidecar started and healthy");
        Ok(())
    }

    /// Stop the llama-server subprocess.
    pub fn stop(&self) -> Result<(), String> {
        let mut child_lock = self
            .child
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;

        if let Some(mut child) = child_lock.take() {
            log::info!("Stopping llama-server sidecar");
            let _ = child.kill();
            let _ = child.wait();
        }

        *self.healthy.lock().unwrap_or_else(|e| e.into_inner()) = false;
        Ok(())
    }

    /// Check if the sidecar is healthy via GET /health.
    pub fn health_check(&self) -> bool {
        let url = format!("http://{}:{}/health", self.config.host, self.config.port);

        match self
            .http_client
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
        {
            Ok(resp) => {
                let healthy = resp.status().is_success();
                *self.healthy.lock().unwrap_or_else(|e| e.into_inner()) = healthy;
                healthy
            }
            Err(_) => {
                *self.healthy.lock().unwrap_or_else(|e| e.into_inner()) = false;
                false
            }
        }
    }

    /// Check cached health status without making an HTTP call.
    pub fn is_healthy(&self) -> bool {
        *self.healthy.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Wait for the sidecar to become healthy, polling every 500ms.
    fn wait_for_healthy(&self, timeout: Duration) -> Result<(), String> {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if self.health_check() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        Err("Sidecar did not become healthy within timeout".to_string())
    }

    /// Extract entities and relations from text via the LLM sidecar.
    ///
    /// Sends a POST /completion request with a structured extraction prompt.
    /// Returns an `ExtractionResult` parsed from the LLM's JSON output.
    pub fn extract_entities(&self, speaker: &str, text: &str) -> Result<ExtractionResult, String> {
        if !self.is_healthy() {
            return Err("Sidecar is not healthy".to_string());
        }

        let prompt = format!(
            r#"Extract entities and relationships from this conversation segment.

Speaker: {}
Text: {}

Output JSON:
{{"entities": [{{"name": "...", "entity_type": "Person|Organization|Location|Event|Topic|Product", "description": "..."}}],
 "relations": [{{"source": "...", "target": "...", "relation_type": "...", "detail": "..."}}]}}"#,
            speaker, text
        );

        let url = format!(
            "http://{}:{}/completion",
            self.config.host, self.config.port
        );

        let request_body = serde_json::json!({
            "prompt": prompt,
            "n_predict": 512,
            "temperature": 0.1,
            "stop": ["\n\n"],
            "grammar": self.json_grammar(),
        });

        let response = self
            .http_client
            .post(&url)
            .json(&request_body)
            .send()
            .map_err(|e| format!("HTTP request failed: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Sidecar returned status: {}", response.status()));
        }

        let response_body: serde_json::Value = response
            .json()
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        // llama-server returns {"content": "...json...", ...}
        let content = response_body
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "No 'content' field in response".to_string())?;

        // Parse the JSON from the LLM output
        let extraction: ExtractionResult = serde_json::from_str(content)
            .map_err(|e| format!("Failed to parse extraction JSON: {} — raw: {}", e, content))?;

        Ok(extraction)
    }

    /// JSON grammar for structured output (GBNF format for llama.cpp).
    fn json_grammar(&self) -> String {
        r#"root   ::= "{" ws "\"entities\"" ws ":" ws entities "," ws "\"relations\"" ws ":" ws relations "}" ws
entities ::= "[" ws (entity ("," ws entity)*)? "]"
entity  ::= "{" ws "\"name\"" ws ":" ws string "," ws "\"entity_type\"" ws ":" ws entity-type ("," ws "\"description\"" ws ":" ws string)? "}" ws
entity-type ::= "\"Person\"" | "\"Organization\"" | "\"Location\"" | "\"Event\"" | "\"Topic\"" | "\"Product\""
relations ::= "[" ws (relation ("," ws relation)*)? "]"
relation ::= "{" ws "\"source\"" ws ":" ws string "," ws "\"target\"" ws ":" ws string "," ws "\"relation_type\"" ws ":" ws string ("," ws "\"detail\"" ws ":" ws string)? "}" ws
string  ::= "\"" [^"\\]* "\""
ws      ::= [ \t\n]*"#.to_string()
    }

    /// Get the base URL for the sidecar.
    pub fn base_url(&self) -> String {
        format!("http://{}:{}", self.config.host, self.config.port)
    }
}

impl Default for SidecarManager {
    fn default() -> Self {
        Self::new(SidecarConfig::default())
    }
}

impl Drop for SidecarManager {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
