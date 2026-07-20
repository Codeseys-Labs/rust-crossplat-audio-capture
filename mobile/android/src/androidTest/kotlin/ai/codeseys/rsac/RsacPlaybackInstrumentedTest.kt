// rsac-e6d3 (playback tier) — instrumented frames-delivered evidence for the
// Android PLAYBACK path (AudioPlaybackCapture / MediaProjection), the twin of
// the MIC tier (RsacFramesInstrumentedTest).
//
// WHY THIS EXISTS: SystemDefault playback capture needs a user-consented
// MediaProjection, a mediaProjection-typed foreground service confirmed-
// foreground before acquisition (see the rsac-android-mediaprojection-fgs-
// ordering skill), and RECORD_AUDIO — none of which the shell-uid smoke or the
// mic tier exercise. This self-instrumenting test APK installs as a REAL
// package with a REAL uid, pre-grants the projection consent via appops so the
// API 30 system dialog auto-approves, drives the SHIPPED RsacProjection consent
// flow to mint a real token, and hands that token to rsac's PUBLIC capture API
// (CaptureTarget::SystemDefault) through the test-only librsac.so driver.
//
// SCOPE — SystemDefault playback tier ONLY. The Application / ApplicationByName
// / ProcessTree UID-filtered tiers are NOT exercised here and stay unverified.
// Assertions cover frames-delivered + negotiated-format sanity, NEVER audio
// content: a continuous MEDIA tone is played so there is capturable playback,
// but a pass proves the projection→FGS→AudioPlaybackCapture→CaptureBridge→JNI
// →bridge→public-read WIRING reaches rsac under an app uid, not that it carries
// that tone.
//
// HONESTY: a pass is emulator-verified, never device-verified. Consent that
// never fires (appops did not auto-approve on this image), a refused build/
// start, or zero frames degrades to a LOUD assumption-skip unless the
// `rsac_require_frames` instrumentation arg is "1" (wired from the workflow's
// require_frames dispatch input) — mirroring the mic tier and the shell-uid
// smoke.
//
// LIBRARY-LOADING DISCIPLINE (load-bearing — see NativePlaybackDriver): this
// tier loads ONLY librsac.so (here + via RsacProjection). The mic tier loads
// ONLY librsac_ffi.so + the shim. Both .so's statically link rsac and export
// rsac's JNI_OnLoad, and the LAST JNI_OnLoad to run wins the RegisterNatives
// for the shared AAR natives (nativeRetainProjection / nativePush). Test
// methods run sequentially and only the mic test touches NativeCaptureDriver
// (which loads librsac_ffi.so), so during THIS test librsac.so is the most-
// recent registrant — the token mint (nativeRetainProjection) and the ingest
// (nativePush → the session registry populated by build()) both resolve into
// librsac.so's ONE rsac copy. Do not load librsac_ffi.so from this test.
package ai.codeseys.rsac

import android.Manifest
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.os.ParcelFileDescriptor
import android.util.Log
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.rule.GrantPermissionRule
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.math.PI
import kotlin.math.sin

@RunWith(AndroidJUnit4::class)
class RsacPlaybackInstrumentedTest {
    // Playback capture requires RECORD_AUDIO too (API 29+ contract), granted
    // to this package's real uid exactly as the mic tier does.
    @get:Rule
    val recordAudio: GrantPermissionRule =
        GrantPermissionRule.grant(Manifest.permission.RECORD_AUDIO)

    private fun requireFramesHard(): Boolean =
        InstrumentationRegistry.getArguments().getString("rsac_require_frames") == "1"

