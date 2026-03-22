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
// Chat commands (backed by native LLM engine)
// ---------------------------------------------------------------------------

/// Send a chat message and get a response from the LLM, informed by the
/// current knowledge graph and transcript context.
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

    // Try LLM engine.
    let response_text = {
        let engine_guard = state
            .llm_engine
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        if let Some(ref engine) = *engine_guard {
            match engine.chat(&messages, &graph_context) {
                Ok(text) => text,
                Err(e) => {
                    log::warn!("LLM chat failed: {}", e);
                    format!(
                        "I can see the knowledge graph has {} entities and {} relationships. \
                         However, I couldn't generate a detailed response (LLM error: {}). \
                         Please check the model configuration.",
                        messages.len(),
                        graph_context.len(),
                        e
                    )
                }
            }
        } else {
            // No LLM loaded — provide a summary from graph context.
            format!(
                "LLM model not loaded. Here's what I know from the knowledge graph:\n\n{}",
                graph_context
            )
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
pub fn list_available_models() -> Vec<crate::models::ModelInfo> {
    crate::models::list_models()
}

/// Download a model by filename, with progress events emitted to the frontend.
#[tauri::command]
pub fn download_model_cmd(
    model_filename: String,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let models = crate::models::list_models();
    let model = models
        .iter()
        .find(|m| m.filename == model_filename)
        .ok_or_else(|| format!("Model not found: {}", model_filename))?;

    let path =
        crate::models::download_model(&model.name, &model.url, &model.filename, &app_handle)?;
    Ok(path.to_string_lossy().to_string())
}
