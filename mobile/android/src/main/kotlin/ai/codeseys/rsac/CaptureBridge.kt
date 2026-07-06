package ai.codeseys.rsac

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioPlaybackCaptureConfiguration
import android.media.AudioRecord
import android.media.projection.MediaProjection
import java.util.concurrent.atomic.AtomicBoolean

/**
 * The Java-side playback-capture loop (docs/MOBILE_BACKEND_DESIGN.md § two
 * data paths): `AudioPlaybackCaptureConfiguration` can only be attached to a
 * Java [AudioRecord], so this dedicated thread loops
 * `audioRecord.read(FloatArray, READ_BLOCKING)` and forwards each period to
 * Rust via [nativePush] — one JNI call per buffer, into
 * `BridgeProducer::push_samples_or_drop()` on the Rust side.
 *
 * **No capture policy** (ADR-0012 §4.2): which UID to match (or none), the
 * sample rate, and the channel count are *inputs*, resolved by Rust per
 * ADR-0013 (`SystemDefault` = usage filters only; `Application*` /
 * `ProcessTree` = `addMatchingUid`). The usage-filter set
 * (MEDIA / GAME / UNKNOWN) is the fixed transport mapping from ADR-0013 —
 * `USAGE_VOICE_COMMUNICATION` is never capturable by third parties.
 *
 * ### Allocation discipline (ADR-0001, adapted for JNI)
 *
 * The read buffer is allocated **once** in the constructor and reused for
 * every read — no per-read garbage, so GC pauses on this thread manifest as
 * ring drops (visible via `overrun_count`) rather than as jank elsewhere.
 * This thread is *not* an OS real-time callback thread ([AudioRecord.read]
 * is a buffered blocking read); the hard-RT rules bind the Rust side of
 * [nativePush] (session-lifetime scratch, no per-call allocation).
 *
 * ### Lifecycle
 *
 * ```
 * CaptureBridge(projection, session, rate, ch, uid) // builds the AudioRecord
 *   .also { RsacCaptureService.registerBridge(it) } // anchor to the FGS
 *   .start()                                        // spawn the read thread
 * ...
 * bridge.stop()                                     // idempotent: stop + join + release
 * RsacCaptureService.unregisterBridge(bridge)
 * ```
 *
 * Constructed and driven by the Rust orchestration (src/audio/android/
 * playback.rs, seed rsac-77f1) through JNI; a mediaProjection foreground
 * service must be running (API 34+ enforces this) and RECORD_AUDIO granted,
 * or [AudioRecord] construction fails.
 *
 * @param projection consent-granted projection (Rust owns its GlobalRef/token)
 * @param session opaque pointer to the per-capture Rust ingest state; valid
 *   for this bridge's whole lifetime (Rust guarantees it outlives [stop])
 * @param sampleRate requested capture rate in Hz (e.g. 48000)
 * @param channels 1 (mono) or 2 (stereo)
 * @param matchUid UID filter for per-app / process-tree capture, or a
 *   negative value for no filter (system-wide capture)
 * @param framesPerRead frames per read/push period (default ~10 ms at 48 kHz)
 */
