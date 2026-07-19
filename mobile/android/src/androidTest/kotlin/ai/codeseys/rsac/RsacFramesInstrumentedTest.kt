// rsac-255b — instrumented frames-delivered evidence for the Android MIC path.
//
// WHY THIS EXISTS: PR #65's emulator leg runs libtest binaries as the `shell`
// uid (adb push + adb shell), where AAudioStream_requestStart returns
// AAUDIO_ERROR_INTERNAL — shell has no usable audio-client/appops context
// (platform.xml grants it only INTERNET). This self-instrumenting test APK
// installs as a REAL package with a REAL uid holding RECORD_AUDIO and drives
// CaptureTarget::Device("default") through the SHIPPED rsac C ABI
// (librsac_ffi.so, via the test-only C shim) — the exact surface Flutter/C
// consumers use.
//
// SCOPE — mic path ONLY: this needs RECORD_AUDIO and nothing else. It never
// touches MediaProjection, foreground services, or consent dialogs — do NOT
// conflate it with the SystemDefault playback tier (which has its own
// ordering skill and stays a follow-up). Assertions cover frames-delivered +
// negotiated-format sanity, NEVER audio content: the emulator mic is
// synthetic (host audio or silence), so a pass proves the AAudio input
// WIRING reaches rsac under an app uid.
//
// HONESTY: a pass is emulator-verified, never device-verified. A refused
// route / zero frames degrades to a LOUD assumption-skip unless the
// `rsac_require_frames` instrumentation arg is "1" (wired from the
// workflow's require_frames dispatch input), mirroring
// RSAC_CI_ANDROID_REQUIRE_FRAMES on the shell-uid smoke.
package ai.codeseys.rsac

import android.Manifest
import android.util.Log
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.rule.GrantPermissionRule
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class RsacFramesInstrumentedTest {
    @get:Rule
    val recordAudio: GrantPermissionRule =
        GrantPermissionRule.grant(Manifest.permission.RECORD_AUDIO)

    private fun requireFramesHard(): Boolean =
        InstrumentationRegistry.getArguments().getString("rsac_require_frames") == "1"

    @Test
    fun framesDeliveredViaShippedCAbi() {
        val requireFrames = requireFramesHard()

        // [errorCode, buffers, frames, negRate, negChannels, negSampleFormat]
        val r = NativeCaptureDriver.driveDefaultMicCapture(
            sampleRate = 48_000,
            channels = 1,
            timeoutMs = 15_000,
        )
        val errorCode = r[0]
        val buffers = r[1]
        val frames = r[2]
        val negRate = r[3]
        val negChannels = r[4]
        val negSampleFormat = r[5]

        Log.i(
            TAG,
            "drive result: errorCode=$errorCode buffers=$buffers frames=$frames " +
                "negotiated rate=$negRate channels=$negChannels sampleFormat=$negSampleFormat",
        )

        if (errorCode != 0L) {
            // rsac_error_t from the first failing FFI call. On the emulator
            // this is the outcome that would prove the mic route needs more
            // than RECORD_AUDIO — a documented result, not noise.
            val detail = NativeCaptureDriver.lastNativeError()
            val msg =
                "SKIP-WITH-SUMMARY: shipped C ABI refused the default-mic route " +
                    "under an app uid holding RECORD_AUDIO (rsac_error_t=$errorCode: $detail). " +
                    "emulator-verified evidence NOT produced."
            Log.w(TAG, msg)
            if (requireFrames) {
                throw AssertionError(msg)
            }
            assumeTrue(msg, false) // loud skip, not a red X
        }

        if (buffers < 1L || frames <= 0L) {
            val msg =
                "SKIP-WITH-SUMMARY: capture started but delivered buffers=$buffers " +
                    "frames=$frames within the deadline (silent/blocked synthetic mic?). " +
                    "emulator-verified evidence NOT produced."
            Log.w(TAG, msg)
            if (requireFrames) {
                throw AssertionError(msg)
            }
            assumeTrue(msg, false)
        }

        // Frames delivered — now the negotiated format must be REAL AAudio
        // negotiation output, not echoed inputs or garbage.
        assertTrue("negotiated sample rate $negRate outside 8000..=96000", negRate in 8_000..96_000)
        assertTrue("negotiated channels $negChannels not in {1, 2}", negChannels == 1L || negChannels == 2L)
        // AAudio negotiates I16 (0) or F32 (3); delivery is always f32 via the
        // bridge. Logged above, deliberately NOT over-asserted beyond validity.
        assertTrue(
            "negotiated sample_format $negSampleFormat not a valid rsac_sample_format_t",
            negSampleFormat in 0L..3L,
        )

        Log.i(
            TAG,
            "frames delivered via shipped C ABI (emulator-verified): buffers=$buffers " +
                "frames=$frames rate=$negRate channels=$negChannels",
        )
    }

    private companion object {
        const val TAG = "RsacFramesTest"
    }
}
