/*
 * rsac.h — C FFI header for the rsac (Rust Cross-Platform Audio Capture) library.
 *
 * This header mirrors the C ABI exposed by the rsac-ffi Rust crate. It is kept
 * in lockstep with bindings/rsac-ffi/include/rsac.h (the source of truth, which
 * matches the #[no_mangle] symbols in bindings/rsac-ffi/src/lib.rs). The Go
 * bindings (rsac-go) use cgo to call these functions.
 *
 * ABI conventions:
 *   - Fallible operations return rsac_error_t (RSAC_OK == 0) and deliver any
 *     produced handle through an out-pointer (T** out). On error *out is set
 *     to NULL. On RSAC_OK with no data (e.g. a non-blocking read with an empty
 *     ring) *out may also be NULL.
 *   - Pure accessors return their value directly. int32_t accessors use -1 as
 *     the "handle was NULL" sentinel; size/count/rate accessors return 0.
 *   - Handles returned via _new / _build / _query / _list_get / _default_device
 *     are heap-allocated and MUST be freed with the matching _free function.
 *   - String pointers (rsac_error_message, rsac_device_name, ...) are owned by
 *     the library and valid until the next FFI call on the same thread.
 *
 * Thread safety: opaque handles must not be shared across threads without
 * external synchronization — the Go bindings enforce this via their own mutex.
 */

#ifndef RSAC_H
#define RSAC_H

#include <stdint.h>
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ── Error codes (discriminants match rsac_ffi::rsac_error_t) ─────────── */

typedef enum {
    RSAC_OK                           = 0,
    RSAC_ERROR_NULL_POINTER           = 1,
    RSAC_ERROR_INVALID_PARAMETER      = 2,
    RSAC_ERROR_DEVICE_NOT_FOUND       = 3,
    RSAC_ERROR_PLATFORM_NOT_SUPPORTED = 4,
    RSAC_ERROR_STREAM_FAILED          = 5,
    RSAC_ERROR_STREAM_READ            = 6,
    RSAC_ERROR_CONFIGURATION          = 7,
    RSAC_ERROR_APPLICATION_NOT_FOUND  = 8,
    RSAC_ERROR_BACKEND                = 9,
    RSAC_ERROR_PERMISSION_DENIED      = 10,
    RSAC_ERROR_TIMEOUT                = 11,
    RSAC_ERROR_INTERNAL               = 12,
    RSAC_ERROR_PANIC                  = 99,
} rsac_error_t;

/* ── Device kind ────────────────────────────────────────────────────── */

typedef enum {
    RSAC_DEVICE_INPUT  = 0,
    RSAC_DEVICE_OUTPUT = 1,
} rsac_device_kind_t;

/* ── Sample format ──────────────────────────────────────────────────── */

/**
 * Sample wire/storage format. All audio data is delivered as interleaved
 * f32 regardless of this value; it describes the negotiated wire format.
 */
typedef enum {
    RSAC_SAMPLE_FORMAT_I16 = 0,
    RSAC_SAMPLE_FORMAT_I24 = 1,
    RSAC_SAMPLE_FORMAT_I32 = 2,
    RSAC_SAMPLE_FORMAT_F32 = 3,
} rsac_sample_format_t;

/* ── Value (out-parameter) types ────────────────────────────────────── */

/**
 * Point-in-time snapshot of a capture's stream statistics.
 *
 * Filled by rsac_capture_stream_stats(). Plain value type — no heap, no free.
 * Before start (or after stop) every field is zero and is_running is 0.
 */
typedef struct {
    uint64_t buffers_captured; /**< Buffers delivered to the consumer. */
    uint64_t buffers_dropped;  /**< Buffers dropped to ring overflow. */
    uint64_t buffers_pushed;   /**< Buffers enqueued by the OS callback. */
    uint64_t overruns;         /**< Ring overruns (== buffers_dropped). */
    double   uptime_secs;      /**< Seconds running; 0.0 when not started. */
    double   dropped_ratio;    /**< Lost fraction in 0.0..=1.0. */
    int32_t  is_running;       /**< 1 if capturing, else 0. */
} RsacStreamStats;

/**
 * Point-in-time WINDOWED backpressure snapshot.
 *
 * Filled by rsac_capture_backpressure_report(). Plain value type — no heap, no
 * free. Unlike RsacStreamStats' lifetime counters, pushed/dropped cover a
 * bounded recent window, so drop_rate surfaces a sustained 1-in-N loss the
 * consecutive-drop flag resets away. window_secs is 0.0 when the span cannot be
 * attributed (unknown buffer size/rate); the tallies are still valid.
 */
typedef struct {
    double   window_secs;          /**< Span the tallies cover; 0.0 if unattributed. */
    uint64_t pushed;               /**< Buffers pushed within the window. */
    uint64_t dropped;              /**< Buffers dropped within the window. */
    double   drop_rate;            /**< Lost fraction in 0.0..=1.0. */
    int32_t  is_under_backpressure;/**< 1 if the legacy consecutive-drop flag is set. */
} RsacBackpressureReport;

