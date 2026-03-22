# AudioGraph 🎙️🔗

> Live audio capture → speech recognition → temporal knowledge graph

[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange)](https://www.rust-lang.org/)
[![Tauri](https://img.shields.io/badge/Tauri-v2-blue)](https://v2.tauri.app/)
[![React](https://img.shields.io/badge/React-18-61dafb)](https://react.dev/)
[![License](https://img.shields.io/badge/license-see%20root-green)](/LICENSE)

---

## Overview

AudioGraph is a desktop application that captures live system audio, performs real-time speech recognition, identifies speakers, extracts entities, and builds an evolving temporal knowledge graph — all visualized in a force-directed graph. Built with Tauri v2 (Rust backend + React frontend).

The pipeline streams audio through Voice Activity Detection, Automatic Speech Recognition (Whisper), speaker diarization, and entity extraction, feeding results into a [`petgraph`](https://docs.rs/petgraph)-based temporal knowledge graph. The React frontend renders the graph live using [`react-force-graph-2d`](https://github.com/vasturiano/react-force-graph) alongside a scrolling transcript and pipeline status monitor.

---

## Features

- **Multi-source audio capture** — System default, specific devices, per-application (Linux PipeWire, Windows WASAPI, macOS CoreAudio)
- **Real-time audio processing** — 48kHz→16kHz resampling via `rubato`, stereo→mono downmix
- **Voice Activity Detection** — Silero VAD v5 (ONNX) for speech segmentation
- **Automatic Speech Recognition** — `whisper-rs` (`whisper.cpp`) with configurable model size
- **Speaker Diarization** — MVP audio-feature-based clustering (RMS energy, zero-crossing rate)
- **Entity Extraction** — 3-tier chain: native LLM (llama-cpp-2) → OpenAI-compatible API → rule-based NER
- 💬 **Chat Sidebar** — Ask questions about the conversation and knowledge graph
- 🧠 **Native LLM Inference** — In-process GGUF model via llama-cpp-2 (replaces HTTP sidecar)
- **Temporal Knowledge Graph** — `petgraph`-based graph with episodic memory, entity resolution (Jaro-Winkler), temporal decay
- **Live Visualization** — `react-force-graph-2d` with color-coded entity types
- **Live Transcript** — Scrolling transcript with speaker labels and timestamps
- **Pipeline Status Monitor** — Real-time display of each pipeline stage
- **Dark Theme** — Full dark theme with CSS custom properties
- **Graceful Degradation** — Falls back to diarization-only mode if Whisper model unavailable

---

## Screenshots

> Screenshots coming soon. Run `cargo tauri dev` to see the UI.

---

## Architecture

AudioGraph uses a **4-thread pipeline model** to keep the UI responsive while processing audio in real time:

```
┌─────────────┐    ┌──────────────────┐    ┌────────────┐    ┌─────────────────────┐
│ Capture      │───▶│ Pipeline thread  │───▶│ VAD thread │───▶│ Speech processor    │
│ thread(s)    │    │ (resample/downmix)│    │ (Silero v5)│    │ thread              │
└─────────────┘    └──────────────────┘    └────────────┘    └─────────────────────┘
                                                                │
                                                                ├─ ASR (Whisper)
                                                                ├─ Diarization
                                                                ├─ Entity Extraction
                                                                ├─ Graph update
                                                                ├─ Tauri events
                                                                └─▶ React UI
```

- **Capture thread(s)** — Pulls audio from `rsac` via ring buffer, sends raw PCM downstream
- **Pipeline thread** — Resamples 48kHz→16kHz (`rubato`), downmixes stereo→mono
- **VAD thread** — Silero VAD v5 segments speech from silence
- **Speech processor thread** — ASR → Diarization → Entity Extraction → Graph → Tauri events → React UI

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the full architecture document.

---

## Setup

### Windows (Step-by-Step)

1. **Install Rust** via [rustup](https://rustup.rs/):
   ```powershell
   winget install Rustlang.Rustup
   # Or download from https://rustup.rs
   ```

2. **Install Visual Studio Build Tools 2019+** with the "Desktop development with C++" workload:
   ```powershell
   winget install Microsoft.VisualStudio.2022.BuildTools
   ```
   > During installation, select the **"Desktop development with C++"** workload. This provides MSVC, Windows SDK, and the C++ toolchain needed by Rust and native dependencies.

3. **Install CMake** (required by `whisper-rs` and `llama-cpp-2` build scripts):
   ```powershell
   winget install Kitware.CMake
   ```

4. **Install LLVM/Clang** (required by `bindgen` for FFI bindings):
   ```powershell
   winget install LLVM.LLVM
   ```

5. **Install Bun** (frontend runtime & package manager):
   ```powershell
   powershell -c "irm bun.sh/install.ps1 | iex"
   ```

6. **Clone the repo and navigate to the app**:
   ```powershell
   git clone https://github.com/user/rust-crossplat-audio-capture.git
   cd rust-crossplat-audio-capture/apps/audio-graph
   ```

7. **Install frontend dependencies**:
   ```powershell
   bun install
   ```

8. **Download ML models** (Whisper for ASR, optional LLM for entity extraction):
   ```powershell
   .\scripts\download-models.ps1
   ```
   > Or skip this step — AudioGraph can download models in-app via the model manager.

9. **Build the Rust backend**:
   ```powershell
   cd src-tauri
   cargo build
   # For NVIDIA GPU acceleration:
   # cargo build --features cuda
   cd ..
   ```

10. **Run in development mode**:
    ```powershell
    bun run tauri dev
    ```

11. **First-run workflow**: Select an audio source from the dropdown → click Start → watch the knowledge graph build in real time.

### macOS (Step-by-Step)

1. **Install Rust** via [rustup](https://rustup.rs/):
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

2. **Install Xcode Command Line Tools** (provides clang, Metal framework, CoreAudio):
   ```bash
   xcode-select --install
   ```

3. **Install CMake** (required by `whisper-rs` and `llama-cpp-2`):
   ```bash
   brew install cmake
   ```
   > CMake may auto-detect via Xcode in some configurations, but explicit installation is recommended.

4. **Install Bun** (frontend runtime & package manager):
   ```bash
   curl -fsSL https://bun.sh/install | bash
   ```

5. **Clone the repo and navigate to the app**:
   ```bash
   git clone https://github.com/user/rust-crossplat-audio-capture.git
   cd rust-crossplat-audio-capture/apps/audio-graph
   ```

6. **Install frontend dependencies**:
   ```bash
   bun install
   ```

7. **Download ML models** (Whisper for ASR, optional LLM for entity extraction):
   ```bash
   ./scripts/download-models.sh
   ```
   > Or skip this step — AudioGraph can download models in-app via the model manager.

8. **Build the Rust backend** (Metal GPU acceleration enabled automatically on macOS):
   ```bash
   cd src-tauri && cargo build && cd ..
   ```

9. **Run in development mode**:
   ```bash
   bun run tauri dev
   ```

10. **Grant microphone permission** when prompted by macOS. AudioGraph needs audio capture access.

11. **For application-specific capture**: Requires **macOS 14.4+** (Sonoma with Process Tap API). On older macOS versions, only system-wide capture is available.

### Linux (Debian/Ubuntu)

1. **Install Rust** via [rustup](https://rustup.rs/):
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

2. **Install build tools, clang, and PipeWire development libraries**:
   ```bash
   # Build essentials + clang/LLVM (for bindgen + llama.cpp)
   sudo apt install build-essential cmake clang libclang-dev

   # PipeWire audio backend
   sudo apt install libpipewire-0.3-dev libspa-0.2-dev

   # Tauri v2 system dependencies (WebKitGTK, etc.)
   sudo apt install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev
   ```

3. **Install Bun**:
   ```bash
   curl -fsSL https://bun.sh/install | bash
   ```

4. **Clone, install, and run**:
   ```bash
   git clone https://github.com/user/rust-crossplat-audio-capture.git
   cd rust-crossplat-audio-capture/apps/audio-graph
   bun install
   ./scripts/download-models.sh    # or use in-app model manager
   bun run tauri dev
   ```

---

## Configuration Reference

### LLM Backend Selection

AudioGraph supports two LLM backends for entity extraction and chat:

| Backend | Description | When to Use |
|---|---|---|
| **Native (llama-cpp-2)** | In-process GGUF model inference | Offline use, low latency, no API keys |
| **API (OpenAI-compatible)** | HTTP calls to external endpoint | Cloud models, larger models, no local GPU |

The extraction chain tries backends in order: **native LLM → API endpoint → rule-based NER**.

### Model Paths and Download

| Model | Purpose | Size | Download |
|---|---|---|---|
| `ggml-small.en.bin` | Whisper ASR (speech recognition) | ~500 MB | `./scripts/download-models.sh` or in-app |
| `lfm2-350m-extract-q4_k_m.gguf` | Entity extraction + chat | ~350 MB | `./scripts/download-models.sh` or in-app |
| Silero VAD v5 | Voice activity detection | ~2 MB | Auto-downloaded on first run |

Models are stored in `apps/audio-graph/models/` (gitignored).

### GPU Acceleration

| Platform | Backend | How to Enable |
|---|---|---|
| **macOS** | Metal | Automatic — enabled by default |
| **Windows / Linux** | CUDA (NVIDIA) | `cargo build --features cuda` |
| **Windows / Linux** | Vulkan (AMD/NVIDIA/Intel) | `cargo build --features vulkan` |
| **All** | CPU only | Default build, no flags needed |

### API Endpoint Configuration

Configure an OpenAI-compatible API endpoint for entity extraction and chat:

```typescript
// OpenAI
configureApiEndpoint({
    endpoint: 'https://api.openai.com/v1',
    apiKey: 'sk-...',
    model: 'gpt-4o-mini',
});

// Ollama (local, no API key)
configureApiEndpoint({
    endpoint: 'http://localhost:11434/v1',
    model: 'qwen2.5:3b',
});

// LM Studio (local, no API key)
configureApiEndpoint({
    endpoint: 'http://localhost:1234/v1',
    model: 'loaded-model-name',
});

// OpenRouter
configureApiEndpoint({
    endpoint: 'https://openrouter.ai/api/v1',
    apiKey: 'sk-or-...',
    model: 'anthropic/claude-sonnet-4',
});
```

### Audio Capture Settings

Configured in [`src-tauri/config/default.toml`](src-tauri/config/default.toml):

| Setting | Default | Description |
|---|---|---|
| `audio.sample_rate` | 48000 | Capture sample rate (Hz) |
| `audio.channels` | 2 | Capture channels (stereo) |
| `audio.buffer_size` | 4096 | Buffer size per read |
| `audio.ring_buffer_capacity` | 262144 | Ring buffer capacity (samples) |
| `pipeline.vad_threshold` | 0.5 | VAD speech probability threshold |
| `asr.model_path` | `models/ggml-small.en.bin` | Whisper model file path |
| `asr.language` | `en` | ASR language |

---

## Platform Capabilities Matrix

### Audio Capture Modes

| Capture Mode | Windows (WASAPI) | Linux (PipeWire) | macOS (CoreAudio) |
|---|---|---|---|
| **System default** | ✅ | ✅ | ✅ |
| **Specific device** | ✅ | ✅ | ✅ |
| **Application (by PID)** | ✅ Process loopback | ✅ pw-dump node | ✅ Process Tap (14.4+) |
| **Application (by name)** | ✅ sysinfo → PID | ✅ pw-dump → node serial | ✅ Process Tap (14.4+) |
| **Process tree** | ✅ Process loopback | ✅ PID → PipeWire node | ✅ Process Tap (14.4+) |

### LLM Backends

| Feature | Native (llama-cpp-2) | API (OpenAI-compatible) |
|---|---|---|
| **Offline operation** | ✅ | ❌ (requires network) |
| **Latency** | Low (in-process) | Varies (network-dependent) |
| **Model size** | Limited by local RAM/VRAM | Unlimited (cloud) |
| **GPU acceleration** | Metal / CUDA / Vulkan | N/A (server-side) |
| **Entity extraction** | ✅ Grammar-constrained JSON | ✅ JSON mode |
| **Chat** | ✅ Free-form generation | ✅ Free-form generation |
| **Cost** | Free (local compute) | Per-token pricing |
| **Providers** | Local GGUF files | OpenAI, OpenRouter, Ollama, LM Studio, vLLM, Groq, Together AI |

---

## Model Setup

### Whisper (Required for ASR)

AudioGraph uses [`whisper-rs`](https://github.com/tazz4843/whisper-rs) for speech recognition, which requires a GGML-format Whisper model file.

> **Planned:** In-app model download with a progress UI is on the [roadmap](#roadmap). For now, use the shell script or manual download below.

1. **Automatic download** (recommended):
   ```bash
   # Linux/macOS
   ./scripts/download-models.sh

   # Windows (PowerShell)
   .\scripts\download-models.ps1
   ```

2. **Manual download**:
   - Download [`ggml-small.en.bin`](https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin) from HuggingFace (`ggerganov/whisper.cpp`)
   - Place it in the `models/` directory relative to the application root:
     ```
     apps/audio-graph/models/ggml-small.en.bin
     ```

### Silero VAD (Auto-downloaded)

The Silero VAD v5 ONNX model is automatically downloaded and cached by the [`voice_activity_detector`](https://crates.io/crates/voice_activity_detector) crate on first run. No manual setup required.

### LFM2-350M-Extract (Optional — Enhanced Entity Extraction)

For improved entity extraction beyond the built-in rule-based NER, download the LLM model. AudioGraph uses native in-process inference via `llama-cpp-2` — no external server required.

The model can be downloaded via the **in-app model manager** (invoke the `list_available_models` and `download_model_cmd` Tauri commands), or manually:

1. Download [`lfm2-350m-extract-q4_k_m.gguf`](https://huggingface.co/LiquidAI/LFM2-350M-Extract-GGUF/resolve/main/lfm2-350m-extract-q4_k_m.gguf) from HuggingFace
2. Place it in the `models/` directory:
   ```
   apps/audio-graph/models/lfm2-350m-extract-q4_k_m.gguf
   ```

Or use the download script:
```bash
# Linux/macOS
./scripts/download-models.sh

# Windows (PowerShell)
.\scripts\download-models.ps1
```

---

## Configuration

AudioGraph's configuration spec is defined in [`src-tauri/config/default.toml`](src-tauri/config/default.toml):

| Section | Keys | Description |
|---|---|---|
| `[audio]` | `sample_rate`, `channels`, `buffer_size`, `ring_buffer_capacity` | Audio capture parameters |
| `[pipeline]` | `vad_threshold`, `vad_min_speech_ms`, `vad_max_speech_ms`, `vad_silence_ms` | Pipeline processing settings |
| `[asr]` | `model_path`, `language`, `beam_size`, `temperature` | Whisper ASR configuration |
| `[diarization]` | `speaker_similarity_threshold`, `max_speakers` | Speaker identification tuning |
| `[llm]` | `model_path`, `ctx_size`, `n_predict` | Native LLM engine settings |
| `[graph]` | `entity_similarity_threshold`, `max_nodes`, `max_edges`, `snapshot_interval_ms` | Knowledge graph parameters |
| `[ui]` | `theme`, `graph_dimension`, `max_transcript_entries` | Frontend display settings |

> **Note:** The config file defines the spec. The current version uses hardcoded defaults at runtime. Runtime config loading from `default.toml` is on the [roadmap](#roadmap).

---

## Chat & LLM Setup

AudioGraph includes a native LLM engine for entity extraction and an interactive chat sidebar.

### Model Download

Download a small GGUF model for entity extraction and chat:

```bash
# Download a Q4 quantized model (~350MB)
./scripts/download-models.sh
```

Or manually download any GGUF model and configure the path in `config/default.toml`.

### Chat Sidebar

The right panel includes a **Transcript | Chat** tab switcher:
- **Transcript** — Live speech-to-text output (default)
- **Chat** — Ask questions about the conversation and knowledge graph

The chat uses the knowledge graph context (entities, relationships) and recent transcript to provide informed answers.

### LLM Architecture

- **Engine**: `llama-cpp-2` (Rust bindings to llama.cpp) — no external server needed
- **Entity Extraction**: Grammar-constrained JSON output via GBNF grammar
- **Chat**: Free-form generation with graph context in system prompt
- **Fallback**: If no model is loaded, rule-based extraction is used automatically

### OpenAI-Compatible API Endpoint (Alternative to Local Model)

Instead of (or in addition to) a local GGUF model, AudioGraph can use any **OpenAI-compatible API endpoint** for entity extraction and chat. This includes:

| Provider | Endpoint | API Key Required |
|---|---|---|
| **OpenAI** | `https://api.openai.com/v1` | Yes |
| **OpenRouter** | `https://openrouter.ai/api/v1` | Yes |
| **Ollama** (local) | `http://localhost:11434/v1` | No |
| **LM Studio** (local) | `http://localhost:1234/v1` | No |
| **vLLM** | `http://localhost:8000/v1` | No |
| **Together AI** | `https://api.together.xyz/v1` | Yes |
| **Groq** | `https://api.groq.com/openai/v1` | Yes |

Configure from the frontend store:
```typescript
import { useAudioGraphStore } from './store';

// Configure OpenRouter
useAudioGraphStore.getState().configureApiEndpoint({
    endpoint: 'https://openrouter.ai/api/v1',
    apiKey: 'sk-or-...',
    model: 'anthropic/claude-sonnet-4',
});

// Configure local Ollama
useAudioGraphStore.getState().configureApiEndpoint({
    endpoint: 'http://localhost:11434/v1',
    model: 'qwen2.5:3b',
});
```

Or invoke the Tauri command directly:
```typescript
import { invoke } from '@tauri-apps/api/core';

await invoke('configure_api_endpoint', {
    endpoint: 'https://openrouter.ai/api/v1',
    apiKey: 'sk-or-...',
    model: 'anthropic/claude-sonnet-4',
});
```

**Extraction chain order:** native LLM → API endpoint → rule-based NER. The first available backend is used. If the native model is loaded, it takes priority (fastest, no network). If only an API endpoint is configured, it handles both entity extraction and chat.

### Build Requirements

The native LLM requires:
- C++17 compiler (gcc 9+ or clang 10+)
- clang (for bindgen)
- cmake

On Ubuntu/Debian:
```bash
sudo apt install build-essential clang cmake
```

---

## Technology Stack

### Rust Backend

| Component | Crate |
|---|---|
| Audio capture | [`rsac`](/) (Rust Cross-Platform Audio Capture) |
| App framework | [`tauri`](https://v2.tauri.app/) v2.10 |
| Resampling | [`rubato`](https://crates.io/crates/rubato) 1.0 |
| VAD | [`voice_activity_detector`](https://crates.io/crates/voice_activity_detector) 0.2 (Silero v5) |
| ASR | [`whisper-rs`](https://crates.io/crates/whisper-rs) 0.16 |
| Graph | [`petgraph`](https://crates.io/crates/petgraph) 0.8 |
| Entity matching | [`strsim`](https://crates.io/crates/strsim) 0.11 (Jaro-Winkler) |
| Native LLM | [`llama-cpp-2`](https://crates.io/crates/llama-cpp-2) 0.1 (llama.cpp bindings) |
| IPC channels | [`crossbeam-channel`](https://crates.io/crates/crossbeam-channel) 0.5 |
| Config format | [`toml`](https://crates.io/crates/toml) 0.9 |
| HTTP (model download) | [`reqwest`](https://crates.io/crates/reqwest) 0.13 |

### React Frontend

| Component | Package |
|---|---|
| UI framework | [`react`](https://react.dev/) 18 |
| State management | [`zustand`](https://github.com/pmndrs/zustand) 5 |
| Graph visualization | [`react-force-graph-2d`](https://github.com/vasturiano/react-force-graph) 1.25 |
| Desktop bridge | [`@tauri-apps/api`](https://v2.tauri.app/reference/javascript/) 2 |
| Build tool | [`vite`](https://vite.dev/) 6 |
| Language | [`typescript`](https://www.typescriptlang.org/) 5.7 |

---

## GPU Acceleration

AudioGraph supports GPU-accelerated inference for both Whisper (ASR) and llama.cpp (LLM). GPU support varies by platform:

| Platform | Backend | How to Enable |
|---|---|---|
| **macOS** | Metal | Automatic — enabled by default in `Cargo.toml` |
| **Windows / Linux** | CUDA (NVIDIA) | `cargo build --features cuda` |
| **Windows / Linux** | Vulkan (AMD, NVIDIA, Intel) | `cargo build --features vulkan` |

### Build Commands

```bash
# CPU only (default — works everywhere)
cd apps/audio-graph && bun run tauri build

# NVIDIA CUDA (requires CUDA Toolkit 11.7+)
cd apps/audio-graph/src-tauri && cargo build --features cuda

# Vulkan (requires Vulkan SDK)
cd apps/audio-graph/src-tauri && cargo build --features vulkan

# macOS Metal — automatic, no extra flags needed
cd apps/audio-graph && bun run tauri build
```

### Prerequisites for GPU Builds

**CUDA (NVIDIA):**
- NVIDIA GPU with Compute Capability 5.0+
- [CUDA Toolkit](https://developer.nvidia.com/cuda-toolkit) 11.7 or later
- NVIDIA driver 515+ (Linux) or 527+ (Windows)

**Vulkan:**
- GPU with Vulkan 1.1+ support (AMD, NVIDIA, or Intel)
- [Vulkan SDK](https://vulkan.lunarg.com/) installed
- Linux: `sudo apt install libvulkan-dev` (Debian/Ubuntu)
- Windows: Install the LunarG Vulkan SDK

> **Note:** GPU features are opt-in Cargo features. The default build is CPU-only and requires no GPU SDKs. On macOS, Metal acceleration is always enabled via platform-specific dependencies.

---

## Development

```bash
# Development mode (hot-reload frontend + Rust rebuild)
bun run tauri dev

# Build for production
bun run tauri build

# Frontend only (no Tauri window)
bun run dev

# Rust backend checks
cd src-tauri && cargo check
cd src-tauri && cargo test

# TypeScript type checking
bun run typecheck
```

---

## Project Structure

```
apps/audio-graph/
├── index.html                          # Vite entry point
├── package.json                        # Frontend dependencies
├── vite.config.ts                      # Vite configuration
├── tsconfig.json                       # TypeScript config
├── scripts/
│   ├── download-models.sh             # Model download helper (Linux/macOS)
│   └── download-models.ps1            # Model download helper (Windows)
├── models/                             # ML models (gitignored)
│   └── ggml-small.en.bin             # Whisper GGML model
├── docs/
│   └── ARCHITECTURE.md                # Full architecture document
├── src/                                # React frontend
│   ├── main.tsx                       # React entry point
│   ├── App.tsx                        # Root component
│   ├── App.css                        # Application styles (dark theme)
│   ├── styles.css                     # Global styles
│   ├── components/
│   │   ├── AudioSourceSelector.tsx    # Audio source dropdown
│   │   ├── ChatSidebar.tsx            # Chat sidebar (LLM Q&A)
│   │   ├── ControlBar.tsx             # Start/stop controls
│   │   ├── KnowledgeGraphViewer.tsx   # Force-directed graph
│   │   ├── LiveTranscript.tsx         # Scrolling transcript
│   │   ├── PipelineStatusBar.tsx      # Pipeline stage monitor
│   │   └── SpeakerPanel.tsx           # Speaker list
│   ├── hooks/
│   │   └── useTauriEvents.ts          # Tauri event subscriptions
│   ├── store/
│   │   └── index.ts                   # Zustand state store
│   └── types/
│       └── index.ts                   # TypeScript type definitions
└── src-tauri/                          # Rust backend
    ├── Cargo.toml                     # Rust dependencies
    ├── tauri.conf.json                # Tauri configuration
    ├── build.rs                       # Tauri build script
    ├── config/
    │   └── default.toml               # Configuration spec
    ├── capabilities/
    │   └── default.json               # Tauri v2 permissions
    ├── src/
    │   ├── main.rs                    # Tauri entry point
    │   ├── lib.rs                     # Tauri app setup
    │   ├── commands.rs                # IPC command handlers
    │   ├── events.rs                  # Tauri event definitions
    │   ├── state.rs                   # Application state
    │   ├── audio/
    │   │   ├── mod.rs                 # Audio module
    │   │   ├── capture.rs             # rsac audio capture
    │   │   ├── pipeline.rs            # Audio processing pipeline
    │   │   └── vad.rs                 # Voice Activity Detection
    │   ├── asr/
    │   │   └── mod.rs                 # Whisper ASR integration
    │   ├── diarization/
    │   │   └── mod.rs                 # Speaker diarization
    │   ├── graph/
    │   │   ├── mod.rs                 # Graph module
    │   │   ├── entities.rs            # Entity type definitions
    │   │   ├── extraction.rs          # Entity extraction (NER)
    │   │   └── temporal.rs            # Temporal knowledge graph
    │   ├── llm/
    │   │   ├── mod.rs                 # LLM module (native + API backends)
    │   │   ├── engine.rs              # Native llama.cpp inference engine
    │   │   └── api_client.rs          # OpenAI-compatible API client
    │   └── models/
    │       └── mod.rs                 # Model management + download
    └── gen/                           # Generated Tauri schemas
```

---

## Tauri Commands (IPC)

These commands are invokable from the React frontend via `@tauri-apps/api`:

| Command | Description | Returns |
|---|---|---|
| `list_audio_sources` | Enumerate available audio capture sources | `Vec<AudioSource>` |
| `start_capture` | Start the audio capture + processing pipeline | `Result<(), String>` |
| `stop_capture` | Stop the active capture pipeline | `Result<(), String>` |
| `get_graph_snapshot` | Get the current knowledge graph state | `GraphSnapshot` |
| `get_transcript` | Get the current transcript entries | `Vec<TranscriptEntry>` |
| `get_pipeline_status` | Get the status of each pipeline stage | `PipelineStatus` |
| `send_chat_message` | Send a chat message to the native LLM | `ChatResponse` |
| `get_chat_history` | Get the chat message history | `Vec<ChatMessage>` |
| `clear_chat_history` | Clear the chat message history | `()` |
| `list_available_models` | List available models and download status | `Vec<ModelInfo>` |
| `download_model_cmd` | Download a model by filename with progress events | `String` (path) |
| `configure_api_endpoint` | Configure an OpenAI-compatible API endpoint | `Result<(), String>` |

---

## Tauri Events

These events are emitted from the Rust backend and consumed by the React frontend:

| Event | Payload | Description |
|---|---|---|
| `transcript-update` | `TranscriptEntry` | New transcript segment with speaker label and text |
| `graph-update` | `GraphSnapshot` | Updated knowledge graph (nodes + edges) |
| `pipeline-status` | `PipelineStatus` | Pipeline stage status changes |
| `speaker-detected` | `SpeakerInfo` | New speaker identified by diarization |
| `capture-error` | `ErrorInfo` | Capture or processing error |
| `model-download-progress` | `DownloadProgress` | Model download progress updates |

---

## Known Limitations

- **MVP speaker diarization** — Uses audio features (RMS, ZCR), not ML speaker embeddings. Speaker identification accuracy is limited.
- **GPU acceleration is opt-in** — macOS uses Metal by default; on Windows/Linux, CUDA and Vulkan are available as Cargo features (see [GPU Acceleration](#gpu-acceleration)).
- **Cross-platform audio** — Platform-conditional Cargo features (`feat_linux`, `feat_windows`, `feat_macos`) are compiled automatically per target OS. Application discovery (PipeWire `pw-dump`) is Linux-only; on Windows/macOS only system-default and device-level capture appear in the source list.
- **Config file** ([`default.toml`](src-tauri/config/default.toml)) defines the spec but runtime uses hardcoded defaults.
- **LLM model not auto-downloaded** — Rule-based entity extraction is used by default. Download the GGUF model via the in-app model manager or shell scripts for native LLM inference.
- **`capture-error` event** is defined but not yet emitted from the backend.
- **`pipeline-status`** is emitted once at start, not periodically updated.

---

## Roadmap

- [x] **In-app model download** — Native Tauri commands (`list_available_models`, `download_model_cmd`) with progress events; frontend model manager UI planned
- [ ] ML-based speaker diarization (pyannote/wespeaker ONNX models)
- [x] GPU-accelerated inference — Metal (macOS, automatic), CUDA and Vulkan (Windows/Linux, opt-in Cargo features)
- [ ] Runtime config loading from `default.toml`
- [x] Cross-platform builds (Windows WASAPI, macOS CoreAudio, Linux PipeWire — platform-conditional Cargo features)
- [ ] Periodic pipeline status updates
- [ ] Capture error forwarding to frontend
- [x] OpenAI-compatible API endpoint support (OpenAI, OpenRouter, Ollama, LM Studio, vLLM, etc.)
- [ ] Graph persistence (save/load knowledge graph)
- [ ] Multi-language ASR support
- [ ] Graph search and entity filtering

---

## License

Part of the [`rsac`](/) (Rust Cross-Platform Audio Capture) project. See the root [LICENSE](/LICENSE) for details.
