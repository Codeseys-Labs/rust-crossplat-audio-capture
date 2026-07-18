// Mobile delegate (ADR-0014 §4, §8). `Rsac<R>` wraps the `PluginHandle` from
// `register_android_plugin` for the consent flow AND the shared in-process
// `Sessions` for the capture lifecycle (rsac captures run in-process even on
// mobile — the AAR pushes samples into rsac via JNI, rsac-77f1).
//
// CONSENT ORDERING (mandatory, PR#64): `request_consent` forwards to the Kotlin
// `RsacTauriPlugin.requestConsent`, which is a thin forwarder onto
// `RsacProjection.request` — it MUST NOT start the FGS itself (deferred-acquire
// trap; .claude/skills/rsac-android-mediaprojection-fgs-ordering). The returned
// `Long` token is wrapped ONCE via `AndroidProjectionToken::from_raw` and
// threaded onto the builder at `start_capture` (§8 steps 5-6).

use serde::de::DeserializeOwned;
use tauri::ipc::Channel;
use tauri::{
    plugin::{PluginApi, PluginHandle},
    AppHandle, Runtime,
};

use crate::models::*;
use crate::session::{self, Sessions};
use crate::{Error, Result};

#[cfg(target_os = "android")]
const PLUGIN_IDENTIFIER: &str = "ai.codeseys.rsac.tauri";

// NOTE (iOS stub): `PluginApi::register_ios_plugin` is feature-gated behind
// tauri's `wry` feature (plugin/mobile.rs: `#[cfg(all(target_os = "ios",
// feature = "wry"))]`), which this crate deliberately drops
// (default-features = false — no webview runtime in a library plugin). The
// iOS half is a stub anyway (§8: consent is an Android-only concept; the iOS
// broadcast path needs no dialog), so no native plugin is registered on iOS
// and `request_consent` fails honestly instead. Revisit when the iOS half
// grows real native commands (tracked with the runtime seeds).

/// Response from the Kotlin `requestConsent` command. The `token` is the opaque
/// `MediaProjection` `GlobalRef` handle minted by `RsacProjection` (jlong),
/// present only when `granted == true`.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConsentResponse {
    granted: bool,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    token: Option<i64>,
}

pub fn init<R: Runtime, C: DeserializeOwned>(
    _app: &AppHandle<R>,
    #[cfg_attr(target_os = "ios", allow(unused_variables))] api: PluginApi<R, C>,
) -> Result<Rsac<R>> {
    #[cfg(target_os = "android")]
    let handle = api.register_android_plugin(PLUGIN_IDENTIFIER, "RsacTauriPlugin")?;
    Ok(Rsac {
        #[cfg(target_os = "android")]
        handle,
        #[cfg(target_os = "ios")]
        _runtime: std::marker::PhantomData,
        sessions: Sessions::default(),
        // No consent token until request_consent succeeds on Android.
        #[cfg(target_os = "android")]
        projection: std::sync::Mutex::new(None),
    })
}

/// Mobile `Rsac<R>`: consent via the native plugin (Android), capture via
/// shared sessions. On iOS no native plugin is registered (stub — see the
/// module note above).
pub struct Rsac<R: Runtime> {
    #[cfg(target_os = "android")]
    handle: PluginHandle<R>,
    #[cfg(target_os = "ios")]
    _runtime: std::marker::PhantomData<fn() -> R>,
    sessions: Sessions,
    /// The single live Android projection token (wrapped once from the raw
    /// jlong; cloned onto each builder). `None` until consent is granted.
    #[cfg(target_os = "android")]
    projection: std::sync::Mutex<Option<rsac::AndroidProjectionToken>>,
}

impl<R: Runtime> Rsac<R> {
    #[cfg(target_os = "ios")]
    pub fn request_consent(&self) -> Result<ConsentResult> {
        // Consent is an Android-only concept (MediaProjection); the iOS
        // broadcast path needs no dialog and no native plugin is registered
        // (stub). Honest denial, never a panic.
        Ok(ConsentResult {
            granted: false,
            reason: Some(
                "consent is not applicable on iOS (MediaProjection is Android-only); \
                 system capture uses the broadcast extension path"
                    .into(),
            ),
        })
    }

    #[cfg(target_os = "android")]
    pub fn request_consent(&self) -> Result<ConsentResult> {
        // Empty payload — the Kotlin side drives the dialog off the activity.
        let resp: ConsentResponse = self
            .handle
            .run_mobile_plugin("requestConsent", ())
            .map_err(Error::from)?;

        #[cfg(target_os = "android")]
        if resp.granted {
            if let Some(raw) = resp.token {
                // SAFETY: `raw` is a live JNI GlobalRef minted by
                // RsacProjection.request on the consent-success path, delivered
                // exactly once per grant. We wrap it ONCE here (from_raw's
                // single-owner contract, config.rs:147-157) and clone onto each
                // builder.
                //
                // LEAK NOTE: `AndroidProjectionToken` has no `Drop` — its
                // GlobalRef is released (and the MediaProjection stopped) only
                // when a capture stream `try_consume`s the token and its
                // teardown runs `stop_and_release_projection`. If consent is
                // granted twice WITHOUT starting a capture in between, replacing
                // the stored token here drops the prior wrapper but NOT its
                // GlobalRef, leaking that ref (and leaving the earlier grant's
                // FGS running until the host stops it). rsac exposes no
                // release-without-consume API to this crate, so the leak is
                // bounded by user actions and by the host's FGS lifecycle rather
                // than reclaimed here (tracked as a seed).
                let token = unsafe { rsac::AndroidProjectionToken::from_raw(raw) };
                *self.projection.lock().expect("projection mutex poisoned") = Some(token);
            }
        }

        Ok(ConsentResult {
            granted: resp.granted,
            reason: resp.reason,
        })
    }

    pub fn start_capture(
        &self,
        target: String,
        config: CaptureConfig,
    ) -> Result<StartCaptureResult> {
        // Thread the Android consent token onto the builder (§8 step 5). On iOS
        // and when no token was obtained, the builder is passed through
        // unchanged — rsac's preflight surfaces UserConsentRequired if the
        // target needs an artifact it lacks (honest failure, not a panic).
        #[cfg(target_os = "android")]
        let configure = |builder: rsac::AudioCaptureBuilder| {
            let guard = self.projection.lock().expect("projection mutex poisoned");
            match guard.as_ref() {
                Some(token) => Ok(builder.with_android_projection(token.clone())),
                None => Ok(builder),
            }
        };
        #[cfg(not(target_os = "android"))]
        let configure = Ok;

        self.sessions.start(&target, config, configure)
    }

    pub fn stop_capture(&self, capture_id: String) -> Result<()> {
        // Dropping the capture releases the token/GlobalRef; the host is
        // responsible for RsacCaptureService.stop() afterward (README step 5,
        // §8 step 6) — driven from the Kotlin side, not from Rust here.
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
