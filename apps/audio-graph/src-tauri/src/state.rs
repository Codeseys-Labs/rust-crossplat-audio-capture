//! Application state managed by Tauri.
//!
//! `AppState` is registered with `tauri::Builder::manage()` and accessed
//! in command handlers via `State<'_, AppState>`.
//!
//! TODO(I6): Load configuration from `config/default.toml` at startup.
//! Currently all config values (channel capacities, VAD thresholds, model
//! paths, etc.) are hardcoded as defaults.  A future PR should parse the
//! TOML file via `toml` + resolve paths via `dirs` and populate `AppState`
//! fields accordingly.  The `toml` and `dirs` crate dependencies have been
//! removed until that work is done to avoid carrying unused dependencies.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, RwLock};

use crate::audio::pipeline::ProcessedAudioChunk;
use crate::audio::vad::SpeechSegment;
use crate::audio::{AudioCaptureManager, AudioChunk};
use crate::events::PipelineStatus;
use crate::graph::entities::GraphSnapshot;
use crate::graph::extraction::RuleBasedExtractor;
use crate::graph::temporal::TemporalKnowledgeGraph;
use crate::llm::engine::ChatMessage;
use crate::llm::{ApiClient, LlmEngine};

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

    // ── Knowledge graph infrastructure ──────────────────────────────────
    /// The temporal knowledge graph engine.
    pub knowledge_graph: Arc<Mutex<TemporalKnowledgeGraph>>,

    /// Rule-based entity extractor (fallback when no LLM available).
    pub graph_extractor: Arc<RuleBasedExtractor>,

    /// Native LLM engine for entity extraction + chat.
    pub llm_engine: Arc<Mutex<Option<LlmEngine>>>,

    /// OpenAI-compatible API client (alternative to native LLM).
    pub api_client: Arc<Mutex<Option<ApiClient>>>,

    /// Chat message history for the sidebar.
    pub chat_history: Arc<RwLock<Vec<ChatMessage>>>,

    // ── Audio capture infrastructure ────────────────────────────────────
    /// The capture manager (behind Mutex because AudioCaptureManager has &mut self methods).
    pub capture_manager: Arc<Mutex<AudioCaptureManager>>,

    /// Sender side of the raw audio channel (capture → pipeline).
    pub pipeline_tx: crossbeam_channel::Sender<AudioChunk>,

    /// Receiver side — cloneable, workers call `.clone()` to get their own handle.
    pub pipeline_rx: crossbeam_channel::Receiver<AudioChunk>,

    /// Sender for processed audio (pipeline → downstream ASR/VAD).
    pub processed_tx: crossbeam_channel::Sender<ProcessedAudioChunk>,

    /// Receiver for processed audio — cloneable for worker threads.
    pub processed_rx: crossbeam_channel::Receiver<ProcessedAudioChunk>,

    /// Handle to the pipeline worker thread.
    pub pipeline_thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,

    // ── Speech processing pipeline ─────────────────────────────────────
    /// Sender for speech segments (VAD → speech processor).
    pub speech_tx: crossbeam_channel::Sender<SpeechSegment>,

    /// Receiver for speech segments — cloneable for worker threads.
    pub speech_rx: crossbeam_channel::Receiver<SpeechSegment>,

    /// Handle to the VAD worker thread.
    pub vad_thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,

    /// Handle to the speech processor (ASR + diarization) orchestrator thread.
    pub speech_processor_thread: Arc<Mutex<Option<std::thread::JoinHandle<()>>>>,

    // ── Settings ─────────────────────────────────────────────────────────
    /// Persisted application settings (ASR provider, LLM config, audio params).
    pub app_settings: Arc<RwLock<crate::settings::AppSettings>>,
}

impl AppState {
    /// Create a new `AppState` with empty defaults.
    pub fn new() -> Self {
        // Bounded channels prevent OOM if downstream consumers stall.
        // Capacities chosen per architecture spec:
        //   pipeline: 64 chunks (~2s of audio at 32ms/chunk)
        //   processed: 16 chunks (VAD processes quickly)
        //   speech: 32 segments (speech segments are larger but less frequent)
        let (pipeline_tx, pipeline_rx) = crossbeam_channel::bounded::<AudioChunk>(64);
        let (processed_tx, processed_rx) = crossbeam_channel::bounded::<ProcessedAudioChunk>(16);
        let (speech_tx, speech_rx) = crossbeam_channel::bounded::<SpeechSegment>(32);

        Self {
            transcript_buffer: Arc::new(RwLock::new(VecDeque::with_capacity(500))),
            graph_snapshot: Arc::new(RwLock::new(GraphSnapshot::default())),
            pipeline_status: Arc::new(RwLock::new(PipelineStatus::default())),
            is_capturing: Arc::new(RwLock::new(false)),
            knowledge_graph: Arc::new(Mutex::new(TemporalKnowledgeGraph::new())),
            graph_extractor: Arc::new(RuleBasedExtractor::new()),
            llm_engine: Arc::new(Mutex::new(None)),
            api_client: Arc::new(Mutex::new(None)),
            chat_history: Arc::new(RwLock::new(Vec::new())),
            capture_manager: Arc::new(Mutex::new(AudioCaptureManager::new())),
            pipeline_tx,
            pipeline_rx,
            processed_tx,
            processed_rx,
            pipeline_thread: Arc::new(Mutex::new(None)),
            speech_tx,
            speech_rx,
            vad_thread: Arc::new(Mutex::new(None)),
            speech_processor_thread: Arc::new(Mutex::new(None)),
            app_settings: Arc::new(RwLock::new(crate::settings::AppSettings::default())),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
