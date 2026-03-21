//! Tauri IPC command handlers.
//!
//! Each function here is exposed to the frontend via `tauri::generate_handler![]`.
//! They access `AppState` through Tauri's managed state.

use tauri::State;

use crate::audio::pipeline::AudioPipeline;
use crate::events::{PipelineStatus, StageStatus};
use crate::graph::entities::GraphSnapshot;
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
pub async fn start_capture(source_id: String, state: State<'_, AppState>) -> Result<(), String> {
    log::info!("start_capture called for source: {}", source_id);

    let target = parse_capture_target(&source_id)?;

    // 1. Start capture via the manager.
    {
        let mut manager = state
            .capture_manager
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        manager.start_capture(&source_id, target, state.pipeline_tx.clone())?;
    }

    // 2. Start pipeline thread if not already running.
    {
        let mut pipeline_handle = state
            .pipeline_thread
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        if pipeline_handle.is_none() {
            let rx = state
                .pipeline_rx
                .lock()
                .map_err(|e| format!("Lock error: {}", e))?
                .take()
                .ok_or("Pipeline receiver already taken")?;
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

    // 3. Update state flags.
    if let Ok(mut capturing) = state.is_capturing.write() {
        *capturing = true;
    }
    if let Ok(mut status) = state.pipeline_status.write() {
        status.capture = StageStatus::Running { processed_count: 0 };
        status.pipeline = StageStatus::Running { processed_count: 0 };
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
