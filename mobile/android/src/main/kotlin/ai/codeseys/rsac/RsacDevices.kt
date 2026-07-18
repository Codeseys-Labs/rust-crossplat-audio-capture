package ai.codeseys.rsac

import android.content.Context
import android.media.AudioDeviceInfo
import android.media.AudioManager

/**
 * Input-device enumeration for the AAudio microphone slice (rsac-ad8a).
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
 * ### API levels
 *
 * `AudioManager.getDevices` and [AudioDeviceInfo.getProductName] both exist
 * since API 23 — safe at the module's `minSdk 29` (build.gradle.kts).
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

    /** Field separator (unit separator, U+001F). */
    private const val US = ''

    /** Record separator (U+001E). */
    private const val RS = ''

    private fun sanitize(name: String): String {
        if (name.indexOf(US) < 0 && name.indexOf(RS) < 0) return name
        return name.filter { it != US && it != RS }
    }
}