/**
 * Point-in-time snapshot of a capture's negotiated delivery format.
 *
 * Filled by rsac_capture_format(). Plain value type — no heap, no free.
 */
typedef struct {
    uint32_t             sample_rate;     /**< Samples per second. */
    uint16_t             channels;        /**< Number of channels. */
    rsac_sample_format_t sample_format;   /**< Negotiated wire format. */
    uint16_t             bits_per_sample; /**< 16, 24, or 32. */
} RsacAudioFormat;

/* ── Opaque handle types ────────────────────────────────────────────── */

/** Opaque handle to an AudioCaptureBuilder. */
typedef struct RsacBuilder          RsacBuilder;
/** Opaque handle to an AudioCapture session. */
typedef struct RsacCapture          RsacCapture;
/** Opaque handle to a captured audio buffer. */
typedef struct RsacAudioBuffer      RsacAudioBuffer;
/** Opaque handle to a device enumerator. */
typedef struct RsacDeviceEnumerator RsacDeviceEnumerator;
/** Opaque handle to a list of audio devices. */
typedef struct RsacDeviceList       RsacDeviceList;
/** Opaque handle to a single audio device. */
typedef struct RsacDevice           RsacDevice;
/** Opaque handle to platform capabilities. */
typedef struct RsacCapabilities     RsacCapabilities;

/* ── Callback type ──────────────────────────────────────────────────── */

/**
 * Callback function type for push-based audio delivery.
 *
 * @param buffer_data  Pointer to interleaved f32 sample data (borrowed; valid
 *                     only for the duration of the callback).
 * @param num_samples  Total number of f32 values in buffer_data (frames * channels).
 * @param channels     Number of audio channels.
 * @param sample_rate  Sample rate in Hz.
 * @param user_data    Opaque pointer passed to rsac_capture_set_callback.
 */
