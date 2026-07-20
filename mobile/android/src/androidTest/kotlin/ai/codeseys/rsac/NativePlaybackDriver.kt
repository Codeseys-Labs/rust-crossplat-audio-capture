// rsac-e6d3 (playback tier) — TEST-ONLY binding to the playback driver
// exported from librsac.so (mobile/androidtest-native). Lives in
// src/androidTest/ so neither the class nor the .so ever enters the production
// rsac.aar.
package ai.codeseys.rsac

/**
 * Drives `CaptureTarget::SystemDefault` playback capture
 * (`AudioPlaybackCapture` / MediaProjection) through rsac's **public** capture
 * API, consuming a real projection token minted by [RsacProjection].
 *
 * ### Why this loads ONLY `librsac.so` (and the mic tier only its own libs)
 *
 * `librsac.so` (this driver + rsac's re-exported `JNI_OnLoad`) and
 * `librsac_ffi.so` (the mic tier's shipped C ABI) each statically link rsac —
 * so each carries its **own** copy of rsac's process statics (the JNI class
 * cache, the ingest-session registry) and each exports rsac's `JNI_OnLoad`,
 * which `RegisterNatives`-binds the shared AAR natives
 * (`nativeRetainProjection`, `nativePush`, …). If both `.so`s load in one
 * process, the **last** `JNI_OnLoad` to run wins that registration.
 *
 * The projection token is minted (`RsacProjection.nativeRetainProjection`) and
 * consumed (the driver's `build()` → capture) inside rsac's statics — a token
 * minted against one copy is not safely consumable against the other. So the
 * two tiers keep disjoint library sets:
 *  - this playback tier loads ONLY `rsac` (here); [RsacProjection] also loads
 *    only `rsac`. `librsac.so` is loaded by the playback path alone, so its
 *    `JNI_OnLoad` is the winning registrant at mint time — mint and consume
 *    share one rsac copy.
 *  - the mic tier ([NativeCaptureDriver]) loads ONLY `rsac_ffi` +
 *    `rsac_androidtest_shim`, never `rsac`.
 *
 * Do not add a `System.loadLibrary("rsac_ffi")` here (or a `"rsac"` load in
 * the mic tier): that would reintroduce the cross-copy registration race.
 */
object NativePlaybackDriver {
    init {
        // The ONE library for the playback tier. Idempotent with
        // RsacProjection.isNativeAvailable()'s own load of the same lib; both
        // resolve the projection natives + this driver's Java_* exports out of
        // the same librsac.so.
        System.loadLibrary("rsac")
    }

    /**
     * Drives `SystemDefault` playback capture end-to-end through rsac's public
     * API (mirrors `tests/android_emu_smoke.rs`):
     * `AndroidProjectionToken::from_raw(tokenRaw) →
     * with_target(SystemDefault) → with_android_projection → sample_rate →
     * channels → build → start → format → bounded read loop → request_stop`.
     *
     * @param tokenRaw the opaque projection token from
     *   [RsacProjection.Callback.onToken]
     * @return `[errorCode, buffers, frames, negRate, negChannels,
     *   negSampleFormat]` — errorCode 0 == no hard error; on a build/start/
     *   format failure the negotiated fields stay 0/0/-1 and
     *   [lastNativeError] carries the `AudioError` text. Frames are counted,
     *   content is never inspected.
     */
    external fun drivePlaybackCapture(
        tokenRaw: Long,
        sampleRate: Int,
        channels: Int,
        timeoutMs: Int,
    ): LongArray

    /** The `AudioError` text captured at the last failing stage ("" if none). */
    external fun lastNativeError(): String
}
