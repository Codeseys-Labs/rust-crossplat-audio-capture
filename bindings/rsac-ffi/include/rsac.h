/**
 * rsac.h — C header for the rsac (Rust Cross-Platform Audio Capture) FFI library.
 *
 * This header exposes rsac's streaming-first audio capture API through
 * opaque handle types and error-code-returning functions.
 *
 * Memory ownership:
 *   - Handles returned by _new / _build / _query functions are heap-allocated.
 *     The caller MUST free them with the corresponding _free function.
 *   - String pointers returned by rsac_error_message, rsac_device_name, etc.
 *     are owned by the library and valid until the next FFI call on the same thread.
 *
 * Error handling:
 *   - All functions return rsac_error_t. On error, call rsac_error_message()
 *     for a human-readable description.
 */

#ifndef RSAC_H
#define RSAC_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── Error codes ────────────────────────────────────────────────────── */

typedef enum {
    RSAC_OK                          = 0,
    RSAC_ERROR_NULL_POINTER          = 1,
    RSAC_ERROR_INVALID_PARAMETER     = 2,
    RSAC_ERROR_DEVICE_NOT_FOUND      = 3,
    RSAC_ERROR_PLATFORM_NOT_SUPPORTED = 4,
    RSAC_ERROR_STREAM_FAILED         = 5,
    RSAC_ERROR_STREAM_READ           = 6,
    RSAC_ERROR_CONFIGURATION         = 7,
    RSAC_ERROR_APPLICATION_NOT_FOUND = 8,
    RSAC_ERROR_BACKEND               = 9,
    RSAC_ERROR_PERMISSION_DENIED     = 10,
    RSAC_ERROR_TIMEOUT               = 11,
    RSAC_ERROR_INTERNAL              = 12,
    RSAC_ERROR_PANIC                 = 99,
} rsac_error_t;

/* ── Device kind ────────────────────────────────────────────────────── */

typedef enum {
    RSAC_DEVICE_INPUT  = 0,
    RSAC_DEVICE_OUTPUT = 1,
} rsac_device_kind_t;

/* ── Opaque handle types ────────────────────────────────────────────── */

/** Opaque handle to an AudioCaptureBuilder. */
typedef struct RsacBuilder        RsacBuilder;
/** Opaque handle to an AudioCapture session. */
typedef struct RsacCapture        RsacCapture;
/** Opaque handle to a captured audio buffer. */
typedef struct RsacAudioBuffer    RsacAudioBuffer;
/** Opaque handle to a device enumerator. */
typedef struct RsacDeviceEnumerator RsacDeviceEnumerator;
/** Opaque handle to a list of audio devices. */
typedef struct RsacDeviceList     RsacDeviceList;
/** Opaque handle to a single audio device. */
typedef struct RsacDevice         RsacDevice;
/** Opaque handle to platform capabilities. */
typedef struct RsacCapabilities   RsacCapabilities;

/* ── Callback type ──────────────────────────────────────────────────── */

/**
 * Callback function type for push-based audio delivery.
 *
 * @param buffer_data  Pointer to interleaved f32 sample data.
 * @param num_samples  Total number of f32 values in buffer_data.
 * @param channels     Number of audio channels.
 * @param sample_rate  Sample rate in Hz.
 * @param user_data    Opaque pointer passed to rsac_capture_set_callback.
 */
typedef void (*rsac_audio_callback_t)(
    const float*  buffer_data,
    size_t        num_samples,
    uint16_t      channels,
    uint32_t      sample_rate,
    void*         user_data
);

/* ── Error retrieval ────────────────────────────────────────────────── */

/**
 * Returns the last error message for the current thread.
 * The returned pointer is valid until the next rsac FFI call on this thread.
 * Returns an empty string if no error has occurred.
 */
const char* rsac_error_message(void);

/* ── Builder ────────────────────────────────────────────────────────── */

/**
 * Creates a new AudioCaptureBuilder with default settings.
 * The caller must free it with rsac_builder_free().
 */
rsac_error_t rsac_builder_new(RsacBuilder** out);

