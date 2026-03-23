# Model Management Design — Gap Closures G1–G6

> **Status:** Design document — not yet implemented.
> **Scope:** `apps/audio-graph/src-tauri/src/models/`, `commands.rs`, `state.rs`, `lib.rs`

---

## 1. Model Directory Resolution Strategy

### Problem

[`get_models_dir()`](../src-tauri/src/models/mod.rs:46) returns `PathBuf::from("models")` — relative to CWD. This works during `cargo tauri dev` (CWD = `src-tauri/`) but breaks in production builds where the CWD is unpredictable (e.g., `/` on macOS, `C:\Windows\system32` on Windows).

The same CWD-relative path also appears in [`AsrConfig::default()`](../src-tauri/src/asr/mod.rs:34) (`"models/ggml-small.en.bin"`).

### Solution

Use Tauri v2's `app.path().app_data_dir()` as the canonical models root. The function signature changes to accept the Tauri `AppHandle`:

```rust
// models/mod.rs

use tauri::Manager; // for .path()

pub fn get_models_dir(app_handle: &tauri::AppHandle) -> Result<PathBuf, String> {
    let base = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to resolve app data dir: {}", e))?;
    let models_dir = base.join("models");
    if !models_dir.exists() {
        fs::create_dir_all(&models_dir)
            .map_err(|e| format!("Failed to create models dir: {}", e))?;
    }
    Ok(models_dir)
}
```

**Platform paths** (example for `com.rsac.audio-graph`):
| Platform | Path |
|---|---|
| **macOS** | `~/Library/Application Support/com.rsac.audio-graph/models/` |
| **Linux** | `~/.local/share/com.rsac.audio-graph/models/` |
| **Windows** | `%APPDATA%\com.rsac.audio-graph\models\` |

### Dev-mode compatibility

During `tauri dev`, `app_data_dir()` already resolves correctly — Tauri creates the app data directory for the dev bundle identifier. No special-casing needed.

### Migration for existing files

If users already have models in the old `./models/` directory:
1. `get_models_dir()` checks the new location first.
2. If a model file does not exist in the new location but exists at `./models/<filename>` (CWD-relative), log a warning suggesting the user move files. Do **not** auto-move, since the old path may point to shared storage.

### Callers that need updating

| Caller | Current pattern | Change needed |
|---|---|---|
| [`list_models()`](../src-tauri/src/models/mod.rs:55) | Calls `get_models_dir()` with no args | Pass `AppHandle` |
| [`download_model()`](../src-tauri/src/models/mod.rs:98) | Calls `get_models_dir()` with no args | Pass `AppHandle` |
| [`list_available_models`](../src-tauri/src/commands.rs:532) | Calls `list_models()` | Pass `app_handle` |
| [`download_model_cmd`](../src-tauri/src/commands.rs:538) | Calls `list_models()` + `download_model()` | Pass `app_handle` |
| [`AsrConfig::default()`](../src-tauri/src/asr/mod.rs:34) | Hardcoded `"models/ggml-small.en.bin"` | Takes `models_dir: PathBuf` param or add `AsrConfig::with_models_dir()` |
| [`run_speech_processor()`](../src-tauri/src/speech/mod.rs:156) | Creates `AsrConfig::default()` | Resolve models dir from AppHandle before spawning thread |

---

## 2. Async Download Approach (G3)

### Problem

[`download_model()`](../src-tauri/src/models/mod.rs:98) uses `reqwest::blocking::Client`, which blocks the Tauri command handler thread. Tauri v2 command handlers run on a shared async runtime — blocking them starves other commands.

### Solution: `tokio::task::spawn_blocking` wrapper

Rather than rewriting the entire download loop to async (which complicates progress reporting), keep the blocking download logic but run it off the command handler thread:

```rust
// commands.rs

