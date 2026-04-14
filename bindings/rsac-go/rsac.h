/*
 * rsac.h — C FFI header for the rsac (Rust Cross-Platform Audio Capture) library.
 *
 * This header defines the C API surface exposed by the rsac-ffi Rust crate.
 * The Go bindings (rsac-go) use CGo to call these functions.
 *
 * All opaque handles are non-null on success. NULL indicates an error;
 * call rsac_error_message() to retrieve the thread-local error string.
 *
 * Thread safety: All functions are safe to call from any thread unless
 * otherwise noted. Opaque handles must not be shared across threads
 * without external synchronization — the Go bindings enforce this via
 * their own mutex wrappers.
 */

#ifndef RSAC_H
#define RSAC_H

#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── Error codes ─────────────────────────────────────────────────────── */

typedef enum {
    RSAC_OK                         = 0,
    RSAC_ERROR_INVALID_PARAMETER    = 1,
    RSAC_ERROR_UNSUPPORTED_FORMAT   = 2,
    RSAC_ERROR_CONFIGURATION        = 3,
    RSAC_ERROR_DEVICE_NOT_FOUND     = 4,
    RSAC_ERROR_DEVICE_NOT_AVAILABLE = 5,
    RSAC_ERROR_DEVICE_ENUMERATION   = 6,
    RSAC_ERROR_STREAM_CREATION      = 7,
    RSAC_ERROR_STREAM_START         = 8,
    RSAC_ERROR_STREAM_STOP          = 9,
    RSAC_ERROR_STREAM_READ          = 10,
    RSAC_ERROR_BUFFER_OVERRUN       = 11,
    RSAC_ERROR_BUFFER_UNDERRUN      = 12,
    RSAC_ERROR_BACKEND              = 13,
    RSAC_ERROR_BACKEND_NOT_AVAILABLE = 14,
    RSAC_ERROR_BACKEND_INIT         = 15,
    RSAC_ERROR_APP_NOT_FOUND        = 16,
    RSAC_ERROR_APP_CAPTURE_FAILED   = 17,
    RSAC_ERROR_PLATFORM_NOT_SUPPORTED = 18,
    RSAC_ERROR_PERMISSION_DENIED    = 19,
    RSAC_ERROR_INTERNAL             = 20,
    RSAC_ERROR_TIMEOUT              = 21,
    RSAC_ERROR_NULL_POINTER         = 22,
    RSAC_ERROR_NOT_RUNNING          = 23,
    RSAC_ERROR_ALREADY_RUNNING      = 24,
} rsac_error_t;

/* ── Thread-local error message ──────────────────────────────────────── */

/*
 * Returns the last error message for the current thread.
 * The returned pointer is valid until the next rsac_* call on the same thread.
 * Returns NULL if no error has occurred.
 */
const char* rsac_error_message(void);

/* ── Opaque handles ──────────────────────────────────────────────────── */

typedef struct rsac_builder     rsac_builder_t;
typedef struct rsac_capture     rsac_capture_t;
typedef struct rsac_audio_buffer rsac_audio_buffer_t;
typedef struct rsac_capabilities rsac_capabilities_t;
typedef struct rsac_device_enumerator rsac_device_enumerator_t;
typedef struct rsac_device      rsac_device_t;
typedef struct rsac_device_list rsac_device_list_t;

/* ── Builder ─────────────────────────────────────────────────────────── */

/*
 * Creates a new capture builder with default settings.
 * Returns NULL on failure (check rsac_error_message()).
 */
rsac_builder_t* rsac_builder_new(void);

/* Set capture target to system default audio. */
rsac_error_t rsac_builder_set_target_system(rsac_builder_t* builder);

/* Set capture target to a specific application by name. */
rsac_error_t rsac_builder_set_target_app_by_name(rsac_builder_t* builder, const char* name);

/* Set capture target to a process tree rooted at the given PID. */
rsac_error_t rsac_builder_set_target_process_tree(rsac_builder_t* builder, uint32_t pid);

/* Set capture target to a specific device by ID string. */
rsac_error_t rsac_builder_set_target_device(rsac_builder_t* builder, const char* device_id);

/* Set capture target to a specific application by PID. */
rsac_error_t rsac_builder_set_target_app_by_pid(rsac_builder_t* builder, const char* app_id);

/* Set the desired sample rate in Hz. */
rsac_error_t rsac_builder_set_sample_rate(rsac_builder_t* builder, uint32_t rate);

/* Set the desired number of audio channels. */
rsac_error_t rsac_builder_set_channels(rsac_builder_t* builder, uint16_t channels);

/* Set the desired buffer size in frames (0 = platform default). */
rsac_error_t rsac_builder_set_buffer_size(rsac_builder_t* builder, uint32_t frames);

/*
 * Build the capture instance from the builder.
 * Consumes the builder (builder pointer becomes invalid).
 * Returns NULL on failure.
 */
rsac_capture_t* rsac_builder_build(rsac_builder_t* builder);

/* Free a builder without building (cleanup on error paths). */
void rsac_builder_free(rsac_builder_t* builder);

/* ── Capture ─────────────────────────────────────────────────────────── */

