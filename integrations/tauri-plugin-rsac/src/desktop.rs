// Desktop passthrough delegate (ADR-0014 §4.3). Commands call directly into
// rsac — zero IPC of audio; only derived meter/format events cross the webview
// boundary (§5). `request_consent` is a no-op success because every desktop
// backend reports `requires_user_consent == false` (capabilities.rs:224,253,274).

use serde::de::DeserializeOwned;
use tauri::ipc::Channel;
use tauri::{plugin::PluginApi, AppHandle, Runtime};

use crate::models::*;
use crate::session::{self, Sessions};
use crate::Result;

/// Desktop `Rsac<R>`: holds the app handle + the live-capture session map.
pub struct Rsac<R: Runtime> {
    _app: AppHandle<R>,
    sessions: Sessions,
}

pub fn init<R: Runtime, C: DeserializeOwned>(
    app: &AppHandle<R>,
    _api: PluginApi<R, C>,
) -> Result<Rsac<R>> {
    Ok(Rsac {
        _app: app.clone(),
        sessions: Sessions::default(),
    })
}

impl<R: Runtime> Rsac<R> {
    pub fn request_consent(&self) -> Result<ConsentResult> {
        // Desktop loopback needs no consent artifact — return success so the JS
        // API is uniform across platforms (ADR-0014 §4.3 passthrough).
        Ok(ConsentResult {
            granted: true,
            reason: None,
        })
    }

    pub fn start_capture(
        &self,
        target: String,
        config: CaptureConfig,
    ) -> Result<StartCaptureResult> {
        // Desktop threads no platform consent artifact onto the builder.
        self.sessions.start(&target, config, Ok)
    }

    pub fn stop_capture(&self, capture_id: String) -> Result<()> {
        self.sessions.stop(&capture_id)
    }

    pub fn list_targets(&self) -> Result<Vec<TargetInfo>> {
        session::list_targets()
    }

    pub fn capabilities(&self) -> Result<Capabilities> {
        Ok(session::capabilities())
    }

    pub fn subscribe_meta(&self, capture_id: String, channel: Channel<ChunkMeta>) -> Result<()> {
        self.sessions.subscribe_meta(&capture_id, channel)
    }

    pub fn subscribe_raw(&self, capture_id: String, channel: Channel<ChunkRaw>) -> Result<()> {
        self.sessions.subscribe_raw(&capture_id, channel)
    }
}
