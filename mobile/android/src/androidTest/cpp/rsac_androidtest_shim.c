/*
 * rsac_androidtest_shim.c — TEST-ONLY JNI bridge driving the SHIPPED rsac C
 * ABI (librsac_ffi.so) from the instrumented androidTest APK (rsac-255b).
 *
 * Why this exists: librsac.so (the AAR's native lib) exports only JNI_OnLoad,
 * which RegisterNatives-binds the playback-ingest surface (nativePush /
 * nativeSessionEnded / nativeDevicesChanged) — no shipped symbol opens a
 * Device("default") mic capture, and Kotlin `external fun` can never bind a
 * bare C symbol like rsac_builder_new. This shim is the smallest honest
 * bridge: one JNI entry point that drives the rsac-ffi C API verbatim, so a
 * REAL app uid (not adb's shell uid, which gets AAUDIO_ERROR_INTERNAL) proves
 * frames-delivered through the exact surface Flutter/C consumers use.
 *
 * Scope discipline:
 *   - This is the MIC path (Device("default")): RECORD_AUDIO only — no
 *     MediaProjection token, no foreground service, no consent dialog. Do not
 *     conflate with the SystemDefault playback tier.
 *   - Content is NEVER inspected: only rsac_audio_buffer_num_frames() is
 *     read; rsac_audio_buffer_data()/rms/peak are never called.
 *   - Packaging: compiled by CI with plain NDK clang (no CMake — an
 *     android.externalNativeBuild block is module-global and would leak this
 *     shim into the production rsac.aar). The .so lands in
 *     src/androidTest/jniLibs/x86_64/, which AGP packages into the TEST APK
 *     only. Both .so files are CI-generated and git-ignored.
 *
 * A pass here is emulator-verified, never device-verified.
 */

#include <jni.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

#include "rsac.h"

/*
 * Last failure message, captured from rsac_error_message() at the failure
 * site (that pointer is only valid until the next rsac FFI call on the same
 * thread, so it must be copied out immediately). Read back by the Kotlin
 * side via lastNativeError() on the same (instrumentation) thread — the
 * driver is single-threaded, so a plain static buffer is race-free here.
 */
static char g_last_error[512];

static void capture_error(const char *stage) {
    const char *msg = rsac_error_message();
    snprintf(g_last_error, sizeof(g_last_error), "%s: %s", stage,
             (msg != NULL && msg[0] != '\0') ? msg : "(no message)");
}

static int64_t now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

/*
 * Drives the shipped C ABI end-to-end:
 *   rsac_builder_new -> set_target_device("default") -> set_sample_rate ->
 *   set_channels -> rsac_builder_build -> rsac_capture_start -> bounded
 *   rsac_capture_try_read poll loop (counting num_frames, freeing every
 *   buffer) until buffers >= 3 && frames > 0 or the deadline ->
 *   rsac_capture_format -> rsac_capture_request_stop -> rsac_capture_free.
 *
 * Returns jlong[6]: [errorCode, buffers, frames, negRate, negChannels,
 * negSampleFormat]. errorCode is the rsac_error_t of the first failing call
 * (0 = RSAC_OK); on failure the negotiated fields stay 0/0/-1 and
 * lastNativeError() carries the rsac_error_message() text.
 */
JNIEXPORT jlongArray JNICALL
Java_ai_codeseys_rsac_NativeCaptureDriver_driveDefaultMicCapture(
    JNIEnv *env, jobject thiz, jint sample_rate, jint channels,
    jint timeout_ms) {
    jlong out[6] = {0, 0, 0, 0, 0, -1};
    rsac_error_t rc;
    RsacBuilder *builder = NULL;
    RsacCapture *capture = NULL;

    (void)thiz;
    g_last_error[0] = '\0';

    rc = rsac_builder_new(&builder);
    if (rc != RSAC_OK) {
        capture_error("rsac_builder_new");
        out[0] = (jlong)rc;
        goto done;
    }

    /* "default" == DEFAULT_INPUT_DEVICE_ID (src/audio/android/mod.rs). */
    rc = rsac_builder_set_target_device(builder, "default");
    if (rc == RSAC_OK) {
        rc = rsac_builder_set_sample_rate(builder, (uint32_t)sample_rate);
    }
    if (rc == RSAC_OK) {
        rc = rsac_builder_set_channels(builder, (uint16_t)channels);
    }
    if (rc != RSAC_OK) {
        capture_error("builder setter");
        rsac_builder_free(builder); /* setters do NOT consume the builder */
        out[0] = (jlong)rc;
        goto done;
    }

    /* build() ALWAYS consumes the builder, success or failure (rsac.h). */
    rc = rsac_builder_build(builder, &capture);
    builder = NULL;
    if (rc != RSAC_OK) {
        capture_error("rsac_builder_build");
        out[0] = (jlong)rc;
        goto done;
    }

    rc = rsac_capture_start(capture);
    if (rc != RSAC_OK) {
        capture_error("rsac_capture_start");
        out[0] = (jlong)rc;
        goto cleanup;
    }

    {
        const int64_t deadline = now_ms() + (int64_t)timeout_ms;
        jlong buffers = 0;
        jlong frames = 0;
        while (now_ms() < deadline && !(buffers >= 3 && frames > 0)) {
            RsacAudioBuffer *buf = NULL;
            rc = rsac_capture_try_read(capture, &buf);
            if (rc != RSAC_OK) {
                capture_error("rsac_capture_try_read");
                out[0] = (jlong)rc;
                break;
            }
            if (buf != NULL) {
                buffers += 1;
                /* Frames are COUNTED; sample content is never read. */
                frames += (jlong)rsac_audio_buffer_num_frames(buf);
                rsac_audio_buffer_free(buf);
            } else {
                struct timespec pause = {0, 10 * 1000 * 1000}; /* 10 ms */
                nanosleep(&pause, NULL);
            }
        }
        out[1] = buffers;
        out[2] = frames;
    }

    if (out[0] == (jlong)RSAC_OK) {
        RsacAudioFormat fmt;
        memset(&fmt, 0, sizeof(fmt));
        rc = rsac_capture_format(capture, &fmt);
        if (rc == RSAC_OK) {
            out[3] = (jlong)fmt.sample_rate;
            out[4] = (jlong)fmt.channels;
            out[5] = (jlong)fmt.sample_format;
        } else {
            capture_error("rsac_capture_format");
            out[0] = (jlong)rc;
        }
    }

cleanup:
    /* Stop, THEN free — never free with a read conceivably in flight. */
    (void)rsac_capture_request_stop(capture);
    rsac_capture_free(capture);
done: {
    jlongArray arr = (*env)->NewLongArray(env, 6);
    if (arr == NULL) {
        return NULL; /* OutOfMemoryError already pending */
    }
    (*env)->SetLongArrayRegion(env, arr, 0, 6, out);
    return arr;
}
}

/* Message captured at the first failing call of the LAST drive; "" if none. */
JNIEXPORT jstring JNICALL
Java_ai_codeseys_rsac_NativeCaptureDriver_lastNativeError(JNIEnv *env,
                                                          jobject thiz) {
    (void)thiz;
    return (*env)->NewStringUTF(env, g_last_error);
}
