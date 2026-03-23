//! Speech processing orchestrator.
//!
//! Contains the speech processor logic (ASR + diarization + entity extraction)
//! extracted from `commands.rs` to keep command handlers thin.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use crossbeam_channel::Receiver;
use tauri::{AppHandle, Emitter};

use crate::asr::{AsrConfig, AsrWorker};
use crate::diarization::{
    DiarizationConfig, DiarizationInput, DiarizationWorker, DiarizedTranscript,
};
use crate::events::{self, PipelineStatus, StageStatus};
use crate::graph::entities::GraphSnapshot;
use crate::graph::extraction::RuleBasedExtractor;
use crate::graph::temporal::TemporalKnowledgeGraph;
use crate::llm::{ApiClient, LlmEngine};
use crate::state::TranscriptSegment;

// ---------------------------------------------------------------------------
// Helper: extraction + graph update + event emission (I1: deduplicated)
// ---------------------------------------------------------------------------

/// Perform entity extraction, update the knowledge graph, and emit events.
///
/// Shared by both the full (ASR + diarization) and diarization-only speech
/// processor loops. Extraction chain: native LLM → API client → rule-based.
#[allow(clippy::too_many_arguments)]
pub(crate) fn process_extraction_and_emit(
    text: &str,
    speaker: &str,
    segment_id: &str,
    timestamp: f64,
    llm_engine: &Arc<Mutex<Option<LlmEngine>>>,
    api_client: &Arc<Mutex<Option<ApiClient>>>,
    graph_extractor: &Arc<RuleBasedExtractor>,
    knowledge_graph: &Arc<Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: &Arc<RwLock<GraphSnapshot>>,
    pipeline_status: &Arc<RwLock<PipelineStatus>>,
    app_handle: &AppHandle,
    extraction_count: &mut u64,
    graph_update_count: &mut u64,
) {
    // 1. Try native LLM engine first
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

    // 2. If native failed, try API client
    let api_result = if llm_result.is_none() {
        let api_guard = api_client.lock().unwrap_or_else(|e| {
            log::warn!("API client mutex poisoned, recovering: {}", e);
            e.into_inner()
        });
        if let Some(ref client) = *api_guard {
            match client.extract_entities(text, speaker) {
                Ok(result) => Some(result),
                Err(e) => {
                    log::warn!("API extraction failed: {}", e);
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    // 3. Pick the best result or fall back to rule-based
    let extraction_result = if let Some(result) = llm_result {
        log::debug!(
            "Native LLM extraction: {} entities, {} relations",
            result.entities.len(),
            result.relations.len()
        );
        result
    } else if let Some(result) = api_result {
        log::debug!(
            "API extraction: {} entities, {} relations",
            result.entities.len(),
            result.relations.len()
        );
        result
    } else {
        // Fallback to rule-based extraction
        graph_extractor.extract(speaker, text)
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
pub(crate) fn run_speech_processor(
    speech_rx: Receiver<crate::audio::vad::SpeechSegment>,
    transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,
    pipeline_status: Arc<RwLock<PipelineStatus>>,
    app_handle: AppHandle,
    knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: Arc<RwLock<GraphSnapshot>>,
    graph_extractor: Arc<RuleBasedExtractor>,
    llm_engine: Arc<Mutex<Option<LlmEngine>>>,
    api_client: Arc<Mutex<Option<ApiClient>>>,
    models_dir: PathBuf,
) {
    use whisper_rs::{WhisperContext, WhisperContextParameters};

    log::info!("Speech processor: loading Whisper model...");

    let asr_config = AsrConfig::with_models_dir(&models_dir);
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
                    llm_engine,
                    api_client,
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
                llm_engine,
                api_client,
            );
            return;
        }
    };

    // Create ASR worker with a dummy output channel — we call
    // `transcribe_segment()` directly rather than using the worker's
    // internal run loop, so the channel is never consumed.  This is a
    // stop-gap until `AsrWorker` gains a `new_standalone()` constructor
    // that doesn't require a channel.  (M2)
    let (dummy_asr_tx, _dummy_asr_rx) = crossbeam_channel::unbounded::<TranscriptSegment>();
    let mut asr_worker = AsrWorker::new(asr_config, dummy_asr_tx);

    // Same pattern for DiarizationWorker — `process_input()` is called
    // directly; the channel output is unused.
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
                            &api_client,
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
pub(crate) fn run_speech_processor_diarization_only(
    speech_rx: Receiver<crate::audio::vad::SpeechSegment>,
    transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,
    pipeline_status: Arc<RwLock<PipelineStatus>>,
    app_handle: AppHandle,
    knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,
    graph_snapshot: Arc<RwLock<GraphSnapshot>>,
    graph_extractor: Arc<RuleBasedExtractor>,
    llm_engine: Arc<Mutex<Option<LlmEngine>>>,
    api_client: Arc<Mutex<Option<ApiClient>>>,
) {
    let diarization_config = DiarizationConfig::default();
    // Same dummy-channel pattern as in `run_speech_processor` — see M2
    // comment there for rationale.
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
                &api_client,
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
