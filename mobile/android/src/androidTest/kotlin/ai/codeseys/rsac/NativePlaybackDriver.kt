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

    /**
     * Drives ONE UID-filtered playback tier end-to-end through rsac's public
     * API — the twin of [drivePlaybackCapture] with the [CaptureTarget]
     * selected by [kind] + [arg]:
     *
     * | [kind] | target | [arg] |
     * |---|---|---|
     * | 0 | `SystemDefault` | ignored |
     * | 1 | `Application(uid)` | the **numeric** app UID string (ADR-0013) |
     * | 2 | `ApplicationByName(package)` | the package name |
     * | 3 | `ProcessTree(pid)` | the decimal PID string |
     *
     * HONESTY: this is a SELF-CAPTURE drive (the test targets its own uid /
     * package / pid), so a pass verifies the UID-filter plumbing (target →
     * resolve → `addMatchingUid` → frames from a matching-uid app), NOT
     * cross-app capture — see [RsacPlaybackInstrumentedTest]'s header.
     *
     * @return `[errorCode, buffers, frames, negRate, negChannels,
     *   negSampleFormat]` — same slot contract as [drivePlaybackCapture];
     *   [lastNativeError] carries the `AudioError` text on a hard error.
     */
    external fun driveTargetedPlaybackCapture(
        tokenRaw: Long,
        kind: Int,
        arg: String,
        sampleRate: Int,
        channels: Int,
        timeoutMs: Int,
    ): LongArray

    /**
     * Drives rsac's PUBLIC device-enumeration facade
     * (`get_device_enumerator()?.enumerate_devices()`) and returns a parseable
     * summary (rsac-ad8a):
     *
     * ```text
     * count=<N>;<id>|<name>|<Input|Output>;…
     * ```
     *
     * or `ERROR: <text>` on failure. Needs no projection token: rsac obtains
     * its `Context` inside its own JNI layer via
     * `ActivityThread.currentApplication()`, which succeeds once this object's
     * `System.loadLibrary("rsac")` has run `JNI_OnLoad` in the test process.
     */
    external fun driveEnumerateDevices(): String

    /** The `AudioError` text captured at the last failing stage ("" if none). */
    external fun lastNativeError(): String
}
