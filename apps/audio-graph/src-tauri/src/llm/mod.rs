//! LLM inference backends.
//!
//! Two backends are available:
//! - **Native** (`engine`): In-process GGUF model inference via llama-cpp-2.
//! - **API** (`api_client`): OpenAI-compatible HTTP API (OpenAI, Ollama, LM Studio, vLLM, etc.).
//!
//! The speech processor and chat commands try native first, then API, then
//! rule-based extraction as a final fallback.

pub mod api_client;
pub mod engine;

pub use api_client::{ApiClient, ApiConfig};
pub use engine::LlmEngine;
