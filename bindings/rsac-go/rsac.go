// Package rsac provides Go bindings for the rsac (Rust Cross-Platform Audio Capture) library.
//
// rsac is a streaming-first audio capture library supporting system audio,
// per-application audio, and process-tree audio capture on Windows (WASAPI),
// Linux (PipeWire), and macOS (CoreAudio Process Tap).
//
// The Go API is designed around Go idioms: channel-based streaming, context
// cancellation, and goroutine-safe operation. Audio data flows through Go
// channels, making it trivial to integrate into concurrent pipelines.
//
// # Quick Start
//
//	capture, err := rsac.NewCaptureBuilder().
//	    WithSystemDefault().
//	    SampleRate(48000).
//	    Channels(2).
//	    Build()
//	if err != nil {
//	    log.Fatal(err)
//	}
//	defer capture.Close()
//
//	if err := capture.Start(); err != nil {
//	    log.Fatal(err)
//	}
//
//	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
//	defer cancel()
//
//	for buf := range capture.Stream(ctx) {
//	    fmt.Printf("%d frames, %d channels\n", buf.NumFrames(), buf.Channels())
//	}
//
// # Streaming Model
//
// rsac is a downstream audio pipeline enabler. The primary consumption model
// is [AudioCapture.Stream], which returns a channel of [AudioBuffer] values
// that respects context cancellation. For lower-level control, use
// [AudioCapture.ReadBuffer] (blocking) or [AudioCapture.TryReadBuffer]
// (non-blocking).
//
// # ABI
//
// The binding tracks the rsac-ffi C ABI declared in rsac.h (kept in lockstep
// with bindings/rsac-ffi/include/rsac.h). Fallible C functions return an
// rsac_error_t and deliver any produced handle through an out-pointer; pure
// accessors return their value directly with -1 (int32) or 0 used as the
// "null handle" sentinel.
package rsac

/*
#cgo CFLAGS: -I${SRCDIR}
#cgo LDFLAGS: -L${SRCDIR}/lib -lrsac_ffi

#include "rsac.h"
#include <stdlib.h>
*/
import "C"
import (
	"context"
	"errors"
	"fmt"
	"runtime"
	"sync"
	"unsafe"
)

// ── Error Types ─────────────────────────────────────────────────────────

// ErrorCode represents a categorized error from the rsac library. The values
// match the rsac_error_t discriminants in the rsac-ffi C ABI.
type ErrorCode int

const (
	ErrOK                   ErrorCode = C.RSAC_OK
	ErrNullPointer          ErrorCode = C.RSAC_ERROR_NULL_POINTER
	ErrInvalidParameter     ErrorCode = C.RSAC_ERROR_INVALID_PARAMETER
	ErrDeviceNotFound       ErrorCode = C.RSAC_ERROR_DEVICE_NOT_FOUND
	ErrPlatformNotSupported ErrorCode = C.RSAC_ERROR_PLATFORM_NOT_SUPPORTED
	ErrStreamFailed         ErrorCode = C.RSAC_ERROR_STREAM_FAILED
	ErrStreamRead           ErrorCode = C.RSAC_ERROR_STREAM_READ
	ErrConfiguration        ErrorCode = C.RSAC_ERROR_CONFIGURATION
	ErrAppNotFound          ErrorCode = C.RSAC_ERROR_APPLICATION_NOT_FOUND
	ErrBackend              ErrorCode = C.RSAC_ERROR_BACKEND
	ErrPermissionDenied     ErrorCode = C.RSAC_ERROR_PERMISSION_DENIED
	ErrTimeout              ErrorCode = C.RSAC_ERROR_TIMEOUT
	ErrInternal             ErrorCode = C.RSAC_ERROR_INTERNAL
	ErrPanic                ErrorCode = C.RSAC_ERROR_PANIC
)

