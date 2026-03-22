# AudioGraph — Architecture Document

> **Source of truth** for the AudioGraph Tauri desktop application.
> All implementation subtasks derive from this document.

---

## Table of Contents

1. [Application Overview](#1-application-overview)
2. [System Architecture Diagram](#2-system-architecture-diagram)
3. [Rust Backend Architecture](#3-rust-backend-architecture)
4. [Frontend Architecture](#4-frontend-architecture)
5. [Data Models](#5-data-models)
6. [Threading Model](#6-threading-model)
7. [IPC Protocol](#7-ipc-protocol)
8. [Configuration](#8-configuration)
9. [Project Structure](#9-project-structure)
10. [Dependencies List](#10-dependencies-list)
11. [Build and Run Instructions](#11-build-and-run-instructions)
12. [Latency Budget](#12-latency-budget)

---

## 1. Application Overview

### Purpose

AudioGraph is a Tauri v2 desktop application that captures live audio from applications and devices, processes it through a speech intelligence pipeline (VAD, ASR, diarization, entity extraction), and builds a live **temporal knowledge graph** visualized in real time.

### Core Capabilities

| Capability | Description |
|---|---|
| **Multi-source audio capture** | Capture system audio, per-application audio, or process-tree audio via rsac |
| **Voice Activity Detection** | Silero VAD filters silence, gating audio chunks to ASR |
| **Speech-to-Text** | whisper.cpp via whisper-rs — batch transcription of VAD-segmented utterances |
| **Speaker Diarization** | pyannote-rs ONNX models — speaker identification and tracking |
| **Entity Extraction** | LLM sidecar (llama-server with LFM2-350M-Extract GGUF) — entities and relations from transcript segments |
| **Temporal Knowledge Graph** | petgraph-based in-memory graph with temporal edges, entity resolution, and real-time mutation |
| **Live Visualization** | react-force-graph-2d/3d rendering with live streaming updates via Tauri events |

### Cross-Platform Support

| Platform | Audio Backend | Status |
|---|---|---|
| **Linux** | PipeWire via rsac | Primary development target |
| **macOS** | CoreAudio Process Tap via rsac | Supported (macOS 14.4+) |
| **Windows** | WASAPI Process Loopback via rsac | Supported |

### Monorepo Placement

AudioGraph lives inside the rsac workspace as a separate Tauri application:

```
rust-crossplat-audio-capture/    # Workspace root
├── Cargo.toml                   # Workspace-level Cargo.toml (members includes apps/audio-graph/src-tauri)
├── src/                         # rsac library crate
└── apps/
    └── audio-graph/             # AudioGraph Tauri application
        ├── src-tauri/           # Rust backend (workspace member)
        ├── src/                 # React frontend
        └── docs/                # This architecture document
```

The `apps/audio-graph/src-tauri/Cargo.toml` depends on `rsac` as a path dependency:
```toml
rsac = { path = "../../../", features = ["feat_linux", "feat_macos", "feat_windows"] }
```

---

## 2. System Architecture Diagram

```
                         ┌──────────────────────────────────────────────────┐
                         │                 FRONTEND (React)                 │
                         │                                                  │
                         │  ┌──────────────┐ ┌─────────────┐ ┌───────────┐ │
                         │  │ AudioSource  │ │   Live      │ │ Knowledge │ │
                         │  │ Selector     │ │ Transcript  │ │ Graph     │ │
                         │  └──────┬───────┘ └──────▲──────┘ │ Viewer    │ │
                         │         │                │        └─────▲─────┘ │
                         │   Tauri │          Tauri │        Tauri │       │
                         │   Cmd   │          Event │        Event │       │
                         └─────────┼────────────────┼──────────────┼───────┘
                    ════════════════╪════════════════╪══════════════╪═══════════
                         IPC       │                │              │
                    ════════════════╪════════════════╪══════════════╪═══════════
                         ┌─────────▼────────────────┴──────────────┴───────┐
                         │              TAURI MAIN THREAD                   │
                         │  ┌─────────────────────────────────────────────┐ │
                         │  │  Commands: start_capture, stop_capture,    │ │
                         │  │  list_audio_sources, get_graph_state,      │ │
                         │  │  get_transcript                            │ │
                         │  └─────────────────────────────────────────────┘ │
                         │  ┌─────────────────────────────────────────────┐ │
                         │  │  Event Emitter:                            │ │
                         │  │  transcript-update, graph-update,          │ │
                         │  │  pipeline-status, speaker-detected         │ │
                         │  └───────────────────────▲─────────────────────┘ │
                         └──────────────────────────┼──────────────────────┘
                                                    │ crossbeam channel
                   ┌────────────────────────────────┼────────────────────────────┐
                   │                                │                            │
    ┌──────────────▼────────────┐  ┌────────────────▼──────────┐  ┌──────────────▼──────────┐
    │    AUDIO CAPTURE THREADS  │  │   ASR WORKER THREAD       │  │  ENTITY EXTRACTION      │
    │    (one per source)       │  │                            │  │  THREAD                 │
    │                           │  │  whisper-rs model          │  │                         │
    │  rsac::AudioCapture       │  │  receives VAD-gated        │  │  HTTP POST to           │
    │  .subscribe() ->          │  │  chunks from pipeline      │  │  llama-server sidecar   │
    │  mpsc::Receiver           │  │  thread                    │  │                         │
    │                           │  │  -> TranscriptSegment      │  │  receives transcript    │
    │  AudioBuffer (f32 stereo  │  │     via crossbeam          │  │  segments, extracts     │
    │  48kHz) pushed to         │  │                            │  │  entities/relations     │
    │  pipeline thread          │  └────────────┬───────────────┘  │                         │
    └───────────┬───────────────┘               │                  │  -> GraphUpdate via     │
                │ crossbeam                     │ crossbeam        │     crossbeam           │
                │ channel                       │ channel          └──────────┬──────────────┘
    ┌───────────▼───────────────┐  ┌────────────▼───────────────┐            │ crossbeam
    │   AUDIO PIPELINE THREAD   │  │  DIARIZATION WORKER THREAD │            │ channel
    │                           │  │                            │  ┌─────────▼──────────────┐
    │  1. Resample              │  │  pyannote-rs ONNX models   │  │  GRAPH MANAGER THREAD  │
    │     48kHz stereo f32      │  │  receives raw 16kHz mono   │  │                         │
    │     -> 16kHz mono f32     │  │  audio segments from       │  │  petgraph::StableGraph  │
    │     (rubato)              │  │  pipeline thread           │  │  entity resolution      │
    │                           │  │                            │  │  temporal edge mgmt     │
    │  2. VAD                   │  │  -> SpeakerLabel aligned   │  │  snapshot generation    │
    │     Silero ONNX model     │  │     to timestamps          │  │                         │
    │     30ms chunks           │  │     via crossbeam          │  │  -> graph-update event  │
    │     speech/silence gate   │  │                            │  │     via crossbeam       │
    │                           │  └────────────────────────────┘  └──────────────────────────┘
    │  3. Buffer speech frames  │
    │     until silence          │           ┌──────────────────────────────┐
    │     (2-30s utterances)    │           │  SIDECAR: llama-server       │
    │                           │           │  (Tauri sidecar process)     │
    │  -> VAD-gated chunk to    │           │                              │
    │     ASR thread             │           │  LFM2-350M-Extract GGUF     │
    │  -> raw audio segment to  │           │  HTTP :8081 /completion      │
    │     diarization thread    │           │  JSON schema constraint      │
    └───────────────────────────┘           └──────────────────────────────┘
```

### Data Flow Summary

```
Audio Source(s)
  │
  ▼
AudioCapture.subscribe() ──► mpsc::Receiver<AudioBuffer>  [f32, stereo, 48kHz]
  │
  ▼
Pipeline Thread: resample (rubato) ──► [f32, mono, 16kHz]
  │
  ├──► VAD (Silero) ──► buffer speech ──► ASR Thread (whisper-rs)
  │                                           │
  │                                           ▼
  │                                    TranscriptSegment {speaker, text, start, end}
  │                                           │
  └──► Diarization Thread (pyannote-rs) ──────┤ (speaker labels merged)
                                              │
                                              ▼
                                    Entity Extraction Thread ──► HTTP ──► llama-server
                                              │
                                              ▼
                                    Graph Manager Thread ──► petgraph mutations
                                              │
                                              ▼
                                    Tauri event ──► Frontend react-force-graph
```

---

## 3. Rust Backend Architecture

The Tauri backend is organized into focused modules under `src-tauri/src/`.

### 3.1 Audio Manager Module (`audio/manager.rs`)

Manages multiple `AudioCapture` instances, each running on a dedicated thread.

**Key design constraints from rsac:**

- `AudioCapture` is **not `Sync`** — it must be owned by exactly one thread.
- `subscribe()` returns `mpsc::Receiver<AudioBuffer>` — a **single-consumer** channel. Multiple subscribers compete for buffers; they do NOT receive copies.
- Each source needs its own dedicated capture thread.

**Architecture:**

```rust
pub struct AudioManager {
    /// Active capture sessions, keyed by a unique source ID.
    sources: HashMap<SourceId, CaptureHandle>,
    /// Channel to send audio buffers to the pipeline thread.
    pipeline_tx: crossbeam_channel::Sender<TaggedAudioBuffer>,
}

struct CaptureHandle {
    /// Join handle for the capture thread.
    thread: Option<JoinHandle<()>>,
    /// Signal to stop the capture thread.
    stop_signal: Arc<AtomicBool>,
    /// Metadata about the source.
    source_info: AudioSourceInfo,
}
```

**Capture thread lifecycle:**

1. Thread spawns, creates `AudioCaptureBuilder::new().with_target(target).sample_rate(48000).channels(2).build()`.
2. Calls `capture.start()` and then `capture.subscribe()` to get `mpsc::Receiver<AudioBuffer>`.
3. Reads from the receiver in a loop, wrapping each buffer in a `TaggedAudioBuffer` (adding `source_id` and wall-clock timestamp).
4. Sends tagged buffers to the pipeline thread via `crossbeam_channel::Sender`.
5. On stop signal, calls `capture.stop()` and exits.

**Fan-out strategy:** Since `subscribe()` does NOT fan-out, and the pipeline, diarization, and ASR all need audio, we implement fan-out at the pipeline thread level. The capture thread sends a single copy to the pipeline, and the pipeline thread is responsible for cloning and distributing to downstream consumers.

### 3.2 Audio Pipeline Module (`audio/pipeline.rs`)

Runs on a dedicated thread. Receives raw audio from all capture sources and applies preprocessing.

**Responsibilities:**

1. **Resampling** — 48kHz stereo f32 to 16kHz mono f32 using `rubato::SincFixedIn`.
2. **VAD** — Feed 16kHz mono chunks to Silero VAD (30ms = 480 samples per chunk). Track speech/silence transitions.
3. **Speech buffering** — Accumulate speech frames until a silence gap is detected (min utterance: 0.5s, max: 30s).
4. **Distribution** — Send VAD-gated speech chunks to the ASR thread. Send raw resampled segments to the diarization thread.

```rust
pub struct AudioPipeline {
    /// Receives tagged audio from AudioManager.
    audio_rx: crossbeam_channel::Receiver<TaggedAudioBuffer>,
    /// Sends VAD-gated utterances to ASR worker.
    asr_tx: crossbeam_channel::Sender<SpeechUtterance>,
    /// Sends audio segments to diarization worker.
    diarization_tx: crossbeam_channel::Sender<AudioSegment>,
    /// Rubato resampler instance.
    resampler: SincFixedIn<f32>,
    /// Silero VAD instance.
    vad: VoiceActivityDetector,
    /// Buffered speech frames per source.
    speech_buffers: HashMap<SourceId, SpeechBuffer>,
}
```

**Resampling details:**

- Input: interleaved stereo f32 at 48kHz (from `AudioBuffer::data()`)
- Downmix to mono: average L+R channels via `AudioBuffer::channel_data(0)` and `channel_data(1)`
- Resample 48kHz mono to 16kHz mono using rubato `SincFixedIn` with quality `SincInterpolationParameters { sinc_len: 256, f_cutoff: 0.95, oversampling_factor: 256 }`
- Output: mono f32 at 16kHz

### 3.3 ASR Module (`asr/mod.rs`)

Wraps `whisper-rs` for speech-to-text transcription.

**Design:**

```rust
pub struct AsrWorker {
    /// Receives VAD-gated speech utterances.
    utterance_rx: crossbeam_channel::Receiver<SpeechUtterance>,
    /// Sends completed transcript segments downstream.
    transcript_tx: crossbeam_channel::Sender<TranscriptSegment>,
    /// Whisper context (model loaded once at startup).
    ctx: WhisperContext,
}
```

**Model loading:**
- Model: `ggml-small.en.bin` (~466MB) loaded at startup via `WhisperContext::new_with_params`.
- Single instance — not thread-safe, must live on dedicated ASR thread.

**Transcription flow:**
1. Receive `SpeechUtterance` (16kHz mono f32 samples + source_id + start/end timestamps).
2. Create `WhisperParams` with language "en", no translation, single segment output.
3. Call `ctx.full(params, &samples)` — blocking, 300-800ms depending on utterance length.
4. Extract text segments with timestamps.
5. Emit `TranscriptSegment` with text, timestamps, and source_id to downstream.

### 3.4 Diarization Module (`diarization/mod.rs`)

Handles speaker identification using pyannote-rs ONNX models.

**Design:**

```rust
pub struct DiarizationWorker {
    /// Receives audio segments for diarization.
    segment_rx: crossbeam_channel::Receiver<AudioSegment>,
    /// Sends speaker labels to be merged with transcript segments.
    speaker_tx: crossbeam_channel::Sender<SpeakerAssignment>,
    /// Segmentation ONNX model.
    segmentation_model: ort::Session,
    /// Speaker embedding ONNX model.
    embedding_model: ort::Session,
    /// Known speaker embeddings for tracking.
    speaker_registry: SpeakerRegistry,
}
```

**Diarization flow:**
1. Receive 10-second audio segments (16kHz mono f32) from the pipeline thread.
2. Run segmentation model to detect speaker change points.
3. Extract speaker embeddings for each segment.
4. Compare against `SpeakerRegistry` (cosine similarity, threshold 0.7).
5. Assign speaker labels (e.g., "Speaker_1", "Speaker_2") — or match to known speakers.
6. Emit `SpeakerAssignment` with timestamp ranges and speaker IDs.

**Speaker tracking:**

```rust
struct SpeakerRegistry {
    /// Known speaker embeddings mapped to speaker IDs.
    speakers: Vec<(SpeakerId, Vec<f32>)>,
    /// Similarity threshold for matching.
    threshold: f32,
    /// Next speaker ID counter.
    next_id: u32,
}
```

### 3.5 Knowledge Graph Module (`graph/mod.rs`)

Manages the in-memory temporal knowledge graph.

**Graph structure:**

```rust
pub struct KnowledgeGraph {
    /// The underlying graph data structure.
    graph: StableGraph<EntityNode, TemporalEdge>,
    /// Index for fast entity lookup by normalized name.
    name_index: HashMap<String, NodeIndex>,
    /// Entity embedding cache for resolution.
    embeddings: HashMap<NodeIndex, Vec<f32>>,
    /// Monotonic event counter for ordering.
    event_counter: u64,
}
```

**Temporal edges:**

```rust
pub struct TemporalEdge {
    pub relation_type: String,
    pub valid_from: f64,      // seconds since capture start
    pub valid_until: Option<f64>, // None = still valid
    pub confidence: f32,
    pub source_segment_id: SegmentId,
}
```

**Entity resolution strategy:**

1. Normalize entity name (lowercase, trim, remove articles).
2. Exact match against `name_index`.
3. If no exact match, compute string similarity (Levenshtein ratio > 0.85).
4. If embedding available, cosine similarity > 0.80.
5. If no match found, create new node.

**Graph Manager Thread:**

The graph lives on its own thread to avoid lock contention. It receives `GraphUpdate` messages via a crossbeam channel:

```rust
pub enum GraphUpdate {
    AddEntities { entities: Vec<ExtractedEntity>, segment_id: SegmentId, timestamp: f64 },
    AddRelations { relations: Vec<ExtractedRelation>, segment_id: SegmentId, timestamp: f64 },
    InvalidateEdge { edge_id: EdgeIndex, timestamp: f64 },
    RequestSnapshot,
}
```

When the graph changes, it generates a `GraphSnapshot` and sends it to the Tauri main thread for event emission.

### 3.6 Sidecar Management (`sidecar/mod.rs`)

Manages the `llama-server` process lifecycle as a Tauri sidecar.

**Design:**

```rust
pub struct SidecarManager {
    /// Handle to the running llama-server process.
    process: Option<CommandChild>,
    /// HTTP endpoint for the sidecar.
    endpoint: String,
    /// Model path.
    model_path: PathBuf,
    /// Health check interval.
    health_interval: Duration,
}
```

**Lifecycle:**

1. **Start:** On app launch or first capture start, spawn `llama-server` sidecar with args:
   ```
   llama-server --model <model_path> --port 8081 --ctx-size 2048 --n-predict 512
   ```
2. **Health check:** Periodic GET to `http://127.0.0.1:8081/health`. If unhealthy, attempt restart (max 3 retries).
3. **Entity extraction:** POST to `http://127.0.0.1:8081/completion` with JSON body:
   ```json
   {
     "prompt": "<transcript segment>",
     "json_schema": { ... entity/relation schema ... },
     "temperature": 0.1,
     "n_predict": 512
   }
   ```
4. **Stop:** On app close, send SIGTERM/kill to sidecar process.

**Entity extraction thread:**

```rust
pub struct EntityExtractor {
    /// Receives transcript segments for entity extraction.
    segment_rx: crossbeam_channel::Receiver<TranscriptSegment>,
    /// Sends graph updates to the graph manager.
    graph_tx: crossbeam_channel::Sender<GraphUpdate>,
    /// HTTP client for llama-server.
    client: reqwest::blocking::Client,
    /// Sidecar endpoint.
    endpoint: String,
}
```

### 3.7 Tauri Commands and Events (`commands.rs`, `events.rs`)

**Commands (frontend → backend):**

```rust
#[tauri::command]
async fn list_audio_sources(state: State<'_, AppState>) -> Result<Vec<AudioSourceInfo>, String>;

#[tauri::command]
async fn start_capture(source_id: String, state: State<'_, AppState>) -> Result<(), String>;

#[tauri::command]
async fn stop_capture(source_id: String, state: State<'_, AppState>) -> Result<(), String>;

#[tauri::command]
async fn get_graph_state(state: State<'_, AppState>) -> Result<GraphSnapshot, String>;

#[tauri::command]
async fn get_transcript(
    source_id: Option<String>,
    since: Option<f64>,
    state: State<'_, AppState>,
) -> Result<Vec<TranscriptSegment>, String>;

#[tauri::command]
async fn get_pipeline_status(state: State<'_, AppState>) -> Result<PipelineStatus, String>;
```

**Events (backend → frontend):**

Defined as string constants in `events.rs`:

```rust
pub const TRANSCRIPT_UPDATE: &str = "transcript-update";
pub const GRAPH_UPDATE: &str = "graph-update";
pub const PIPELINE_STATUS: &str = "pipeline-status";
pub const SPEAKER_DETECTED: &str = "speaker-detected";
pub const CAPTURE_ERROR: &str = "capture-error";
```

Events are emitted from a dedicated **event relay thread** that receives messages from worker threads via crossbeam channels and calls `app_handle.emit(event_name, payload)`.

### 3.8 State Management (`state.rs`)

```rust
pub struct AppState {
    /// Audio source manager (owned by audio manager thread, accessed via channel).
    audio_cmd_tx: crossbeam_channel::Sender<AudioCommand>,
    /// Recent transcript segments (ring buffer for UI).
    transcript_buffer: Arc<RwLock<VecDeque<TranscriptSegment>>>,
    /// Latest graph snapshot (updated by graph manager thread).
    graph_snapshot: Arc<RwLock<GraphSnapshot>>,
    /// Pipeline health status.
    pipeline_status: Arc<RwLock<PipelineStatus>>,
    /// App configuration.
    config: Arc<RwLock<AppConfig>>,
}
```

The `AppState` is registered with Tauri via `app.manage(app_state)` and accessed in commands via `State<'_, AppState>`.

**Design principle:** The `AppState` does NOT hold `AudioCapture` instances directly (since they are not `Sync`). Instead, it holds channel senders to communicate with the audio manager thread. The audio manager owns all `AudioCapture` instances.

---

## 4. Frontend Architecture

### 4.1 Technology Stack

| Technology | Purpose |
|---|---|
| React 18+ | UI framework |
| TypeScript | Type safety |
| Vite | Build tooling (Tauri v2 default) |
| zustand | Lightweight state management |
| react-force-graph-2d | Knowledge graph visualization |
| @tauri-apps/api | IPC bridge to Rust backend |
| TailwindCSS | Styling |

### 4.2 Component Hierarchy

```
App
├── ControlBar
│   ├── CaptureButton (start/stop)
│   ├── SourceDropdown
│   └── PipelineIndicators
├── MainLayout
│   ├── LeftPanel
│   │   ├── AudioSourceSelector
│   │   │   ├── SourceList
│   │   │   └── SourceItem
│   │   └── SpeakerPanel
│   │       ├── SpeakerList
│   │       └── SpeakerBadge
│   ├── CenterPanel
│   │   └── KnowledgeGraphViewer
│   │       ├── ForceGraph (react-force-graph-2d)
│   │       ├── GraphControls (zoom, filter, layout)
│   │       └── NodeTooltip
│   └── RightPanel
│       └── LiveTranscript
│           ├── TranscriptEntry
│           └── TranscriptTimestamp
└── StatusBar
    ├── LatencyDisplay
    ├── OverrunCounter
    └── SidecarStatus
```

### 4.3 Zustand Store

```typescript
interface AudioGraphStore {
  // Audio sources
  sources: AudioSource[];
  activeSourceIds: Set<string>;
  fetchSources: () => Promise<void>;
  startCapture: (sourceId: string) => Promise<void>;
  stopCapture: (sourceId: string) => Promise<void>;

  // Transcript
  segments: TranscriptSegment[];
  addSegment: (segment: TranscriptSegment) => void;

  // Knowledge graph
  graphData: GraphSnapshot;
  updateGraph: (snapshot: GraphSnapshot) => void;

  // Speakers
  speakers: Map<string, SpeakerInfo>;
  addSpeaker: (speaker: SpeakerInfo) => void;

  // Pipeline status
  pipelineStatus: PipelineStatus;
  updatePipelineStatus: (status: PipelineStatus) => void;
}
```

### 4.4 Tauri Event Listeners

Set up in the root `App.tsx` via `useEffect`:

```typescript
import { listen } from '@tauri-apps/api/event';

useEffect(() => {
  const unlisten = Promise.all([
    listen<TranscriptSegment>('transcript-update', (event) => {
      store.addSegment(event.payload);
    }),
    listen<GraphSnapshot>('graph-update', (event) => {
      store.updateGraph(event.payload);
    }),
    listen<PipelineStatus>('pipeline-status', (event) => {
      store.updatePipelineStatus(event.payload);
    }),
    listen<SpeakerInfo>('speaker-detected', (event) => {
      store.addSpeaker(event.payload);
    }),
  ]);

  return () => { unlisten.then(fns => fns.forEach(fn => fn())); };
}, []);
```

### 4.5 react-force-graph Integration

The `KnowledgeGraphViewer` component maps `GraphSnapshot` to the format expected by `react-force-graph-2d`:

```typescript
const graphData = useMemo(() => ({
  nodes: snapshot.entities.map(e => ({
    id: e.id,
    name: e.name,
    type: e.entity_type,
    val: e.mention_count, // node size proportional to mentions
    color: entityTypeColor(e.entity_type),
  })),
  links: snapshot.relations.map(r => ({
    source: r.source_id,
    target: r.target_id,
    label: r.relation_type,
    opacity: r.valid_until ? 0.3 : 1.0, // faded if invalidated
  })),
}), [snapshot]);
```

**Real-time update strategy:** When a `graph-update` event arrives, the graph data is replaced atomically. react-force-graph handles animated transitions between states via its built-in force simulation. The `cooldownTicks` prop is set to allow the graph to stabilize after updates.

---

## 5. Data Models

### 5.1 Rust Structs (`src-tauri/src/models.rs`)

```rust
use serde::{Serialize, Deserialize};

// ── Audio ──────────────────────────────────────────────────────

/// Unique identifier for an audio capture source.
pub type SourceId = String;

/// Information about an available audio source (for UI display).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioSourceInfo {
    pub id: SourceId,
    pub name: String,
    pub source_type: AudioSourceType,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AudioSourceType {
    SystemDefault,
    Device { device_id: String },
    Application { pid: u32, app_name: String },
}

/// An audio buffer tagged with source metadata.
pub struct TaggedAudioBuffer {
    pub source_id: SourceId,
    pub buffer: rsac::AudioBuffer,
    pub wall_clock: std::time::Instant,
}

/// A VAD-gated speech utterance ready for ASR.
pub struct SpeechUtterance {
    pub source_id: SourceId,
    pub samples: Vec<f32>,        // 16kHz mono f32
    pub start_time: f64,          // seconds since capture start
    pub end_time: f64,
}

/// A raw audio segment for diarization.
pub struct AudioSegment {
    pub source_id: SourceId,
    pub samples: Vec<f32>,        // 16kHz mono f32
    pub start_time: f64,
    pub end_time: f64,
}

// ── Transcript ─────────────────────────────────────────────────

/// A completed transcript segment with speaker and timestamps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub id: String,               // UUID
    pub source_id: SourceId,
    pub speaker_id: Option<String>,
    pub speaker_label: Option<String>,
    pub text: String,
    pub start_time: f64,          // seconds since capture start
    pub end_time: f64,
    pub confidence: f32,
}

/// Speaker label assignment from diarization.
pub struct SpeakerAssignment {
    pub source_id: SourceId,
    pub speaker_id: String,
    pub start_time: f64,
    pub end_time: f64,
    pub embedding: Vec<f32>,
}

// ── Knowledge Graph ────────────────────────────────────────────

/// A node in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEntity {
    pub id: String,               // Stable node ID
    pub name: String,
    pub entity_type: String,      // PERSON, ORG, LOCATION, EVENT, CONCEPT, etc.
    pub mention_count: u32,
    pub first_seen: f64,
    pub last_seen: f64,
    pub aliases: Vec<String>,
}

/// An edge in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphRelation {
    pub id: String,               // Stable edge ID
    pub source_id: String,        // Source entity ID
    pub target_id: String,        // Target entity ID
    pub relation_type: String,    // WORKS_AT, LOCATED_IN, KNOWS, etc.
    pub valid_from: f64,
    pub valid_until: Option<f64>,
    pub confidence: f32,
    pub source_segment_id: String,
}

/// A serializable snapshot of the entire graph for the frontend.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphSnapshot {
    pub entities: Vec<GraphEntity>,
    pub relations: Vec<GraphRelation>,
    pub last_updated: f64,
    pub node_count: usize,
    pub edge_count: usize,
}

/// Entities and relations extracted from a transcript segment by the LLM.
#[derive(Debug, Clone, Deserialize)]
pub struct ExtractionResult {
    pub entities: Vec<ExtractedEntity>,
    pub relations: Vec<ExtractedRelation>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedEntity {
    pub name: String,
    pub entity_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExtractedRelation {
    pub source: String,
    pub target: String,
    pub relation_type: String,
}

// ── Pipeline Status ────────────────────────────────────────────

/// Health and status of each pipeline stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStatus {
    pub capture: StageStatus,
    pub pipeline: StageStatus,
    pub asr: StageStatus,
    pub diarization: StageStatus,
    pub entity_extraction: StageStatus,
    pub graph: StageStatus,
    pub sidecar: SidecarStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StageStatus {
    Idle,
    Running { processed_count: u64 },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SidecarStatus {
    NotStarted,
    Starting,
    Healthy,
    Unhealthy { reason: String },
    Stopped,
}

// ── Speaker ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerInfo {
    pub id: String,
    pub label: String,
    pub color: String,            // hex color for UI
    pub total_speaking_time: f64, // seconds
    pub segment_count: u32,
}
```

### 5.2 TypeScript Interfaces (`src/types/index.ts`)

```typescript
// ── Audio ──────────────────────────────────────────────────────

export interface AudioSource {
  id: string;
  name: string;
  source_type: AudioSourceType;
  is_active: boolean;
}

export type AudioSourceType =
  | { type: 'SystemDefault' }
  | { type: 'Device'; device_id: string }
  | { type: 'Application'; pid: number; app_name: string };

// ── Transcript ─────────────────────────────────────────────────

export interface TranscriptSegment {
  id: string;
  source_id: string;
  speaker_id: string | null;
  speaker_label: string | null;
  text: string;
  start_time: number;
  end_time: number;
  confidence: number;
}

// ── Knowledge Graph ────────────────────────────────────────────

export interface GraphEntity {
  id: string;
  name: string;
  entity_type: string;
  mention_count: number;
  first_seen: number;
  last_seen: number;
  aliases: string[];
}

export interface GraphRelation {
  id: string;
  source_id: string;
  target_id: string;
  relation_type: string;
  valid_from: number;
  valid_until: number | null;
  confidence: number;
  source_segment_id: string;
}

export interface GraphSnapshot {
  entities: GraphEntity[];
  relations: GraphRelation[];
  last_updated: number;
  node_count: number;
  edge_count: number;
}

// ── Pipeline Status ────────────────────────────────────────────

export interface PipelineStatus {
  capture: StageStatus;
  pipeline: StageStatus;
  asr: StageStatus;
  diarization: StageStatus;
  entity_extraction: StageStatus;
  graph: StageStatus;
  sidecar: SidecarStatus;
}

export type StageStatus =
  | { type: 'Idle' }
  | { type: 'Running'; processed_count: number }
  | { type: 'Error'; message: string };

export type SidecarStatus =
  | 'NotStarted'
  | 'Starting'
  | 'Healthy'
  | { type: 'Unhealthy'; reason: string }
  | 'Stopped';

// ── Speaker ────────────────────────────────────────────────────

export interface SpeakerInfo {
  id: string;
  label: string;
  color: string;
  total_speaking_time: number;
  segment_count: number;
}
```

---

## 6. Threading Model

### Thread Inventory

```
┌─────────────────────────────────────────────────────────────────────────┐
│ Thread Name              │ Responsibility             │ Communication   │
├──────────────────────────┼────────────────────────────┼─────────────────┤
│ main (Tauri)             │ Tauri runtime, commands,   │ AppState via    │
│                          │ event emission, UI IPC     │ Arc<RwLock>     │
├──────────────────────────┼────────────────────────────┼─────────────────┤
│ audio-mgr                │ Owns AudioManager,         │ Receives        │
│                          │ processes AudioCommands     │ AudioCommand    │
│                          │                            │ via crossbeam   │
├──────────────────────────┼────────────────────────────┼─────────────────┤
│ capture-{source_id}      │ Owns one AudioCapture      │ Sends Tagged    │
│ (N threads, one per src) │ instance, reads via        │ AudioBuffer     │
│                          │ subscribe(), forwards      │ via crossbeam   │
│                          │ to pipeline                │                 │
├──────────────────────────┼────────────────────────────┼─────────────────┤
│ audio-pipeline           │ Resample + VAD + buffer    │ Receives audio  │
│                          │ speech frames              │ from captures,  │
│                          │                            │ sends to ASR +  │
│                          │                            │ diarization     │
├──────────────────────────┼────────────────────────────┼─────────────────┤
│ asr-worker               │ whisper-rs transcription   │ Receives speech │
│                          │ (blocking, 300-800ms)      │ chunks, sends   │
│                          │                            │ transcript segs │
├──────────────────────────┼────────────────────────────┼─────────────────┤
│ diarization-worker       │ pyannote-rs speaker        │ Receives audio  │
│                          │ diarization (ONNX)         │ segments, sends │
│                          │                            │ speaker labels  │
├──────────────────────────┼────────────────────────────┼─────────────────┤
│ entity-extractor         │ HTTP client to             │ Receives merged  │
│                          │ llama-server sidecar       │ transcript segs, │
│                          │                            │ sends GraphUpdate│
├──────────────────────────┼────────────────────────────┼─────────────────┤
│ graph-manager            │ Owns petgraph, entity      │ Receives Graph   │
│                          │ resolution, snapshot gen   │ Update, sends    │
│                          │                            │ GraphSnapshot    │
├──────────────────────────┼────────────────────────────┼─────────────────┤
│ event-relay              │ Bridges crossbeam channels │ Receives events  │
│                          │ to Tauri event emission    │ from workers,    │
│                          │                            │ calls app.emit() │
├──────────────────────────┼────────────────────────────┼─────────────────┤
│ llama-server (sidecar)   │ LLM inference for entity   │ HTTP on :8081   │
│ (separate process)       │ extraction                 │                 │
└──────────────────────────┴────────────────────────────┴─────────────────┘
```

### Thread Diagram (Channels)

```
                   AudioCommand
  main ──────────────────────────────────► audio-mgr
  thread            crossbeam                  │
                                               │ spawns/stops
                                               ▼
                                    capture-{id} threads
                                          │
                                          │ TaggedAudioBuffer
                                          │ crossbeam (bounded, 64)
                                          ▼
                                    audio-pipeline
                                     │          │
             SpeechUtterance         │          │  AudioSegment
             crossbeam (bounded, 16) │          │  crossbeam (bounded, 16)
                                     ▼          ▼
                               asr-worker    diarization-worker
                                     │          │
           TranscriptSegment         │          │  SpeakerAssignment
           crossbeam (bounded, 32)   │          │  crossbeam (bounded, 32)
                                     ▼          ▼
                              ┌──── segment-merger ────┐
                              │  (inside asr-worker    │
                              │   or separate merger)  │
                              └──────────┬─────────────┘
                                         │ TranscriptSegment (with speaker)
                                         │ crossbeam (bounded, 32)
                                         ▼
                                  entity-extractor
                                         │
                                         │ GraphUpdate
                                         │ crossbeam (bounded, 32)
                                         ▼
                                   graph-manager
                                         │
             ┌───────────────────────────┤
             │                           │
             ▼                           ▼
     GraphSnapshot              TranscriptEvent
     PipelineStatusEvent        SpeakerEvent
             │                           │
             └───────────┬───────────────┘
                         │ crossbeam (bounded, 64)
                         ▼
                    event-relay ──► Tauri app.emit()
```

### Channel Buffer Sizing Rationale

| Channel | Capacity | Rationale |
|---|---|---|
| Audio capture → pipeline | 64 | ~1.3s of audio at 48kHz/480-sample chunks. Absorbs ASR processing spikes. |
| Pipeline → ASR | 16 | Utterances are 2-30s each. Unlikely to queue more than a few. |
| Pipeline → diarization | 16 | 10s segments. Similar reasoning. |
| ASR → merger | 32 | Buffer for ASR output during entity extraction bottlenecks. |
| Diarization → merger | 32 | Buffer for speaker labels waiting to be merged. |
| Merger → entity extractor | 32 | Buffer for LLM processing delays (200-500ms per segment). |
| Entity extractor → graph | 32 | Buffer for graph update processing. |
| Workers → event relay | 64 | Multiple event sources feeding single relay. |

---

## 7. IPC Protocol

### 7.1 Tauri Commands (Frontend → Backend)

#### `list_audio_sources`

Lists all available audio sources on the system.

```typescript
// Frontend call
const sources: AudioSource[] = await invoke('list_audio_sources');
```

```json
// Response payload
[
  {
    "id": "system-default",
    "name": "System Audio",
    "source_type": { "type": "SystemDefault" },
    "is_active": false
  },
  {
    "id": "app-firefox-1234",
    "name": "Firefox (PID 1234)",
    "source_type": { "type": "Application", "pid": 1234, "app_name": "firefox" },
    "is_active": false
  }
]
```

#### `start_capture`

Begins capturing audio from the specified source.

```typescript
await invoke('start_capture', { sourceId: 'app-firefox-1234' });
```

```json
// Error response (if any)
{ "error": "Device not found: ..." }
```

#### `stop_capture`

Stops capturing audio from the specified source.

```typescript
await invoke('stop_capture', { sourceId: 'app-firefox-1234' });
```

#### `get_graph_state`

Returns the current knowledge graph snapshot.

```typescript
const snapshot: GraphSnapshot = await invoke('get_graph_state');
```

```json
// Response payload
{
  "entities": [
    {
      "id": "ent-001",
      "name": "Acme Corp",
      "entity_type": "ORG",
      "mention_count": 5,
      "first_seen": 12.3,
      "last_seen": 45.6,
      "aliases": ["Acme", "Acme Corporation"]
    }
  ],
  "relations": [
    {
      "id": "rel-001",
      "source_id": "ent-002",
      "target_id": "ent-001",
      "relation_type": "WORKS_AT",
      "valid_from": 12.3,
      "valid_until": null,
      "confidence": 0.92,
      "source_segment_id": "seg-005"
    }
  ],
  "last_updated": 45.6,
  "node_count": 15,
  "edge_count": 22
}
```

#### `get_transcript`

Returns transcript segments, optionally filtered.

```typescript
const segments: TranscriptSegment[] = await invoke('get_transcript', {
  sourceId: 'app-firefox-1234',  // optional
  since: 30.0,                    // optional, seconds
});
```

#### `get_pipeline_status`

Returns the current pipeline health status.

```typescript
const status: PipelineStatus = await invoke('get_pipeline_status');
```

### 7.2 Tauri Events (Backend → Frontend)

#### `transcript-update`

Emitted when a new transcript segment is available.

```json
{
  "id": "seg-042",
  "source_id": "app-firefox-1234",
  "speaker_id": "spk-1",
  "speaker_label": "Speaker 1",
  "text": "We should schedule a meeting with Acme Corp next Tuesday.",
  "start_time": 42.1,
  "end_time": 45.3,
  "confidence": 0.89
}
```

#### `graph-update`

Emitted when the knowledge graph changes. Contains a **delta** or **full snapshot** depending on the change magnitude.

```json
{
  "entities": [ ... ],
  "relations": [ ... ],
  "last_updated": 45.3,
  "node_count": 16,
  "edge_count": 24
}
```

#### `pipeline-status`

Emitted periodically (every 2s) or on status change.

```json
{
  "capture": { "type": "Running", "processed_count": 1250 },
  "pipeline": { "type": "Running", "processed_count": 1248 },
  "asr": { "type": "Running", "processed_count": 42 },
  "diarization": { "type": "Running", "processed_count": 40 },
  "entity_extraction": { "type": "Running", "processed_count": 38 },
  "graph": { "type": "Running", "processed_count": 38 },
  "sidecar": "Healthy"
}
```

#### `speaker-detected`

Emitted when a new speaker is first identified.

```json
{
  "id": "spk-3",
  "label": "Speaker 3",
  "color": "#e74c3c",
  "total_speaking_time": 3.2,
  "segment_count": 1
}
```

#### `capture-error`

Emitted when a capture-related error occurs.

```json
{
  "source_id": "app-firefox-1234",
  "error": "Application process terminated",
  "recoverable": true
}
```

---

## 8. Configuration

### 8.1 Configuration File (`audio-graph.toml`)

Located at `~/.config/audio-graph/audio-graph.toml` (Linux/macOS) or `%APPDATA%\audio-graph\audio-graph.toml` (Windows).

```toml
[audio]
sample_rate = 48000
channels = 2
buffer_size = 480             # frames per buffer (10ms at 48kHz)
ring_buffer_capacity = 65536  # frames in ring buffer

[pipeline]
vad_threshold = 0.5           # VAD probability threshold
vad_min_speech_ms = 500       # minimum speech duration to trigger ASR
vad_max_speech_ms = 30000     # maximum utterance length before forced split
vad_silence_ms = 300          # silence duration to end an utterance

[asr]
model_path = "models/ggml-small.en.bin"
language = "en"
beam_size = 5
temperature = 0.0

[diarization]
segmentation_model = "models/pyannote-segmentation-3.0.onnx"
embedding_model = "models/wespeaker-voxceleb-resnet34.onnx"
speaker_similarity_threshold = 0.7
max_speakers = 10

[sidecar]
model_path = "models/lfm2-350m-extract.Q8_0.gguf"
port = 8081
ctx_size = 2048
n_predict = 512
health_check_interval_ms = 5000
max_restart_attempts = 3

[graph]
entity_similarity_threshold = 0.85
max_nodes = 1000
max_edges = 5000
snapshot_interval_ms = 500    # minimum interval between graph snapshots

[ui]
theme = "dark"
graph_dimension = "2d"        # "2d" or "3d"
max_transcript_entries = 500
```

### 8.2 Model Paths

Models are stored in `apps/audio-graph/models/` (not checked into git). A download script is provided (see Section 11).

| Model | File | Size | Purpose |
|---|---|---|---|
| Whisper small.en | `ggml-small.en.bin` | ~466 MB | ASR |
| Silero VAD | `silero_vad.onnx` | ~2 MB | Voice Activity Detection |
| pyannote segmentation | `pyannote-segmentation-3.0.onnx` | ~5 MB | Speaker segmentation |
| WeSpeaker embedding | `wespeaker-voxceleb-resnet34.onnx` | ~25 MB | Speaker embeddings |
| LFM2-350M-Extract | `lfm2-350m-extract.Q8_0.gguf` | ~200 MB | Entity extraction |

**Total model storage:** ~700 MB

---

## 9. Project Structure

```
apps/audio-graph/
├── docs/
│   └── ARCHITECTURE.md           # This document
├── models/                        # ML models (gitignored)
│   ├── .gitkeep
│   └── README.md                  # Model download instructions
├── scripts/
│   └── download-models.sh         # Model download script
├── src-tauri/
│   ├── Cargo.toml                 # Rust dependencies
│   ├── tauri.conf.json            # Tauri configuration
│   ├── capabilities/              # Tauri v2 capability permissions
│   │   └── default.json
│   ├── binaries/                  # Sidecar binaries (llama-server)
│   │   └── .gitkeep
│   ├── icons/                     # Application icons
│   └── src/
│       ├── main.rs                # Tauri entry point
│       ├── lib.rs                 # Module declarations
│       ├── state.rs               # AppState definition
│       ├── commands.rs            # Tauri command handlers
│       ├── events.rs              # Event name constants
│       ├── models.rs              # All data model structs
│       ├── config.rs              # Configuration loading
│       ├── audio/
│       │   ├── mod.rs             # Audio module root
│       │   ├── manager.rs         # AudioManager — multi-source capture
│       │   └── pipeline.rs        # AudioPipeline — resample + VAD + buffering
│       ├── asr/
│       │   ├── mod.rs             # ASR module root
│       │   └── worker.rs          # AsrWorker — whisper-rs integration
│       ├── diarization/
│       │   ├── mod.rs             # Diarization module root
│       │   ├── worker.rs          # DiarizationWorker — pyannote-rs
│       │   └── speaker.rs         # SpeakerRegistry — speaker tracking
│       ├── graph/
│       │   ├── mod.rs             # Graph module root
│       │   ├── knowledge.rs       # KnowledgeGraph — petgraph wrapper
│       │   ├── resolution.rs      # Entity resolution logic
│       │   └── manager.rs         # GraphManager thread
│       └── sidecar/
│           ├── mod.rs             # Sidecar module root
│           ├── manager.rs         # SidecarManager — llama-server lifecycle
│           └── extractor.rs       # EntityExtractor — HTTP client + extraction
├── src/
│   ├── App.tsx                    # Root component + event listeners
│   ├── main.tsx                   # React entry point
│   ├── index.html                 # HTML template
│   ├── components/
│   │   ├── ControlBar.tsx
│   │   ├── AudioSourceSelector.tsx
│   │   ├── LiveTranscript.tsx
│   │   ├── KnowledgeGraphViewer.tsx
│   │   ├── SpeakerPanel.tsx
│   │   ├── StatusBar.tsx
│   │   ├── NodeTooltip.tsx
│   │   └── GraphControls.tsx
│   ├── hooks/
│   │   ├── useTauriEvent.ts       # Generic Tauri event listener hook
│   │   ├── useAudioSources.ts     # Audio source management hook
│   │   └── useGraphData.ts        # Graph data transformation hook
│   ├── store/
│   │   └── index.ts               # Zustand store definition
│   ├── types/
│   │   └── index.ts               # TypeScript interfaces (mirrors Rust models)
│   └── styles/
│       └── globals.css            # TailwindCSS imports + custom styles
├── package.json                   # Frontend dependencies (managed by Bun)
├── tsconfig.json                  # TypeScript config
├── vite.config.ts                 # Vite config (Tauri plugin)
├── tailwind.config.js             # Tailwind config
├── postcss.config.js              # PostCSS config
└── .gitignore                     # Ignore models/, node_modules/, target/
```

---

## 10. Dependencies List

### 10.1 Rust Crate Dependencies (`src-tauri/Cargo.toml`)

#### Core

| Crate | Version | Purpose |
|---|---|---|
| `tauri` | `2.*` | Application framework (features: tray-icon, shell-sidecar) |
| `tauri-build` | `2.*` | Build script for Tauri |
| `rsac` | `path = "../../../"` | Audio capture library |
| `serde` | `1.0` | Serialization (features: derive) |
| `serde_json` | `1.0` | JSON serialization |
| `log` | `0.4` | Logging facade |
| `env_logger` | `0.11` | Logging implementation |
| `uuid` | `1.0` | UUID generation (features: v4) |
| `toml` | `0.8` | Configuration file parsing |
| `dirs` | `5.0` | Platform config directory resolution |
| `crossbeam-channel` | `0.5` | Multi-producer multi-consumer channels |

#### Audio Processing

| Crate | Version | Purpose |
|---|---|---|
| `rubato` | `0.16` | Audio resampling (48kHz → 16kHz) |
| `voice_activity_detector` | `0.2` | Silero VAD wrapper (ONNX Runtime) |

#### ASR

| Crate | Version | Purpose |
|---|---|---|
| `whisper-rs` | `0.12` | whisper.cpp Rust bindings |

#### Diarization

| Crate | Version | Purpose |
|---|---|---|
| `ort` | `2.0` | ONNX Runtime for pyannote models |
| `ndarray` | `0.16` | N-dimensional arrays for model I/O |

#### Knowledge Graph

| Crate | Version | Purpose |
|---|---|---|
| `petgraph` | `0.7` | Graph data structure (StableGraph) |
| `strsim` | `0.11` | String similarity for entity resolution |

#### HTTP

| Crate | Version | Purpose |
|---|---|---|
| `reqwest` | `0.12` | HTTP client for llama-server (features: blocking, json) |

### 10.2 Frontend Dependencies (`package.json`)

#### Core

| Package | Version | Purpose |
|---|---|---|
| `react` | `^18.3` | UI framework |
| `react-dom` | `^18.3` | React DOM renderer |
| `@tauri-apps/api` | `^2.0` | Tauri IPC bridge |
| `@tauri-apps/plugin-shell` | `^2.0` | Sidecar management from frontend |

#### State Management

| Package | Version | Purpose |
|---|---|---|
| `zustand` | `^5.0` | Lightweight state management |

#### Visualization

| Package | Version | Purpose |
|---|---|---|
| `react-force-graph-2d` | `^1.25` | 2D force-directed graph |
| `react-force-graph-3d` | `^1.25` | 3D force-directed graph (optional) |
| `three` | `^0.170` | 3D rendering engine (peer dep of 3D graph) |

#### Styling

| Package | Version | Purpose |
|---|---|---|
| `tailwindcss` | `^4.0` | Utility-first CSS |
| `postcss` | `^8.5` | CSS processing |
| `autoprefixer` | `^10.4` | CSS vendor prefixes |

#### Dev Dependencies

| Package | Version | Purpose |
|---|---|---|
| `typescript` | `^5.7` | TypeScript compiler |
| `@types/react` | `^18.3` | React type definitions |
| `@types/react-dom` | `^18.3` | ReactDOM type definitions |
| `vite` | `^6.0` | Build tool |
| `@vitejs/plugin-react` | `^4.3` | React Vite plugin |
| `@tauri-apps/cli` | `^2.0` | Tauri CLI tools |

### 10.3 Model Files

| Model | File | Download Size | Source |
|---|---|---|---|
| Whisper small.en | `ggml-small.en.bin` | 466 MB | huggingface.co/ggerganov/whisper.cpp |
| Silero VAD v5 | `silero_vad.onnx` | 2.3 MB | github.com/snakers4/silero-vad |
| pyannote seg 3.0 | `pyannote-segmentation-3.0.onnx` | 5.4 MB | huggingface.co/pyannote |
| WeSpeaker ResNet34 | `wespeaker-voxceleb-resnet34.onnx` | 25 MB | huggingface.co/pyannote |
| LFM2-350M-Extract | `lfm2-350m-extract.Q8_0.gguf` | 200 MB | huggingface.co/Salesforce |

**Total download:** ~700 MB

### 10.4 Sidecar Binaries

| Binary | Source | Purpose |
|---|---|---|
| `llama-server` | github.com/ggml-org/llama.cpp | LLM inference server for entity extraction |

The `llama-server` binary must be placed in `src-tauri/binaries/` with the Tauri sidecar naming convention: `llama-server-{target_triple}` (e.g., `llama-server-x86_64-unknown-linux-gnu`).

---

## 11. Build and Run Instructions

### 11.1 Prerequisites

| Requirement | Version | Notes |
|---|---|---|
| Rust | 1.82+ | Stable toolchain |
| Bun | 1.0+ | JavaScript runtime & package manager |
| Tauri CLI | 2.x | `cargo install tauri-cli` |
| System libs | | See platform sections below |

**Linux (PipeWire):**
```bash
sudo apt install libwebkit2gtk-4.1-dev libappindicator3-dev \
  librsvg2-dev patchelf libpipewire-0.3-dev \
  pkg-config build-essential clang libclang-dev llvm-dev
```

**macOS:**
```bash
xcode-select --install
# Xcode 15+ for macOS 14.4+ Process Tap APIs
```

**Windows:**
```powershell
# Visual Studio Build Tools 2022 with C++ workload
# WebView2 runtime (usually pre-installed on Windows 10+)
```

### 11.2 Model Download

```bash
cd apps/audio-graph
./scripts/download-models.sh
```

The script downloads all required models to the `models/` directory. Estimated download: ~700 MB.

### 11.3 Sidecar Setup

Pre-built `llama-server` binaries can be downloaded from llama.cpp releases:

```bash
# Linux x86_64 example
curl -L https://github.com/ggml-org/llama.cpp/releases/latest/download/llama-server-linux-x86_64 \
  -o src-tauri/binaries/llama-server-x86_64-unknown-linux-gnu
chmod +x src-tauri/binaries/llama-server-x86_64-unknown-linux-gnu
```

### 11.4 Development Mode

```bash
cd apps/audio-graph

# Install frontend dependencies
bun install

# Run in development mode (hot-reload frontend + Rust recompilation)
cargo tauri dev
```

### 11.5 Production Build

```bash
cd apps/audio-graph

# Build optimized release
cargo tauri build

# Output: src-tauri/target/release/bundle/
# Linux: .deb, .AppImage
# macOS: .dmg, .app
# Windows: .msi, .exe
```

### 11.6 Workspace Integration

Add the AudioGraph Tauri crate to the workspace in the root `Cargo.toml`:

```toml
[workspace]
members = ["apps/audio-graph/src-tauri"]
```

---

## 12. Latency Budget

### End-to-End Pipeline Latency

Target: **< 3 seconds** from spoken word to knowledge graph update.

| Stage | Latency | Cumulative | Notes |
|---|---|---|---|
| Audio capture | ~10ms | 10ms | Buffer size: 480 frames at 48kHz = 10ms |
| Channel transfer | ~0.1ms | ~10ms | crossbeam bounded channel |
| Resampling (rubato) | ~2ms | ~12ms | SincFixedIn, 48kHz→16kHz, single buffer |
| VAD (Silero) | ~3ms | ~15ms | 30ms chunks, ONNX inference ~3ms each |
| Speech buffering | 300-30000ms | 315-30015ms | Waiting for silence gap (configurable) |
| ASR (whisper-rs) | 300-800ms | 615-30815ms | Depends on utterance length |
| Diarization | 50-200ms | 665-31015ms | Runs in parallel with ASR |
| Speaker merge | ~1ms | ~666ms | Timestamp alignment |
| Entity extraction | 200-500ms | 866-31515ms | HTTP to llama-server |
| Graph update | ~5ms | ~871ms | petgraph mutation + snapshot |
| Event emission | ~1ms | ~872ms | Tauri IPC |
| Frontend render | ~16ms | ~888ms | React re-render + force-graph tick |

**Typical case (5s utterance):** ~1.2s from end of speech to graph update.
**Best case (0.5s utterance):** ~0.9s from end of speech to graph update.

### Optimization Strategies

| Strategy | Impact | Description |
|---|---|---|
| **Streaming VAD** | Reduces buffering latency | Start ASR on partial utterance with speculative segmentation |
| **ASR model quantization** | 30-50% faster inference | Use `ggml-small.en-q5_1.bin` quantized model |
| **Batch entity extraction** | Reduce HTTP overhead | Batch multiple segments per LLM request |
| **Incremental graph updates** | Reduce IPC payload | Send delta updates instead of full snapshots |
| **GPU acceleration** | 2-5x faster ASR/VAD | CUDA/Metal support via whisper.cpp and ort GPU providers |
| **Ring buffer tuning** | Reduce overruns | Monitor `overrun_count()` and adjust capacity |
| **Pipeline parallelism** | Hide latency | ASR and diarization run concurrently on the same utterance |

### Memory Budget

| Component | Estimated RAM | Notes |
|---|---|---|
| Whisper model (small.en) | ~500 MB | Loaded once at startup |
| Silero VAD | ~10 MB | Small ONNX model |
| pyannote segmentation | ~20 MB | ONNX model |
| WeSpeaker embeddings | ~50 MB | ONNX model |
| llama-server (sidecar) | ~400 MB | LFM2-350M GGUF model |
| Audio ring buffers | ~10 MB | Per source, 64K frames * 4 bytes * 2 channels |
| Knowledge graph | ~10-100 MB | Depends on graph size |
| Frontend | ~100 MB | React + force-graph + WebView |
| **Total** | **~1.1-1.2 GB** | Baseline with all models loaded |

---

## Appendix A: Key rsac API Usage Patterns

The following patterns document how AudioGraph interfaces with the rsac library. These are derived from the actual rsac API as implemented in the workspace.

### Creating a capture for system audio

```rust
use rsac::{AudioCaptureBuilder, CaptureTarget};

let mut capture = AudioCaptureBuilder::new()
    .with_target(CaptureTarget::SystemDefault)
    .sample_rate(48000)
    .channels(2)
    .build()?;

capture.start()?;
let rx = capture.subscribe()?;  // mpsc::Receiver<AudioBuffer>

// Read loop (on dedicated thread)
loop {
    match rx.recv() {
        Ok(buffer) => {
            let data: &[f32] = buffer.data();
            let frames = buffer.num_frames();
            let duration = buffer.duration();
            // Process audio...
        }
        Err(_) => break, // Channel closed
    }
}

capture.stop()?;
```

### Creating a capture for a specific application

```rust
use rsac::{AudioCaptureBuilder, CaptureTarget};

let mut capture = AudioCaptureBuilder::new()
    .with_target(CaptureTarget::ApplicationByName("Firefox".to_string()))
    .sample_rate(48000)
    .channels(2)
    .build()?;
```

### Checking platform capabilities

```rust
use rsac::PlatformCapabilities;

let caps = PlatformCapabilities::query();
if caps.supports_application_capture {
    // Safe to use CaptureTarget::Application or ApplicationByName
}
if caps.supports_process_tree_capture {
    // Safe to use CaptureTarget::ProcessTree
}
```

### Monitoring ring buffer health

```rust
let overruns = capture.overrun_count();
if overruns > 0 {
    log::warn!("Ring buffer overruns detected: {} buffers dropped", overruns);
}
```

### Device enumeration

```rust
use rsac::{get_device_enumerator, DeviceKind};

let enumerator = get_device_enumerator()?;
let devices = enumerator.enumerate_devices()?;
for device in &devices {
    println!("{}: {} (default: {})", device.id(), device.name(), device.is_default());
}
```

### Critical constraints

1. **`AudioCapture` is not `Sync`** — Cannot be shared across threads. Must be owned by a single thread.
2. **`subscribe()` is single-consumer** — The returned `mpsc::Receiver` competes with other readers for the same ring buffer. Do NOT call `subscribe()` multiple times on the same capture and expect both receivers to get all data. Fan-out must be implemented downstream.
3. **`subscribe()` and `read_buffer()` compete** — Do not mix pull-based `read_buffer()` calls with `subscribe()` on the same capture.
4. **Capture cannot be restarted** — After `stop()`, the stream is released. Create a new `AudioCapture` instance to restart.

---

*This document is the source of truth for the AudioGraph architecture. All implementation tasks should reference sections of this document. Last updated: 2026-03-20.*
