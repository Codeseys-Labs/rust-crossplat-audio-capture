package ai.codeseys.rsac

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import androidx.activity.ComponentActivity
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import java.util.UUID
import java.util.concurrent.atomic.AtomicBoolean

/**
 * MediaProjection consent flow — the "explicit builder token" half of the
 * consent-token design (docs/MOBILE_BACKEND_DESIGN.md § consent-token flow,
 * ADR-0012/ADR-0013).
 *
 * Flow:
 *
 * 1. The host app calls [request] with a [ComponentActivity]. rsac launches
 *    the system consent dialog via the ActivityResult API.
 * 2. On user approval, rsac stashes the consent result and starts
 *    [RsacCaptureService]; once that service is confirmed-foreground it calls
 *    back into [onForegroundServiceReady], which acquires the
 *    [MediaProjection] and hands it to the native side
 *    ([nativeRetainProjection]) — a JNI `GlobalRef` wrapped as an **opaque
 *    token** (`jlong`, pointer-sized across FFI) delivered via [Callback.onToken].
 * 3. The token crosses into Rust and is given to
 *    `AudioCaptureBuilder::with_android_projection(AndroidProjectionToken)`.
 * 4. Token lifetime is owned by Rust: released (`DeleteGlobalRef` +
 *    `MediaProjection.stop()`) when the owning capture is dropped.
 *    **One token = one projection session** — do not reuse a token across
 *    captures.
 *
 * There is deliberately no process-global token *registry* — the token is
 * returned to the caller and nowhere else. The one piece of transient state
 * is a single-slot `pending` consent acquisition bridging [request] to the
 * service callback (a concurrent second [request] is rejected, not queued).
 *
 * ### Ordering on API 34+ (Android 14) — and why acquisition is deferred
 *
 * Two platform constraints collide (API Q+, still enforced on 14):
 * - [MediaProjectionManager.getMediaProjection] internally calls
 *   `IMediaProjection.start()`, which throws `SecurityException` unless a
 *   `mediaProjection`-typed foreground service is **already
 *   confirmed-foreground**.
 * - That FGS may be started only **after** consent is granted — starting it
 *   earlier throws `SecurityException`.
 *
 * So [request] cannot acquire the projection inline: it stashes the consent
 * result, starts [RsacCaptureService], and the service — after its
 * synchronous `startForeground()` returns (type registered) — drives
 * [onForegroundServiceReady] to acquire the projection and deliver the token.
 * **Hosts must NOT call [RsacCaptureService.start] before [request]** — a
 * pre-consent typed-FGS start throws `SecurityException` (now caught and
 * surfaced as [Callback.onDenied] rather than crashing). The host only stops
 * the service ([RsacCaptureService.stop]) after dropping the capture.
 *
 * ### Native availability
 *
 * The native symbols ship with the Rust JNI layer (rsac-77f1), packaged as
 * `librsac.so` in the AAR's jniLibs (rsac-0aa9). In a build without the
 * native library (e.g. a stripped-down repackaging), calling
 * [nativeRetainProjection] throws [UnsatisfiedLinkError]. Guard with
 * [isNativeAvailable]; [request] fails fast with [IllegalStateException]
 * when the native library is absent.
 *
 * No capture policy lives here (ADR-0012 §4.2): this object launches the
 * consent dialog and forwards the projection to Rust — nothing more.
 */
object RsacProjection {

    /**
     * Name of the Rust cdylib, as passed to [System.loadLibrary]
     * (`librsac.so` on disk).
     */
    // CI-VERIFY: must match the cdylib artifact name produced by cargo-ndk
    // for the rsac crate (rsac-77f1 / rsac-1a6e); rename here + README table
    // if the crate ships a differently-named mobile cdylib.
    const val NATIVE_LIBRARY_NAME: String = "rsac"

    /** Result callback for [request]. Invoked on the main thread. */
    interface Callback {
        /** Consent granted; [token] is the opaque projection token for Rust. */
        fun onToken(token: Long)