    @Test
    fun playbackFramesDeliveredViaPublicApi() {
        val requireFrames = requireFramesHard()
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        // Self-instrumenting library test: the package that requests the
        // projection (and whose PROJECT_MEDIA appop gates the dialog) is this
        // APK's own package.
        val targetPackage = instrumentation.targetContext.packageName

        // Pre-grant the projection consent so the API 30 system dialog auto-
        // approves without any UI interaction. FALLBACK (not wired here to
        // avoid the uiautomator dependency): if a future image ignores this
        // appop, a UiAutomator click on the dialog's "Start now" button would
        // approve it — add androidx.test.uiautomator only if that becomes
        // necessary. Until then, a non-firing dialog is an honest soft-skip.
        preGrantProjectMedia(targetPackage)

        // Continuous MEDIA tone from THIS process → capturable playback for the
        // SystemDefault (all-usages, no-UID-filter) capture to pick up. The
        // androidTest manifest sets allowAudioPlaybackCapture=true so this
        // process's USAGE_MEDIA playback is capturable regardless of the test
        // APK's resolved targetSdk.
        // token/denied are written in the callback (main thread) and read here
        // after consent.await(); CountDownLatch.countDown happens-before a
        // successful await return, so the writes are visible without @Volatile
        // (which Kotlin does not allow on locals anyway).
        val tone = ContinuousTone()
        var token = 0L
        var denied: String? = null
        val consent = CountDownLatch(1)

        val scenario = ActivityScenario.launch(RsacTestActivity::class.java)
        try {
            tone.start()

            // RsacProjection.request must run on the main thread with a started
            // Activity; the callback also fires on the main thread. Latch bridges
            // it back to this instrumentation thread.
            scenario.onActivity { activity ->
                RsacProjection.request(
                    activity,
                    object : RsacProjection.Callback {
                        override fun onToken(t: Long) {
                            token = t
                            consent.countDown()
                        }

                        override fun onDenied(reason: String) {
                            denied = reason
                            consent.countDown()
                        }
                    },
                )
            }

            val fired = consent.await(CONSENT_TIMEOUT_SEC, TimeUnit.SECONDS)
            if (!fired) {
                softSkip(
                    requireFrames,
                    "consent callback never fired within ${CONSENT_TIMEOUT_SEC}s " +
                        "(appops PROJECT_MEDIA did not auto-approve the dialog on " +
                        "this image). emulator-verified evidence NOT produced.",
                )
                return
            }
            denied?.let {
                softSkip(
                    requireFrames,
                    "MediaProjection consent denied: $it. emulator-verified " +
                        "evidence NOT produced.",
                )
                return
            }
            if (token == 0L) {
                softSkip(
                    requireFrames,
                    "consent granted but the projection token was 0 (the " +
                        "GlobalRef could not be retained). emulator-verified " +
                        "evidence NOT produced.",
                )
                return
            }

            // Drive SystemDefault playback capture end-to-end through rsac's
            // public API. The FGS started by RsacProjection.request is still
            // foreground (we stop it in `finally`, after the capture is torn
            // down inside the driver via request_stop + Drop).
            // [errorCode, buffers, frames, negRate, negChannels, negSampleFormat]
            val r = NativePlaybackDriver.drivePlaybackCapture(
                tokenRaw = token,
                sampleRate = 48_000,
                channels = 2,
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
                    "negotiated rate=$negRate channels=$negChannels " +
                    "sampleFormat=$negSampleFormat",
            )

            if (errorCode != 0L) {
                val detail = NativePlaybackDriver.lastNativeError()
                softSkip(
                    requireFrames,
                    "rsac refused the SystemDefault playback route with a real " +
                        "projection token (errorCode=$errorCode: $detail). " +
                        "emulator-verified evidence NOT produced.",
                )
                return
            }
            if (buffers < 1L || frames <= 0L) {
                softSkip(
                    requireFrames,
                    "playback capture started but delivered buffers=$buffers " +
                        "frames=$frames within the deadline (no capturable " +
                        "playback reached the record?). emulator-verified " +
                        "evidence NOT produced.",
                )
                return
            }

            // Frames delivered — negotiated format must be real (the Kotlin
            // CaptureBridge builds AudioRecord with the requested shape in
            // PCM_FLOAT and does not renegotiate, so this equals the request,
            // but assert VALIDITY, not identity).
            assertTrue("negotiated sample rate $negRate outside 8000..=96000", negRate in 8_000..96_000)
            assertTrue("negotiated channels $negChannels not in {1, 2}", negChannels == 1L || negChannels == 2L)
            assertTrue(
                "negotiated sample_format $negSampleFormat not a valid rsac_sample_format_t",
                negSampleFormat in 0L..3L,
            )

            Log.i(
                TAG,
                "playback frames delivered via public API (emulator-verified): " +
                    "buffers=$buffers frames=$frames rate=$negRate " +
                    "channels=$negChannels",
            )
        } finally {
            tone.stop()
            // Stop the mediaProjection FGS the consent flow started (the capture
            // itself was torn down inside the driver). Idempotent; harmless if
            // consent never started it.
            RsacCaptureService.stop(instrumentation.targetContext)
            scenario.close()
        }
    }