// String returns the human-readable name of the error code.
func (e ErrorCode) String() string {
	switch e {
	case ErrOK:
		return "OK"
	case ErrNullPointer:
		return "NullPointer"
	case ErrInvalidParameter:
		return "InvalidParameter"
	case ErrDeviceNotFound:
		return "DeviceNotFound"
	case ErrPlatformNotSupported:
		return "PlatformNotSupported"
	case ErrStreamFailed:
		return "StreamFailed"
	case ErrStreamRead:
		return "StreamRead"
	case ErrConfiguration:
		return "Configuration"
	case ErrAppNotFound:
		return "AppNotFound"
	case ErrBackend:
		return "Backend"
	case ErrPermissionDenied:
		return "PermissionDenied"
	case ErrTimeout:
		return "Timeout"
	case ErrInternal:
		return "Internal"
	case ErrPanic:
		return "Panic"
	default:
		return fmt.Sprintf("Unknown(%d)", int(e))
	}
}

// Error represents an error returned by the rsac library.
// It implements the error interface and supports errors.Is/As.
type Error struct {
	Code    ErrorCode
	Message string
}

// Error implements the error interface.
func (e *Error) Error() string {
	if e.Message != "" {
		return fmt.Sprintf("rsac: %s: %s", e.Code, e.Message)
	}
	return fmt.Sprintf("rsac: %s", e.Code)
}

// Is supports errors.Is matching by error code.
func (e *Error) Is(target error) bool {
	var t *Error
	if errors.As(target, &t) {
		return e.Code == t.Code
	}
	return false
}

// Sentinel errors for use with errors.Is.
var (
	// ErrClosed is returned by operations on a closed [AudioCapture].
	ErrClosed = &Error{Code: ErrStreamRead, Message: "capture closed"}
)

// newError creates an Error from a C error code, fetching the thread-local message.
func newError(code C.rsac_error_t) error {
	if code == C.RSAC_OK {
		return nil
	}
	msg := ""
	cmsg := C.rsac_error_message()
	if cmsg != nil {
		msg = C.GoString(cmsg)
	}
	return &Error{
		Code:    ErrorCode(code),
		Message: msg,
	}
}

// ── Capture Target ──────────────────────────────────────────────────────

// CaptureTarget specifies what audio to capture.
// Use the constructor functions to create targets.
type CaptureTarget struct {
	kind  targetKind
	name  string
	pid   uint32
	devID string
	appID string
}

type targetKind int

const (
	targetSystemDefault targetKind = iota
	targetDevice
	targetApplication
	targetApplicationByName
	targetProcessTree
)

// SystemDefault returns a CaptureTarget for the system default audio device/mix.
func SystemDefault() CaptureTarget {
	return CaptureTarget{kind: targetSystemDefault}
}

// ApplicationByName returns a CaptureTarget that captures audio from an application
// matched by name (e.g., "Firefox", "Spotify").
func ApplicationByName(name string) CaptureTarget {
	return CaptureTarget{kind: targetApplicationByName, name: name}
}

// ProcessTree returns a CaptureTarget that captures audio from a process and all
// its child processes, identified by the root process ID.
func ProcessTree(pid uint32) CaptureTarget {
	return CaptureTarget{kind: targetProcessTree, pid: pid}
}

// Device returns a CaptureTarget for a specific audio device identified by its
// platform-specific ID string.
func Device(id string) CaptureTarget {
	return CaptureTarget{kind: targetDevice, devID: id}
}

// Application returns a CaptureTarget for a specific application audio session
// identified by its platform-specific application ID.
func Application(id string) CaptureTarget {
	return CaptureTarget{kind: targetApplication, appID: id}
}

// ── Audio Buffer ────────────────────────────────────────────────────────

// AudioBuffer holds a chunk of interleaved f32 audio data along with format
// metadata. Buffers are the primary data unit flowing through the capture
// pipeline.
//
// AudioBuffer values are safe to use after the capture that produced them
// is closed — the data is copied into Go-managed memory.
type AudioBuffer struct {
	data       []float32
	numFrames  int
	channels   int
	sampleRate int
}

// Data returns the interleaved audio samples as a float32 slice.
// The slice length is NumFrames() * Channels().
func (b AudioBuffer) Data() []float32 {
	return b.data
}

// NumFrames returns the number of audio frames in the buffer.
// Each frame contains one sample per channel.
func (b AudioBuffer) NumFrames() int {
	return b.numFrames
}

// Channels returns the number of audio channels.
func (b AudioBuffer) Channels() int {
	return b.channels
}

// SampleRate returns the sample rate in Hz.
func (b AudioBuffer) SampleRate() int {
	return b.sampleRate
}

