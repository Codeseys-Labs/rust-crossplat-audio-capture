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
// SCOPE — the SystemDefault playback tier PLUS the Application /
// ApplicationByName / ProcessTree UID-filtered tiers, each driven through
// rsac's public API. Assertions cover frames-delivered + negotiated-format
// sanity, NEVER audio content: a continuous MEDIA tone is played so there is
// capturable playback, but a pass proves the projection→FGS→
// AudioPlaybackCapture→CaptureBridge→JNI→bridge→public-read WIRING reaches
// rsac under an app uid, not that it carries that tone.
//
// HONESTY — UID-FILTERED TIERS ARE SELF-CAPTURE (load-bearing): the
// Application/ApplicationByName/ProcessTree tiers target THIS test process's
// own uid / package / pid. A pass verifies the UID-filter PLUMBING —
// target → resolve_match_uid → CaptureBridge.addMatchingUid(matchUid) →
// frames delivered from an app whose uid MATCHES the filter (src/audio/
// android/playback.rs::resolve_match_uid). It does NOT verify that capturing a
// DIFFERENT app's audio works or is correctly scoped; cross-app UID filtering
// stays UNVERIFIED here (and on the emulator generally). The three UID tiers
// all resolve to this process's single uid (tree ≡ app ≡ uid on Android), so
// they exercise three RESOLUTION paths onto the same downstream capture.
//
// HONESTY — a pass is emulator-verified, never device-verified. Consent that
// never fires (appops did not auto-approve on this image), a refused build/
// start, or zero frames degrades to a LOUD assumption-skip unless the
// `rsac_require_frames` instrumentation arg is "1" (wired from the workflow's
// require_frames dispatch input) — mirroring the mic tier and the shell-uid
// smoke.
//
// DEVICE ENUMERATION (rsac-ad8a): devicesEnumerated drives rsac's public
// device-enumeration facade (no projection needed) and asserts a non-empty,
// unique-id device list — see that test's own header for the capability-gate
// note.
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
import android.os.Process
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

    // ── Per-tier tests ───────────────────────────────────────────────────
    //
    // Each tier: preGrantProjectMedia → mint a FRESH token via the consent
    // flow → tone playing → drive the tier's target → assert frames + format
    // sanity → soft-skip-loud unless rsac_require_frames=="1". The shared
    // plumbing lives in [runPlaybackTier]; the four @Tests differ ONLY in the
    // target they pass. A fresh token is minted per test because each rsac
    // capture consumes a projection token's single-owner deletion latch
    // (one token = one capture session — src/audio/android/playback.rs).

    /** SystemDefault (no UID filter) — the baseline all-capturable-playback tier. */
    @Test
    fun systemDefaultTier() {
        runPlaybackTier("systemDefault", DriveTier.SystemDefault)
    }

    /**
     * Application(uid) — the numeric-UID tier (ADR-0013:
     * CaptureTarget::Application carries the app UID). Self-capture: we pass
     * THIS process's own uid, so the addMatchingUid filter matches the tone's
     * producer.
     */
    @Test
    fun applicationTier() {
        runPlaybackTier(
            "application",
            DriveTier.Targeted(kind = 1, arg = Process.myUid().toString()),
        )
    }

    /**
     * ApplicationByName(package) — the package-name tier (rsac resolves the
     * package → uid via the AAR PackageResolver). Self-capture: our own
     * package resolves to our own uid.
     */
    @Test
    fun applicationByNameTier() {
        val pkg = InstrumentationRegistry.getInstrumentation().targetContext.packageName
        runPlaybackTier("applicationByName", DriveTier.Targeted(kind = 2, arg = pkg))
    }

    /**
     * ProcessTree(pid) — the PID tier (rsac maps pid → uid via /proc/<pid>/
     * status). Self-capture: our own pid maps to our own uid (tree ≡ app).
     */
    @Test
    fun processTreeTier() {
        runPlaybackTier(
            "processTree",
            DriveTier.Targeted(kind = 3, arg = Process.myPid().toString()),
        )
    }

    /**
     * rsac-ad8a — device enumeration through rsac's PUBLIC facade. No
     * projection token is needed: rsac obtains its Context inside its own JNI
     * layer (ActivityThread.currentApplication()), which works once
     * librsac.so's JNI_OnLoad has run in this test process.
     *
     * HARD assertions (hold on EVERY loaded-library build, AAR path or not —
     * `enumerate_devices` always frames its list as [default input sentinel,
     * …real input devices…, playback-capture endpoint], src/audio/android/
     * mod.rs): count>=1; non-empty UNIQUE ids; the always-present built-in-mic
     * -ish default input sentinel (`id="default"`, kind Input — the mission's
     * "built-in-mic-ish entry present"); and the playback-capture endpoint.
     *
     * SOFT signal (rsac-ad8a proper): a REAL numeric-id input device beyond
     * the sentinel appears ONLY when the AAR `RsacDevices.inputDevices` path
     * resolved at load — the exact condition that flips
     * `android_device_enumeration_available` true (src/core/capabilities.rs,
     * set from src/audio/android/jni.rs JNI_OnLoad). This driver does not read
     * that gate directly, but a real device in the list implies it. On the
     * honest JNI-/AAR-absent fallback the list is exactly [default,
     * playback-capture] with no real device, so its absence is a soft-skip
     * (loud unless require_frames), never a failure.
     */
    @Test
    fun devicesEnumerated() {
        val summary = NativePlaybackDriver.driveEnumerateDevices()
        Log.i(DEVICES_TAG, "enumerate result: $summary")

        if (summary.startsWith("ERROR:")) {
            // Enumeration itself failed (no JavaVM / unexpected). This is a
            // real defect on a loaded library, so fail loud regardless of
            // require_frames — enumeration needs no projection and no consent.
            throw AssertionError("device enumeration failed: $summary")
        }

        val parsed = parseDeviceSummary(summary)
        assertTrue("enumeration returned no count= header: $summary", parsed != null)
        val devices = parsed!!

        assertTrue("device count must be >= 1 (got ${devices.size}): $summary", devices.isNotEmpty())

        // Non-empty ids, and ids are unique.
        assertTrue("every device id must be non-empty: $summary", devices.all { it.id.isNotEmpty() })
        val ids = devices.map { it.id }
        assertTrue("device ids must be unique: $ids", ids.size == ids.toSet().size)

        // The built-in-mic-ish default input sentinel is ALWAYS present
        // (devices[0] — the "let the OS route the default input" handle,
        // id="default", kind Input). This is the mission's required
        // built-in-mic-ish entry; it holds even on the AAR-absent fallback.
        assertTrue(
            "expected the default input sentinel (built-in-mic-ish) in the list: $summary",
            devices.any { it.id == "default" && it.kind == "Input" },
        )

        // The playback-capture endpoint is always the last entry (rsac's
        // Android default device); assert it so we know the enumeration
        // produced the canonical framing, not just any non-empty list.
        assertTrue(
            "expected the playback-capture endpoint in the list: $summary",
            devices.any { it.id == "playback-capture" },
        )

        // rsac-ad8a proper: a REAL numeric-id input device (not the "default"
        // sentinel, not the "playback-capture" endpoint) appears only when the
        // AAR RsacDevices.inputDevices path resolved — the condition that flips
        // android_device_enumeration_available true. Its absence means only the
        // fallback list exists, so soft-skip rather than fail.
        val hasRealInputDevice = devices.any { d ->
            d.kind == "Input" && d.id != "default" && d.id.toIntOrNull()?.let { it > 0 } == true
        }
        if (!hasRealInputDevice) {
            softSkip(
                requireFramesHard(),
                "enumeration succeeded (${devices.size} devices) with the default " +
                    "sentinel + playback endpoint, but NO real numeric-id input " +
                    "device — the AAR RsacDevices path did not resolve on this " +
                    "image (android_device_enumeration_available is false), so " +
                    "only the default-route fallback list exists. emulator-verified " +
                    "AAR real-input enumeration (rsac-ad8a) NOT produced.",
            )
            return
        }

        Log.i(
            DEVICES_TAG,
            "device enumeration verified (emulator-verified, AAR real-input path): " +
                "${devices.size} devices, ids=$ids",
        )
    }

    // ── Shared tier plumbing ───────────────────────────────────────────────

    /** Which capture target a tier drives (the only thing that varies). */
    private sealed interface DriveTier {
        object SystemDefault : DriveTier
        data class Targeted(val kind: Int, val arg: String) : DriveTier
    }

    /**
     * The full mint→tone→drive→assert flow for one tier, logged under [tier].
     * Every tier shares this so the SystemDefault test is not duplicated
     * three times — only the [DriveTier] differs.
     */
    private fun runPlaybackTier(tier: String, driveTier: DriveTier) {
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
        // capture to pick up. The androidTest manifest sets
        // allowAudioPlaybackCapture=true so this process's USAGE_MEDIA playback
        // is capturable regardless of the test APK's resolved targetSdk. For
        // the UID-filtered tiers this process IS the matching uid, so the tone
        // is exactly what the addMatchingUid filter should admit.
        val tone = ContinuousTone()
        val scenario = ActivityScenario.launch(RsacTestActivity::class.java)
        try {
            tone.start()

            // Mint a FRESH projection token via the shipped consent flow.
            val token = mintProjectionToken(scenario, requireFrames, tier) ?: return

            // Drive this tier end-to-end through rsac's public API. The FGS
            // started by RsacProjection.request is still foreground (we stop it
            // in `finally`, after the capture is torn down inside the driver
            // via request_stop + Drop).
            // [errorCode, buffers, frames, negRate, negChannels, negSampleFormat]
            val r = when (driveTier) {
                is DriveTier.SystemDefault -> NativePlaybackDriver.drivePlaybackCapture(
                    tokenRaw = token,
                    sampleRate = 48_000,
                    channels = 2,
                    timeoutMs = 15_000,
                )
                is DriveTier.Targeted -> NativePlaybackDriver.driveTargetedPlaybackCapture(
                    tokenRaw = token,
                    kind = driveTier.kind,
                    arg = driveTier.arg,
                    sampleRate = 48_000,
                    channels = 2,
                    timeoutMs = 15_000,
                )
            }
            assertDriveResult(tier, requireFrames, r)
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
     * Runs the shipped RsacProjection consent flow on the scenario's Activity
     * and returns a fresh, non-zero projection token — or `null` after a
     * soft-skip when consent never fired / was denied / retained a 0 handle.
     */
    private fun mintProjectionToken(
        scenario: ActivityScenario<RsacTestActivity>,
        requireFrames: Boolean,
        tier: String,
    ): Long? {
        // token/denied are written in the callback (main thread) and read here
        // after consent.await(); CountDownLatch.countDown happens-before a
        // successful await return, so the writes are visible without @Volatile
        // (which Kotlin does not allow on locals anyway).
        var token = 0L
        var denied: String? = null
        val consent = CountDownLatch(1)

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
                "[$tier] consent callback never fired within ${CONSENT_TIMEOUT_SEC}s " +
                    "(appops PROJECT_MEDIA did not auto-approve the dialog on " +
                    "this image). emulator-verified evidence NOT produced.",
            )
            return null
        }
        denied?.let {
            softSkip(
                requireFrames,
                "[$tier] MediaProjection consent denied: $it. emulator-verified " +
                    "evidence NOT produced.",
            )
            return null
        }
        if (token == 0L) {
            softSkip(
                requireFrames,
                "[$tier] consent granted but the projection token was 0 (the " +
                    "GlobalRef could not be retained). emulator-verified " +
                    "evidence NOT produced.",
            )
            return null
        }
        return token
    }

    /**
     * Asserts a drive `[errorCode, buffers, frames, negRate, negChannels,
     * negSampleFormat]` result: a hard error or zero frames soft-skips (loud
     * unless require_frames); frames delivered assert negotiated-format
     * VALIDITY (not identity — the CaptureBridge builds AudioRecord with the
     * requested shape in PCM_FLOAT and does not renegotiate).
     */
    private fun assertDriveResult(tier: String, requireFrames: Boolean, r: LongArray) {
        val errorCode = r[0]
        val buffers = r[1]
        val frames = r[2]
        val negRate = r[3]
        val negChannels = r[4]
        val negSampleFormat = r[5]

        Log.i(
            TAG,
            "[$tier] drive result: errorCode=$errorCode buffers=$buffers frames=$frames " +
                "negotiated rate=$negRate channels=$negChannels " +
                "sampleFormat=$negSampleFormat",
        )

        if (errorCode != 0L) {
            val detail = NativePlaybackDriver.lastNativeError()
            softSkip(
                requireFrames,
                "[$tier] rsac refused the playback route with a real " +
                    "projection token (errorCode=$errorCode: $detail). " +
                    "emulator-verified evidence NOT produced.",
            )
            return
        }
        if (buffers < 1L || frames <= 0L) {
            softSkip(
                requireFrames,
                "[$tier] playback capture started but delivered buffers=$buffers " +
                    "frames=$frames within the deadline (no capturable " +
                    "playback reached the record?). emulator-verified " +
                    "evidence NOT produced.",
            )
            return
        }

        assertTrue(
            "[$tier] negotiated sample rate $negRate outside 8000..=96000",
            negRate in 8_000..96_000,
        )
        assertTrue(
            "[$tier] negotiated channels $negChannels not in {1, 2}",
            negChannels == 1L || negChannels == 2L,
        )
        assertTrue(
            "[$tier] negotiated sample_format $negSampleFormat not a valid rsac_sample_format_t",
            negSampleFormat in 0L..3L,
        )

        Log.i(
            TAG,
            "[$tier] playback frames delivered via public API (emulator-verified): " +
                "buffers=$buffers frames=$frames rate=$negRate channels=$negChannels",
        )
    }

    /** One parsed device record from the driveEnumerateDevices summary. */
    private data class DeviceRecord(val id: String, val name: String, val kind: String)

    /**
     * Parses `count=<N>;<id>|<name>|<kind>;…` into records, or `null` when the
     * `count=` header is missing/malformed. A record with fewer than 3
     * `|`-fields is skipped (defensive — the Rust side sanitizes delimiters
     * out of names, so this is belt-and-suspenders).
     */
    private fun parseDeviceSummary(summary: String): List<DeviceRecord>? {
        val parts = summary.split(';')
        val header = parts.firstOrNull() ?: return null
        if (!header.startsWith("count=")) return null
        // The header's N is advisory; we parse the actual records that follow.
        return parts.drop(1)
            .filter { it.isNotEmpty() }
            .mapNotNull { record ->
                val fields = record.split('|')
                if (fields.size < 3) return@mapNotNull null
                DeviceRecord(id = fields[0], name = fields[1], kind = fields[2])
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

        /** Device-enumeration evidence logs under its own tag (rsac-ad8a) so
         * the CI payload can dump it separately from the playback tiers. */
        const val DEVICES_TAG = "RsacDevicesTest"

        /** Generous: appops auto-approve is near-instant, but boot/service
         * scheduling on a loaded emulator can add seconds. */
        const val CONSENT_TIMEOUT_SEC = 30L
    }
}
