//! Tauri IPC command handlers.
//!
//! Each function here is exposed to the frontend via `tauri::generate_handler![]`.
//! They access `AppState` through Tauri's managed state.
//!
//! Heavy processing logic (speech, extraction) lives in the [`crate::speech`]
//! module — this file only contains thin `#[tauri::command]` wrappers.

use tauri::{Emitter, State};

use crate::audio::pipeline::AudioPipeline;
use crate::audio::vad::{VadConfig, VadProcessor};
use crate::events::{self, PipelineStatus, StageStatus};
use crate::graph::entities::GraphSnapshot;
use crate::llm::engine::{ChatMessage, ChatResponse};
use crate::llm::{ApiClient, ApiConfig};
use crate::speech;
use crate::state::{AppState, AudioSourceInfo, TranscriptSegment};

// ---------------------------------------------------------------------------
// Helper: parse source_id string into rsac::CaptureTarget
// ---------------------------------------------------------------------------

/// Map a frontend source ID string to an rsac [`CaptureTarget`].
///
/// Supported formats:
/// - `"system-default"`          → `CaptureTarget::SystemDefault`
/// - `"device:<device_id>"`      → `CaptureTarget::Device(DeviceId(device_id))`
/// - `"app:<pid>"`               → `CaptureTarget::Application(ApplicationId(pid))`
/// - `"app-name:<name>"`         → `CaptureTarget::ApplicationByName(name)`
fn parse_capture_target(source_id: &str) -> Result<rsac::CaptureTarget, String> {
    if source_id == "system-default" {
        Ok(rsac::CaptureTarget::SystemDefault)
    } else if let Some(device_id) = source_id.strip_prefix("device:") {
        Ok(rsac::CaptureTarget::Device(rsac::DeviceId(
            device_id.to_string(),
        )))
    } else if let Some(pid_str) = source_id.strip_prefix("app:") {
        // ApplicationId wraps a String (the PID as a string).
        Ok(rsac::CaptureTarget::Application(rsac::ApplicationId(
            pid_str.to_string(),
        )))
    } else if let Some(name) = source_id.strip_prefix("app-name:") {
        Ok(rsac::CaptureTarget::ApplicationByName(name.to_string()))
    } else {
        Err(format!("Unknown source ID format: {}", source_id))
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// List available audio sources (devices + running applications).
#[tauri::command]
pub async fn list_audio_sources(
    state: State<'_, AppState>,
) -> Result<Vec<AudioSourceInfo>, String> {
    log::info!("list_audio_sources called");
    let manager = state
        .capture_manager
        .lock()
        .map_err(|e| format!("Lock error: {}", e))?;
    Ok(manager.list_sources())
}

/// Start capturing audio from the specified source.
#[tauri::command]
pub async fn start_capture(
    source_id: String,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    log::info!("start_capture called for source: {}", source_id);

    let target = parse_capture_target(&source_id)?;

    // 1. Start capture via the manager.
    {
        let mut manager = state
            .capture_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        manager.start_capture(&source_id, target, state.pipeline_tx.clone(), app.clone())?;
    }

    // 2. Start pipeline thread if not already running.
    {
        let mut pipeline_handle = state
            .pipeline_thread
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        if pipeline_handle.is_none() {
            let rx = state.pipeline_rx.clone();
            let tx = state.processed_tx.clone();
            let handle = std::thread::Builder::new()
                .name("audio-pipeline".to_string())
                .spawn(move || {
                    let mut pipeline = AudioPipeline::new(rx, tx);
                    pipeline.run();
                })
                .map_err(|e| format!("Failed to spawn pipeline thread: {}", e))?;
            *pipeline_handle = Some(handle);
            log::info!("Pipeline thread spawned");
        }
    }

    // 3. Start VAD thread if not already running.
    {
        let mut vad_handle = state
            .vad_thread
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        if vad_handle.is_none() {
            let processed_rx = state.processed_rx.clone();
            let speech_tx = state.speech_tx.clone();
            let vad_config = VadConfig::default();

            let handle = std::thread::Builder::new()
                .name("vad-worker".to_string())
                .spawn(move || {
                    let mut processor = VadProcessor::new(vad_config, speech_tx);
                    processor.run(processed_rx);
                    log::info!("VAD worker thread exited");
                })
                .map_err(|e| format!("Failed to spawn VAD thread: {}", e))?;
            *vad_handle = Some(handle);
            log::info!("VAD worker thread spawned");
        }
    }

    // 4. Start speech processor thread (ASR + Diarization orchestrator).
    {
        let mut sp_handle = state
            .speech_processor_thread
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        if sp_handle.is_none() {
            let speech_rx = state.speech_rx.clone();

            let transcript_buffer = state.transcript_buffer.clone();
            let pipeline_status = state.pipeline_status.clone();
            let app_handle = app.clone();
            let knowledge_graph = state.knowledge_graph.clone();
            let graph_snapshot_clone = state.graph_snapshot.clone();
            let graph_extractor = state.graph_extractor.clone();
            let llm_engine = state.llm_engine.clone();
            let api_client = state.api_client.clone();

            let models_dir = crate::models::get_models_dir(&app);

            // Read the ASR provider setting so the speech processor knows
            // whether to load the local Whisper model or skip it.
            let asr_provider = state
                .app_settings
                .read()
                .map(|s| s.asr_provider.clone())
                .unwrap_or_default();

            // Read the LLM provider setting so the speech processor knows
            // whether to use the local LLM engine or the API client for
            // entity extraction.
            let llm_provider = state
                .app_settings
                .read()
                .map(|s| s.llm_provider.clone())
                .unwrap_or_default();

            // If the user selected local LLM and the engine is not yet
            // loaded, attempt to load it now on a blocking background task.
            if matches!(llm_provider, crate::settings::LlmProvider::LocalLlama) {
                let engine_empty = state
                    .llm_engine
                    .lock()
                    .map(|g| g.is_none())
                    .unwrap_or(false);
                if engine_empty {
                    let models_dir_clone = models_dir.clone();
                    let llm_engine_clone = state.llm_engine.clone();
                    let model_path = models_dir_clone.join(crate::models::LLM_MODEL_FILENAME);
                    if model_path.exists() {
                        log::info!("Auto-loading local LLM model for LocalLlama provider...");
                        let _ = std::thread::Builder::new()
                            .name("llm-autoload".to_string())
                            .spawn(move || {
                                match crate::llm::LlmEngine::new(&model_path.to_string_lossy()) {
                                    Ok(engine) => {
                                        if let Ok(mut guard) = llm_engine_clone.lock() {
                                            *guard = Some(engine);
                                            log::info!("Local LLM model auto-loaded successfully");
                                        }
                                    }
                                    Err(e) => {
                                        log::warn!("Failed to auto-load local LLM model: {}", e);
                                    }
                                }
                            });
                    }
                }
            }

            // If the user selected API LLM provider, configure the API
            // client from the provider settings.
            if let crate::settings::LlmProvider::Api {
                ref endpoint,
                ref api_key,
                ref model,
            } = llm_provider
            {
                let api_empty = state
                    .api_client
                    .lock()
                    .map(|g| g.is_none())
                    .unwrap_or(false);
                if api_empty && !endpoint.is_empty() {
                    // Read advanced parameters from the user's persisted
                    // llm_api_config settings, falling back to
                    // extraction-oriented defaults.
                    let (api_max_tokens, api_temperature) = state
                        .app_settings
                        .read()
                        .ok()
                        .and_then(|s| {
                            s.llm_api_config
                                .as_ref()
                                .map(|c| (c.max_tokens, c.temperature))
                        })
                        .unwrap_or((512, 0.1));

                    let config = crate::llm::ApiConfig {
                        endpoint: endpoint.clone(),
                        api_key: if api_key.is_empty() {
                            None
                        } else {
                            Some(api_key.clone())
                        },
                        model: model.clone(),
                        max_tokens: api_max_tokens,
                        temperature: api_temperature,
                    };
                    let client = crate::llm::ApiClient::new(config);
                    if client.is_configured() {
                        if let Ok(mut guard) = state.api_client.lock() {
                            *guard = Some(client);
                            log::info!("API client auto-configured from LLM provider settings");
                        }
                    }
                }
            }

            let handle = std::thread::Builder::new()
                .name("speech-processor".to_string())
                .spawn(move || {
                    speech::run_speech_processor(
                        speech_rx,
                        transcript_buffer,
                        pipeline_status,
                        app_handle,
                        knowledge_graph,
                        graph_snapshot_clone,
                        graph_extractor,
                        llm_engine,
                        api_client,
                        models_dir,
                        asr_provider,
                        llm_provider,
                    );
                })
                .map_err(|e| format!("Failed to spawn speech processor thread: {}", e))?;
            *sp_handle = Some(handle);
            log::info!("Speech processor thread spawned");
        }
    }

    // 5. Update state flags.
    if let Ok(mut capturing) = state.is_capturing.write() {
        *capturing = true;
    }
    if let Ok(mut status) = state.pipeline_status.write() {
        status.capture = StageStatus::Running { processed_count: 0 };
        status.pipeline = StageStatus::Running { processed_count: 0 };
        status.asr = StageStatus::Running { processed_count: 0 };
        status.diarization = StageStatus::Running { processed_count: 0 };
        status.entity_extraction = StageStatus::Running { processed_count: 0 };
        status.graph = StageStatus::Running { processed_count: 0 };
    }

    // Emit initial pipeline status event
    if let Ok(status) = state.pipeline_status.read() {
        let _ = app.emit(events::PIPELINE_STATUS_EVENT, &*status);
    }

    log::info!("Started capture for source: {}", source_id);
    Ok(())
}

/// Stop capturing audio from the specified source.
#[tauri::command]
pub async fn stop_capture(source_id: String, state: State<'_, AppState>) -> Result<(), String> {
    log::info!("stop_capture called for source: {}", source_id);

    let remaining;
    {
        let mut manager = state
            .capture_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        manager.stop_capture(&source_id)?;
        remaining = manager.active_captures().len();
    }

    if remaining == 0 {
        if let Ok(mut capturing) = state.is_capturing.write() {
            *capturing = false;
        }
        if let Ok(mut status) = state.pipeline_status.write() {
            status.capture = StageStatus::Idle;
        }
    }

    log::info!("Stopped capture for source: {}", source_id);
    Ok(())
}

/// Get the current knowledge graph snapshot.
#[tauri::command]
pub async fn get_graph_snapshot(state: State<'_, AppState>) -> Result<GraphSnapshot, String> {
    let snapshot = state
        .graph_snapshot
        .read()
        .map_err(|e| format!("Failed to read graph snapshot: {}", e))?;
    Ok(snapshot.clone())
}

/// Get transcript segments, optionally filtered by source and time.
#[tauri::command]
pub async fn get_transcript(
    source_id: Option<String>,
    since: Option<f64>,
    state: State<'_, AppState>,
) -> Result<Vec<TranscriptSegment>, String> {
    let buffer = state
        .transcript_buffer
        .read()
        .map_err(|e| format!("Failed to read transcript buffer: {}", e))?;

    let segments: Vec<TranscriptSegment> = buffer
        .iter()
        .filter(|seg| {
            let source_match = source_id
                .as_ref()
                .map(|id| &seg.source_id == id)
                .unwrap_or(true);
            let time_match = since.map(|t| seg.start_time >= t).unwrap_or(true);
            source_match && time_match
        })
        .cloned()
        .collect();

    Ok(segments)
}

/// Get the current pipeline status.
#[tauri::command]
pub async fn get_pipeline_status(state: State<'_, AppState>) -> Result<PipelineStatus, String> {
    let status = state
        .pipeline_status
        .read()
        .map_err(|e| format!("Failed to read pipeline status: {}", e))?;
    Ok(status.clone())
}

// ---------------------------------------------------------------------------
// API endpoint configuration
// ---------------------------------------------------------------------------

/// Configure an OpenAI-compatible API endpoint for LLM inference.
///
/// This allows using cloud providers (OpenAI, OpenRouter) or local servers
/// (Ollama, LM Studio, vLLM) as an alternative to the native llama-cpp-2 engine.
#[tauri::command]
pub async fn configure_api_endpoint(
    endpoint: String,
    api_key: Option<String>,
    model: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    log::info!(
        "configure_api_endpoint: endpoint={}, model={}",
        endpoint,
        model
    );

    let config = ApiConfig {
        endpoint,
        api_key,
        model,
        max_tokens: 512,
        temperature: 0.1,
    };
    let client = ApiClient::new(config);

    if !client.is_configured() {
        return Err("Invalid API configuration: endpoint and model must be non-empty".to_string());
    }

    *state
        .api_client
        .lock()
        .map_err(|e| format!("Lock error: {}", e))? = Some(client);

    log::info!("API endpoint configured successfully");
    Ok(())
}

// ---------------------------------------------------------------------------
// Chat commands (backed by native LLM engine or API client)
// ---------------------------------------------------------------------------

/// Send a chat message and get a response from the LLM, informed by the
/// current knowledge graph and transcript context.
///
/// Tries backends in order: native LLM → API client → graph context fallback.
///
/// I4 fix: takes a snapshot of the graph and transcript, releases the locks,
/// then builds the context string from the snapshot (no lock held during
/// string formatting).
#[tauri::command]
pub async fn send_chat_message(
    message: String,
    state: State<'_, AppState>,
) -> Result<ChatResponse, String> {
    log::info!(
        "send_chat_message called: {}",
        &message[..message.len().min(50)]
    );

    // I4: Take a snapshot of graph data, then release the lock immediately.
    let snapshot = {
        let kg = state
            .knowledge_graph
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        kg.snapshot() // returns cloned GraphSnapshot
    }; // lock released here

    // Take a snapshot of recent transcript, then release that lock too.
    let recent_transcript: Vec<TranscriptSegment> = {
        let transcript = state
            .transcript_buffer
            .read()
            .map_err(|e| format!("Lock error: {}", e))?;
        transcript.iter().rev().take(10).cloned().collect()
    }; // lock released here

    // Build graph context string from snapshots — no locks held.
    let graph_context = {
        let mut ctx = String::new();

        ctx.push_str(&format!("Entities ({}):\n", snapshot.nodes.len()));
        for node in &snapshot.nodes {
            ctx.push_str(&format!("- {} ({})", node.name, node.entity_type));
            if let Some(ref desc) = node.description {
                ctx.push_str(&format!(": {}", desc));
            }
            ctx.push('\n');
        }

        ctx.push_str(&format!("\nRelationships ({}):\n", snapshot.links.len()));
        for link in &snapshot.links {
            ctx.push_str(&format!(
                "- {} → {} ({})\n",
                link.source, link.target, link.relation_type
            ));
        }

        // Add recent transcript from snapshot
        if !recent_transcript.is_empty() {
            ctx.push_str("\nRecent Transcript:\n");
            for seg in recent_transcript.iter().rev() {
                let speaker = seg.speaker_label.as_deref().unwrap_or("Unknown");
                ctx.push_str(&format!("[{}]: {}\n", speaker, seg.text));
            }
        }

        ctx
    };

    // Add user message to history.
    let user_msg = ChatMessage {
        role: "user".to_string(),
        content: message,
    };

    {
        let mut history = state
            .chat_history
            .write()
            .map_err(|e| format!("Lock error: {}", e))?;
        history.push(user_msg.clone());
    }

    // Get chat history for context.
    let messages: Vec<ChatMessage> = {
        let history = state
            .chat_history
            .read()
            .map_err(|e| format!("Lock error: {}", e))?;
        history.clone()
    };

    // Try backends in order: native LLM → API client → graph context fallback.
    let response_text = {
        // 1. Try native LLM engine first.
        let native_result = {
            let engine_guard = state
                .llm_engine
                .lock()
                .map_err(|e| format!("Lock error: {}", e))?;
            if let Some(ref engine) = *engine_guard {
                match engine.chat(&messages, &graph_context) {
                    Ok(text) => Some(Ok(text)),
                    Err(e) => {
                        log::warn!("Native LLM chat failed: {}", e);
                        Some(Err(e))
                    }
                }
            } else {
                None // No native LLM loaded
            }
        };

        match native_result {
            Some(Ok(text)) => text,
            _ => {
                // 2. Try API client.
                let api_result = {
                    let api_guard = state
                        .api_client
                        .lock()
                        .map_err(|e| format!("Lock error: {}", e))?;
                    if let Some(ref client) = *api_guard {
                        match client.chat_with_history(&messages, &graph_context) {
                            Ok(text) => Some(Ok(text)),
                            Err(e) => {
                                log::warn!("API chat failed: {}", e);
                                Some(Err(e))
                            }
                        }
                    } else {
                        None // No API client configured
                    }
                };

                match api_result {
                    Some(Ok(text)) => text,
                    Some(Err(e)) => {
                        // API configured but failed — report error with context.
                        format!(
                            "I can see the knowledge graph has {} entities and {} relationships. \
                             However, I couldn't generate a detailed response (API error: {}). \
                             Please check the API endpoint configuration.",
                            snapshot.nodes.len(),
                            snapshot.links.len(),
                            e
                        )
                    }
                    None => {
                        // 3. No backend available — provide graph context fallback.
                        if let Some(Err(e)) = native_result {
                            format!(
                                "Native LLM error: {}. No API endpoint configured.\n\n\
                                 Here's what I know from the knowledge graph:\n\n{}",
                                e, graph_context
                            )
                        } else {
                            format!(
                                "No LLM backend available. Configure a native model or API endpoint.\n\n\
                                 Here's what I know from the knowledge graph:\n\n{}",
                                graph_context
                            )
                        }
                    }
                }
            }
        }
    };

    let assistant_msg = ChatMessage {
        role: "assistant".to_string(),
        content: response_text,
    };

    // Add assistant message to history.
    {
        let mut history = state
            .chat_history
            .write()
            .map_err(|e| format!("Lock error: {}", e))?;
        history.push(assistant_msg.clone());
    }

    Ok(ChatResponse {
        message: assistant_msg,
        tokens_used: 0, // TODO: track actual token usage
    })
}

/// Get the current chat message history.
#[tauri::command]
pub async fn get_chat_history(state: State<'_, AppState>) -> Result<Vec<ChatMessage>, String> {
    let history = state
        .chat_history
        .read()
        .map_err(|e| format!("Lock error: {}", e))?;
    Ok(history.clone())
}

/// Clear the chat message history.
#[tauri::command]
pub async fn clear_chat_history(state: State<'_, AppState>) -> Result<(), String> {
    let mut history = state
        .chat_history
        .write()
        .map_err(|e| format!("Lock error: {}", e))?;
    history.clear();
    Ok(())
}

// ---------------------------------------------------------------------------
// Model management commands
// ---------------------------------------------------------------------------

/// List available models and their download status.
#[tauri::command]
pub fn list_available_models(app: tauri::AppHandle) -> Vec<crate::models::ModelInfo> {
    crate::models::list_models(&app)
}

/// Download a model by filename, with progress events emitted to the frontend.
///
/// Runs the blocking HTTP download on a background thread via
/// `tokio::task::spawn_blocking` so the IPC handler stays async (G3).
#[tauri::command]
pub async fn download_model_cmd(
    app: tauri::AppHandle,
    model_filename: String,
) -> Result<String, String> {
    let handle = app.clone();
    tokio::task::spawn_blocking(move || crate::models::download_model(&handle, &model_filename))
        .await
        .map_err(|e| format!("Download task failed: {}", e))?
}

/// Get the readiness status of all known models (G1).
#[tauri::command]
pub fn get_model_status(app: tauri::AppHandle) -> crate::models::ModelStatus {
    crate::models::get_model_status(&app)
}

/// Load the native LLM model into memory (G2).
///
/// Resolves the model path from the app data directory, then loads it on a
/// background thread. On success the engine is stored in `AppState.llm_engine`.
#[tauri::command]
pub async fn load_llm_model(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let models_dir = crate::models::get_models_dir(&app);
    let model_path = models_dir.join(crate::models::LLM_MODEL_FILENAME);

    if !model_path.exists() {
        return Err("LLM model not downloaded".to_string());
    }

    let path = model_path.clone();
    let engine =
        tokio::task::spawn_blocking(move || crate::llm::LlmEngine::new(&path.to_string_lossy()))
            .await
            .map_err(|e| format!("Failed to spawn LLM loading task: {}", e))?
            .map_err(|e| format!("Failed to load LLM model: {}", e))?;

    let mut guard = state.llm_engine.lock().map_err(|e| e.to_string())?;
    *guard = Some(engine);

    Ok("LLM model loaded successfully".to_string())
}

// ---------------------------------------------------------------------------
// Settings commands
// ---------------------------------------------------------------------------

/// Load application settings from disk (returns defaults if missing).
/// Syncs the loaded settings into the in-memory `AppState.app_settings` cache
/// so other backend modules (e.g. speech processor) can read them without I/O.
#[tauri::command]
pub fn load_settings_cmd(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> crate::settings::AppSettings {
    let settings = crate::settings::load_settings(&app);
    // Sync in-memory cache
    if let Ok(mut cached) = state.app_settings.write() {
        *cached = settings.clone();
    }
    settings
}

/// Save application settings to disk (atomic write).
/// Also updates the in-memory `AppState.app_settings` cache.
#[tauri::command]
pub fn save_settings_cmd(
    app: tauri::AppHandle,
    settings: crate::settings::AppSettings,
    state: State<'_, AppState>,
) -> Result<(), String> {
    crate::settings::save_settings(&app, &settings)?;
    // Sync in-memory cache
    if let Ok(mut cached) = state.app_settings.write() {
        *cached = settings;
    }
    Ok(())
}

/// Delete a downloaded model file by filename.
#[tauri::command]
pub fn delete_model_cmd(app: tauri::AppHandle, model_filename: String) -> Result<String, String> {
    crate::models::delete_model(&app, &model_filename)
}

// ---------------------------------------------------------------------------
// Process enumeration
// ---------------------------------------------------------------------------

/// A running system process (for target-selection UI).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub exe_path: Option<String>,
}

/// List running system processes (deduplicated by name, sorted alphabetically).
#[tauri::command]
pub fn list_running_processes() -> Vec<ProcessInfo> {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let mut processes: Vec<ProcessInfo> = sys
        .processes()
        .iter()
        .filter(|(_, p)| !p.name().to_string_lossy().is_empty())
        .map(|(pid, p)| ProcessInfo {
            pid: pid.as_u32(),
            name: p.name().to_string_lossy().to_string(),
            exe_path: p.exe().map(|e| e.to_string_lossy().to_string()),
        })
        .collect();

    processes.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    processes.dedup_by(|a, b| a.name == b.name);
    processes
}
