//! OpenAI-compatible API client for LLM inference.
//!
//! Calls any OpenAI-compatible chat completions endpoint (OpenAI, Ollama,
//! LM Studio, vLLM, OpenRouter, Anthropic via proxy, etc.).
//! Used as an alternative to the native llama-cpp-2 engine.

use serde::{Deserialize, Serialize};

use crate::graph::entities::ExtractionResult;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for an OpenAI-compatible API endpoint.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiConfig {
    /// Base URL, e.g. `"https://api.openai.com/v1"` or `"http://localhost:11434/v1"`.
    pub endpoint: String,
    /// Bearer token.  `None` for local servers (Ollama, LM Studio).
    pub api_key: Option<String>,
    /// Model identifier, e.g. `"gpt-4o-mini"`, `"llama3.2"`, `"qwen2.5:3b"`.
    pub model: String,
    /// Maximum tokens to generate (default 512).
    pub max_tokens: u32,
    /// Sampling temperature (default 0.1 for extraction, 0.7 for chat).
    pub temperature: f32,
}

// ---------------------------------------------------------------------------
// Request / Response types (OpenAI Chat Completions)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ApiMessage>,
    max_tokens: u32,
    temperature: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

#[derive(Serialize)]
struct ApiMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    format_type: String,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    content: String,
}

// ---------------------------------------------------------------------------
// ApiClient
// ---------------------------------------------------------------------------

/// OpenAI-compatible API client.
///
/// Thread-safe: `reqwest::blocking::Client` is `Send + Sync`.
pub struct ApiClient {
    config: ApiConfig,
    client: reqwest::blocking::Client,
}

impl ApiClient {
    /// Create a new API client with the given configuration.
    pub fn new(config: ApiConfig) -> Self {
        Self {
            config,
            client: reqwest::blocking::Client::new(),
        }
    }

    /// Returns `true` if the client has a non-empty endpoint and model.
    pub fn is_configured(&self) -> bool {
        !self.config.endpoint.is_empty() && !self.config.model.is_empty()
    }

    // ------------------------------------------------------------------
    // Low-level chat completion
    // ------------------------------------------------------------------

    /// Send a chat completion request and return the assistant's reply.
    ///
    /// `messages` is a list of `(role, content)` tuples.
    /// When `json_mode` is true, the request includes `response_format: { type: "json_object" }`.
    pub fn chat_completion(
        &self,
        messages: Vec<(String, String)>,
        json_mode: bool,
    ) -> Result<String, String> {
        let api_messages: Vec<ApiMessage> = messages
            .into_iter()
            .map(|(role, content)| ApiMessage { role, content })
            .collect();

        let request = ChatCompletionRequest {
            model: self.config.model.clone(),
            messages: api_messages,
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
            response_format: if json_mode {
                Some(ResponseFormat {
                    format_type: "json_object".to_string(),
                })
            } else {
                None
            },
        };

        let url = format!(
            "{}/chat/completions",
            self.config.endpoint.trim_end_matches('/')
        );

        let mut req = self.client.post(&url).json(&request);

        if let Some(ref key) = self.config.api_key {
            if !key.is_empty() {
                req = req.header("Authorization", format!("Bearer {}", key));
            }
        }

        let response = req
            .send()
            .map_err(|e| format!("API request to {} failed: {}", url, e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            return Err(format!("API error {} from {}: {}", status, url, body));
        }

        let completion: ChatCompletionResponse = response
            .json()
            .map_err(|e| format!("Failed to parse API response: {}", e))?;

        completion
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .ok_or_else(|| "No response choices from API".to_string())
    }

    // ------------------------------------------------------------------
    // Entity extraction (JSON mode)
    // ------------------------------------------------------------------

    /// Extract entities and relationships from a transcript segment.
    ///
    /// Uses JSON mode to request structured output matching [`ExtractionResult`].
    pub fn extract_entities(&self, text: &str, speaker: &str) -> Result<ExtractionResult, String> {
        let system_prompt = "Extract entities and relationships from this conversation segment. \
             Output valid JSON with this exact structure: \
             {\"entities\": [{\"name\": \"...\", \"entity_type\": \"Person|Organization|Location|Event|Topic|Product\", \"description\": \"...\"}], \
             \"relations\": [{\"source\": \"...\", \"target\": \"...\", \"relation_type\": \"...\", \"detail\": \"...\"}]}. \
             If no entities are found, return {\"entities\": [], \"relations\": []}.".to_string();

        let user_prompt = format!("[{}]: {}", speaker, text);

        let raw = self.chat_completion(
            vec![
                ("system".to_string(), system_prompt),
                ("user".to_string(), user_prompt),
            ],
            true, // JSON mode
        )?;

        serde_json::from_str::<ExtractionResult>(&raw).map_err(|e| {
            format!(
                "Failed to parse extraction JSON from API: {} — raw: {}",
                e, raw
            )
        })
    }

    // ------------------------------------------------------------------
    // Chat with knowledge graph context
    // ------------------------------------------------------------------

    /// Chat with the knowledge graph context, using the OpenAI-compatible API.
    pub fn chat(&self, user_message: &str, graph_context: &str) -> Result<String, String> {
        let system_prompt = format!(
            "You are a knowledge graph assistant analyzing a live audio conversation. \
             Here is the current knowledge graph context:\n\n{}\n\n\
             Answer the user's question about the conversation, people, topics, or relationships discussed.",
            graph_context
        );

        // Use a higher temperature for chat
        let messages = vec![
            ("system".to_string(), system_prompt),
            ("user".to_string(), user_message.to_string()),
        ];

        self.chat_completion(messages, false)
    }

    /// Chat with full message history and knowledge graph context.
    pub fn chat_with_history(
        &self,
        messages: &[crate::llm::engine::ChatMessage],
        graph_context: &str,
    ) -> Result<String, String> {
        let system_prompt = format!(
            "You are a knowledge graph assistant analyzing a live audio conversation. \
             Here is the current knowledge graph context:\n\n{}\n\n\
             Answer the user's question about the conversation, people, topics, or relationships discussed.",
            graph_context
        );

        let mut api_messages = vec![("system".to_string(), system_prompt)];

        for msg in messages {
            api_messages.push((msg.role.clone(), msg.content.clone()));
        }

        self.chat_completion(api_messages, false)
    }
}