/* Start capturing audio. */
rsac_error_t rsac_capture_start(rsac_capture_t* capture);

/* Stop capturing audio. */
rsac_error_t rsac_capture_stop(rsac_capture_t* capture);

/* Returns true if capture is currently running. */
bool rsac_capture_is_running(const rsac_capture_t* capture);

/* Returns the current overrun (dropped buffer) count. */
uint64_t rsac_capture_overrun_count(const rsac_capture_t* capture);

/*
 * Try to read an audio buffer without blocking.
 * Returns NULL if no data is available (not an error).
 * On error, returns NULL and sets the thread-local error message.
 * Use rsac_last_error() to distinguish "no data" from "error".
 */
rsac_audio_buffer_t* rsac_capture_try_read(rsac_capture_t* capture);

/*
 * Read an audio buffer, blocking until data is available.
 * Returns NULL on error (check rsac_error_message()).
 */
rsac_audio_buffer_t* rsac_capture_read(rsac_capture_t* capture);

/*
 * Returns the last error code from the most recent operation on this capture.
 * Use after rsac_capture_try_read() returns NULL to distinguish no-data from error.
 */
rsac_error_t rsac_capture_last_error(const rsac_capture_t* capture);

/* ── Callback (push model) ───────────────────────────────────────────── */

/*
 * Callback function type for push-based audio delivery.
 * Called on a background thread with each captured audio buffer.
 * The buffer is valid only for the duration of the callback.
 */
typedef void (*rsac_audio_callback_t)(const rsac_audio_buffer_t* buffer, void* user_data);

/*
 * Set a callback for push-based audio delivery.
 * Must be called before rsac_capture_start().
 * Pass NULL to clear the callback.
 */
rsac_error_t rsac_capture_set_callback(
    rsac_capture_t* capture,
    rsac_audio_callback_t callback,
    void* user_data
);

/* Free the capture instance and all associated resources. */
void rsac_capture_free(rsac_capture_t* capture);

/* ── Audio Buffer ────────────────────────────────────────────────────── */

/* Returns a pointer to the interleaved f32 sample data. */
const float* rsac_audio_buffer_data(const rsac_audio_buffer_t* buffer);

/* Returns the number of frames in the buffer. */
uint32_t rsac_audio_buffer_num_frames(const rsac_audio_buffer_t* buffer);

/* Returns the number of channels. */
uint16_t rsac_audio_buffer_channels(const rsac_audio_buffer_t* buffer);

/* Returns the sample rate in Hz. */
uint32_t rsac_audio_buffer_sample_rate(const rsac_audio_buffer_t* buffer);

/* Returns the total number of samples (frames * channels). */
uint32_t rsac_audio_buffer_num_samples(const rsac_audio_buffer_t* buffer);

/* Free an audio buffer. */
void rsac_audio_buffer_free(rsac_audio_buffer_t* buffer);

/* ── Platform Capabilities ───────────────────────────────────────────── */

/* Query platform capabilities. Caller must free with rsac_capabilities_free(). */
rsac_capabilities_t* rsac_capabilities_query(void);

bool rsac_capabilities_supports_system_capture(const rsac_capabilities_t* caps);
bool rsac_capabilities_supports_app_capture(const rsac_capabilities_t* caps);
bool rsac_capabilities_supports_process_tree(const rsac_capabilities_t* caps);
bool rsac_capabilities_supports_device_selection(const rsac_capabilities_t* caps);
const char* rsac_capabilities_backend_name(const rsac_capabilities_t* caps);
uint32_t rsac_capabilities_max_channels(const rsac_capabilities_t* caps);
uint32_t rsac_capabilities_min_sample_rate(const rsac_capabilities_t* caps);
uint32_t rsac_capabilities_max_sample_rate(const rsac_capabilities_t* caps);

void rsac_capabilities_free(rsac_capabilities_t* caps);

/* ── Device Enumeration ──────────────────────────────────────────────── */

/* Create a device enumerator. Caller must free with rsac_device_enumerator_free(). */
rsac_device_enumerator_t* rsac_device_enumerator_new(void);

/*
 * Enumerate all audio devices.
 * Returns a device list. Caller must free with rsac_device_list_free().
 */
rsac_device_list_t* rsac_enumerate_devices(rsac_device_enumerator_t* enumerator);

/*
 * Get the default device of the given kind.
 * kind: 0 = Input, 1 = Output.
 * Returns NULL on error.
 */
rsac_device_t* rsac_default_device(rsac_device_enumerator_t* enumerator, uint32_t kind);

/* Device list accessors. */
uint32_t rsac_device_list_count(const rsac_device_list_t* list);
const rsac_device_t* rsac_device_list_get(const rsac_device_list_t* list, uint32_t index);
void rsac_device_list_free(rsac_device_list_t* list);

/* Device accessors. */
const char* rsac_device_id(const rsac_device_t* device);
const char* rsac_device_name(const rsac_device_t* device);
bool rsac_device_is_default(const rsac_device_t* device);
void rsac_device_free(rsac_device_t* device);

void rsac_device_enumerator_free(rsac_device_enumerator_t* enumerator);

#ifdef __cplusplus
}
#endif

#endif /* RSAC_H */
