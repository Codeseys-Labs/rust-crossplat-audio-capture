//! # tauri-plugin-rsac
//!
//! A thin Tauri v2 plugin exposing [rsac](https://crates.io/crates/rsac)'s
//! audio-capture API to a webview, plus the Android MediaProjection consent
//! flow (ADR-0014).
//!
//! **Thin adapter, no capture policy.** Consent flow + lifecycle commands +
//! subscription events only — capture policy and backends stay in rsac
//! (ADR-0012 ownership boundary). Event payloads carry **derived data by
//! default** (meters/format/stats via [`models::ChunkMeta`]); raw interleaved
//! samples are opt-in ([`models::ChunkRaw`]) and gated behind the
//! `allow-subscribe-raw` permission — a documented slow path.
//!
//! **Desktop is a passthrough.** Commands call rsac directly (zero IPC of
//! audio); [`commands::request_consent`] returns success because desktop
//! backends report `requires_user_consent == false`. On mobile the plugin
//! bridges to the first-party `mobile/android/` AAR (`RsacProjection`) and
//! `mobile/ios/` glue. Mobile **runtime** verification tracks rsac-e6d3 /
//! rsac-97c8; this crate ships compile-proof.
//!
//! Plugin identifier is `rsac`, so the invoke namespace is `plugin:rsac|<cmd>`
//! and event channels use the `rsac://…` scheme.

use tauri::{
    plugin::{Builder, TauriPlugin},
    Manager, Runtime,
};

pub use error::{Error, Result};

mod commands;
mod error;
pub mod models;
mod session;

#[cfg(desktop)]
mod desktop;
#[cfg(mobile)]
mod mobile;

#[cfg(desktop)]
use desktop::Rsac;
#[cfg(mobile)]
use mobile::Rsac;

/// Extension trait giving [`tauri::App`], [`tauri::AppHandle`], and the window
/// managers access to the rsac plugin delegate.
pub trait RsacExt<R: Runtime> {
    fn rsac(&self) -> &Rsac<R>;
}

impl<R: Runtime, T: Manager<R>> RsacExt<R> for T {
    fn rsac(&self) -> &Rsac<R> {
        self.state::<Rsac<R>>().inner()
    }
}

/// Initializes the plugin. Register it on the Tauri builder:
///
/// ```no_run
/// # // Runtime-generic so the example compiles without pulling a concrete
/// # // runtime (this crate's `tauri` dep is `default-features = false`, so the
/// # // default `Wry` runtime is intentionally absent — see Cargo.toml).
/// use tauri::Runtime;
/// fn register<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
///     builder.plugin(tauri_plugin_rsac::init())
/// }
/// ```
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("rsac")
        .invoke_handler(tauri::generate_handler![
            commands::request_consent,
            commands::start_capture,
            commands::stop_capture,
            commands::list_targets,
            commands::capabilities,
            commands::subscribe_meta,
            commands::subscribe_raw,
        ])
        .setup(|app, api| {
            #[cfg(mobile)]
            let rsac = mobile::init(app, api)?;
            #[cfg(desktop)]
            let rsac = desktop::init(app, api)?;
            app.manage(rsac);
            Ok(())
        })
        .build()
}