        /** Consent denied, cancelled, or the projection could not be created. */
        fun onDenied(reason: String)
    }

    @Volatile
    private var nativeLoadState: Boolean? = null

    /**
     * Returns `true` when `librsac.so` is present and loaded — i.e. the JNI
     * symbols registered from Rust's `JNI_OnLoad` (rsac-77f1) are available.
     *
     * Loading is attempted at most once and the outcome cached; safe to call
     * from any thread.
     */
    fun isNativeAvailable(): Boolean {
        nativeLoadState?.let { return it }
        synchronized(this) {
            nativeLoadState?.let { return it }
            val loaded = try {
                System.loadLibrary(NATIVE_LIBRARY_NAME)
                true
            } catch (_: UnsatisfiedLinkError) {
                false
            }
            nativeLoadState = loaded
            return loaded
        }
    }

    /**
     * Launches the MediaProjection consent dialog and, on approval, converts
     * the resulting [MediaProjection] into an opaque native token.
     *
     * Must be called from the main thread with a started [activity]. The
     * [callback] fires on the main thread exactly once.
     *
     * @throws IllegalStateException if the rsac native library is not loaded
     *   (see [isNativeAvailable]) — the token could not be retained anyway.
     */
    @JvmStatic
    fun request(activity: ComponentActivity, callback: Callback) {
        check(isNativeAvailable()) {
            "librsac.so is not available: the JNI layer (rsac-77f1) is not " +
                "packaged in this build, so a MediaProjection token cannot " +
                "be retained. See mobile/android/README.md § Native library."
        }

        val manager = activity.getSystemService(Context.MEDIA_PROJECTION_SERVICE)
            as MediaProjectionManager

        // ActivityResultRegistry.register() is legal after onCreate; the
        // trade-off (documented): if the consent dialog outlives the activity
        // (process death / config change), this one-shot registration is not
        // re-delivered — the host observes onDenied via a fresh request().
        val delivered = AtomicBoolean(false)
        var launcher: ActivityResultLauncher<Intent>? = null
        launcher = activity.activityResultRegistry.register(
            "rsac-projection-" + UUID.randomUUID(),
            ActivityResultContracts.StartActivityForResult(),
        ) { result ->
            launcher?.unregister()
            if (!delivered.compareAndSet(false, true)) return@register

            val data = result.data
            if (result.resultCode != Activity.RESULT_OK || data == null) {
                callback.onDenied("user declined the media-projection consent dialog")
                return@register
            }

            // getMediaProjection() internally calls IMediaProjection.start(),
            // which the platform (API Q+, enforced through 14) rejects unless
            // a mediaProjection-typed foreground service is ALREADY
            // confirmed-foreground — yet that FGS may only be started AFTER
            // consent exists (starting it earlier throws SecurityException).
            // Both constraints are met by acquiring the projection INSIDE the
            // service, right after startForeground() returns (a synchronous
            // AMS binder call, so the FGS type is registered on return).
            // Stash the consent result, start the service, and let
            // RsacCaptureService.onStartCommand drive onForegroundServiceReady
            // to finish the handoff. Same-main-thread ordering guarantees
            // `pending` is set before onStartCommand runs (rsac-cabf).
            //
            // `pending` is a single slot: reject a second concurrent request()
            // rather than overwrite (which would orphan the first callback —
            // it would never fire onToken/onDenied). Callbacks run on the main
            // thread, so this check-then-set is race-free.
            if (pending != null) {
                callback.onDenied(
                    "another media-projection consent request is already in " +
                        "progress; retry after it completes"
                )
                return@register
            }
            pending = PendingAcquisition(result.resultCode, data, callback)
            try {
                RsacCaptureService.start(activity)
            } catch (e: Exception) {
                pending = null
                callback.onDenied(
                    "failed to start the mediaProjection foreground service " +
                        "after consent: ${e.message}"
                )
            }
        }
        launcher.launch(manager.createScreenCaptureIntent())
    }

