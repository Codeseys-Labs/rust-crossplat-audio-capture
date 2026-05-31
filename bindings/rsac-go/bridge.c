/*
 * bridge.c — implementation of the cgo bridge shims declared in bridge.h.
 *
 * goAudioCallback is the Go function exported via //export in callback.go; cgo
 * emits its prototype into _cgo_export.h, which we include here.
 */

#include "_cgo_export.h"
#include "bridge.h"

/*
 * Typed forwarding shim with EXACTLY the rsac_audio_callback_t signature
 * (const float* first arg). cgo emits goAudioCallback with a non-const
 * `float*` first parameter, so casting its address to rsac_audio_callback_t
 * and calling through that cast is a call through an incompatible function
 * pointer type — undefined behavior in C (C11 6.3.2.3/8, 6.5.2.2/9). This
 * shim IS a valid rsac_audio_callback_t, and the implicit `const float*` ->
 * `float*` argument pass to goAudioCallback is a normal well-defined qualifier
 * drop, so there is no UB and no cast of the function pointer itself.
 */
static void rsac_go_audio_trampoline(const float* buffer_data,
                                     size_t num_samples,
                                     uint16_t channels,
                                     uint32_t sample_rate,
                                     void* user_data) {
    goAudioCallback((float*)buffer_data, num_samples, channels, sample_rate, user_data);
}

rsac_error_t rsac_go_set_callback(RsacCapture* capture, uintptr_t handle) {
    return rsac_capture_set_callback(capture,
                                     rsac_go_audio_trampoline,
                                     (void*)handle);
}