class CaptureBridge(
    projection: MediaProjection,
    private val session: Long,
    private val sampleRate: Int,
    private val channels: Int,
    matchUid: Int = NO_UID_FILTER,
    framesPerRead: Int = DEFAULT_FRAMES_PER_READ,
) {

    private val audioRecord: AudioRecord

    /** Allocated ONCE; reused for every read (ADR-0001 adapted — see class docs). */
    private val readBuffer: FloatArray

    private val running = AtomicBoolean(false)
    private val released = AtomicBoolean(false)

    @Volatile
    private var thread: Thread? = null

    init {
        require(sampleRate > 0) { "sampleRate must be positive, got $sampleRate" }
        require(channels == 1 || channels == 2) { "channels must be 1 or 2, got $channels" }
        require(framesPerRead > 0) { "framesPerRead must be positive, got $framesPerRead" }

        readBuffer = FloatArray(framesPerRead * channels)

        // Fixed transport mapping (ADR-0013): all capturable playback usages.
        // UID selection is the ONLY policy input, and it comes from Rust.
        val captureConfig = AudioPlaybackCaptureConfiguration.Builder(projection)
            .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
            .addMatchingUsage(AudioAttributes.USAGE_GAME)
            .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
            .apply { if (matchUid >= 0) addMatchingUid(matchUid) }
            .build()

        val channelMask =
            if (channels == 1) AudioFormat.CHANNEL_IN_MONO else AudioFormat.CHANNEL_IN_STEREO
        val format = AudioFormat.Builder()
            .setEncoding(AudioFormat.ENCODING_PCM_FLOAT)
            .setSampleRate(sampleRate)
            .setChannelMask(channelMask)
            .build()

        // Device buffer: at least the OS minimum, and at least two of our
        // read periods so a briefly-descheduled reader doesn't overrun the
        // AudioRecord's own ring.
        val minBytes = AudioRecord.getMinBufferSize(
            sampleRate, channelMask, AudioFormat.ENCODING_PCM_FLOAT
        )
        val periodBytes = readBuffer.size * BYTES_PER_FLOAT
        val bufferBytes = maxOf(if (minBytes > 0) minBytes else 0, periodBytes * 2)

        // NOTE: no setAudioSource() — it is mutually exclusive with
        // setAudioPlaybackCaptureConfig (the config implies the source).
        // Throws (UnsupportedOperationException / SecurityException) when
        // RECORD_AUDIO is not granted or the projection is not usable —
        // surfaced to the Rust caller as the JNI exception.
        audioRecord = AudioRecord.Builder()
            .setAudioPlaybackCaptureConfig(captureConfig)
            .setAudioFormat(format)
            .setBufferSizeInBytes(bufferBytes)
            .build()

        check(audioRecord.state == AudioRecord.STATE_INITIALIZED) {
            "AudioRecord failed to initialize for playback capture " +
                "($sampleRate Hz, $channels ch, uid=$matchUid)"
        }
    }

    /**
     * Starts recording and spawns the dedicated read thread. Idempotent —
     * a second call while running is a no-op. Fails fast (before touching
     * the AudioRecord) when the native push symbol is unavailable.
     *
     * @throws IllegalStateException if librsac.so is absent (rsac-77f1 not
     *   packaged) or the record cannot start.
     */
    fun start() {
        check(RsacProjection.isNativeAvailable()) {
            "librsac.so is not available: nativePush (rsac-77f1) cannot be " +
                "called. See mobile/android/README.md § Native library."
        }
        check(!released.get()) { "CaptureBridge already released" }
        if (!running.compareAndSet(false, true)) return

        try {
            audioRecord.startRecording()
            check(audioRecord.recordingState == AudioRecord.RECORDSTATE_RECORDING) {
                "AudioRecord.startRecording() did not enter RECORDING state"
            }
        } catch (t: Throwable) {
            running.set(false)
            throw t
        }

        thread = Thread(::readLoop, "rsac-capture-bridge").apply {
            // No policy here either: normal priority; the Rust ring absorbs
            // scheduling jitter and surfaces sustained lag as overruns.
            isDaemon = true
            start()
        }
    }

    /**
     * The dedicated capture loop: blocking reads into the single reused
     * buffer, one [nativePush] per successful period.
     */
    private fun readLoop() {
        val buf = readBuffer
        while (running.get()) {
            val read = audioRecord.read(buf, 0, buf.size, AudioRecord.READ_BLOCKING)
            when {
                read > 0 -> {
                    // read() returns a float (sample) count; whole frames
                    // only are forwarded (a partial trailing frame — not
                    // expected from AudioRecord — is dropped, not split).
                    val frames = read / channels
                    if (frames > 0) {
                        // Rust side: GetFloatArrayRegion into session-lifetime
                        // scratch → push_samples_or_drop (ring-full ⇒ drop +
                        // overrun count; never blocks this thread).
                        nativePush(session, buf, frames, channels, sampleRate)
                    }
                }
                read == 0 -> {
                    // Spurious empty read (e.g. racing stop()) — loop.
                }
                else -> {
                    // ERROR_INVALID_OPERATION / ERROR_DEAD_OBJECT / ERROR:
                    // the record can no longer deliver. Exit; the Rust
                    // orchestration observes the producer going quiet and
                    // drives terminal semantics (ADR-0010) — no policy here.
                    break
                }
            }
        }
    }

    /**
     * Stops the loop, joins the thread (bounded), and releases the
     * [AudioRecord]. Idempotent and safe from any thread except the read
     * thread itself. After this returns, no further [nativePush] call for
     * this bridge is in flight — the Rust side may then invalidate `session`.
     */
    fun stop() {
        if (!released.compareAndSet(false, true)) return
        running.set(false)
        try {
            // Unblocks a parked READ_BLOCKING read.
            audioRecord.stop()
        } catch (_: IllegalStateException) {
            // Never started / already stopped — fine.
        }
        thread?.let {
            it.join(JOIN_TIMEOUT_MS)
            // CI-VERIFY: on-device, confirm AudioRecord.stop() reliably
            // unblocks READ_BLOCKING within the timeout; if a device keeps
            // the read parked, switch the loop to bounded reads or
            // release() before join.
        }
        thread = null
        audioRecord.release()
    }

    companion object {
        /** Pass as `matchUid` for no UID filter (SystemDefault capture). */
        const val NO_UID_FILTER: Int = -1

        /** 480 frames = 10 ms at 48 kHz — a conventional capture period. */
        const val DEFAULT_FRAMES_PER_READ: Int = 480

        private const val BYTES_PER_FLOAT = 4
        private const val JOIN_TIMEOUT_MS = 2_000L

        /**
         * Ingest entry point into Rust: copies `frames * channels` samples
         * out of [buf] and pushes them into the capture's ring buffer.
         *
         * Registered from Rust via `RegisterNatives` (`JNI_OnLoad`,
         * src/audio/android/jni.rs — seed rsac-77f1) as
         * `Java_ai_codeseys_rsac_CaptureBridge_nativePush`. **Lockstep
         * contract**: renaming this method, its class, or its signature
         * breaks the Rust registration (CI drift guard arrives with
         * rsac-1a6e). Throws [UnsatisfiedLinkError] until librsac.so ships —
         * guard with [RsacProjection.isNativeAvailable].
         *
         * @param session opaque per-capture ingest state pointer (Rust-owned)
         * @param buf interleaved f32 samples; only the first
         *   `frames * channels` entries are read
         * @param frames whole frames in this period
         * @param channels interleaved channel count
         * @param sampleRate delivery rate in Hz
         */
        @JvmStatic
        external fun nativePush(
            session: Long,
            buf: FloatArray,
            frames: Int,
            channels: Int,
            sampleRate: Int,
        )
    }
}
