package ai.codeseys.rsac

import android.content.Context
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.os.Handler
import android.os.HandlerThread
import java.util.concurrent.ConcurrentHashMap

/**
 * Input-device enumeration for the AAudio microphone slice (rsac-ad8a) and
 * input-device change notifications (rsac-d3e2).
 *
 * `AudioManager.getDevices` is a framework API with **no NDK equivalent**, so
 * — exactly like [PackageResolver] — enumeration is a Rust → Java call. This
 * is a regular `fun` (not an `external fun`): Rust invokes it via
 * `CallStaticObjectMethod` (see `src/audio/android/jni.rs`), it is **not**
 * registered through `RegisterNatives`, and it is therefore absent from the
 * JNI symbol-contract table in `README.md`.
 *
 * ### Wire contract (why a single flat delimited string)
 *
 * [inputDevices] returns **one** string, decoded on the Rust side with a
 * single `GetStringUTFChars` and parsed with zero further JNI (fewest
 * round-trips, no per-element local refs, no bespoke class to cache). Each
 * device is a record `id␟typeInt␟name`; records are joined by `␞`:
 *
 * - `␟` = US, U+001F (unit / field separator)
 * - `␞` = RS, U+001E (record separator)
 *
 * Both are C0 control characters defined by ASCII **as** separators; they
 * never occur in a human-readable [AudioDeviceInfo.getProductName] label or a
 * decimal integer, so no escaping is needed and the Rust parser can drop any
 * malformed record without ambiguity. The name is still guarded defensively
 * for embedded separators (stripped) even though they are not expected. Do
 * NOT switch to `|`/`\n`/`\t`: product names are arbitrary `CharSequence`s
 * and can contain those.
 *
 * ### No capture policy (ADR-0012 §4.2)
 *
 * Pure lookup, mirroring [PackageResolver]: any failure returns `""` (empty =
 * "none / could not enumerate"). Rust classifies — a caller that gets `""`
 * falls back to the default-route sentinel + playback device.
 *
 * ### Change notifications (rsac-d3e2)
 *
 * `AudioManager.registerAudioDeviceCallback` is likewise framework-only. The
 * callback does **not** marshal `AudioDeviceInfo[]` across JNI: on every
 * `onAudioDevicesAdded` / `onAudioDevicesRemoved` it calls the single
 * [nativeDevicesChanged] native, and Rust re-enumerates + diffs the input
 * list itself (same source of truth as [inputDevices]). The callback fires
 * on a dedicated `HandlerThread` per registration — never the main looper,
 * never any real-time audio thread. Fidelity is deliberately add/remove
 * only: `AudioDeviceCallback` exposes no default-route-changed or
 * state-changed signal, so Rust never claims those events on Android.
 *
 * ### API levels
 *
 * `AudioManager.getDevices` and [AudioDeviceInfo.getProductName] both exist
 * since API 23 — safe at the module's `minSdk 29` (build.gradle.kts).
 * [AudioDeviceCallback] and `registerAudioDeviceCallback` are also API 23+.
 */
object RsacDevices {

    /**
     * Enumerates the current audio **input** devices as the flat delimited
     * string documented above, or `""` on any failure.
     */
    @JvmStatic
    fun inputDevices(context: Context): String {
        return try {
            val am = context.getSystemService(AudioManager::class.java) ?: return ""
            val devices = am.getDevices(AudioManager.GET_DEVICES_INPUTS)
            buildString {
                for (device in devices) {
                    if (isNotEmpty()) append(RS)
                    append(device.id)
                    append(US)
                    append(device.type)
                    append(US)
                    // Guard against embedded separators (not expected in a
                    // human-readable product name, but arbitrary CharSequences
                    // are possible): strip them so the record grammar holds.
                    append(sanitize(device.productName?.toString().orEmpty()))
                }
            }
        } catch (_: Throwable) {
            ""
        }
    }