/** Frees a builder handle. No-op if null. */
void rsac_builder_free(RsacBuilder* builder);

/** Sets the capture target to system default audio. */
rsac_error_t rsac_builder_set_target_system(RsacBuilder* builder);

/** Sets the capture target to a specific device by ID. */
rsac_error_t rsac_builder_set_target_device(RsacBuilder* builder,
                                            const char* device_id);

/** Sets the capture target to an application by name. */
rsac_error_t rsac_builder_set_target_app_by_name(RsacBuilder* builder,
                                                  const char* app_name);

/** Sets the capture target to an application by ID. */
rsac_error_t rsac_builder_set_target_app_by_id(RsacBuilder* builder,
                                                const char* app_id);

/** Sets the capture target to a process tree by PID. */
rsac_error_t rsac_builder_set_target_process_tree(RsacBuilder* builder,
                                                   uint32_t pid);

/** Sets the desired sample rate in Hz. */
rsac_error_t rsac_builder_set_sample_rate(RsacBuilder* builder,
                                           uint32_t sample_rate);

/** Sets the desired number of audio channels. */
rsac_error_t rsac_builder_set_channels(RsacBuilder* builder,
                                        uint16_t channels);

/**
 * Validates the builder and creates an AudioCapture handle.
 * On success, *out receives the capture handle. The builder is consumed.
 * On failure, *out is null and the builder is also consumed.
 */
rsac_error_t rsac_builder_build(RsacBuilder* builder, RsacCapture** out);

/* ── Capture lifecycle ──────────────────────────────────────────────── */

/** Starts the audio capture stream. */
rsac_error_t rsac_capture_start(RsacCapture* capture);

/** Stops the audio capture stream. */
rsac_error_t rsac_capture_stop(RsacCapture* capture);

/**
 * Returns 1 if the capture is running, 0 if stopped, -1 on error (null).
 */
int32_t rsac_capture_is_running(const RsacCapture* capture);

/**
 * Returns the number of ring buffer overruns (dropped buffers).
 * Returns 0 if the capture handle is null.
 */
uint64_t rsac_capture_overrun_count(const RsacCapture* capture);

/** Frees a capture handle. Stops the stream if running. No-op if null. */
void rsac_capture_free(RsacCapture* capture);

/* ── Reading audio data ─────────────────────────────────────────────── */

/**
 * Non-blocking read. On success with data, *out receives a buffer handle.
 * On success with no data available, *out is null and RSAC_OK is returned.
 * The buffer must be freed with rsac_audio_buffer_free().
 */
rsac_error_t rsac_capture_try_read(RsacCapture* capture,
                                    RsacAudioBuffer** out);

/**
 * Blocking read. Blocks until an audio buffer is available.
 * The buffer must be freed with rsac_audio_buffer_free().
 */
rsac_error_t rsac_capture_read(RsacCapture* capture,
                                RsacAudioBuffer** out);

/* ── Callback-based capture ─────────────────────────────────────────── */

/**
 * Sets a push-based callback for audio delivery.
 * Must be called before rsac_capture_start().
 * Pass callback=NULL to clear the callback.
 *
 * The callback is invoked on a background thread. The caller is responsible
 * for thread safety of user_data.
 */
rsac_error_t rsac_capture_set_callback(RsacCapture* capture,
                                        rsac_audio_callback_t callback,
                                        void* user_data);

/* ── Audio buffer accessors ─────────────────────────────────────────── */

/**
 * Returns a pointer to interleaved f32 sample data.
 * Valid until rsac_audio_buffer_free() is called. Returns null if buffer is null.
 */
const float* rsac_audio_buffer_data(const RsacAudioBuffer* buffer);

/** Returns the total number of f32 samples (frames * channels). */
size_t rsac_audio_buffer_len(const RsacAudioBuffer* buffer);

/** Returns the number of audio frames. */
size_t rsac_audio_buffer_num_frames(const RsacAudioBuffer* buffer);

/** Returns the number of audio channels. */
uint16_t rsac_audio_buffer_channels(const RsacAudioBuffer* buffer);