// NumSamples returns the total number of samples (NumFrames * Channels).
func (b AudioBuffer) NumSamples() int {
	return len(b.data)
}

// Duration returns the duration of the audio data in this buffer, in seconds.
func (b AudioBuffer) Duration() float64 {
	if b.sampleRate == 0 {
		return 0
	}
	return float64(b.numFrames) / float64(b.sampleRate)
}

// IsEmpty returns true if the buffer contains no audio data.
func (b AudioBuffer) IsEmpty() bool {
	return len(b.data) == 0
}

// audioBufferFromC copies data from a C audio buffer into a Go AudioBuffer,
// then frees the C buffer. The returned AudioBuffer is fully Go-managed.
func audioBufferFromC(cbuf *C.RsacAudioBuffer) AudioBuffer {
	if cbuf == nil {
		return AudioBuffer{}
	}
	defer C.rsac_audio_buffer_free(cbuf)

	numFrames := int(C.rsac_audio_buffer_num_frames(cbuf))
	channels := int(C.rsac_audio_buffer_channels(cbuf))
	sampleRate := int(C.rsac_audio_buffer_sample_rate(cbuf))
	numSamples := int(C.rsac_audio_buffer_len(cbuf))

	// Copy the sample data into Go memory.
	cdata := C.rsac_audio_buffer_data(cbuf)
	if cdata == nil || numSamples == 0 {
		return AudioBuffer{
			numFrames:  numFrames,
			channels:   channels,
			sampleRate: sampleRate,
		}
	}

	data := make([]float32, numSamples)
	// Use unsafe.Slice to create a Go slice view over the C data, then copy.
	cSlice := unsafe.Slice((*float32)(unsafe.Pointer(cdata)), numSamples)
	copy(data, cSlice)

	return AudioBuffer{
		data:       data,
		numFrames:  numFrames,
		channels:   channels,
		sampleRate: sampleRate,
	}
}

// ── Capture Builder ─────────────────────────────────────────────────────

// CaptureBuilder configures and creates an [AudioCapture] instance using a
// fluent API. The builder validates parameters at Build() time.
//
// Example:
//
//	capture, err := rsac.NewCaptureBuilder().
//	    WithApplicationByName("Firefox").
//	    SampleRate(48000).
//	    Channels(2).
//	    Build()
type CaptureBuilder struct {
	target     CaptureTarget
	sampleRate uint32
	channels   uint16
}

// NewCaptureBuilder creates a new builder with default settings:
// system default target, 48000 Hz, 2 channels.
func NewCaptureBuilder() *CaptureBuilder {
	return &CaptureBuilder{
		target:     SystemDefault(),
		sampleRate: 48000,
		channels:   2,
	}
}

// WithTarget sets the capture target.
func (b *CaptureBuilder) WithTarget(target CaptureTarget) *CaptureBuilder {
	b.target = target
	return b
}

// WithSystemDefault sets the capture target to the system default audio device.
func (b *CaptureBuilder) WithSystemDefault() *CaptureBuilder {
	b.target = SystemDefault()
	return b
}

// WithApplicationByName sets the capture target to an application matched by name.
func (b *CaptureBuilder) WithApplicationByName(name string) *CaptureBuilder {
	b.target = ApplicationByName(name)
	return b
}

// WithProcessTree sets the capture target to a process tree rooted at the given PID.
func (b *CaptureBuilder) WithProcessTree(pid uint32) *CaptureBuilder {
	b.target = ProcessTree(pid)
	return b
}

// WithDevice sets the capture target to a specific audio device.
func (b *CaptureBuilder) WithDevice(deviceID string) *CaptureBuilder {
	b.target = Device(deviceID)
	return b
}

// WithApplication sets the capture target to a specific application session.
func (b *CaptureBuilder) WithApplication(appID string) *CaptureBuilder {
	b.target = Application(appID)
	return b
}

// SampleRate sets the desired sample rate in Hz.
// Common values: 22050, 32000, 44100, 48000, 88200, 96000.
func (b *CaptureBuilder) SampleRate(rate uint32) *CaptureBuilder {
	b.sampleRate = rate
	return b
}

// Channels sets the desired number of audio channels.
func (b *CaptureBuilder) Channels(ch uint16) *CaptureBuilder {
	b.channels = ch
	return b
}