    /** A consent result awaiting the foreground service to reach the foreground. */
    private class PendingAcquisition(
        val resultCode: Int,
        val data: Intent,
        val callback: Callback,
    )

    @Volatile
    private var pending: PendingAcquisition? = null

    /**
     * Called by [RsacCaptureService.onStartCommand] AFTER `startForeground()`
     * has put the `mediaProjection` FGS in the foreground — the earliest legal
     * moment to call [MediaProjectionManager.getMediaProjection] on API Q+,
     * which internally requires a running mediaProjection FGS (rsac-cabf).
     * Runs on the main thread (onStartCommand); a no-op when no consent
     * acquisition is pending. [context] is the service, used to obtain the
     * [MediaProjectionManager]. Delivers the token (or the denial) to the
     * stashed callback exactly once.
     *
     * // CI-VERIFY (rsac-e6d3): confirm on an API 34+ emulator that
     * // getMediaProjection succeeds here (FGS confirmed-foreground on
     * // startForeground's synchronous return) — the SecurityException arm
     * // should not fire in the happy path.
     */
    internal fun onForegroundServiceReady(context: Context) {
        val p = pending ?: return
        pending = null
        val manager = context.getSystemService(Context.MEDIA_PROJECTION_SERVICE)
            as MediaProjectionManager
        val projection: MediaProjection? = try {
            manager.getMediaProjection(p.resultCode, p.data)
        } catch (e: SecurityException) {
            // No projection was acquired, so nothing will ever stop the FGS we
            // just started — stop it here, else it leaks (rsac-cabf review).
            RsacCaptureService.stop(context)
            p.callback.onDenied(
                "getMediaProjection failed after the FGS reached the " +
                    "foreground: ${e.message}"
            )
            return
        }
        if (projection == null) {
            RsacCaptureService.stop(context)
            p.callback.onDenied("MediaProjectionManager returned no projection")
            return
        }
        // Hand ownership to Rust: GlobalRef + opaque token. From here, release
        // (DeleteGlobalRef + MediaProjection.stop()) is Rust's job, tied to
        // the owning capture's Drop. nativeRetainProjection returns 0 when the
        // GlobalRef could not be created — that is a failure, not a token, so
        // stop the FGS and deny rather than handing a 0 to onToken (a 0 token
        // would otherwise fail stream creation much later with a vaguer error).
        val token = nativeRetainProjection(projection)
        if (token == 0L) {
            projection.stop()
            RsacCaptureService.stop(context)
            p.callback.onDenied(
                "failed to retain the MediaProjection natively (librsac " +
                    "returned a null token)"
            )
            return
        }
        p.callback.onToken(token)
    }

    /**
     * Called by [RsacCaptureService] when it could not reach the foreground
     * (e.g. `startForeground` threw), so a pending consent acquisition is not
     * left hanging. No-op when nothing is pending. Runs on the main thread.
     */
    internal fun onForegroundServiceFailed(reason: String) {
        val p = pending ?: return
        pending = null
        p.callback.onDenied(reason)
    }

    /**
     * Wraps [projection] in a JNI `GlobalRef` and returns the opaque token
     * consumed by `AudioCaptureBuilder::with_android_projection`.
     *
     * Registered from Rust via `RegisterNatives` (`JNI_OnLoad`,
     * src/audio/android/jni.rs — rsac-77f1). **Lockstep contract**: renaming
     * this method, its class, or its signature breaks the Rust registration
     * — guarded by the host-run `jni_lockstep` tests in src/audio/mod.rs.
     *
     * Returns `0` when the projection could not be retained (a `0` token
     * fails stream creation with an actionable error). Throws
     * [UnsatisfiedLinkError] when the native library is absent — guard with
     * [isNativeAvailable].
     */
    @JvmStatic
    external fun nativeRetainProjection(projection: MediaProjection): Long
}