    /**
     * Rust → Java: start delivering input-device change notifications for
     * [handle] (rsac-d3e2).
     *
     * Creates a dedicated [HandlerThread] (`rsac-device-callback`) plus an
     * [AudioDeviceCallback] and registers them with the [AudioManager], so
     * [nativeDevicesChanged] fires on our background thread — never the main
     * looper. Regular `fun` (Rust invokes it via `CallStaticBooleanMethod`),
     * NOT `external` — like [inputDevices]. Pure lookup / no capture policy
     * (ADR-0012 §4.2). Returns `true` on success; `false` (no
     * [AudioManager], [handle] already registered, or any framework
     * throwable) on failure — Rust classifies.
     */
    @JvmStatic
    fun registerDeviceCallback(context: Context, handle: Long): Boolean {
        val am = try {
            context.getSystemService(AudioManager::class.java) ?: return false
        } catch (_: Throwable) {
            return false
        }
        if (callbacks.containsKey(handle)) return false
        val thread = HandlerThread("rsac-device-callback").apply { start() }
        return try {
            val handler = Handler(thread.looper)
            val cb = object : AudioDeviceCallback() {
                override fun onAudioDevicesAdded(added: Array<out AudioDeviceInfo>?) =
                    nativeDevicesChanged(handle)
                override fun onAudioDevicesRemoved(removed: Array<out AudioDeviceInfo>?) =
                    nativeDevicesChanged(handle)
            }
            am.registerAudioDeviceCallback(cb, handler)
            callbacks[handle] = Holder(am, cb, thread)
            true
        } catch (_: Throwable) {
            // Reclaim the already-started HandlerThread — Rust only calls
            // unregisterDeviceCallback on the success path, so leaving it
            // running here would leak the thread (review F1, PR wave-7).
            thread.quitSafely()
            false
        }
    }

    /**
     * Rust → Java: stop + tear down the callback for [handle]. Idempotent.
     *
     * Unregisters the callback from the [AudioManager], then `quitSafely()` +
     * bounded-`join()` the [HandlerThread]. This stops new callbacks and
     * usually drains in-flight ones, but the join is BOUNDED so it cannot
     * prove a slow in-flight [nativeDevicesChanged] has returned. The
     * guarantee that the consumer's handler never fires after the watcher is
     * dropped comes from the Rust side: its teardown clears an `active` flag
     * under the same per-watcher lock the callback holds while firing, so a
     * late/in-flight call either finishes before teardown or no-ops
     * (rsac-d3e2).
     */
    @JvmStatic
    fun unregisterDeviceCallback(handle: Long) {
        val holder = callbacks.remove(handle) ?: return
        try {
            holder.am.unregisterAudioDeviceCallback(holder.cb)
        } catch (_: Throwable) {
            // Teardown is best-effort; keep going to the thread join.
        }
        holder.thread.quitSafely()
        try {
            holder.thread.join(JOIN_TIMEOUT_MS)
        } catch (_: InterruptedException) {
            Thread.currentThread().interrupt()
        }
    }

    /**
     * Java → Rust: an audio device was added or removed (rsac-d3e2). Rust
     * re-enumerates the input list and diffs against the watcher's previous
     * id-set. Registered via `RegisterNatives` on this class in `JNI_OnLoad`.
     *
     * LOCKSTEP: name/class/signature pinned by the `jni_lockstep` tests in
     * `src/audio/mod.rs`. Throws [UnsatisfiedLinkError] until `librsac.so`
     * is loaded — never called before [registerDeviceCallback] succeeds,
     * which requires the loaded library's `JNI_OnLoad` to have resolved this
     * class first.
     */
    @JvmStatic
    external fun nativeDevicesChanged(handle: Long)

    /** Everything one registration owns: its manager, callback, and thread. */
    private class Holder(
        val am: AudioManager,
        val cb: AudioDeviceCallback,
        val thread: HandlerThread,
    )

    /** Live registrations, keyed by the Rust watcher registry id. */
    private val callbacks = ConcurrentHashMap<Long, Holder>()

    /** Bounded HandlerThread join (matches CaptureBridge.JOIN_TIMEOUT_MS). */
    private const val JOIN_TIMEOUT_MS = 2_000L

    /** Field separator (unit separator, U+001F). */
    private const val US = ''

    /** Record separator (U+001E). */
    private const val RS = ''

    private fun sanitize(name: String): String {
        if (name.indexOf(US) < 0 && name.indexOf(RS) < 0) return name
        return name.filter { it != US && it != RS }
    }
}
