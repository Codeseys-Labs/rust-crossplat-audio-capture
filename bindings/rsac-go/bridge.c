/*
 * bridge.c — implementation of the cgo bridge shims declared in bridge.h.
 *
 * goAudioCallback is the Go function exported via //export in callback.go; cgo
 * emits its prototype into _cgo_export.h, which we include here.
 */

#include "_cgo_export.h"
#include "bridge.h"

rsac_error_t rsac_go_set_callback(RsacCapture* capture, uintptr_t handle) {
    /* cgo generates goAudioCallback with a `float*` (non-const) first argument,
     * while rsac_audio_callback_t takes `const float*`. The cast reconciles the
     * (compatible) const qualifier without a warning; the function is identical. */
    return rsac_capture_set_callback(capture,
                                     (rsac_audio_callback_t)goAudioCallback,
                                     (void*)handle);
}
