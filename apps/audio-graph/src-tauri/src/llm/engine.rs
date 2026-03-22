//! LLM inference engine backed by llama-cpp-2.
//!
//! Wraps [`LlamaBackend`] and [`LlamaModel`] for in-process GGUF model
//! inference.  Supports grammar-constrained entity extraction (via GBNF) and
//! free-form chat generation.  A fresh [`LlamaContext`] is created per
//! inference call because `LlamaContext` is **not** `Send`.

use std::num::NonZeroU32;
use std::sync::Arc;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

use crate::graph::entities::ExtractionResult;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A chat message with role and content.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatMessage {
    pub role: String, // "user", "assistant", "system"
    pub content: String,
}

/// Response from the chat endpoint.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatResponse {
    pub message: ChatMessage,
    pub tokens_used: u32,
}

// ---------------------------------------------------------------------------
// LlmEngine
// ---------------------------------------------------------------------------

/// Native LLM engine using llama.cpp via llama-cpp-2 bindings.
///
/// `LlamaModel` is `Send + Sync` so the engine can live inside
/// `Arc<Mutex<Option<LlmEngine>>>` in application state.  Each inference call
/// creates its own `LlamaContext` (which is **not** `Send`).
pub struct LlmEngine {
    backend: LlamaBackend,
    model: Arc<LlamaModel>,
}

impl LlmEngine {
    /// Load a GGUF model from disk.
    pub fn new(model_path: &str) -> Result<Self, String> {
        let backend =
            LlamaBackend::init().map_err(|e| format!("Failed to init llama backend: {}", e))?;

        let model_params = LlamaModelParams::default();

        let model = LlamaModel::load_from_file(&backend, model_path, &model_params)
            .map_err(|e| format!("Failed to load model '{}': {}", model_path, e))?;

        log::info!("LLM model loaded from: {}", model_path);

        Ok(Self {
            backend,
            model: Arc::new(model),
        })
    }

    /// Check if model is loaded and ready.
    pub fn is_loaded(&self) -> bool {
        true // If we constructed successfully, model is loaded
    }

    // ------------------------------------------------------------------
    // Entity extraction (grammar-constrained)
    // ------------------------------------------------------------------

    /// Extract entities and relations from text using grammar-constrained
    /// generation.  The output is forced to match the JSON schema expected by
    /// [`ExtractionResult`].
    pub fn extract_entities(&self, text: &str, speaker: &str) -> Result<ExtractionResult, String> {
        let prompt = format!(
            r#"Extract entities and relationships from this conversation segment.

Speaker: {}
Text: {}

Output JSON:
"#,
            speaker, text
        );

        let grammar_str = Self::json_grammar();

        let output = self.generate_with_grammar(&prompt, &grammar_str, 512, 0.1)?;

        let extraction: ExtractionResult = serde_json::from_str(&output)
            .map_err(|e| format!("Failed to parse extraction JSON: {} — raw: {}", e, output))?;

        Ok(extraction)
    }

    // ------------------------------------------------------------------
    // Chat
    // ------------------------------------------------------------------

    /// Chat with the LLM, providing graph context in the system prompt.
    pub fn chat(&self, messages: &[ChatMessage], graph_context: &str) -> Result<String, String> {
        let mut prompt = String::new();

        // Build system prompt with graph context
        prompt.push_str(
            "<|system|>\nYou are a helpful assistant that answers questions about an \
             audio conversation and its knowledge graph. Use the following context from \
             the knowledge graph and recent transcript to answer questions.\n\n",
        );
        prompt.push_str("Knowledge Graph Context:\n");
        prompt.push_str(graph_context);
        prompt.push_str("\n</s>\n");

        // Add message history
        for msg in messages {
            match msg.role.as_str() {
                "user" => {
                    prompt.push_str("<|user|>\n");
                    prompt.push_str(&msg.content);
                    prompt.push_str("\n</s>\n");
                }
                "assistant" => {
                    prompt.push_str("<|assistant|>\n");
                    prompt.push_str(&msg.content);
                    prompt.push_str("\n</s>\n");
                }
                _ => {}
            }
        }

        // Prompt for assistant response
        prompt.push_str("<|assistant|>\n");

        self.generate(&prompt, 512, 0.7)
    }

    // ------------------------------------------------------------------
    // Internal generation helpers
    // ------------------------------------------------------------------

    /// Generate text with GBNF grammar constraint.
    fn generate_with_grammar(
        &self,
        prompt: &str,
        grammar_str: &str,
        max_tokens: u32,
        temperature: f32,
    ) -> Result<String, String> {
        let grammar_sampler = LlamaSampler::grammar(&self.model, grammar_str, "root")
            .map_err(|e| format!("Failed to create grammar sampler: {}", e))?;

        let sampler = LlamaSampler::chain_simple([
            LlamaSampler::temp(temperature),
            grammar_sampler,
            LlamaSampler::dist(42),
        ]);

        self.run_inference(prompt, max_tokens, sampler)
    }