// Build validates the configuration and creates an [AudioCapture] instance.
// Returns an error if the configuration is invalid or the platform does not
// support the requested capture target.
func (b *CaptureBuilder) Build() (*AudioCapture, error) {
	var cbuilder *C.RsacBuilder
	if rc := C.rsac_builder_new(&cbuilder); rc != C.RSAC_OK {
		return nil, newError(rc)
	}
	if cbuilder == nil {
		return nil, newError(C.RSAC_ERROR_INTERNAL)
	}

	// Set capture target.
	var rc C.rsac_error_t
	switch b.target.kind {
	case targetSystemDefault:
		rc = C.rsac_builder_set_target_system(cbuilder)
	case targetApplicationByName:
		cname := C.CString(b.target.name)
		defer C.free(unsafe.Pointer(cname))
		rc = C.rsac_builder_set_target_app_by_name(cbuilder, cname)
	case targetProcessTree:
		rc = C.rsac_builder_set_target_process_tree(cbuilder, C.uint32_t(b.target.pid))
	case targetDevice:
		cid := C.CString(b.target.devID)
		defer C.free(unsafe.Pointer(cid))
		rc = C.rsac_builder_set_target_device(cbuilder, cid)
	case targetApplication:
		cid := C.CString(b.target.appID)
		defer C.free(unsafe.Pointer(cid))
		rc = C.rsac_builder_set_target_app_by_id(cbuilder, cid)
	}
	if rc != C.RSAC_OK {
		C.rsac_builder_free(cbuilder)
		return nil, newError(rc)
	}

	// Set audio parameters.
	if rc = C.rsac_builder_set_sample_rate(cbuilder, C.uint32_t(b.sampleRate)); rc != C.RSAC_OK {
		C.rsac_builder_free(cbuilder)
		return nil, newError(rc)
	}
	if rc = C.rsac_builder_set_channels(cbuilder, C.uint16_t(b.channels)); rc != C.RSAC_OK {
		C.rsac_builder_free(cbuilder)
		return nil, newError(rc)
	}

	// Build the capture. This consumes (and frees) the builder regardless of outcome.
	var ccapture *C.RsacCapture
	if rc = C.rsac_builder_build(cbuilder, &ccapture); rc != C.RSAC_OK {
		return nil, newError(rc)
	}
	if ccapture == nil {
		return nil, newError(C.RSAC_ERROR_INTERNAL)
	}

	capture := &AudioCapture{
		handle: ccapture,
	}
	runtime.SetFinalizer(capture, (*AudioCapture).destroy)
	return capture, nil
}

// ── Audio Capture ───────────────────────────────────────────────────────

// AudioCapture represents an active audio capture session. It is created via
// [CaptureBuilder.Build] and provides methods to start, stop, read, and stream
// captured audio data.
//
// AudioCapture is safe for concurrent use from multiple goroutines.
// All methods acquire an internal mutex before accessing the underlying C handle.
//
// The caller must call [AudioCapture.Close] when done to release resources.
// A finalizer is registered as a safety net, but explicit Close() is preferred.
type AudioCapture struct {
	mu       sync.Mutex
	handle   *C.RsacCapture
	closed   bool
	callback uintptr // backing cgo.Handle for the active callback, 0 if none
}

// Start begins audio capture. The underlying OS stream is created and begins
// producing data. After Start(), use [ReadBuffer], [TryReadBuffer], or
// [Stream] to consume audio data.
func (c *AudioCapture) Start() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return ErrClosed
	}
	rc := C.rsac_capture_start(c.handle)
	return newError(rc)
}

// Stop halts audio capture. The OS stream is stopped and resources begin to be
// released. After Stop(), no more audio data will be produced. The capture
// cannot be restarted — create a new [AudioCapture] instead.
func (c *AudioCapture) Stop() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return nil // already closed, stop is a no-op
	}
	rc := C.rsac_capture_stop(c.handle)
	return newError(rc)
}

// Close stops the capture (if running) and frees all associated resources.
// After Close(), the AudioCapture must not be used.
//
// Close is idempotent — calling it multiple times is safe.
func (c *AudioCapture) Close() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return nil
	}
	return c.closeLocked()
}

