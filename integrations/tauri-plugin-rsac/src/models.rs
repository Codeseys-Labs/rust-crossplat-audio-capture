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
    /// iOS only: the App Group identifier shared with the embedded
    /// RsacBroadcastKit extension (`"group.…"`), required by `SystemDefault`
    /// captures (ADR-0013 — threaded to
    /// `AudioCaptureBuilder::with_ios_app_group`). Ignored on every other
    /// platform. `None` on an iOS system capture yields the honest
    /// `UserConsentRequired` preflight error.
    pub ios_app_group: Option<String>,
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
    /// Negotiated sample rate in Hz.
    pub sample_rate: u32,
    /// Negotiated channel count.
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
    /// (e.g. `"system"`, `"device:hw:0,0"`, `"app:1234"`) — every id this
    /// command returns round-trips through the target parser.
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
    /// System-mix (loopback) capture is available.
    pub supports_system_capture: bool,
    /// Per-application capture is available.
    pub supports_application_capture: bool,
    /// Process-tree capture is available.
    pub supports_process_tree_capture: bool,
    /// Non-default input devices can be selected.
    pub supports_device_selection: bool,
    /// Device hot-plug notifications are available.
    pub supports_device_change_notifications: bool,
    /// A consent artifact (MediaProjection token / App Group) is required
    /// for the capture tiers that claim support.
    pub requires_user_consent: bool,
    /// Supported sample formats as lowercase strings.
    pub supported_sample_formats: Vec<String>,
    /// Inclusive `(min, max)` sample-rate range in Hz.
    pub sample_rate_range: (u32, u32),
    /// Maximum channel count the backend accepts.
    pub max_channels: u16,
    /// Backend identifier (e.g. `"CoreAudio"`, `"WASAPI"`, `"PipeWire"`).
    pub backend_name: String,
}

/// Derived per-chunk meter event payload — the DEFAULT event
/// (`rsac://chunk-meta`, ADR-0014 §4.2). Computed Rust-side from `AudioBuffer`
/// before the buffer is dropped; raw samples never cross IPC on this path.
/// Mirrors the napi `ChunkMeta` shape (bindings/rsac-napi/src/lib.rs:50-94).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkMeta {
    /// Chunk sample rate in Hz.
    pub sample_rate: u32,
    /// Chunk channel count.
    pub channels: u16,
    /// Frames in this chunk (samples per channel).
    pub frames: usize,
    /// Chunk duration in seconds.
    pub duration_secs: f64,
    /// RMS level across all samples, linear `0.0..=1.0`.
    pub rms: f32,
    /// Peak (max absolute) level, linear `0.0..=1.0`.
    pub peak: f32,
    /// RMS in dBFS (`-inf` at silence).
    pub rms_dbfs: f32,
    /// Peak in dBFS (`-inf` at silence).
    pub peak_dbfs: f32,
    /// Per-channel RMS levels in channel order.
    pub channel_rms: Vec<f32>,
    /// Per-channel peak levels in channel order.
    pub channel_peak: Vec<f32>,
    /// The negotiated stream format.
    pub format: FormatInfo,
}

/// Raw interleaved-f32 chunk event payload (`rsac://chunk-raw`) — the OPT-IN
/// slow path (ADR-0014 §4.2, gated behind `allow-subscribe-raw`). Present so a
/// no-Rust JS app can access samples, at a documented cost.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChunkRaw {
    /// Chunk sample rate in Hz.
    pub sample_rate: u32,
    /// Chunk channel count.
    pub channels: u16,
    /// Frames in this chunk (samples per channel).
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
    /// RT-callback buffers dropped because the bridge ring was full.
    pub overruns: u64,
    /// Buffers captured from the OS since start.
    pub buffers_captured: u64,
    /// Buffers dropped before reaching the consumer.
    pub buffers_dropped: u64,
    /// Buffers pushed into the bridge ring.
    pub buffers_pushed: u64,
    /// Seconds since the capture started.
    pub uptime_secs: f64,
    /// Whether the stream is currently running.
    pub is_running: bool,
    /// Human-readable negotiated format (e.g. `"48000 Hz, 2 ch, f32"`).
    pub format_description: String,
}
