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
 * 2. On user approval, the resulting [MediaProjection] is handed to the
 *    native side ([nativeRetainProjection]), which wraps it in a JNI
 *    `GlobalRef` and returns an **opaque token** (`jlong`, pointer-sized
 *    across FFI).
 * 3. The token crosses into Rust and is given to
 *    `AudioCaptureBuilder::with_android_projection(AndroidProjectionToken)`.
 * 4. Token lifetime is owned by Rust: released (`DeleteGlobalRef` +
 *    `MediaProjection.stop()`) when the owning capture is dropped.
 *    **One token = one projection session** — do not reuse a token across
 *    captures.
 *
 * There is deliberately no process-global token registry (no hidden state);
 * the token is returned to the caller and nowhere else.
 *
 * ### Ordering on API 34+ (Android 14)
 *
 * Apps targeting SDK 34+ must have a `mediaProjection`-typed foreground
 * service running before media-projection capture may start — start
 * [RsacCaptureService] (or the host's own equivalent) before/around the
 * consent flow. See README.md § Lifecycle ordering.
 * // CI-VERIFY: whether MediaProjectionManager.getMediaProjection() itself
 * // throws SecurityException on API 34+ without the running FGS, or whether
 * // enforcement only triggers at capture start — adjust the KDoc/README
 * // ordering guidance to match observed behavior on an API 34+ emulator.
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

            val projection: MediaProjection? = try {
                manager.getMediaProjection(result.resultCode, data)
            } catch (e: SecurityException) {
                // API 34+: thrown when no mediaProjection foreground service
                // is running, or the consent data was already consumed.
                callback.onDenied(
                    "getMediaProjection failed: ${e.message} (on API 34+ a " +
                        "mediaProjection foreground service must be running " +
                        "first — see README.md § Lifecycle ordering)"
                )
                return@register
            }
            if (projection == null) {
                callback.onDenied("MediaProjectionManager returned no projection")
                return@register
            }

            // Hand ownership to Rust: GlobalRef + opaque token. From here,
            // release (DeleteGlobalRef + MediaProjection.stop()) is Rust's
            // job, tied to the owning capture's Drop.
            callback.onToken(nativeRetainProjection(projection))
        }
        launcher.launch(manager.createScreenCaptureIntent())
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