// closeLocked frees the C handle. Must be called with c.mu held.
//
// Ordering matters for the #28 callback use-after-free: the cgo.Handle backing
// an active callback must never be Delete()d while a callback could still
// resolve it on the FFI delivery thread. We therefore (1) clear the C-layer
// callback and stop+free the capture — which tears down the delivery path and,
// for any callback already dispatched, is the point past which no new
// invocation can begin — and only then (2) Delete the cgo.Handle. The C ABI
// guarantees rsac_capture_free stops the stream if running, so the explicit
// set_callback(NULL) is belt-and-suspenders: it makes the "clear at the C layer
// precedes handle Delete" ordering unconditional even on backends where free is
// a thinner teardown. goAudioCallback additionally recovers from a resolve of an
// already-deleted handle, closing the residual in-flight-callback window.
func (c *AudioCapture) closeLocked() error {
	c.closed = true
	runtime.SetFinalizer(c, nil)
	// Clear the C-layer callback first so the FFI pump stops dispatching to
	// goAudioCallback before the handle it resolves is deleted.
	if c.callback != 0 {
		C.rsac_capture_set_callback(c.handle, nil, nil)
	}
	C.rsac_capture_free(c.handle)
	c.handle = nil
	// Now that delivery is torn down, it is safe to delete the cgo.Handle.
	c.clearCallbackHandleLocked()
	return nil
}

// destroy is the Go finalizer — a safety net for handles that were not
// explicitly closed.
func (c *AudioCapture) destroy() {
	_ = c.Close()
}

// IsRunning returns true if the capture is currently producing audio data.
func (c *AudioCapture) IsRunning() bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return false
	}
	// rsac_capture_is_running returns 1 (running), 0 (stopped), or -1 (null).
	return C.rsac_capture_is_running(c.handle) == 1
}

// OverrunCount returns the number of audio buffers dropped due to ring buffer
// overflow. A non-zero value indicates the consumer is not reading fast enough.
func (c *AudioCapture) OverrunCount() uint64 {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return 0
	}
	return uint64(C.rsac_capture_overrun_count(c.handle))
}

// ReadBuffer reads the next chunk of audio data, blocking until data is
// available. Returns an error if the capture is not running or has been closed.
//
// For non-blocking reads, use [TryReadBuffer].
// For channel-based streaming, use [Stream].
func (c *AudioCapture) ReadBuffer() (AudioBuffer, error) {
	c.mu.Lock()
	if c.closed {
		c.mu.Unlock()
		return AudioBuffer{}, ErrClosed
	}
	handle := c.handle
	c.mu.Unlock()

	// Note: ReadBuffer releases the lock during the blocking C call so that
	// other goroutines can call IsRunning(), OverrunCount(), or Stop().
	// The handle is safe to use without the lock because Close() only frees
	// the handle, and we re-check closed afterward.
	var cbuf *C.RsacAudioBuffer
	rc := C.rsac_capture_read(handle, &cbuf)
	if rc != C.RSAC_OK {
		c.mu.Lock()
		defer c.mu.Unlock()
		if c.closed {
			return AudioBuffer{}, ErrClosed
		}
		return AudioBuffer{}, newError(rc)
	}
	if cbuf == nil {
		// RSAC_OK with no buffer should not happen for a blocking read, but
		// guard against it rather than dereferencing nil.
		return AudioBuffer{}, nil
	}
	return audioBufferFromC(cbuf), nil
}

// TryReadBuffer attempts to read audio data without blocking.
// Returns (buffer, true) if data was available, or (AudioBuffer{}, false) if
// no data is currently buffered. Returns an error on stream failure.
func (c *AudioCapture) TryReadBuffer() (AudioBuffer, bool, error) {
	c.mu.Lock()
	if c.closed {
		c.mu.Unlock()
		return AudioBuffer{}, false, ErrClosed
	}
	handle := c.handle
	c.mu.Unlock()

	var cbuf *C.RsacAudioBuffer
	rc := C.rsac_capture_try_read(handle, &cbuf)
	if rc != C.RSAC_OK {
		c.mu.Lock()
		defer c.mu.Unlock()
		if c.closed {
			return AudioBuffer{}, false, ErrClosed
		}
		return AudioBuffer{}, false, newError(rc)
	}
	if cbuf == nil {
		// RSAC_OK with a null buffer means "no data available" (not an error).
		return AudioBuffer{}, false, nil
	}
	return audioBufferFromC(cbuf), true, nil
}

