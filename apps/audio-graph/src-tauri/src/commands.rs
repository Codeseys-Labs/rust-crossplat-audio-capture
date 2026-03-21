//! Tauri IPC command handlers.
//!
//! Each function here is exposed to the frontend via `tauri::generate_handler![]`.
//! They access `AppState` through Tauri's managed state.

use tauri::State;

use crate::events::PipelineStatus;
use crate::graph::entities::GraphSnapshot;
use crate::state::{AppState, AudioSourceInfo, TranscriptSegment};

/// List available audio sources (devices + running applications).
#[tauri::command]
pub async fn list_audio_sources(
    _state: State<'_, AppState>,
) -> Result<Vec<AudioSourceInfo>, String> {
    // TODO: Use rsac to enumerate audio devices and running applications
    log::info!("list_audio_sources called");
    Ok(vec![])
}

/// Start capturing audio from the specified source.
#[tauri::command]
pub async fn start_capture(source_id: String, _state: State<'_, AppState>) -> Result<(), String> {
    // TODO: Create AudioCaptureBuilder, configure with source_id, start capture thread
    log::info!("start_capture called for source: {}", source_id);
    Ok(())
}

/// Stop capturing audio from the specified source.
#[tauri::command]
pub async fn stop_capture(source_id: String, _state: State<'_, AppState>) -> Result<(), String> {
    // TODO: Signal the capture thread to stop, clean up resources
    log::info!("stop_capture called for source: {}", source_id);
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