#[tauri::command]
pub async fn download_model_cmd(
    model_filename: String,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let models = crate::models::list_models(&app_handle)?;
    let model = models
        .iter()
        .find(|m| m.filename == model_filename)
        .ok_or_else(|| format!("Model not found: {}", model_filename))?
        .clone();

    let handle = app_handle.clone();
    let path = tokio::task::spawn_blocking(move || {
        crate::models::download_model(&model.name, &model.url, &model.filename, &handle)
    })
    .await
    .map_err(|e| format!("Download task panicked: {}", e))?
    .map_err(|e| format!("Download failed: {}", e))?;

    Ok(path.to_string_lossy().to_string())
}
```

**Why `spawn_blocking` and not full async reqwest:**
- `reqwest` async is already in `Cargo.toml` (the `blocking` feature is additive). Either approach works.
- `spawn_blocking` is simpler — the existing download loop with progress events can be kept as-is.
- The blocking thread emits progress events to the frontend via `app_handle.emit()`, which is `Send + Sync` and works from any thread.

**Alternative (for a future iteration):** Replace with `reqwest` async streaming (`response.chunk().await`) for true async download. This is cleaner but requires rewriting the progress loop. Not needed for the initial fix.

### Preventing concurrent downloads of the same model

Add an `Arc<Mutex<HashSet<String>>>` to `AppState` (or a dedicated `DownloadState`):

```rust
// state.rs
pub downloads_in_progress: Arc<Mutex<HashSet<String>>>,
```

Check and insert before downloading; remove on completion/error. Return a clear error if already downloading.

---

## 3. New/Modified Tauri Commands

### 3.1 `get_model_status` — new (G1)

Returns the readiness status of all models. Called by the frontend at startup.

```rust
#[derive(Debug, Clone, Serialize)]
pub struct ModelStatus {
    pub whisper: ModelReadiness,
    pub llm: ModelReadiness,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status")]
pub enum ModelReadiness {
    /// File exists and passed verification.
    Ready { path: String },
    /// File exists but verification failed.
    Invalid { path: String, reason: String },
    /// File is currently being downloaded.
    Downloading { percent: f32 },
    /// File not present — needs download.
    NotDownloaded,
}

#[tauri::command]
pub async fn get_model_status(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<ModelStatus, String> {
    let models_dir = crate::models::get_models_dir(&app_handle)?;

    let whisper = check_model_readiness(
        &models_dir,
        WHISPER_MODEL_FILENAME,
        Some(WHISPER_MODEL_SIZE),
        &state.downloads_in_progress,
    );
    let llm = check_model_readiness(
        &models_dir,
        LLM_MODEL_FILENAME,
        Some(LLM_MODEL_EXPECTED_SIZE),
        &state.downloads_in_progress,
    );

    Ok(ModelStatus { whisper, llm })
}
```

**Frontend usage:** Call `get_model_status` on mount. If whisper is `NotDownloaded`, show download prompt. If `Ready`, enable the "Start Capture" button.

### 3.2 `load_llm_model` — new (G2)

Initializes the `LlmEngine` from a downloaded GGUF file. Must run on a background thread because `llama-cpp-2` model loading is blocking and slow (~1–5s).

```rust
#[tauri::command]
pub async fn load_llm_model(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let models_dir = crate::models::get_models_dir(&app_handle)?;
    let model_path = models_dir.join(LLM_MODEL_FILENAME);

    if !model_path.exists() {
        return Err(format!(
            "LLM model not found at {}. Download it first.",
            model_path.display()
        ));
    }

    let path_str = model_path.to_string_lossy().to_string();
    let engine_arc = state.llm_engine.clone();

    tokio::task::spawn_blocking(move || {
        let engine = LlmEngine::new(&path_str)?;
        let mut guard = engine_arc
            .lock()
            .map_err(|e| format!("Lock error: {}", e))?;
        *guard = Some(engine);
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| format!("Load task panicked: {}", e))??;

    log::info!("LLM model loaded successfully");
    Ok(())
}
```

**Frontend flow:**
1. Call `get_model_status` → LLM shows `Ready`.
2. Call `load_llm_model` → engine initialized in `AppState.llm_engine`.
3. Entity extraction and chat now use the native engine.

### 3.3 Modified: `list_available_models` → accepts `AppHandle`

```rust
#[tauri::command]
pub async fn list_available_models(
    app_handle: tauri::AppHandle,
) -> Result<Vec<crate::models::ModelInfo>, String> {
    crate::models::list_models(&app_handle)
}
```

Change from sync `pub fn` to `pub async fn` — returns `Result` now instead of bare `Vec` to propagate path resolution errors.

### 3.4 Modified: `download_model_cmd` → async wrapper

As described in section 2.

### 3.5 Command registration

```rust
// lib.rs — add to generate_handler![]
commands::get_model_status,
commands::load_llm_model,
```

### Summary of command surface changes

| Command | Change | Breaking? |
|---|---|---|
| `list_available_models` | Returns `Result<Vec<ModelInfo>, String>` instead of `Vec<ModelInfo>` | No — Tauri commands already wrap in Result at IPC level |
| `download_model_cmd` | Now `async`, uses `spawn_blocking` internally | No — same IPC signature |
| `get_model_status` | **New** | No |
| `load_llm_model` | **New** | No |

---

## 4. Model Status Reporting (G1)

### Event-based approach for downloads

The existing `model-download-progress` event is kept. The `DownloadProgress` struct already has a `status` field (`"downloading"`, `"complete"`, `"error"`).

### Startup flow

```
Frontend mount
  ├── invoke("get_model_status")
  │     └── Returns { whisper: Ready/NotDownloaded, llm: Ready/NotDownloaded }
  ├── If whisper NotDownloaded → show download UI
  ├── If llm NotDownloaded → show optional download prompt
  └── If llm Ready → optionally invoke("load_llm_model") to pre-warm
```

### Frontend type additions

```typescript
// types/index.ts

type ModelReadiness =
    | { status: "Ready"; path: string }
    | { status: "Invalid"; path: string; reason: string }
    | { status: "Downloading"; percent: number }
    | { status: "NotDownloaded" };

interface ModelStatus {
    whisper: ModelReadiness;
    llm: ModelReadiness;
}
```

### Store additions

```typescript
// store/index.ts

modelStatus: ModelStatus | null;
llmLoaded: boolean;
fetchModelStatus: () => Promise<void>;
loadLlmModel: () => Promise<void>;
```

---

## 5. LLM Initialization Flow (G2)

### Current state

[`AppState.llm_engine`](../src-tauri/src/state.rs:89) is `Arc<Mutex<Option<LlmEngine>>>`, initialized as `None`. There is no code path that sets it to `Some(...)` — the native LLM engine is never loaded. Chat and entity extraction always fall through to the API client or rule-based extractor.

### New flow

```
┌─────────────────────────────────────────────────────────────┐
│                     Frontend                                 │
├─────────────────────────────────────────────────────────────┤
│  1. get_model_status -> llm: Ready                          │
│  2. load_llm_model   -> spawns blocking task                │
│  3. UI shows Loading... spinner                             │
│  4. Command returns Ok -> UI shows LLM Ready checkmark      │
│  5. send_chat_message -> native LLM responds                │
└─────────────────────────────────────────────────────────────┘
         │                           │
         ▼                           ▼
   ┌─ spawn_blocking ──┐    ┌─ command handler ──┐
   │  LlmEngine::new() │    │  engine.chat()     │
   │  model = load GGUF │    │  creates fresh     │
   │  *guard = Some(..) │    │  LlamaContext      │
   └────────────────────┘    └────────────────────┘
```

### Thread safety

- `LlmEngine` contains `LlamaBackend` + `Arc<LlamaModel>` — both `Send + Sync`.
- `LlamaContext` is **not** `Send`, but [`LlmEngine::run_inference()`](../src-tauri/src/llm/engine.rs:197) creates a fresh context per call, so the engine itself can live in `Arc<Mutex<...>>`.
- `load_llm_model` does the heavy I/O on `spawn_blocking`, acquires the mutex only briefly to swap in the engine.
- Speech processor thread holds the mutex lock during `extract_entities()` calls — this is the existing pattern and is acceptable since extraction is fast relative to ASR.

### Unloading

Add a `unload_llm_model` command that sets `*guard = None`. This releases the model memory. Useful for switching models or freeing RAM.

---

## 6. File Verification Approach (G5)

### Problem

Downloaded files are not verified. A partial download (interrupted network, disk full) leaves a corrupt file that causes `whisper-rs` or `llama-cpp-2` to panic or return opaque errors.

### Solution: Size-based verification

After download completes, check the file size against expected values:

```rust
// models/mod.rs

pub fn verify_model(path: &Path, expected_size: Option<u64>) -> Result<(), String> {
    let metadata = fs::metadata(path)
        .map_err(|e| format!("Cannot stat {}: {}", path.display(), e))?;

    if metadata.len() == 0 {
        fs::remove_file(path).ok(); // clean up empty file
        return Err("Downloaded file is empty (0 bytes)".to_string());
    }

    if let Some(expected) = expected_size {
        let actual = metadata.len();
        // Allow 1% tolerance for content-length header inaccuracy
        let tolerance = expected / 100;
        if actual < expected.saturating_sub(tolerance) {
            fs::remove_file(path).ok(); // clean up partial download
            return Err(format!(
                "File size mismatch: expected ~{} bytes, got {} bytes (likely partial download)",
                expected, actual
            ));
        }
    }

    Ok(())
}
```

**Integration point:** Called at the end of `download_model()` before emitting "complete". Also called by `check_model_readiness()` in `get_model_status`.

### Known model sizes

| Model | Filename | Expected size | Source |
|---|---|---|---|
| Whisper small.en | `ggml-small.en.bin` | ~487,654,400 bytes | Already in code as `WHISPER_MODEL_SIZE` |
| LFM2-350M Q4_K_M | `lfm2-350m-extract-q4_k_m.gguf` | ~210,000,000 bytes (estimate) | Measure after first download; store as `LLM_MODEL_EXPECTED_SIZE` |

### Future: SHA256 checksums

A stronger approach would be SHA256 verification. This can be added later by storing checksums alongside URLs in the model registry. Not in scope for the initial pass — size check catches the most common failure mode (partial downloads).

---

## 7. LFM2 Model URL Alignment (G4)

### The mismatch

Three sources reference the LFM2 model with different URLs, repos, quantizations, and filenames:

| Source | Repo | File | Quantization |
|---|---|---|---|
| [Rust `models/mod.rs`](../src-tauri/src/models/mod.rs:17) | `LiquidAI/LFM2-350M-Extract-GGUF` | `lfm2-350m-extract-q4_k_m.gguf` | Q4_K_M |
| [Shell `download-models.sh`](../scripts/download-models.sh:153) | `QuantFactory/LFM2-350M-Extract-GGUF` | `LFM2-350M-Extract.Q8_0.gguf` | Q8_0 |
| [Config `default.toml`](../src-tauri/config/default.toml:26) | N/A | `lfm2-350m-extract.Q8_0.gguf` | Q8_0 (different casing) |

### Decision: Standardize on Q4_K_M from LiquidAI

**Rationale:**
- Q4_K_M is significantly smaller (~210 MB vs ~350 MB for Q8_0), making downloads faster.
- Quality difference is negligible for 350M-parameter models — quantization loss matters more at larger scales.
- `LiquidAI` is the official model author's org on HuggingFace.
- The Rust code already uses Q4_K_M with the correct URL.

### Changes

| File | Change |
|---|---|
| `models/mod.rs` | **No change** — already correct |
| `download-models.sh` | Update `SIDECAR_URL` to `https://huggingface.co/LiquidAI/LFM2-350M-Extract-GGUF/resolve/main/lfm2-350m-extract-q4_k_m.gguf`, update `SIDECAR_FILE` to `lfm2-350m-extract-q4_k_m.gguf` |
| `download-models.ps1` | Same URL/filename update |
| `config/default.toml` | Update `[sidecar] model_path` to `models/lfm2-350m-extract-q4_k_m.gguf` |
| `README.md` | Update any references to the Q8_0 filename |

---

## 8. Migration Plan: Scripts → Rust-Native

### Current state

Model downloading is available through three paths:
1. **Shell scripts** (`download-models.sh` / `.ps1`) — manual, pre-first-run
2. **Rust `download_model_cmd`** — invoked from frontend UI
3. **Manual download** — documented in README

### Target state

The Rust-native path becomes the **primary** mechanism:

```
App launch
  └── Frontend calls get_model_status()
        ├── whisper: NotDownloaded → prompt + download_model_cmd("ggml-small.en.bin")
        ├── whisper: Ready → proceed
        ├── llm: NotDownloaded → optional prompt
        └── llm: Ready → optional load_llm_model()
```

### Script deprecation

The shell scripts remain for:
- CI environments where the app isn't running
- Pre-populating models before first launch
- Users who prefer CLI

Add a deprecation notice to the scripts:
```bash
# NOTE: This script is provided for convenience. The recommended approach
# is to use the in-app model manager which downloads models on first run.
```

Update the scripts to use the **same URLs and filenames** as the Rust code (section 7).

### Implementation order

1. **G6: Fix `get_models_dir()`** — highest impact, prerequisite for everything else
2. **G3: Make download async** — required for non-blocking UX
3. **G1: Add `get_model_status`** — enables startup checks
4. **G5: Add verification** — prevents corrupt model loading
5. **G4: Align URLs** — data consistency fix
6. **G2: Add `load_llm_model`** — enables native LLM from frontend

Each step is independently shippable. G6 + G3 together form a minimal viable improvement.

---

## Appendix A: Files modified per gap

| Gap | Files to modify |
|---|---|
| **G1** `get_model_status` | `models/mod.rs`, `commands.rs`, `lib.rs`, `state.rs` |
| **G2** `load_llm_model` | `commands.rs`, `lib.rs` |
| **G3** Async download | `models/mod.rs`, `commands.rs` |
| **G4** URL alignment | `models/mod.rs`, `scripts/download-models.sh`, `scripts/download-models.ps1`, `config/default.toml` |
| **G5** Verification | `models/mod.rs` |
| **G6** Models dir | `models/mod.rs`, `commands.rs`, `asr/mod.rs`, `speech/mod.rs` |

## Appendix B: Frontend type/store changes

```typescript
// New types
type ModelReadiness = { status: "Ready"; path: string }
    | { status: "Invalid"; path: string; reason: string }
    | { status: "Downloading"; percent: number }
    | { status: "NotDownloaded" };

interface ModelStatus {
    whisper: ModelReadiness;
    llm: ModelReadiness;
}

// New store fields
modelStatus: ModelStatus | null;
llmLoaded: boolean;

// New store actions
fetchModelStatus: () => Promise<void>;  // invoke("get_model_status")
loadLlmModel: () => Promise<void>;      // invoke("load_llm_model")
```