// Stream returns a channel that delivers AudioBuffer values as they are
// captured. The channel is closed when the context is cancelled, the capture
// is stopped, or an error occurs.
//
// Stream spawns a goroutine that reads from the capture in a loop. The
// goroutine exits cleanly when ctx is done.
//
// Only one Stream() call should be active at a time per AudioCapture, as
// multiple consumers would compete for audio data from the same ring buffer.
//
// Example:
//
//	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
//	defer cancel()
//
//	for buf := range capture.Stream(ctx) {
//	    process(buf.Data())
//	}
func (c *AudioCapture) Stream(ctx context.Context) <-chan AudioBuffer {
	ch := make(chan AudioBuffer, 16) // buffered to reduce goroutine wake-ups
	go c.streamLoop(ctx, ch)
	return ch
}

// StreamWithErrors returns a channel of (AudioBuffer, error) pairs for callers
// that need to observe streaming errors.
func (c *AudioCapture) StreamWithErrors(ctx context.Context) <-chan StreamResult {
	ch := make(chan StreamResult, 16)
	go c.streamLoopWithErrors(ctx, ch)
	return ch
}

// StreamResult holds an AudioBuffer and an optional error from the stream.
type StreamResult struct {
	Buffer AudioBuffer
	Err    error
}

func (c *AudioCapture) streamLoop(ctx context.Context, ch chan<- AudioBuffer) {
	defer close(ch)
	for {
		select {
		case <-ctx.Done():
			return
		default:
		}

		buf, ok, err := c.TryReadBuffer()
		if err != nil {
			return // stream error, stop delivering
		}
		if !ok {
			// No data available. Yield briefly to avoid busy-spinning.
			runtime.Gosched()
			continue
		}
		select {
		case ch <- buf:
		case <-ctx.Done():
			return
		}
	}
}

func (c *AudioCapture) streamLoopWithErrors(ctx context.Context, ch chan<- StreamResult) {
	defer close(ch)
	for {
		select {
		case <-ctx.Done():
			ch <- StreamResult{Err: ctx.Err()}
			return
		default:
		}

		buf, ok, err := c.TryReadBuffer()
		if err != nil {
			select {
			case ch <- StreamResult{Err: err}:
			case <-ctx.Done():
			}
			return
		}
		if !ok {
			runtime.Gosched()
			continue
		}
		select {
		case ch <- StreamResult{Buffer: buf}:
		case <-ctx.Done():
			return
		}
	}
}

// ── Platform Capabilities ───────────────────────────────────────────────

// Capabilities describes what the current platform's audio backend supports.
type Capabilities struct {
	// SupportsSystemCapture indicates whether system-wide audio capture is available.
	SupportsSystemCapture bool
	// SupportsAppCapture indicates whether per-application audio capture is available.
	SupportsAppCapture bool
	// SupportsProcessTree indicates whether process-tree audio capture is available.
	SupportsProcessTree bool
	// SupportsDeviceSelection indicates whether specific device selection is available.
	SupportsDeviceSelection bool
	// BackendName is the name of the audio backend (e.g., "WASAPI", "CoreAudio", "PipeWire").
	BackendName string
	// MaxChannels is the maximum number of audio channels supported.
	MaxChannels int
}

// PlatformCapabilities queries and returns the audio capabilities of the
// current platform. This is a static query that does not require an active
// capture session.
func PlatformCapabilities() (Capabilities, error) {
	var ccaps *C.RsacCapabilities
	if rc := C.rsac_capabilities_query(&ccaps); rc != C.RSAC_OK {
		return Capabilities{}, newError(rc)
	}
	if ccaps == nil {
		return Capabilities{}, newError(C.RSAC_ERROR_INTERNAL)
	}
	defer C.rsac_capabilities_free(ccaps)

	backendName := ""
	if cname := C.rsac_capabilities_backend_name(ccaps); cname != nil {
		backendName = C.GoString(cname)
	}

	return Capabilities{
		SupportsSystemCapture:   C.rsac_capabilities_supports_system_capture(ccaps) == 1,
		SupportsAppCapture:      C.rsac_capabilities_supports_app_capture(ccaps) == 1,
		SupportsProcessTree:     C.rsac_capabilities_supports_process_tree(ccaps) == 1,
		SupportsDeviceSelection: C.rsac_capabilities_supports_device_selection(ccaps) == 1,
		BackendName:             backendName,
		MaxChannels:             int(C.rsac_capabilities_max_channels(ccaps)),
	}, nil
}

