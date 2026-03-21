//! Application state managed by Tauri.
//!
//! `AppState` is registered with `tauri::Builder::manage()` and accessed
//! in command handlers via `State<'_, AppState>`.

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};

use crate::events::PipelineStatus;
use crate::graph::entities::GraphSnapshot;

/// Transcript segment for frontend consumption.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TranscriptSegment {
    pub id: String,
    pub source_id: String,
    pub speaker_id: Option<String>,
    pub speaker_label: Option<String>,
    pub text: String,
    pub start_time: f64,
    pub end_time: f64,
    pub confidence: f32,
}

/// Audio source information.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AudioSourceInfo {
    pub id: String,
    pub name: String,
    pub source_type: AudioSourceType,
    pub is_active: bool,
}

/// Type of audio source.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum AudioSourceType {
    SystemDefault,
    Device { device_id: String },
    Application { pid: u32, app_name: String },
}

/// Speaker information for the frontend.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SpeakerInfo {
    pub id: String,
    pub label: String,
    pub color: String,
    pub total_speaking_time: f64,
    pub segment_count: u32,
}

/// Central application state, shared across Tauri commands and worker threads.
pub struct AppState {
    /// Buffer of transcript segments (most recent last).
    pub transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,

    /// Current knowledge graph snapshot.
    pub graph_snapshot: Arc<RwLock<GraphSnapshot>>,

    /// Current pipeline status.
    pub pipeline_status: Arc<RwLock<PipelineStatus>>,

    /// Whether capture is currently active.
    pub is_capturing: Arc<RwLock<bool>>,
}

impl AppState {
    /// Create a new `AppState` with empty defaults.
    pub fn new() -> Self {
        Self {
            transcript_buffer: Arc::new(RwLock::new(VecDeque::with_capacity(500))),
            graph_snapshot: Arc::new(RwLock::new(GraphSnapshot::default())),
            pipeline_status: Arc::new(RwLock::new(PipelineStatus::default())),
            is_capturing: Arc::new(RwLock::new(false)),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