    /// Generate text without grammar constraint.
    ///
    /// Uses a time-based seed for the distribution sampler so that chat
    /// responses are non-deterministic (unlike entity extraction which keeps
    /// seed 42 for reproducibility).
    fn generate(&self, prompt: &str, max_tokens: u32, temperature: f32) -> Result<String, String> {
        // I9: Use a non-deterministic seed for chat generation.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();

        let sampler = LlamaSampler::chain_simple([
            LlamaSampler::top_k(40),
            LlamaSampler::top_p(0.95, 1),
            LlamaSampler::temp(temperature),
            LlamaSampler::dist(seed),
        ]);

        self.run_inference(prompt, max_tokens, sampler)
    }

    /// Core inference loop shared by grammar-constrained and free-form
    /// generation.
    ///
    /// Creates a fresh [`LlamaContext`] per call (required because
    /// `LlamaContext` is not `Send`).
    fn run_inference(
        &self,
        prompt: &str,
        max_tokens: u32,
        mut sampler: LlamaSampler,
    ) -> Result<String, String> {
        let ctx_params =
            LlamaContextParams::default().with_n_ctx(Some(NonZeroU32::new(2048).unwrap()));

        let mut ctx = self
            .model
            .new_context(&self.backend, ctx_params)
            .map_err(|e| format!("Failed to create context: {}", e))?;

        // Tokenize prompt
        let tokens = self
            .model
            .str_to_token(prompt, AddBos::Always)
            .map_err(|e| format!("Tokenization failed: {}", e))?;

        // Create batch and add prompt tokens
        let mut batch = LlamaBatch::new(2048, 1);

        for (i, token) in tokens.iter().enumerate() {
            let is_last = i == tokens.len() - 1;
            batch
                .add(*token, i as i32, &[0], is_last)
                .map_err(|e| format!("Failed to add token to batch: {}", e))?;
        }

        // Evaluate prompt
        ctx.decode(&mut batch)
            .map_err(|e| format!("Failed to decode prompt: {}", e))?;

        let mut output = String::new();
        let mut decoder = encoding_rs::UTF_8.new_decoder();

        // Generate tokens
        for _ in 0..max_tokens {
            // Sample next token using the configured sampler chain
            let new_token = sampler.sample(&ctx, batch.n_tokens() - 1);
            sampler.accept(new_token);

            // Check for end-of-generation
            if self.model.is_eog_token(new_token) {
                break;
            }

            // Decode token to string
            let piece = self
                .model
                .token_to_piece(new_token, &mut decoder, false, None)
                .map_err(|e| format!("Token decode failed: {}", e))?;
            output.push_str(&piece);

            // Prepare next batch
            batch.clear();
            batch
                .add(new_token, batch.n_tokens(), &[0], true)
                .map_err(|e| format!("Failed to add token: {}", e))?;

            ctx.decode(&mut batch)
                .map_err(|e| format!("Decode failed: {}", e))?;
        }

        Ok(output.trim().to_string())
    }

    // ------------------------------------------------------------------
    // Grammar definition
    // ------------------------------------------------------------------

    /// GBNF grammar for structured JSON entity extraction output.
    ///
    /// Matches the format expected by [`ExtractionResult`]:
    /// ```json
    /// {
    ///   "entities": [{ "name": "...", "entity_type": "Person", "description": "..." }],
    ///   "relations": [{ "source": "...", "target": "...", "relation_type": "...", "detail": "..." }]
    /// }
    /// ```
    fn json_grammar() -> String {
        r#"root   ::= "{" ws "\"entities\"" ws ":" ws entities "," ws "\"relations\"" ws ":" ws relations "}" ws
entities ::= "[" ws (entity ("," ws entity)*)? "]"
entity  ::= "{" ws "\"name\"" ws ":" ws string "," ws "\"entity_type\"" ws ":" ws entity-type ("," ws "\"description\"" ws ":" ws string)? "}" ws
entity-type ::= "\"Person\"" | "\"Organization\"" | "\"Location\"" | "\"Event\"" | "\"Topic\"" | "\"Product\""
relations ::= "[" ws (relation ("," ws relation)*)? "]"
relation ::= "{" ws "\"source\"" ws ":" ws string "," ws "\"target\"" ws ":" ws string "," ws "\"relation_type\"" ws ":" ws string ("," ws "\"detail\"" ws ":" ws string)? "}" ws
string  ::= "\"" [^"\\]* "\""
ws      ::= [ \t\n]*"#
            .to_string()
    }
}