// ── Device Enumeration ──────────────────────────────────────────────────

// DeviceKind represents the type of audio device.
type DeviceKind int

const (
	// DeviceInput is an audio input (recording) device.
	DeviceInput DeviceKind = C.RSAC_DEVICE_INPUT
	// DeviceOutput is an audio output (playback) device.
	DeviceOutput DeviceKind = C.RSAC_DEVICE_OUTPUT
)

// AudioDevice describes an audio device discovered by the platform backend.
type AudioDevice struct {
	// ID is the platform-specific unique identifier for this device.
	ID string
	// Name is the human-readable device name.
	Name string
	// IsDefault indicates whether this is the system default device.
	IsDefault bool
}

// deviceFromC reads the fields of a C device handle into a Go AudioDevice.
// It does not free the handle.
func deviceFromC(cdev *C.RsacDevice) AudioDevice {
	dev := AudioDevice{
		IsDefault: C.rsac_device_is_default(cdev) == 1,
	}
	if cid := C.rsac_device_id(cdev); cid != nil {
		dev.ID = C.GoString(cid)
	}
	if cname := C.rsac_device_name(cdev); cname != nil {
		dev.Name = C.GoString(cname)
	}
	return dev
}

// ListDevices enumerates all audio devices available on the current platform.
func ListDevices() ([]AudioDevice, error) {
	var cenum *C.RsacDeviceEnumerator
	if rc := C.rsac_device_enumerator_new(&cenum); rc != C.RSAC_OK {
		return nil, newError(rc)
	}
	if cenum == nil {
		return nil, newError(C.RSAC_ERROR_INTERNAL)
	}
	defer C.rsac_device_enumerator_free(cenum)

	var clist *C.RsacDeviceList
	if rc := C.rsac_device_list_new(cenum, &clist); rc != C.RSAC_OK {
		return nil, newError(rc)
	}
	if clist == nil {
		return nil, newError(C.RSAC_ERROR_INTERNAL)
	}
	defer C.rsac_device_list_free(clist)

	count := int(C.rsac_device_list_count(clist))
	devices := make([]AudioDevice, 0, count)
	for i := 0; i < count; i++ {
		var cdev *C.RsacDevice
		if rc := C.rsac_device_list_get(clist, C.size_t(i), &cdev); rc != C.RSAC_OK || cdev == nil {
			continue
		}
		devices = append(devices, deviceFromC(cdev))
		// Devices returned by rsac_device_list_get are owned snapshots and
		// must be freed individually.
		C.rsac_device_free(cdev)
	}
	return devices, nil
}

// DefaultDevice returns the default audio device of the given kind.
//
// NOTE: rsac is a loopback (output) capture library; only DeviceOutput is
// supported. Any other kind returns ErrPlatformNotSupported rather than
// silently returning the default output device.
func DefaultDevice(kind DeviceKind) (AudioDevice, error) {
	if kind != DeviceOutput {
		return AudioDevice{}, newError(C.RSAC_ERROR_PLATFORM_NOT_SUPPORTED)
	}
	var cenum *C.RsacDeviceEnumerator
	if rc := C.rsac_device_enumerator_new(&cenum); rc != C.RSAC_OK {
		return AudioDevice{}, newError(rc)
	}
	if cenum == nil {
		return AudioDevice{}, newError(C.RSAC_ERROR_INTERNAL)
	}
	defer C.rsac_device_enumerator_free(cenum)

	var cdev *C.RsacDevice
	if rc := C.rsac_default_device(cenum, C.rsac_device_kind_t(kind), &cdev); rc != C.RSAC_OK {
		return AudioDevice{}, newError(rc)
	}
	if cdev == nil {
		return AudioDevice{}, newError(C.RSAC_ERROR_DEVICE_NOT_FOUND)
	}
	defer C.rsac_device_free(cdev)

	return deviceFromC(cdev), nil
}