typedef void (*rsac_audio_callback_t)(
    const float* buffer_data,
    size_t       num_samples,
    uint16_t     channels,
    uint32_t     sample_rate,
    void*        user_data
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
 * On success *out receives a handle that must be freed with rsac_builder_free().
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

/**
 * Sets the capture target by parsing a canonical target string.
 *
 * `spec` uses the CaptureTarget string grammar (case-insensitive scheme):
 *   "system" | "device:<id>" | "app:<pid-or-id>" | "name:<name>" | "tree:<pid>"
 * Convenience over the typed rsac_builder_set_target_* setters (which remain).
 *
 * A malformed string returns RSAC_ERROR_INVALID_PARAMETER and leaves the
 * builder's existing target unchanged (parse-then-commit). Returns
 * RSAC_ERROR_NULL_POINTER if builder or spec is null, and
 * RSAC_ERROR_INVALID_PARAMETER if spec is not valid UTF-8 or not a valid spec.
 */
rsac_error_t rsac_builder_set_target_str(RsacBuilder* builder,
                                         const char* spec);

/** Sets the desired sample rate in Hz. */
rsac_error_t rsac_builder_set_sample_rate(RsacBuilder* builder,
                                          uint32_t sample_rate);

/** Sets the desired number of audio channels. */
rsac_error_t rsac_builder_set_channels(RsacBuilder* builder,
                                       uint16_t channels);

/**
 * Validates the builder and creates an AudioCapture handle.
 * On success *out receives the capture handle. The builder is consumed
 * (and freed) regardless of outcome; on failure *out is NULL.
 */
rsac_error_t rsac_builder_build(RsacBuilder* builder, RsacCapture** out);

/* ── Capture lifecycle ──────────────────────────────────────────────── */

/** Starts the audio capture stream. */
rsac_error_t rsac_capture_start(RsacCapture* capture);

/** Stops the audio capture stream. */
rsac_error_t rsac_capture_stop(RsacCapture* capture);

/**
 * Best-effort unblock of a parked rsac_capture_read by transitioning the
 * stream to a terminal state. Idempotent; a no-op when no stream exists or it
 * is already stopped. Safe to call concurrently with an in-flight
 * rsac_capture_read / rsac_capture_try_read to unblock it. NOT safe to call
 * concurrently with rsac_capture_free — order request_stop + drain of
 * in-flight reads BEFORE free. Returns RSAC_ERROR_NULL_POINTER if capture is
 * null, else RSAC_OK.
 */
rsac_error_t rsac_capture_request_stop(const RsacCapture* capture);

/** Returns 1 if the capture is running, 0 if stopped, -1 on null handle. */
int32_t rsac_capture_is_running(const RsacCapture* capture);

/**
 * Returns the number of ring buffer overruns (dropped buffers).
 * Returns 0 if the capture handle is null or no stream exists.
 */
uint64_t rsac_capture_overrun_count(const RsacCapture* capture);

/**
 * Fills *out with a point-in-time stream-statistics snapshot.
 * *out is an out-parameter (not a handle) — nothing to free.
 * Before start (or after stop) the snapshot is all-zero (is_running == 0).
 * Returns RSAC_ERROR_NULL_POINTER if capture or out is null, else RSAC_OK.
 */
rsac_error_t rsac_capture_stream_stats(const RsacCapture* capture,
                                       RsacStreamStats* out);

/**
 * Fills *out with a point-in-time WINDOWED backpressure report.
 * *out is an out-parameter (not a handle) — nothing to free.
 * Unlike rsac_capture_stream_stats' lifetime counters, drop_rate here reflects
 * a bounded recent window. Before start (or after stop) the report is all-zero.
 * Returns RSAC_ERROR_NULL_POINTER if capture or out is null, else RSAC_OK.
 */
rsac_error_t rsac_capture_backpressure_report(const RsacCapture* capture,
                                              RsacBackpressureReport* out);

/**
 * Fills *out with the negotiated delivery format.
 * *out is an out-parameter (not a handle) — nothing to free.
 * Returns RSAC_ERROR_STREAM_FAILED (leaving *out untouched) when no stream
 * has been created yet; RSAC_ERROR_NULL_POINTER if capture or out is null.
 */
rsac_error_t rsac_capture_format(const RsacCapture* capture,
                                 RsacAudioFormat* out);

/** Frees a capture handle. Stops the stream if running. No-op if null. */
void rsac_capture_free(RsacCapture* capture);

/* ── Reading audio data ─────────────────────────────────────────────── */

/**
 * Non-blocking read. On success with data, *out receives a buffer handle.
 * On success with no data available, *out is NULL and RSAC_OK is returned.
 * The buffer must be freed with rsac_audio_buffer_free().
 *
 * Takes a const capture: it only reads, so it may run concurrently with
 * rsac_capture_request_stop. Still NOT safe against a concurrent
 * rsac_capture_free (free the handle only after draining in-flight reads).
 */
rsac_error_t rsac_capture_try_read(const RsacCapture* capture,
                                   RsacAudioBuffer** out);

/**
 * Blocking read. Blocks until an audio buffer is available.
 * On success *out receives a buffer handle that must be freed with
 * rsac_audio_buffer_free().
 *
 * Takes a const capture: a concurrent rsac_capture_request_stop can unblock a
 * thread parked here. Still NOT safe against a concurrent rsac_capture_free —
 * drain in-flight reads before freeing the handle.
 */
rsac_error_t rsac_capture_read(const RsacCapture* capture,
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

/** Returns the total number of f32 samples (frames * channels). 0 if null. */
size_t rsac_audio_buffer_len(const RsacAudioBuffer* buffer);

/** Returns the number of audio frames. 0 if null. */
size_t rsac_audio_buffer_num_frames(const RsacAudioBuffer* buffer);

/** Returns the number of audio channels. 0 if null. */
uint16_t rsac_audio_buffer_channels(const RsacAudioBuffer* buffer);

/** Returns the sample rate in Hz. 0 if null. */
uint32_t rsac_audio_buffer_sample_rate(const RsacAudioBuffer* buffer);

/**
 * Returns the RMS level across all samples/channels: sqrt(mean(x^2)).
 * Non-finite samples are skipped; silence/empty yields 0.0 (never NaN).
 * Read-only measurement. Returns 0.0 if buffer is null.
 */
float rsac_audio_buffer_rms(const RsacAudioBuffer* buffer);

/**
 * Returns the peak (max absolute) level across all samples/channels: max(|x|).
 * Non-finite samples are skipped; silence/empty yields 0.0 (never NaN).
 * Read-only measurement. Returns 0.0 if buffer is null.
 */
float rsac_audio_buffer_peak(const RsacAudioBuffer* buffer);

/**
 * Returns the RMS level in dBFS: 20*log10(rms). Full scale (RMS 1.0) == 0.0 dBFS.
 * Returns -infinity for silence/empty, and also -infinity if buffer is null.
 */
float rsac_audio_buffer_rms_dbfs(const RsacAudioBuffer* buffer);

/**
 * Returns the peak level in dBFS: 20*log10(peak). Full scale (peak 1.0) == 0.0 dBFS.
 * Returns -infinity for silence/empty, and also -infinity if buffer is null.
 */
float rsac_audio_buffer_peak_dbfs(const RsacAudioBuffer* buffer);

/** Frees an audio buffer handle. No-op if null. */
void rsac_audio_buffer_free(RsacAudioBuffer* buffer);

/* ── Device enumeration ─────────────────────────────────────────────── */

/**
 * Creates a new device enumerator.
 * On success *out must be freed with rsac_device_enumerator_free().
 */
rsac_error_t rsac_device_enumerator_new(RsacDeviceEnumerator** out);

/** Frees a device enumerator handle. No-op if null. */
void rsac_device_enumerator_free(RsacDeviceEnumerator* enumerator);

/**
 * Enumerates all audio devices into a device list.
 * On success *out must be freed with rsac_device_list_free().
 */
rsac_error_t rsac_device_list_new(const RsacDeviceEnumerator* enumerator,
                                  RsacDeviceList** out);

/** Returns the number of devices in the list. Returns 0 if null. */
size_t rsac_device_list_count(const RsacDeviceList* list);

/**
 * Gets a device from the list by index. On success *out receives a device
 * handle that must be freed with rsac_device_free(). Returns an error if the
 * index is out of bounds.
 */
rsac_error_t rsac_device_list_get(const RsacDeviceList* list,
                                  size_t index,
                                  RsacDevice** out);

/** Frees a device list handle. No-op if null. */
void rsac_device_list_free(RsacDeviceList* list);

/**
 * Gets the default audio device. On success *out receives a device handle
 * that must be freed with rsac_device_free(). Only RSAC_DEVICE_OUTPUT is
 * supported (rsac is a loopback capture library); RSAC_DEVICE_INPUT returns
 * RSAC_ERROR_INVALID_PARAMETER.
 */
rsac_error_t rsac_default_device(const RsacDeviceEnumerator* enumerator,
                                 rsac_device_kind_t kind,
                                 RsacDevice** out);

/* ── Device accessors ───────────────────────────────────────────────── */

/**
 * Returns the device name as a NUL-terminated C string.
 * Valid until the next rsac_device_name()/rsac_device_id() call on this thread.
 * Returns null if the device handle is null.
 */
const char* rsac_device_name(const RsacDevice* device);

/**
 * Returns the device ID as a NUL-terminated C string.
 * Valid until the next rsac_device_name()/rsac_device_id() call on this thread.
 * Returns null if the device handle is null.
 */
const char* rsac_device_id(const RsacDevice* device);

/** Returns 1 if the device is the system default, 0 if not, -1 if null. */
int32_t rsac_device_is_default(const RsacDevice* device);

/** Frees a device handle. No-op if null. */
void rsac_device_free(RsacDevice* device);

/* ── Platform capabilities ──────────────────────────────────────────── */

/**
 * Queries platform capabilities.
 * On success *out must be freed with rsac_capabilities_free().
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

/**
 * Returns 1 if the backend can deliver device hot-plug / default-change
 * notifications, 0 if not, -1 if null.
 */
int32_t rsac_capabilities_supports_device_change_notifications(const RsacCapabilities* caps);

/**
 * Returns 1 if starting a capture requires a config-time user-consent
 * artifact (mobile platforms; see docs/MOBILE_BACKEND_DESIGN.md), 0 if not,
 * -1 if null. Always 0 on the desktop backends.
 */
int32_t rsac_capabilities_requires_user_consent(const RsacCapabilities* caps);

/** Returns the maximum number of channels supported. Returns 0 if null. */
uint16_t rsac_capabilities_max_channels(const RsacCapabilities* caps);

/**
 * Returns the number of sample formats the backend supports.
 * Returns 0 if null.
 */
size_t rsac_capabilities_supported_sample_format_count(const RsacCapabilities* caps);

/**
 * Returns the supported sample format at `index` as one of the
 * rsac_sample_format_t constants, carried as a plain int32_t. Valid indices
 * are 0..rsac_capabilities_supported_sample_format_count(). Returns -1 if
 * the handle is null or `index` is out of bounds.
 */
int32_t rsac_capabilities_supported_sample_format_at(const RsacCapabilities* caps,
                                                     size_t index);

/**
 * Returns the minimum of the backend's device-negotiable sample-rate range,
 * in Hz. Returns 0 if null.
 */
uint32_t rsac_capabilities_min_sample_rate(const RsacCapabilities* caps);

/**
 * Returns the maximum of the backend's device-negotiable sample-rate range,
 * in Hz. Returns 0 if null.
 */
uint32_t rsac_capabilities_max_sample_rate(const RsacCapabilities* caps);

/**
 * Returns the number of entries in the builder's config-time sample-rate
 * whitelist (the exact set rsac_builder_set_sample_rate() values are
 * validated against at rsac_builder_build(); narrower than the min/max
 * device-negotiable range above). Returns 0 if null.
 */
size_t rsac_capabilities_supported_sample_rate_count(const RsacCapabilities* caps);

/**
 * Returns the config-time whitelisted sample rate at `index`, in Hz. Valid
 * indices are 0..rsac_capabilities_supported_sample_rate_count(). Returns 0
 * if the handle is null or `index` is out of bounds (0 is never a valid
 * sample rate).
 */
uint32_t rsac_capabilities_supported_sample_rate_at(const RsacCapabilities* caps,
                                                    size_t index);

/**
 * Returns the backend name (e.g. "WASAPI", "CoreAudio", "PipeWire").
 * Valid until the next call on the same thread. Returns null if null.
 */
const char* rsac_capabilities_backend_name(const RsacCapabilities* caps);

/* ── Multi-source composition (compose feature) ─────────────────────── */

/*
 * The declarations below exist only in a librsac_ffi built with the `compose`
 * cargo feature (cargo build -p rsac-ffi --features compose). Define
 * RSAC_FEATURE_COMPOSE when compiling C code against such a build; without it
 * this whole section is preprocessed away, matching a library that does not
 * export these symbols.
 *
 * Model: sources (capture targets) are declared in named GROUPS; each group
 * contributes output channels according to its layout (mono mixdown = 1,
 * stereo mixdown = 2, keep-channels = the single source's native width).
 * Groups append in declaration order into ONE interleaved-f32 stream at the
 * session rate. The composition owns its inner captures.
 *
 * Ownership / free order:
 *   - A group is consumed by a successful rsac_composition_builder_add_group;
 *     free it with rsac_group_free only if it was never added (or adding failed).
 *   - rsac_composition_builder_build ALWAYS consumes the builder (even on
 *     failure), like rsac_builder_build.
 *   - rsac_composition_free stops the compositor engine (joining its thread)
 *     and releases every inner capture. Buffers returned by reads own their
 *     samples and remain valid after the composition is freed.
 *   - rsac_composition_stop may run concurrently with a parked
 *     rsac_composition_read to unblock it; it is NOT safe concurrently with
 *     rsac_composition_free (stop + drain reads BEFORE freeing).
 *
 * Threading: rsac_composition_stop is the ONLY call on this surface that may
 * overlap another in-flight call on the same handle (see above). Every other
 * pair of calls on one composition (or group, or builder) handle must not
 * overlap from multiple threads — the caller must serialize them externally
 * (e.g. rsac_composition_start racing a parked rsac_composition_read is a
 * data race). Distinct handles are independent and need no synchronization.
 */
#if defined(RSAC_FEATURE_COMPOSE)

/**
 * How a group's sources map onto the composed output channels.
 *
 * Constants-only type: rsac_group_set_layout() takes its `layout` parameter
 * as a plain int32_t (C's implicit enum→int conversion keeps call sites
 * source-compatible), so an out-of-range integer is rejected with
 * RSAC_ERROR_INVALID_PARAMETER instead of being undefined behavior on the
 * Rust side of the ABI.
 */
typedef enum {
    /** Fold every source to mono; gain-weighted-sum into 1 output channel. */
    RSAC_GROUP_LAYOUT_MONO = 0,
    /** Fold every source to stereo; sum into 2 output channels (default). */
    RSAC_GROUP_LAYOUT_STEREO = 1,
    /** Pass the group's SINGLE source through with its native channels. */
    RSAC_GROUP_LAYOUT_KEEP_CHANNELS = 2,
} rsac_group_layout_t;

/**
 * Point-in-time snapshot of a running composition's counters.
 *
 * Filled by rsac_composition_stats(). Plain value type — no heap, no free.
 * Before rsac_composition_start() succeeds every field is 0.
 */
typedef struct {
    uint64_t ticks;          /**< Composed buffers (ticks) emitted so far. */
    uint64_t fallback_ticks; /**< Wall-clock fallback ticks (master stalled). */
    size_t   num_sources;    /**< Composed sources; valid indices are [0, num_sources). */
} RsacCompositionStats;

/**
 * Point-in-time snapshot of one composed source's counters.
 *
 * Filled by rsac_composition_source_stats(). Plain value type — no heap, no
 * free. The source's group name and target string are available via
 * rsac_composition_source_group() and rsac_composition_source_target().
 */
typedef struct {
    uint64_t buffers_received; /**< Buffers received from the inner capture. */
    uint64_t padded_frames;    /**< Silence frames inserted (source behind). */
    uint64_t trimmed_frames;   /**< Frames trimmed (source past buffering bound). */
    int32_t  resampling;       /**< 1 if resampled to the session rate, else 0. */
    int32_t  ended;            /**< 1 if the source's stream has ended, else 0. */
} RsacSourceStats;

/** Opaque handle to a composition builder. */
typedef struct RsacCompositionBuilder RsacCompositionBuilder;
/** Opaque handle to a composition group under construction. */
typedef struct RsacGroup              RsacGroup;
/** Opaque handle to a composition session. */
typedef struct RsacComposition        RsacComposition;

/* ── Group construction ── */

/**
 * Creates a composition group with the given name and the default stereo
 * layout. The name must be non-empty and unique within a composition (both
 * enforced at rsac_composition_builder_build(), not here). The caller owns
 * the handle: hand it to rsac_composition_builder_add_group() (which consumes
 * it) or free it with rsac_group_free().
 */
rsac_error_t rsac_group_new(const char* name, RsacGroup** out);

/**
 * Frees a group handle. No-op if null. Only for groups NOT consumed by a
 * successful rsac_composition_builder_add_group().
 */
void rsac_group_free(RsacGroup* group);

/**
 * Sets the group's layout. `layout` takes one of the rsac_group_layout_t
 * constants as a plain int32_t (implicit enum→int conversion — existing call
 * sites compile unchanged); any other value is rejected with
 * RSAC_ERROR_INVALID_PARAMETER and the group is unchanged. A keep-channels
 * group must contain exactly one source (arity enforced at
 * rsac_composition_builder_build()).
 */
rsac_error_t rsac_group_set_layout(RsacGroup* group,
                                   int32_t layout);

/**
 * Adds a capture source with unit gain (1.0). `spec` uses the same target
 * string grammar as rsac_builder_set_target_str():
 *   "system" | "device:<id>" | "app:<pid-or-id>" | "name:<name>" | "tree:<pid>"
 * On a parse error the group is unchanged and RSAC_ERROR_INVALID_PARAMETER
 * is returned.
 */
rsac_error_t rsac_group_add_source(RsacGroup* group, const char* spec);

/**
 * Adds a capture source with an explicit linear mixdown gain (1.0 = unity).
 * The gain must be finite and >= 0; invalid gains are rejected eagerly with
 * RSAC_ERROR_INVALID_PARAMETER (the group is unchanged on error).
 */
rsac_error_t rsac_group_add_source_with_gain(RsacGroup* group,
                                             const char* spec,
                                             float gain);

/* ── Composition builder ── */

/**
 * Creates a composition builder with default settings (48 kHz session rate,
 * no output clamping, no groups). Free with rsac_composition_builder_free()
 * unless consumed by rsac_composition_builder_build().
 */
rsac_error_t rsac_composition_builder_new(RsacCompositionBuilder** out);

/**
 * Frees a composition builder handle. No-op if null. Only for builders NOT
 * consumed by rsac_composition_builder_build().
 */
void rsac_composition_builder_free(RsacCompositionBuilder* builder);

/**
 * Sets the session sample rate in Hz (default 48000). Sources delivering a
 * different rate are resampled. Unsupported rates are rejected at
 * rsac_composition_builder_build().
 */
rsac_error_t rsac_composition_builder_set_sample_rate(
    RsacCompositionBuilder* builder,
    uint32_t sample_rate);

/**
 * Enables (nonzero) or disables (0) saturating output clamping to
 * [-1.0, 1.0] after summation. Default off.
 */
rsac_error_t rsac_composition_builder_set_clamp_output(
    RsacCompositionBuilder* builder,
    int32_t clamp);

/**
 * Sets the composed tick quantum (output buffer duration) in milliseconds
 * (default 10). The setter is thin — any value is accepted here; a ZERO
 * quantum is rejected at rsac_composition_builder_preflight() /
 * rsac_composition_builder_build() with RSAC_ERROR_CONFIGURATION. At start
 * the quantum is additionally clamped to at least one frame at the session
 * rate. Returns RSAC_ERROR_NULL_POINTER if builder is null.
 */
rsac_error_t rsac_composition_builder_set_quantum_ms(
    RsacCompositionBuilder* builder,
    uint64_t millis);

/**
 * Sets how long the compositor waits for the master-clock source before
 * emitting a wall-clock fallback tick (so a stalled master never freezes the
 * session), in milliseconds (default 250). Thin like the other setters: a
 * ZERO timeout is rejected at rsac_composition_builder_preflight() /
 * rsac_composition_builder_build() with RSAC_ERROR_CONFIGURATION.
 * Returns RSAC_ERROR_NULL_POINTER if builder is null.
 */
rsac_error_t rsac_composition_builder_set_stall_timeout_ms(
    RsacCompositionBuilder* builder,
    uint64_t millis);

/**
 * Sets the per-source buffering bound in milliseconds (default 1000). A
 * source drifting ahead of the master beyond this bound has its oldest
 * samples trimmed (RsacSourceStats.trimmed_frames). Any value — including
 * 0 — passes validation: the bound is clamped to at least one quantum when
 * the composition starts. Returns RSAC_ERROR_NULL_POINTER if builder is null.
 */
rsac_error_t rsac_composition_builder_set_max_buffer_ms(
    RsacCompositionBuilder* builder,
    uint64_t millis);

/**
 * Appends a group. Groups contribute output channels in the order added.
 * On RSAC_OK the group handle is CONSUMED — do not use or free it afterwards.
 * On any error (including a caught panic, RSAC_ERROR_PANIC) the caller still
 * owns the group: the handle is consumed only after the append has fully
 * succeeded.
 */
rsac_error_t rsac_composition_builder_add_group(
    RsacCompositionBuilder* builder,
    RsacGroup* group);

/**
 * Runs every device-independent validation rsac_composition_builder_build()
 * performs, WITHOUT consuming the builder. Because build always consumes its
 * builder (even on failure), this is how a C caller iterates: preflight, fix
 * the reported error on the SAME builder, preflight again, then build.
 *
 * RSAC_OK means build's validation phase would pass. It is NOT a guarantee
 * the composition will start: no devices are touched here, so device /
 * capability errors (device resolution, format negotiation, stream creation)
 * can still surface at rsac_composition_start().
 *
 * Error codes mirror build's validation phase exactly:
 * RSAC_ERROR_CONFIGURATION (no groups, empty group, duplicate/empty group
 * name, keep-channels group without exactly one source, invalid gain, too
 * many sources/channels, zero quantum or stall timeout),
 * RSAC_ERROR_INVALID_PARAMETER (unsupported session sample rate),
 * RSAC_ERROR_PLATFORM_NOT_SUPPORTED (a target this platform cannot capture).
 * Returns RSAC_ERROR_NULL_POINTER if builder is null.
 */
rsac_error_t rsac_composition_builder_preflight(
    const RsacCompositionBuilder* builder);

/**
 * Validates the configuration and builds a (not yet started) composition.
 * The builder is ALWAYS consumed, even on failure. No devices are touched
 * here — inner captures are created and started by rsac_composition_start().
 * On success *out must be freed with rsac_composition_free().
 *
 * Validation errors: RSAC_ERROR_CONFIGURATION (no groups, empty group,
 * duplicate/empty group name, keep-channels group without exactly one source,
 * invalid gain, too many sources/channels), RSAC_ERROR_INVALID_PARAMETER
 * (unsupported session sample rate), RSAC_ERROR_PLATFORM_NOT_SUPPORTED
 * (a target this platform cannot capture).
 */
rsac_error_t rsac_composition_builder_build(
    RsacCompositionBuilder* builder,
    RsacComposition** out);

/* ── Composition lifecycle ── */

/**
 * Builds and starts one capture per source, resolves the composed channel
 * layout, and spawns the compositor thread. On failure every already-started
 * inner capture is stopped. Starting twice returns RSAC_ERROR_CONFIGURATION.
 */
rsac_error_t rsac_composition_start(RsacComposition* comp);

/**
 * Signals the composition to stop (idempotent; RSAC_OK before start). Takes a
 * const composition: safe to call concurrently with an in-flight
 * rsac_composition_read / rsac_composition_try_read to unblock it. The
 * compositor thread is joined by rsac_composition_free(); do NOT call this
 * concurrently with free. An explicit stop discards any buffered composed
 * tail — read until the terminal error first to capture everything.
 */
rsac_error_t rsac_composition_stop(const RsacComposition* comp);

/** Returns 1 if the composed stream is running, 0 if not, -1 if null. */
int32_t rsac_composition_is_running(const RsacComposition* comp);

/**
 * Returns the number of composed-ring overruns: composed buffers dropped
 * because the consumer read slower than the compositor produced (the ring
 * holds ~128 composed buffers, about 1.3 s at the default 10 ms quantum).
 * Mirrors rsac_capture_overrun_count(). Counts loss at the COMPOSED ring
 * only — loss inside an inner source's own capture is reported per source
 * via rsac_composition_source_stats(). Returns 0 if the handle is null or
 * the composition has not been started.
 */
uint64_t rsac_composition_overrun_count(const RsacComposition* comp);

/**
 * Frees a composition handle. Stops the composition if running (joining the
 * compositor thread and stopping every inner capture). No-op if null.
 * Buffers previously returned by reads own their samples and stay valid.
 */
void rsac_composition_free(RsacComposition* comp);

/* ── Reading composed audio ── */

/**
 * Non-blocking read of the next composed buffer (mirrors
 * rsac_capture_try_read). On success with data, *out receives a buffer handle
 * (free with rsac_audio_buffer_free); interleaved f32 at the session rate
 * with rsac_composition_channel_count() channels. On success with no data
 * yet, *out is null and RSAC_OK is returned. A drained, ended composition
 * returns the fatal RSAC_ERROR_STREAM_FAILED (do not retry). A not-started
 * composition returns RSAC_ERROR_STREAM_READ.
 */
rsac_error_t rsac_composition_try_read(const RsacComposition* comp,
                                       RsacAudioBuffer** out);

/**
 * Blocking read of the next composed buffer (mirrors rsac_capture_read).
 * Free the buffer with rsac_audio_buffer_free(). A drained, ended composition
 * returns the fatal RSAC_ERROR_STREAM_FAILED. A concurrent
 * rsac_composition_stop() unblocks a thread parked here.
 */
rsac_error_t rsac_composition_read(const RsacComposition* comp,
                                   RsacAudioBuffer** out);

/* ── Channel-map introspection ── */

/**
 * Returns the number of composed output channels. The layout is resolved by
 * rsac_composition_start(), so this is 0 before a successful start (and 0 if
 * the handle is null).
 */
uint16_t rsac_composition_channel_count(const RsacComposition* comp);

/**
 * Returns the name of the group producing composed output channel `channel`
 * (0-based). Valid until the next compose string accessor call on the same
 * thread. Returns null if the handle is null, the composition has not been
 * started, or `channel` is out of bounds.
 */
const char* rsac_composition_channel_group(const RsacComposition* comp,
                                           size_t channel);

/**
 * Returns the index of output channel `channel` WITHIN its group (0-based;
 * e.g. 0 = L, 1 = R for a stereo group). Returns -1 if the handle is null,
 * the composition has not been started, or `channel` is out of bounds.
 */
int32_t rsac_composition_channel_in_group(const RsacComposition* comp,
                                          size_t channel);

/* ── Composition stats ── */

/**
 * Fills *out with a point-in-time composition-counters snapshot. Before
 * rsac_composition_start() succeeds the snapshot is all-zero (num_sources 0).
 * *out is an out-parameter, not a handle — nothing to free.
 * Returns RSAC_ERROR_NULL_POINTER if comp or out is null, else RSAC_OK.
 */
rsac_error_t rsac_composition_stats(const RsacComposition* comp,
                                    RsacCompositionStats* out);

/**
 * Fills *out with the counters of the source at `index` (flat declaration
 * order; valid indices are [0, num_sources) from rsac_composition_stats()).
 * Returns RSAC_ERROR_STREAM_READ before start and
 * RSAC_ERROR_INVALID_PARAMETER if `index` is out of bounds.
 */
rsac_error_t rsac_composition_source_stats(const RsacComposition* comp,
                                           size_t index,
                                           RsacSourceStats* out);

/**
 * Returns the group name of the source at `index` (flat declaration order).
 * Valid until the next compose string accessor call on the same thread.
 * Returns null if the handle is null, the composition has not been started,
 * or `index` is out of bounds.
 */
const char* rsac_composition_source_group(const RsacComposition* comp,
                                          size_t index);

/**
 * Returns the capture-target string of the source at `index`, rendered in
 * the canonical target grammar (e.g. "system", "name:discord"). Valid until
 * the next compose string accessor call on the same thread. Returns null if
 * the handle is null, the composition has not been started, or `index` is
 * out of bounds.
 */
const char* rsac_composition_source_target(const RsacComposition* comp,
                                           size_t index);

/* ── Live per-source gain / mute (rsac-5a2d) ── */

/**
 * Sets a source's live mix gain on a RUNNING composition. `comp` is const
 * (the underlying method takes &self). The source is addressed by its group
 * `group` (a NUL-terminated UTF-8 string) plus its `source_idx` WITHIN that
 * group (0-based, declaration order) — NOT the flat cross-group index the
 * rsac_composition_source_*() accessors take. `gain` must be finite and >= 0;
 * it is validated by the core after any f32 narrowing the caller performed.
 * Returns RSAC_ERROR_NULL_POINTER if comp or group is null,
 * RSAC_ERROR_INVALID_PARAMETER if group is not valid UTF-8,
 * RSAC_ERROR_STREAM_READ before start or after stop/end, and
 * RSAC_ERROR_CONFIGURATION for an unknown group, out-of-range index, or
 * invalid gain.
 */
rsac_error_t rsac_composition_set_gain(const RsacComposition* comp,
                                       const char* group,
                                       size_t source_idx,
                                       float gain);

/**
 * Mutes (`muted` nonzero) or unmutes (`muted` 0) a source on a RUNNING
 * composition. Muting is a separate flag from gain: unmute restores its gain.
 * Same addressing and errors as rsac_composition_set_gain() (minus the
 * invalid-gain case).
 */
rsac_error_t rsac_composition_set_muted(const RsacComposition* comp,
                                        const char* group,
                                        size_t source_idx,
                                        int32_t muted);

/**
 * Reads a source's current effective mix gain into *out_gain. Same addressing
 * as rsac_composition_set_gain(). Unlike the setter this KEEPS WORKING on a
 * stopped or ended composition — it fails with RSAC_ERROR_STREAM_READ only
 * before the first successful start. Returns RSAC_ERROR_NULL_POINTER if comp,
 * group, or out_gain is null, RSAC_ERROR_INVALID_PARAMETER for a non-UTF-8
 * group, and RSAC_ERROR_CONFIGURATION for an unknown group or out-of-range
 * index. *out_gain is written only on RSAC_OK (nothing to free).
 */
rsac_error_t rsac_composition_gain(const RsacComposition* comp,
                                   const char* group,
                                   size_t source_idx,
                                   float* out_gain);

/**
 * Reads whether a source is currently muted into *out_muted (0/1). Same
 * addressing and stopped-composition behavior as rsac_composition_gain().
 * *out_muted is written only on RSAC_OK (nothing to free).
 */
rsac_error_t rsac_composition_is_muted(const RsacComposition* comp,
                                       const char* group,
                                       size_t source_idx,
                                       int32_t* out_muted);

#endif /* RSAC_FEATURE_COMPOSE */

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