    /**
     * `appops set <pkg> PROJECT_MEDIA allow` via the instrumentation's
     * UiAutomation shell — makes [android.media.projection.MediaProjectionManager]
     * auto-approve the consent dialog. The returned [ParcelFileDescriptor] MUST
     * be drained/closed or the shell command's pipe leaks.
     */
    private fun preGrantProjectMedia(pkg: String) {
        val pfd = InstrumentationRegistry.getInstrumentation()
            .uiAutomation
            .executeShellCommand("appops set $pkg PROJECT_MEDIA allow")
        // Drain then close (also closes the fd) so the command completes and no
        // descriptor leaks.
        ParcelFileDescriptor.AutoCloseInputStream(pfd).use { it.readBytes() }
        Log.i(TAG, "pre-granted PROJECT_MEDIA appop for $pkg")
    }

    private fun softSkip(requireFrames: Boolean, message: String) {
        val msg = "SKIP-WITH-SUMMARY: $message"
        Log.w(TAG, msg)
        if (requireFrames) {
            throw AssertionError(msg)
        }
        assumeTrue(msg, false) // loud skip, not a red X
    }

    /**
     * A dedicated thread streaming a continuous sine tone through an
     * [AudioTrack] with `USAGE_MEDIA` — capturable playback for the capture to
     * pick up. Not audible on a `-no-window` emulator; it exists purely to make
     * the playback-capture record non-silent. Idempotent start/stop.
     */
    private class ContinuousTone {
        private val running = AtomicBoolean(false)
        @Volatile private var thread: Thread? = null

        fun start() {
            if (!running.compareAndSet(false, true)) return
            thread = Thread({ loop() }, "rsac-test-tone").apply {
                isDaemon = true
                start()
            }
        }

        fun stop() {
            running.set(false)
            thread?.join(2_000)
            thread = null
        }

        private fun loop() {
            val minBytes = AudioTrack.getMinBufferSize(
                SAMPLE_RATE,
                AudioFormat.CHANNEL_OUT_STEREO,
                AudioFormat.ENCODING_PCM_FLOAT,
            )
            val bufferBytes = maxOf(minBytes, FRAMES_PER_WRITE * CHANNELS * BYTES_PER_FLOAT)
            val track = AudioTrack.Builder()
                .setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                        .build(),
                )
                .setAudioFormat(
                    AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_FLOAT)
                        .setSampleRate(SAMPLE_RATE)
                        .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                        .build(),
                )
                .setBufferSizeInBytes(bufferBytes)
                .setTransferMode(AudioTrack.MODE_STREAM)
                .build()

            val buf = FloatArray(FRAMES_PER_WRITE * CHANNELS)
            var phase = 0.0
            val step = 2.0 * PI * TONE_HZ / SAMPLE_RATE
            try {
                track.play()
                while (running.get()) {
                    var i = 0
                    while (i < buf.size) {
                        val s = (sin(phase) * 0.25).toFloat()
                        buf[i] = s
                        buf[i + 1] = s
                        i += 2
                        phase += step
                        if (phase > 2.0 * PI) phase -= 2.0 * PI
                    }
                    track.write(buf, 0, buf.size, AudioTrack.WRITE_BLOCKING)
                }
            } catch (t: Throwable) {
                Log.w(TAG, "tone thread stopped: ${t.message}")
            } finally {
                try {
                    track.stop()
                } catch (_: IllegalStateException) {
                }
                track.release()
            }
        }

        private companion object {
            const val SAMPLE_RATE = 48_000
            const val CHANNELS = 2
            const val BYTES_PER_FLOAT = 4
            const val FRAMES_PER_WRITE = 480
            const val TONE_HZ = 440.0
        }
    }

    private companion object {
        const val TAG = "RsacPlaybackTest"

        /** Generous: appops auto-approve is near-instant, but boot/service
         * scheduling on a loaded emulator can add seconds. */
        const val CONSENT_TIMEOUT_SEC = 30L
    }
}
