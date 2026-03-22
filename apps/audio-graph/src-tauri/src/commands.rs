//! Tauri IPC command handlers.
//!
//! Each function here is exposed to the frontend via `tauri::generate_handler![]`.
//! They access `AppState` through Tauri's managed state.

use tauri::{Emitter, State};

use crate::asr::{AsrConfig, AsrWorker};
use crate::audio::pipeline::AudioPipeline;
use crate::audio::vad::{VadConfig, VadProcessor};
use crate::diarization::{
    DiarizationConfig, DiarizationInput, DiarizationWorker, DiarizedTranscript,
};
use crate::events::{self, PipelineStatus, StageStatus};
use crate::graph::entities::GraphSnapshot;
use crate::graph::extraction::RuleBasedExtractor;
use crate::graph::temporal::TemporalKnowledgeGraph;
use crate::llm::engine::{ChatMessage, ChatResponse};
use crate::llm::LlmEngine;
use crate::sidecar::SidecarManager;
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
            let sidecar_manager = state.sidecar_manager.clone();
            let llm_engine = state.llm_engine.clone();

            let handle = std::thread::Builder::new()
                .name("speech-processor".to_string())
                .spawn(move || {
                    run_speech_processor(
                        speech_rx,
                        transcript_buffer,
                        pipeline_status,
                        app_handle,
                        knowledge_graph,
                        graph_snapshot_clone,
                        graph_extractor,
                        sidecar_manager,
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

// ---------------------------------------------------------------------------
// Helper: extraction + graph update + event emission (I1: deduplicated)
// ---------------------------------------------------------------------------

/// Perform entity extraction, update the knowledge graph, and emit events.
///
/// Shared by both the full (ASR + diarization) and diarization-only speech
/// processor loops. Tries the native LLM engine first, falls back to the
/// sidecar LLM, then to rule-based extraction.
#[allow(clippy::too_many_arguments)]
fn process_extraction_and_emit(
    text: &str,
    speaker: &str,
    segment_id: &str,
    timestamp: f64,
    llm_engine: &std::sync::Arc<std::sync::Mutex<Option<LlmEngine>>>,
    sidecar_manager: &std::sync::Arc<std::sync::Mutex<SidecarManager>>,
    graph_extractor: &std::sync::Arc<RuleBasedExtractor>,
    knowledge_graph: &std::sync::Arc<std::sync::Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: &std::sync::Arc<std::sync::RwLock<GraphSnapshot>>,
    pipeline_status: &std::sync::Arc<std::sync::RwLock<PipelineStatus>>,
    app_handle: &tauri::AppHandle,
    extraction_count: &mut u64,
    graph_update_count: &mut u64,
) {
    // Try native LLM engine first, then sidecar, then rule-based
    let llm_result = {
        let engine_guard = llm_engine.lock().unwrap_or_else(|e| {
            log::warn!("LLM engine mutex poisoned, recovering: {}", e);
            e.into_inner()
        });
        if let Some(ref engine) = *engine_guard {
            match engine.extract_entities(text, speaker) {
                Ok(result) => Some(result),
                Err(e) => {
                    log::warn!("Native LLM extraction failed: {}", e);
                    None
                }
            }
        } else {
            None
        }
    };

    let extraction_result = if let Some(result) = llm_result {
        log::debug!(
            "Native LLM extraction: {} entities, {} relations",
            result.entities.len(),
            result.relations.len()
        );
        result
    } else {
        // Fallback: try sidecar, then rule-based
        let sidecar = sidecar_manager.lock().unwrap_or_else(|e| {
            log::warn!("Sidecar manager mutex poisoned, recovering: {}", e);
            e.into_inner()
        });
        if sidecar.is_healthy() {
            match sidecar.extract_entities(speaker, text) {
                Ok(result) => {
                    log::debug!(
                        "Sidecar LLM extraction: {} entities, {} relations",
                        result.entities.len(),
                        result.relations.len()
                    );
                    result
                }
                Err(e) => {
                    log::warn!("Sidecar extraction failed, using rule-based: {}", e);
                    graph_extractor.extract(speaker, text)
                }
            }
        } else {
            graph_extractor.extract(speaker, text)
        }
    };

    *extraction_count += 1;

    // Feed extraction into the knowledge graph
    if !extraction_result.entities.is_empty() {
        let mut graph = knowledge_graph.lock().unwrap_or_else(|e| {
            log::warn!("Knowledge graph mutex poisoned, recovering: {}", e);
            e.into_inner()
        });
        graph.process_extraction(&extraction_result, timestamp, speaker, segment_id);

        *graph_update_count += 1;

        // Update graph snapshot for frontend
        let snapshot = graph.snapshot();
        if let Ok(mut gs) = graph_snapshot.write() {
            *gs = snapshot.clone();
        }

        // Emit graph-update event
        let _ = app_handle.emit(crate::events::GRAPH_UPDATE, &snapshot);

        log::debug!(
            "Graph updated: {} nodes, {} edges",
            snapshot.stats.total_nodes,
            snapshot.stats.total_edges
        );
    }

    // Update entity_extraction and graph status, then emit pipeline status
    if let Ok(mut status) = pipeline_status.write() {
        status.entity_extraction = StageStatus::Running {
            processed_count: *extraction_count,
        };
        status.graph = StageStatus::Running {
            processed_count: *graph_update_count,
        };
    }
    if let Ok(status) = pipeline_status.read() {
        let _ = app_handle.emit(events::PIPELINE_STATUS_EVENT, &*status);
    }
}

// ---------------------------------------------------------------------------
// Speech processor threads
// ---------------------------------------------------------------------------

/// Speech processor orchestrator — runs ASR and diarization inline on a
/// single thread. Receives `SpeechSegment`s from VAD, transcribes each via
/// Whisper, diarizes, then emits Tauri events and stores results.
fn run_speech_processor(
    speech_rx: crossbeam_channel::Receiver<crate::audio::vad::SpeechSegment>,
    transcript_buffer: std::sync::Arc<
        std::sync::RwLock<std::collections::VecDeque<TranscriptSegment>>,
    >,
    pipeline_status: std::sync::Arc<std::sync::RwLock<PipelineStatus>>,
    app_handle: tauri::AppHandle,
    knowledge_graph: std::sync::Arc<std::sync::Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: std::sync::Arc<std::sync::RwLock<GraphSnapshot>>,
    graph_extractor: std::sync::Arc<RuleBasedExtractor>,
    sidecar_manager: std::sync::Arc<std::sync::Mutex<SidecarManager>>,
    llm_engine: std::sync::Arc<std::sync::Mutex<Option<LlmEngine>>>,
) {
    use whisper_rs::{WhisperContext, WhisperContextParameters};

    log::info!("Speech processor: loading Whisper model...");

    let asr_config = AsrConfig::default();
    let model_path_str = asr_config.model_path.display().to_string();

    // Load Whisper model — must stay on this thread
    let ctx =
        match WhisperContext::new_with_params(&model_path_str, WhisperContextParameters::default())
        {
            Ok(ctx) => {
                log::info!(
                    "Speech processor: Whisper model loaded from {}",
                    model_path_str
                );
                ctx
            }
            Err(e) => {
                log::error!(
                    "Speech processor: failed to load Whisper model from {}: {}. \
                 ASR disabled — will still run diarization on speech segments.",
                    model_path_str,
                    e
                );
                // Run in diarization-only mode (no ASR)
                run_speech_processor_diarization_only(
                    speech_rx,
                    transcript_buffer,
                    pipeline_status,
                    app_handle,
                    knowledge_graph,
                    graph_snapshot,
                    graph_extractor,
                    sidecar_manager,
                    llm_engine,
                );
                return;
            }
        };

    let mut whisper_state = match ctx.create_state() {
        Ok(s) => s,
        Err(e) => {
            log::error!("Speech processor: failed to create Whisper state: {}", e);
            run_speech_processor_diarization_only(
                speech_rx,
                transcript_buffer,
                pipeline_status,
                app_handle,
                knowledge_graph,
                graph_snapshot,
                graph_extractor,
                sidecar_manager,
                llm_engine,
            );
            return;
        }
    };

    // Create ASR worker (with a dummy output channel — we call transcribe_segment directly)
    let (dummy_asr_tx, _dummy_asr_rx) = crossbeam_channel::unbounded::<TranscriptSegment>();
    let mut asr_worker = AsrWorker::new(asr_config, dummy_asr_tx);

    // Create Diarization worker (with a dummy output channel — we call process_input directly)
    let diarization_config = DiarizationConfig::default();
    let (dummy_diar_tx, _dummy_diar_rx) = crossbeam_channel::unbounded::<DiarizedTranscript>();
    let mut diarization_worker = DiarizationWorker::new(diarization_config, dummy_diar_tx);

    let mut asr_count: u64 = 0;
    let mut diarization_count: u64 = 0;
    let mut extraction_count: u64 = 0;
    let mut graph_update_count: u64 = 0;

    log::info!("Speech processor: entering processing loop (ASR + diarization)");

    while let Ok(speech_segment) = speech_rx.recv() {
        // 1. Run ASR transcription
        match asr_worker.transcribe_segment(&mut whisper_state, &speech_segment) {
            Ok(transcripts) => {
                for transcript in transcripts {
                    asr_count += 1;

                    // 2. Run diarization
                    let input = DiarizationInput {
                        transcript,
                        speech_audio: speech_segment.audio.clone(),
                        speech_start_time: speech_segment.start_time,
                        speech_end_time: speech_segment.end_time,
                    };
                    let diarized = diarization_worker.process_input(input);
                    diarization_count += 1;

                    // 3. Store in transcript buffer
                    if let Ok(mut buffer) = transcript_buffer.write() {
                        buffer.push_back(diarized.segment.clone());
                        if buffer.len() > 500 {
                            buffer.pop_front();
                        }
                    }

                    // 4. Emit Tauri events
                    let _ = app_handle.emit(events::TRANSCRIPT_UPDATE, &diarized.segment);
                    let _ = app_handle.emit(events::SPEAKER_DETECTED, &diarized.speaker_info);

                    // 5. Update pipeline status counts
                    if let Ok(mut status) = pipeline_status.write() {
                        status.asr = StageStatus::Running {
                            processed_count: asr_count,
                        };
                        status.diarization = StageStatus::Running {
                            processed_count: diarization_count,
                        };
                    }

                    log::debug!(
                        "Speech processor: emitted transcript #{} speaker={:?} \"{}\"",
                        asr_count,
                        diarized.segment.speaker_label,
                        &diarized.segment.text,
                    );

                    // 6. Knowledge Graph Extraction (delegated to helper)
                    {
                        let speaker = diarized
                            .segment
                            .speaker_label
                            .as_deref()
                            .unwrap_or("Unknown");
                        process_extraction_and_emit(
                            &diarized.segment.text,
                            speaker,
                            &diarized.segment.id,
                            diarized.segment.start_time,
                            &llm_engine,
                            &sidecar_manager,
                            &graph_extractor,
                            &knowledge_graph,
                            &graph_snapshot,
                            &pipeline_status,
                            &app_handle,
                            &mut extraction_count,
                            &mut graph_update_count,
                        );
                    }
                }
            }
            Err(e) => {
                log::warn!("Speech processor: ASR failed for segment: {}", e);
            }
        }
    }

    log::info!(
        "Speech processor: exiting. ASR segments={}, diarized={}",
        asr_count,
        diarization_count,
    );
}

/// Fallback speech processor — diarization only (no ASR).
///
/// Used when the Whisper model fails to load. Generates placeholder transcript
/// segments with `[speech]` text and still performs speaker attribution.
fn run_speech_processor_diarization_only(
    speech_rx: crossbeam_channel::Receiver<crate::audio::vad::SpeechSegment>,
    transcript_buffer: std::sync::Arc<
        std::sync::RwLock<std::collections::VecDeque<TranscriptSegment>>,
    >,
    pipeline_status: std::sync::Arc<std::sync::RwLock<PipelineStatus>>,
    app_handle: tauri::AppHandle,
    knowledge_graph: std::sync::Arc<std::sync::Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: std::sync::Arc<std::sync::RwLock<GraphSnapshot>>,
    graph_extractor: std::sync::Arc<RuleBasedExtractor>,
    sidecar_manager: std::sync::Arc<std::sync::Mutex<SidecarManager>>,
    llm_engine: std::sync::Arc<std::sync::Mutex<Option<LlmEngine>>>,
) {
    let diarization_config = DiarizationConfig::default();
    let (dummy_diar_tx, _dummy_diar_rx) = crossbeam_channel::unbounded::<DiarizedTranscript>();
    let mut diarization_worker = DiarizationWorker::new(diarization_config, dummy_diar_tx);

    let mut count: u64 = 0;
    let mut extraction_count: u64 = 0;
    let mut graph_update_count: u64 = 0;

    // Mark ASR as errored since model didn't load
    if let Ok(mut status) = pipeline_status.write() {
        status.asr = StageStatus::Error {
            message: "Whisper model not loaded".to_string(),
        };
        status.entity_extraction = StageStatus::Running { processed_count: 0 };
        status.graph = StageStatus::Running { processed_count: 0 };
    }

    log::info!("Speech processor (diarization-only): entering processing loop");

    while let Ok(speech_segment) = speech_rx.recv() {
        count += 1;

        // Create a placeholder transcript segment (no ASR)
        let placeholder_transcript = TranscriptSegment {
            id: uuid::Uuid::new_v4().to_string(),
            source_id: speech_segment.source_id.clone(),
            speaker_id: None,
            speaker_label: None,
            text: "[speech]".to_string(),
            start_time: speech_segment.start_time.as_secs_f64(),
            end_time: speech_segment.end_time.as_secs_f64(),
            confidence: 0.0,
        };

        let input = DiarizationInput {
            transcript: placeholder_transcript,
            speech_audio: speech_segment.audio.clone(),
            speech_start_time: speech_segment.start_time,
            speech_end_time: speech_segment.end_time,
        };
        let diarized = diarization_worker.process_input(input);

        if let Ok(mut buffer) = transcript_buffer.write() {
            buffer.push_back(diarized.segment.clone());
            if buffer.len() > 500 {
                buffer.pop_front();
            }
        }

        let _ = app_handle.emit(events::TRANSCRIPT_UPDATE, &diarized.segment);
        let _ = app_handle.emit(events::SPEAKER_DETECTED, &diarized.speaker_info);

        if let Ok(mut status) = pipeline_status.write() {
            status.diarization = StageStatus::Running {
                processed_count: count,
            };
        }

        // Knowledge Graph Extraction (delegated to helper)
        {
            let speaker = diarized
                .segment
                .speaker_label
                .as_deref()
                .unwrap_or("Unknown");
            process_extraction_and_emit(
                &diarized.segment.text,
                speaker,
                &diarized.segment.id,
                diarized.segment.start_time,
                &llm_engine,
                &sidecar_manager,
                &graph_extractor,
                &knowledge_graph,
                &graph_snapshot,
                &pipeline_status,
                &app_handle,
                &mut extraction_count,
                &mut graph_update_count,
            );
        }
    }

    log::info!(
        "Speech processor (diarization-only): exiting. Segments processed={}",
        count,
    );
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
#[tauri::command]
pub async fn send_chat_message(
    message: String,
    state: State<'_, AppState>,
) -> Result<ChatResponse, String> {
    log::info!(
        "send_chat_message called: {}",
        &message[..message.len().min(50)]
    );

    // Build graph context string for the LLM system prompt.
    let graph_context = {
        let kg = state
            .knowledge_graph
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        let snapshot = kg.snapshot();
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

        // Add recent transcript
        let transcript = state
            .transcript_buffer
            .read()
            .map_err(|e| format!("Lock error: {}", e))?;
        let recent: Vec<_> = transcript.iter().rev().take(10).collect();
        if !recent.is_empty() {
            ctx.push_str("\nRecent Transcript:\n");
            for seg in recent.iter().rev() {
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
