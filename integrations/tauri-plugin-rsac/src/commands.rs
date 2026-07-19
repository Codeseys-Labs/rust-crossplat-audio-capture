// #[tauri::command] entry points (ADR-0014 §4). Each command is thin: it
// delegates to the platform `Rsac<R>` delegate reached via the `RsacExt` trait.
// No capture policy lives here — that stays in rsac (ADR-0012 ownership
// boundary). Namespace is `plugin:rsac|<cmd>`.

use tauri::ipc::Channel;
use tauri::{command, AppHandle, Runtime};

use crate::models::*;
use crate::{Result, RsacExt};

/// Drives the platform consent flow. Android: MediaProjection dialog →
/// token. Desktop: immediate `granted: true` (no consent artifact required).
#[command]
pub(crate) async fn request_consent<R: Runtime>(app: AppHandle<R>) -> Result<ConsentResult> {
    app.rsac().request_consent().await
}

/// Builds + starts a capture for `target` with the given `config`, returning an
/// opaque `captureId` and the negotiated format.
#[command]
pub(crate) async fn start_capture<R: Runtime>(
    app: AppHandle<R>,
    target: String,
    config: Option<CaptureConfig>,
) -> Result<StartCaptureResult> {
    app.rsac().start_capture(target, config.unwrap_or_default())
}

/// Stops (and releases) the capture identified by `captureId`. Idempotent.
#[command]
pub(crate) async fn stop_capture<R: Runtime>(app: AppHandle<R>, capture_id: String) -> Result<()> {
    app.rsac().stop_capture(capture_id)
}

/// Lists capturable audio sources (`rsac::list_audio_sources()`).
#[command]
pub(crate) async fn list_targets<R: Runtime>(app: AppHandle<R>) -> Result<Vec<TargetInfo>> {
    app.rsac().list_targets()
}

/// Returns `PlatformCapabilities::query()` verbatim — the honesty surface.
#[command]
pub(crate) async fn capabilities<R: Runtime>(app: AppHandle<R>) -> Result<Capabilities> {
    app.rsac().capabilities()
}

/// Subscribes to derived per-chunk meter events (`rsac://chunk-meta`) for the
/// given capture, streamed over `channel`. Raw samples never cross this path.
#[command]
pub(crate) async fn subscribe_meta<R: Runtime>(
    app: AppHandle<R>,
    capture_id: String,
    channel: Channel<ChunkMeta>,
) -> Result<()> {
    app.rsac().subscribe_meta(capture_id, channel)
}

/// Subscribes to raw interleaved-f32 chunk events (`rsac://chunk-raw`) — the
/// documented slow path, gated behind the `allow-subscribe-raw` permission
/// (NOT in the default set).
#[command]
pub(crate) async fn subscribe_raw<R: Runtime>(
    app: AppHandle<R>,
    capture_id: String,
    channel: Channel<ChunkRaw>,
) -> Result<()> {
    app.rsac().subscribe_raw(capture_id, channel)
}
