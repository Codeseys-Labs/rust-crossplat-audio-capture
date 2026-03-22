//! Tauri event name constants and payload types.
//!
//! These constants define the event names emitted from the Rust backend
//! to the frontend. The frontend subscribes using `listen()` from `@tauri-apps/api`.

/// Event emitted when a new transcript segment is available.
pub const TRANSCRIPT_UPDATE: &str = "transcript-update";

/// Event emitted when the knowledge graph changes.
pub const GRAPH_UPDATE: &str = "graph-update";

/// Event emitted periodically (every ~2s) or on status change.
pub const PIPELINE_STATUS_EVENT: &str = "pipeline-status";

/// Event emitted when a new speaker is first identified.
pub const SPEAKER_DETECTED: &str = "speaker-detected";

/// Event emitted when a capture error occurs.
pub const CAPTURE_ERROR: &str = "capture-error";

/// Status of an individual pipeline stage.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum StageStatus {
    Idle,
    Running { processed_count: u64 },
    Error { message: String },
}

impl Default for StageStatus {
    fn default() -> Self {
        StageStatus::Idle
    }
}

/// Overall pipeline status, combining all stages.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct PipelineStatus {
    pub capture: StageStatus,
    pub pipeline: StageStatus,
    pub asr: StageStatus,
    pub diarization: StageStatus,
    pub entity_extraction: StageStatus,
    pub graph: StageStatus,
}

/// Payload for capture error events.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CaptureErrorPayload {
    pub source_id: String,
    pub error: String,
    pub recoverable: bool,
}
