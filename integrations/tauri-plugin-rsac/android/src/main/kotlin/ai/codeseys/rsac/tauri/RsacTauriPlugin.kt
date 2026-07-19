// Copyright 2026 Codeseys Labs
// SPDX-License-Identifier: MIT OR Apache-2.0

package ai.codeseys.rsac.tauri

import android.app.Activity
import androidx.activity.ComponentActivity
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import ai.codeseys.rsac.RsacProjection

/**
 * Tauri v2 Android plugin for rsac (ADR-0014, §8).
 *
 * This class carries **no capture policy**. It is a thin forwarder onto the
 * first-party `ai.codeseys.rsac.RsacProjection` consent flow (mobile/android
 * AAR, ADR-0012) — target resolution, error classification, and stream
 * semantics all live in Rust (src/audio/android/). The plugin bridges the
 * MediaProjection consent dialog to a Tauri `@Command` and returns the opaque
 * projection token to Rust.
 *
 * ## Consent ordering (MANDATORY — PR#64 deferred-FGS-acquire)
 *
 * `requestConsent` MUST forward to [RsacProjection.request] and MUST NOT start
 * the `mediaProjection` foreground service itself. `RsacProjection.request`
 * launches the consent dialog, stashes a single-slot `PendingAcquisition`, and
 * starts `RsacCaptureService` on the **consent-success path**; the projection
 * is acquired **inside the service**, after `startForeground()` returns, via
 * `onForegroundServiceReady`. Calling `RsacCaptureService.start()` here (before
 * consent) would throw `SecurityException` on API 34+ (typed-FGS-before-consent
 * trap). See `.claude/skills/rsac-android-mediaprojection-fgs-ordering` and
 * `mobile/android/README.md` step 3.
 *
 * A concurrent second consent request is rejected by `RsacProjection` itself
 * (single-slot `pending`), surfaced here as `onDenied`.
 */
@TauriPlugin
class RsacTauriPlugin(private val activity: Activity) : Plugin(activity) {

    /**
     * Drives the MediaProjection consent dialog and resolves with
     * `{ granted, reason?, token? }`.
     *
     * - `granted == true`  → `token` is the opaque `MediaProjection` handle
     *   (a `jlong` `GlobalRef`) that Rust wraps via
     *   `AndroidProjectionToken::from_raw` and threads into
     *   `AudioCaptureBuilder::with_android_projection` at `start_capture`.
     * - `granted == false` → `reason` explains the denial.
     *
     * The [Invoke] is resolved exactly once, on the main thread, from
     * `RsacProjection.Callback`.
     */
    @Command
    fun requestConsent(invoke: Invoke) {
        val componentActivity = activity as? ComponentActivity
        if (componentActivity == null) {
            val result = JSObject()
            result.put("granted", false)
            result.put(
                "reason",
                "host activity is not a ComponentActivity; the MediaProjection " +
                    "consent flow requires the ActivityResult API"
            )
            invoke.resolve(result)
            return
        }

        // Forward onto the first-party consent flow. RsacProjection owns the
        // dialog, the single-slot pending state, and the deferred FGS-acquire
        // ordering — we only marshal its callback back to the Tauri Invoke.
        RsacProjection.request(
            componentActivity,
            object : RsacProjection.Callback {
                override fun onToken(token: Long) {
                    val result = JSObject()
                    result.put("granted", true)
                    result.put("token", token)
                    invoke.resolve(result)
                }

                override fun onDenied(reason: String) {
                    val result = JSObject()
                    result.put("granted", false)
                    result.put("reason", reason)
                    invoke.resolve(result)
                }
            }
        )
    }
}
