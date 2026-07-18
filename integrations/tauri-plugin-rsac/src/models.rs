// serde payload/response types for the JS-facing plugin API (ADR-0014 §4/§5).
//
// All types are `camelCase` on the wire (the JS/TS convention) and mirror the
// proven napi `ChunkMeta` shape (bindings/rsac-napi/src/lib.rs:50-94) for the
// derived-data event path. Nothing here carries capture policy — these are the
// pure data-transfer objects between the webview and the platform `Rsac<R>`
// delegate.

use serde::{Deserialize, Serialize};

// ── Command payloads (webview → plugin) ──────────────────────────────────

/// `start_capture` config args. All fields optional so a JS caller can send
/// only what it wants to override; unset fields fall back to rsac's
/// `StreamConfig::default()` (48 kHz / 2ch / F32 / backend-default buffer).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureConfig {
    /// Desired sample rate in Hz (e.g. 44100, 48000).
    pub sample_rate: Option<u32>,
    /// Desired channel count.
    pub channels: Option<u16>,
    /// Desired sample format: one of `"i16"`, `"i24"`, `"i32"`, `"f32"`
    /// (case-insensitive). Unknown values are rejected by `start_capture`.
    pub sample_format: Option<String>,
    /// Ring-buffer depth in **slots** (not frames); honored on Windows today
    /// (see `AudioCaptureBuilder::buffer_size`). `None` → backend default.
    pub buffer_size: Option<usize>,
}

// ── Command responses (plugin → webview) ─────────────────────────────────

/// Result of `request_consent`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsentResult {
    /// Whether consent is available for capture. `true` on desktop (no consent
    /// artifact required) and on Android once the MediaProjection dialog is
    /// approved.
    pub granted: bool,
    /// Human-readable reason when `granted == false` (e.g. the Android denial
    /// string). `None` on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The negotiated stream format returned alongside a started capture.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormatInfo {
    pub sample_rate: u32,
    pub channels: u16,
    /// Sample format as a lowercase string (`"f32"`, `"i16"`, …).
    pub sample_format: String,
}

/// Result of `start_capture`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartCaptureResult {
    /// Opaque handle for subsequent `stop_capture` / `subscribe_*` calls.
    pub capture_id: String,
    /// The format the backend negotiated (may differ from the requested one).
    pub format: FormatInfo,
}

/// One capturable audio source (`list_targets`). Maps `rsac::AudioSource`
/// (introspection.rs:30) to a flat JS object.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetInfo {
    /// Canonical id usable as a `start_capture` target string
    /// (e.g. `"system-default"`, `"device:hw:0,0"`, `"app:1234"`).
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Source classification: `"systemDefault"`, `"device"`, or `"application"`.
    pub kind: String,
}

/// Platform capabilities surfaced verbatim from `PlatformCapabilities::query()`
/// (capabilities.rs:160). This is the honesty surface a JS UI must consult
/// before offering system/app capture — every field mirrors the Rust struct
/// exactly; the plugin never claims a feature `query()` reports `false`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub supports_system_capture: bool,
    pub supports_application_capture: bool,
    pub supports_process_tree_capture: bool,
    pub supports_device_selection: bool,
    pub supports_device_change_notifications: bool,
    pub requires_user_consent: bool,
    /// Supported sample formats as lowercase strings.
    pub supported_sample_formats: Vec<String>,
    /// Inclusive `(min, max)` sample-rate range in Hz.
    pub sample_rate_range: (u32, u32),
    pub max_channels: u16,
    pub backend_name: String,
}

/// Derived per-chunk meter event payload — the DEFAULT event
/// (`rsac://chunk-meta`, ADR-0014 §4.2). Computed Rust-side from `AudioBuffer`
/// before the buffer is dropped; raw samples never cross IPC on this path.
/// Mirrors the napi `ChunkMeta` shape (bindings/rsac-napi/src/lib.rs:50-94).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkMeta {
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: usize,
    pub duration_secs: f64,
    pub rms: f32,
    pub peak: f32,
    pub rms_dbfs: f32,
    pub peak_dbfs: f32,
    pub channel_rms: Vec<f32>,
    pub channel_peak: Vec<f32>,
    pub format: FormatInfo,
}

/// Raw interleaved-f32 chunk event payload (`rsac://chunk-raw`) — the OPT-IN
/// slow path (ADR-0014 §4.2, gated behind `allow-subscribe-raw`). Present so a
/// no-Rust JS app can access samples, at a documented cost.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkRaw {
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: usize,
    /// Interleaved f32 PCM samples (`frames * channels` values).
    pub samples: Vec<f32>,
}

/// Periodic stream-stats snapshot. **Reserved, not yet emitted:** the
/// `rsac://stats` channel is defined in ADR-0014 §6.3 but no code path emits
/// this payload today. The type is kept as the frozen wire shape a future
/// stats pump will send (mirrors `rsac::StreamStats`, introspection.rs:464) so
/// the JS side can be typed against it ahead of the emitter landing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamStatsInfo {
    pub overruns: u64,
    pub buffers_captured: u64,
    pub buffers_dropped: u64,
    pub buffers_pushed: u64,
    pub uptime_secs: f64,
    pub is_running: bool,
    pub format_description: String,
}
