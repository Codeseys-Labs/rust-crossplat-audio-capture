// rsac-255b — TEST-ONLY binding to the androidTest C shim
// (rsac_androidtest_shim.c), which drives the SHIPPED rsac C ABI
// (librsac_ffi.so). Lives in src/androidTest/ so neither the class nor its
// jniLibs ever enter the production rsac.aar.
package ai.codeseys.rsac

/**
 * Drives `CaptureTarget::Device("default")` (the mic path — RECORD_AUDIO
 * only, never MediaProjection) through the rsac C ABI from a real app uid.
 */
object NativeCaptureDriver {
    init {
        // Explicit ordering, deterministically: the shim's DT_NEEDED entry
        // for librsac_ffi.so resolves from the test APK's nativeLibraryDir,
        // but loading the dependency first removes any resolver ambiguity.
        System.loadLibrary("rsac_ffi")
        System.loadLibrary("rsac_androidtest_shim")
    }

    /**
     * Returns `[errorCode, buffers, frames, negRate, negChannels,
     * negSampleFormat]` — errorCode is the `rsac_error_t` of the first
     * failing FFI call (0 = RSAC_OK). Frames are counted, content is never
     * inspected.
     */
    external fun driveDefaultMicCapture(
        sampleRate: Int,
        channels: Int,
        timeoutMs: Int,
    ): LongArray

    /** rsac_error_message() text captured at the last failure ("" if none). */
    external fun lastNativeError(): String
}