/** Returns the sample rate in Hz. */
uint32_t rsac_audio_buffer_sample_rate(const RsacAudioBuffer* buffer);

/** Frees an audio buffer handle. No-op if null. */
void rsac_audio_buffer_free(RsacAudioBuffer* buffer);

/* ── Device enumeration ─────────────────────────────────────────────── */

/**
 * Creates a new device enumerator. Must be freed with
 * rsac_device_enumerator_free().
 */
rsac_error_t rsac_device_enumerator_new(RsacDeviceEnumerator** out);

/** Frees a device enumerator handle. No-op if null. */
void rsac_device_enumerator_free(RsacDeviceEnumerator* enumerator);

/**
 * Enumerates all audio devices into a device list.
 * Must be freed with rsac_device_list_free().
 */
rsac_error_t rsac_device_list_new(const RsacDeviceEnumerator* enumerator,
                                   RsacDeviceList** out);

/** Returns the number of devices in the list. Returns 0 if null. */
size_t rsac_device_list_count(const RsacDeviceList* list);

/**
 * Gets a device from the list by index. The returned device must be freed
 * with rsac_device_free(). Returns an error if index is out of bounds.
 */
rsac_error_t rsac_device_list_get(const RsacDeviceList* list,
                                   size_t index,
                                   RsacDevice** out);

/** Frees a device list handle. No-op if null. */
void rsac_device_list_free(RsacDeviceList* list);

/**
 * Gets the default audio device. The returned device must be freed
 * with rsac_device_free().
 */
rsac_error_t rsac_default_device(const RsacDeviceEnumerator* enumerator,
                                  rsac_device_kind_t kind,
                                  RsacDevice** out);

/* ── Device accessors ───────────────────────────────────────────────── */

/**
 * Returns the device name as a NUL-terminated C string.
 * Valid until rsac_device_free() is called on this device. Returns null on error.
 */
const char* rsac_device_name(const RsacDevice* device);

/**
 * Returns the device ID as a NUL-terminated C string.
 * Valid until rsac_device_free() is called on this device. Returns null on error.
 */
const char* rsac_device_id(const RsacDevice* device);

/** Returns 1 if the device is the system default, 0 otherwise, -1 if null. */
int32_t rsac_device_is_default(const RsacDevice* device);

/** Frees a device handle. No-op if null. */
void rsac_device_free(RsacDevice* device);

/* ── Platform capabilities ──────────────────────────────────────────── */

/**
 * Queries platform capabilities. Must be freed with rsac_capabilities_free().
 */
rsac_error_t rsac_capabilities_query(RsacCapabilities** out);

/** Frees a capabilities handle. No-op if null. */
void rsac_capabilities_free(RsacCapabilities* caps);

/** Returns 1 if system capture is supported, 0 if not, -1 if null. */
int32_t rsac_capabilities_supports_system_capture(const RsacCapabilities* caps);

/** Returns 1 if application capture is supported, 0 if not, -1 if null. */
int32_t rsac_capabilities_supports_app_capture(const RsacCapabilities* caps);

/** Returns 1 if process tree capture is supported, 0 if not, -1 if null. */
int32_t rsac_capabilities_supports_process_tree(const RsacCapabilities* caps);

/** Returns 1 if device selection is supported, 0 if not, -1 if null. */
int32_t rsac_capabilities_supports_device_selection(const RsacCapabilities* caps);

/** Returns the maximum number of channels supported. Returns 0 if null. */
uint16_t rsac_capabilities_max_channels(const RsacCapabilities* caps);

/**
 * Returns the backend name (e.g. "WASAPI", "CoreAudio", "PipeWire").
 * Valid until the next call on the same thread. Returns null if null.
 */
const char* rsac_capabilities_backend_name(const RsacCapabilities* caps);

/* ── Version info ───────────────────────────────────────────────────── */

/**
 * Returns the rsac-ffi version string (e.g. "0.1.0").
 * The returned pointer is a static string valid for the library's lifetime.
 */
const char* rsac_version(void);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* RSAC_H */
