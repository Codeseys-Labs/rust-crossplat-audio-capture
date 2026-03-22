//! Native LLM engine using llama-cpp-2.
//!
//! In-process GGUF model inference for entity extraction and chat.
//! Supports grammar-constrained entity extraction and free-form chat.

pub mod engine;

pub use engine::LlmEngine;
