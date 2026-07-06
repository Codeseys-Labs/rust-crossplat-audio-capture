package ai.codeseys.rsac

import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import java.io.File
import java.io.IOException

/**
 * Name/PID → UID resolution helpers for the ADR-0013 target mapping.
 *
 * On Android, `AudioPlaybackCaptureConfiguration.addMatchingUid` filters by
 * **UID**, and all processes of an app share one UID — so both
 * `ApplicationByName` (package name) and `ProcessTree` (PID) resolve to a
 * UID before reaching [CaptureBridge]:
 *
 * | `CaptureTarget` | Resolution here |
 * |---|---|
 * | `ApplicationByName(String)` | [uidForPackage] (PackageManager) |
 * | `ProcessTree(ProcessId)` | [uidForPid] (`/proc/<pid>/status` `Uid:` line) |
 *
 * Pure lookup, **no capture policy** (ADR-0012 §4.2): a `null` return means
 * "could not resolve" — classification into an `AudioError` happens in Rust.
 */
object PackageResolver {

    /**
     * Resolves an installed package's UID, or `null` when the package is not
     * found / not visible.
     *
     * Note (API 30+): package visibility filtering applies — the host app
     * may need a `<queries>` declaration (or `QUERY_ALL_PACKAGES`, with its
     * Play policy implications) for packages it does not otherwise interact
     * with. Documented in README.md; not solvable in the library.
     */
    @JvmStatic
    fun uidForPackage(context: Context, packageName: String): Int? {
        if (packageName.isEmpty()) return null
        val pm = context.packageManager
        return try {
            val info = if (Build.VERSION.SDK_INT >= 33) {
                pm.getApplicationInfo(
                    packageName,
                    PackageManager.ApplicationInfoFlags.of(0L),
                )
            } else {
                @Suppress("DEPRECATION")
                pm.getApplicationInfo(packageName, 0)
            }
            info.uid
        } catch (_: PackageManager.NameNotFoundException) {
            null
        }
    }

    /**
     * Resolves a PID to its UID by parsing the `Uid:` line of
     * `/proc/<pid>/status` (first field = real UID), or `null` when the
     * process does not exist or is not visible.
     *
     * Honest limitation: Android mounts `/proc` with `hidepid=2` (since 7.0),
     * so **other apps' processes are generally not readable** — this works
     * for the caller's own UID's processes (which covers the tree ≡ app
     * equivalence of ADR-0013) and for PIDs the platform exposes to the
     * caller. A `null` here is a resolution failure for Rust to classify.
     */
    @JvmStatic
    fun uidForPid(pid: Int): Int? {
        if (pid <= 0) return null
        return try {
            File("/proc/$pid/status").useLines { lines ->
                lines.firstOrNull { it.startsWith("Uid:") }
                    ?.removePrefix("Uid:")
                    ?.trim()
                    ?.split(WHITESPACE)
                    ?.firstOrNull()
                    ?.toIntOrNull()
            }
        } catch (_: IOException) {
            null
        } catch (_: SecurityException) {
            null
        }
    }

    private val WHITESPACE = Regex("\\s+")
}
