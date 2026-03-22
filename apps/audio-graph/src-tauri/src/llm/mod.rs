//! Native LLM engine using llama-cpp-2.
//!
//! Replaces the HTTP sidecar with in-process GGUF model inference.
//! Supports grammar-constrained entity extraction and free-form chat.

pub mod engine;

pub use engine::LlmEngine;
