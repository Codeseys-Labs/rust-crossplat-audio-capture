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

// Per-OS system libraries the Rust std staticlib pulls in. The Makefile passes
// these via CGO_LDFLAGS too, but declaring them here lets a bare `go build` /
// `go test` (not driven by the Makefile) link as well.
//   - Windows: WASAPI/COM + the Win32 libs Rust std needs (ws2_32/ntdll/bcrypt/…).
//     cgo links via MinGW, so the rsac-ffi staticlib must be the *-pc-windows-gnu
//     build (see Makefile / docs/CROSS_LANGUAGE_BINDINGS.md).
//   - Linux: PipeWire + the usual pthread/dl/m.
//   - macOS: the CoreAudio/AudioToolbox/CoreFoundation/Security frameworks.
#cgo windows LDFLAGS: -lole32 -loleaut32 -lwinmm -lksuser -luuid -lbcrypt -lntdll -luserenv -lws2_32 -ladvapi32 -lkernel32
#cgo linux LDFLAGS: -lpipewire-0.3 -lpthread -ldl -lm
#cgo darwin LDFLAGS: -framework CoreAudio -framework AudioToolbox -framework CoreFoundation -framework Security -framework SystemConfiguration

#include "rsac.h"
#include <stdlib.h>
*/
import "C"
import (
	"context"
	"errors"
	"fmt"
	"math"
	"runtime"
	"sync"
	"sync/atomic"
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

// IsRecoverable reports whether an error of this code is a transient hiccup the
// caller should retry, rather than a fatal/terminal condition that ends the
// stream. It mirrors the rsac core's recoverability classification (ADR-0003)
// as projected onto the FFI error codes by map_rsac_error in
// bindings/rsac-ffi/src/lib.rs:
//
//   - Recoverable: ErrStreamRead (transient read / over- or under-run),
//     ErrTimeout, ErrBackend (TransientRetry).
//   - Fatal: ErrStreamFailed (covers the terminal StreamEnded), and every
//     other code (configuration, device, platform, internal, panic, …).
//
// Consumers (e.g. [AudioCapture.Stream]) continue past a recoverable error and
// stop only on a fatal one.
func (e ErrorCode) IsRecoverable() bool {
	switch e {
	case ErrStreamRead, ErrTimeout, ErrBackend:
		return true
	default:
		return false
	}
}

// IsRecoverable reports whether err is a recoverable rsac error (see
// [ErrorCode.IsRecoverable]). It unwraps to the underlying *Error via
// errors.As; a nil error or a non-rsac error is treated as not recoverable
// (callers should already have handled nil before asking).
func IsRecoverable(err error) bool {
	if err == nil {
		return false
	}
	var e *Error
	if errors.As(err, &e) {
		return e.Code.IsRecoverable()
	}
	return false
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
	// spec is the raw canonical target string for a targetString target,
	// applied via rsac_builder_set_target_str at Build time.
	spec string
}

type targetKind int

const (
	targetSystemDefault targetKind = iota
	targetDevice
	targetApplication
	targetApplicationByName
	targetProcessTree
	targetString
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

// ParseTarget parses a canonical target string into a CaptureTarget using the
// rsac CaptureTarget grammar (the same parser the Rust core uses, reached
// through the FFI — no grammar is reimplemented in Go):
//
//	"system"            → the system default mix
//	"device:<id>"       → a specific device by ID
//	"app:<pid-or-id>"   → an application session
//	"name:<name>"       → an application matched by name
//	"tree:<pid>"        → a process tree rooted at <pid>
//
// The scheme is case-insensitive. A malformed string returns an *Error with
// code ErrInvalidParameter; ParseTarget never panics. The returned target can
// be passed to [CaptureBuilder.WithTarget] or applied directly via
// [CaptureBuilder.WithTargetString].
func ParseTarget(spec string) (CaptureTarget, error) {
	// Validate the spec now (fail fast at parse time) by routing it through a
	// throwaway builder. The grammar lives entirely in the Rust core, so this
	// is the single source of truth — Go does not duplicate the parser.
	var cbuilder *C.RsacBuilder
	if rc := C.rsac_builder_new(&cbuilder); rc != C.RSAC_OK {
		return CaptureTarget{}, newError(rc)
	}
	if cbuilder == nil {
		return CaptureTarget{}, newError(C.RSAC_ERROR_INTERNAL)
	}
	defer C.rsac_builder_free(cbuilder)

	cspec := C.CString(spec)
	defer C.free(unsafe.Pointer(cspec))
	if rc := C.rsac_builder_set_target_str(cbuilder, cspec); rc != C.RSAC_OK {
		return CaptureTarget{}, newError(rc)
	}
	return CaptureTarget{kind: targetString, spec: spec}, nil
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

// RMS returns the root-mean-square level across all samples and channels:
// sqrt(mean(x^2)). Non-finite samples (NaN/±Inf) are skipped; an empty or
// all-non-finite buffer yields 0 (never NaN). This mirrors the rsac core
// AudioBuffer::rms metering on the Go-owned sample copy (read-only measurement).
func (b AudioBuffer) RMS() float32 {
	var sumSq float64
	var count uint64
	for _, x := range b.data {
		v := float64(x)
		if isFinite(v) {
			sumSq += v * v
			count++
		}
	}
	if count == 0 {
		return 0
	}
	return float32(math.Sqrt(sumSq / float64(count)))
}

// Peak returns the peak (maximum absolute) level across all samples and
// channels: max(|x|). Non-finite samples are skipped; an empty or
// all-non-finite buffer yields 0 (never NaN). Mirrors core AudioBuffer::peak.
func (b AudioBuffer) Peak() float32 {
	var peak float32
	for _, x := range b.data {
		v := float64(x)
		if isFinite(v) {
			a := float32(math.Abs(v))
			if a > peak {
				peak = a
			}
		}
	}
	return peak
}

// RMSDbfs returns the RMS level in dBFS: 20*log10(RMS()). Returns negative
// infinity for silence or an empty buffer; full scale (RMS 1.0) maps to 0 dBFS.
// Mirrors core AudioBuffer::rms_dbfs.
func (b AudioBuffer) RMSDbfs() float32 {
	return linToDbfs(b.RMS())
}

// PeakDbfs returns the peak level in dBFS: 20*log10(Peak()). Returns negative
// infinity for silence or an empty buffer; a full-scale signal (peak 1.0) maps
// to 0 dBFS. Mirrors core AudioBuffer::peak_dbfs.
func (b AudioBuffer) PeakDbfs() float32 {
	return linToDbfs(b.Peak())
}

// isFinite reports whether v is neither NaN nor ±Inf.
func isFinite(v float64) bool {
	return !math.IsNaN(v) && !math.IsInf(v, 0)
}

// linToDbfs converts a linear amplitude to dBFS, returning negative infinity at
// or below 0 (silence) to match the core metering's NEG_INFINITY convention.
func linToDbfs(lin float32) float32 {
	if lin <= 0 {
		return float32(math.Inf(-1))
	}
	return float32(20 * math.Log10(float64(lin)))
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

// ── Stream Statistics ─────────────────────────────────────────────────────

// StreamStats is a point-in-time snapshot of a capture's diagnostic counters.
// It mirrors the C-ABI RsacStreamStats out-parameter filled by the FFI; it is a
// plain value (nothing to free). Obtain one via [AudioCapture.StreamStats].
type StreamStats struct {
	// BuffersCaptured is the number of buffers delivered to the consumer
	// (popped off the ring) since Start.
	BuffersCaptured uint64
	// BuffersDropped is the number of buffers dropped to ring overflow since Start.
	BuffersDropped uint64
	// BuffersPushed is the number of buffers enqueued by the OS callback since Start.
	BuffersPushed uint64
	// Overruns counts ring-buffer overruns; equal to BuffersDropped (retained alias).
	Overruns uint64
	// UptimeSecs is how long the stream has been running, in seconds (0 when not started).
	UptimeSecs float64
	// DroppedRatio is the fraction of accounted-for buffers lost to overflow, in 0.0..=1.0.
	DroppedRatio float64
	// IsRunning is true when the stream is currently capturing.
	IsRunning bool
}

// ── Audio Format ──────────────────────────────────────────────────────────

// SampleFormat is the negotiated wire/storage format the backend reports.
// All audio data is still delivered as interleaved float32 regardless of this
// value. The discriminants match rsac_sample_format_t in the C ABI.
type SampleFormat int

const (
	// SampleFormatI16 is signed 16-bit integer.
	SampleFormatI16 SampleFormat = C.RSAC_SAMPLE_FORMAT_I16
	// SampleFormatI24 is signed 24-bit integer (packed in a 32-bit container).
	SampleFormatI24 SampleFormat = C.RSAC_SAMPLE_FORMAT_I24
	// SampleFormatI32 is signed 32-bit integer.
	SampleFormatI32 SampleFormat = C.RSAC_SAMPLE_FORMAT_I32
	// SampleFormatF32 is 32-bit IEEE 754 floating-point (the library's internal standard).
	SampleFormatF32 SampleFormat = C.RSAC_SAMPLE_FORMAT_F32
)

// String returns the human-readable name of the sample format.
func (f SampleFormat) String() string {
	switch f {
	case SampleFormatI16:
		return "I16"
	case SampleFormatI24:
		return "I24"
	case SampleFormatI32:
		return "I32"
	case SampleFormatF32:
		return "F32"
	default:
		return fmt.Sprintf("Unknown(%d)", int(f))
	}
}

// AudioFormat describes a capture's negotiated delivery format. It mirrors the
// C-ABI RsacAudioFormat out-parameter; it is a plain value (nothing to free).
// Obtain one via [AudioCapture.Format].
type AudioFormat struct {
	// SampleRate is samples per second (e.g. 44100, 48000).
	SampleRate uint32
	// Channels is the number of audio channels (e.g. 1 mono, 2 stereo).
	Channels uint16
	// SampleFormat is the negotiated sample wire format.
	SampleFormat SampleFormat
	// BitsPerSample is the bits per sample for SampleFormat (16, 24, or 32).
	BitsPerSample uint16
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

// WithTargetString sets the capture target from a canonical target string (see
// [ParseTarget] for the grammar). The string is validated immediately; an
// invalid spec is reported by [SetTargetString], whereas this fluent setter
// stores the (unvalidated) spec and defers any error to [CaptureBuilder.Build].
//
// For fail-fast validation in a fluent chain, prefer [SetTargetString] or
// [ParseTarget] + [CaptureBuilder.WithTarget].
func (b *CaptureBuilder) WithTargetString(spec string) *CaptureBuilder {
	b.target = CaptureTarget{kind: targetString, spec: spec}
	return b
}

// SetTargetString parses and sets the capture target from a canonical target
// string (see [ParseTarget] for the grammar), validating it immediately. A
// malformed string returns an *Error with code ErrInvalidParameter and leaves
// the builder's current target unchanged; it never panics.
//
// Unlike the fluent [CaptureBuilder.WithTargetString], this returns an error so
// callers can react to a bad spec at configuration time rather than at Build().
func (b *CaptureBuilder) SetTargetString(spec string) error {
	target, err := ParseTarget(spec)
	if err != nil {
		return err
	}
	b.target = target
	return nil
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
	case targetString:
		cspec := C.CString(b.target.spec)
		defer C.free(unsafe.Pointer(cspec))
		rc = C.rsac_builder_set_target_str(cbuilder, cspec)
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

	// reads tracks in-flight ReadBuffer/TryReadBuffer C calls so Close() can
	// drain them before freeing the handle. Add(1) happens under c.mu (ordered
	// before any Close observes closed=true), and Done() runs via defer after
	// the C call returns. Close() releases c.mu, then reads.Wait() to drain.
	reads sync.WaitGroup
	// closing is set true (under c.mu, before closed) at the start of Close().
	// A reader that has already taken the WaitGroup barrier re-checks it after
	// dropping c.mu and bails before entering C if a close has begun. This is
	// the #28 use-after-free fix: the handle is never freed underneath a parked
	// read because Close() drains reads.Wait() before rsac_capture_free.
	closing atomic.Bool
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

// closeLocked frees the C handle. Must be called with c.mu held on entry; it
// briefly RELEASES c.mu while draining in-flight reads (see below) and
// re-acquires it before freeing, so on return c.mu is held again (Close's
// deferred Unlock balances it).
//
// Ordering matters for two distinct use-after-free races:
//
//  1. Callback (#28, already fixed): the cgo.Handle backing an active callback
//     must never be Delete()d while a callback could still resolve it on the
//     FFI delivery thread. We clear the C-layer callback first, then free, then
//     Delete the cgo.Handle (goAudioCallback also recovers from a resolve of an
//     already-deleted handle).
//
//  2. Blocking-read (#28, this fix): a goroutine parked in C.rsac_capture_read
//     holds no Go lock during the C call, so a naive free would yank the handle
//     out from under it. The reads WaitGroup keeps the handle alive for the
//     duration of every in-flight C read; we set closing+closed FIRST (so new
//     reads bail), call rsac_capture_request_stop to UNBLOCK a parked reader
//     (it observes a terminal stream and returns within ~1ms instead of waiting
//     out the blocking-read timeout), then UNLOCK c.mu and reads.Wait() to
//     drain, RE-LOCK, and only then free. Releasing c.mu during Wait() is
//     mandatory: the parked reader's error path re-takes c.mu, so holding it
//     across Wait() would deadlock.
func (c *AudioCapture) closeLocked() error {
	// Set closing BEFORE closed (both under c.mu). A reader either took the
	// WaitGroup barrier before Close started (and we drain it) or it sees
	// closed=true under c.mu and bails without Add — so reads.Add never races
	// reads.Wait (the "Add after Wait" panic the sync docs warn about).
	c.closing.Store(true)
	c.closed = true
	runtime.SetFinalizer(c, nil)
	// Clear the C-layer callback first so the FFI pump stops dispatching to
	// goAudioCallback before the handle it resolves is deleted.
	if c.callback != 0 {
		C.rsac_capture_set_callback(c.handle, nil, nil)
	}
	// Snapshot the handle for the unlocked drain window: request_stop only
	// reads, so it is safe to call concurrently with the in-flight reads we are
	// about to drain. It transitions the stream terminal, unblocking any parked
	// rsac_capture_read so reads.Wait() returns promptly.
	handle := c.handle
	C.rsac_capture_request_stop(handle)

	// Drain in-flight reads with c.mu released so parked readers can finish
	// their C call and run their deferred reads.Done() (and, on error, re-take
	// c.mu). After Wait() no read can be inside C against this handle.
	c.mu.Unlock()
	c.reads.Wait()
	c.mu.Lock()

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

// StreamStats returns a point-in-time snapshot of the capture's diagnostic
// counters bundled with running state, uptime, and the overflow ratio. Reading
// it never allocates on or blocks the OS audio callback thread.
//
// Before Start (or after Stop) every counter is zero, UptimeSecs is 0, and
// IsRunning is false. Returns ErrClosed if the capture has been closed.
//
// OverrunCount() and StreamStats().Overruns report the same ring-overflow count.
func (c *AudioCapture) StreamStats() (StreamStats, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return StreamStats{}, ErrClosed
	}
	var cs C.RsacStreamStats
	if rc := C.rsac_capture_stream_stats(c.handle, &cs); rc != C.RSAC_OK {
		return StreamStats{}, newError(rc)
	}
	return StreamStats{
		BuffersCaptured: uint64(cs.buffers_captured),
		BuffersDropped:  uint64(cs.buffers_dropped),
		BuffersPushed:   uint64(cs.buffers_pushed),
		Overruns:        uint64(cs.overruns),
		UptimeSecs:      float64(cs.uptime_secs),
		DroppedRatio:    float64(cs.dropped_ratio),
		IsRunning:       cs.is_running != 0,
	}, nil
}

// Format returns the negotiated delivery format the backend actually produces,
// atomically published once a stream is created.
//
// Returns an *Error with code ErrStreamFailed when no stream has been created
// yet (before Start, or after Stop) — call this only on a started capture, or
// after checking IsRunning(). Returns ErrClosed if the capture has been closed.
func (c *AudioCapture) Format() (AudioFormat, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed {
		return AudioFormat{}, ErrClosed
	}
	var cf C.RsacAudioFormat
	if rc := C.rsac_capture_format(c.handle, &cf); rc != C.RSAC_OK {
		return AudioFormat{}, newError(rc)
	}
	return AudioFormat{
		SampleRate:    uint32(cf.sample_rate),
		Channels:      uint16(cf.channels),
		SampleFormat:  SampleFormat(cf.sample_format),
		BitsPerSample: uint16(cf.bits_per_sample),
	}, nil
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
	// Join the in-flight-read barrier UNDER c.mu, so the Add(1) is ordered
	// before any Close() observes closed=true. Close() drains reads.Wait()
	// before freeing the handle, so the handle cannot be freed while this C
	// call is in flight (#28 fix). The matching Done() runs via defer below.
	c.reads.Add(1)
	c.mu.Unlock()
	defer c.reads.Done()

	// A close may have started after we dropped c.mu (it sets closing first).
	// Bail before entering C so we never park in a blocking read against a
	// handle Close() is about to drain+free.
	if c.closing.Load() {
		return AudioBuffer{}, ErrClosed
	}

	// The barrier keeps the handle alive for the duration of this C read;
	// Close() sets closing, signals rsac_capture_request_stop, then drains
	// in-flight reads (reads.Wait) before rsac_capture_free, so the handle is
	// never freed underneath a parked read. We re-check closed on the error
	// path (a concurrent request_stop surfaces as a terminal stream error).
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
	// Same in-flight-read barrier as ReadBuffer. The C call is non-blocking so
	// the window is tiny, but the Add(1)/Done() span must still bracket it so a
	// concurrent Close()+free cannot land mid-call (#28 fix).
	c.reads.Add(1)
	c.mu.Unlock()
	defer c.reads.Done()

	if c.closing.Load() {
		return AudioBuffer{}, false, ErrClosed
	}

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
			// A recoverable hiccup must NOT end the stream — yield and retry
			// (mirrors the in-process subscribe loop and the napi pump). A
			// fatal/terminal error (ErrStreamFailed for the terminal
			// StreamEnded, or anything non-recoverable) stops delivery and
			// closes the channel. ErrClosed (the capture was closed underneath
			// us) always stops, even though its code is in the recoverable set.
			if IsRecoverable(err) && !errors.Is(err, ErrClosed) {
				runtime.Gosched()
				continue
			}
			return // fatal terminal (or closed): stop delivering, close channel
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
			// A recoverable hiccup must NOT be reported as a terminal error
			// (that would mislead a `for r := range ch` consumer into stopping)
			// — swallow it and keep delivering, matching the value-only
			// Stream() loop. A fatal/terminal error (or ErrClosed) is delivered
			// as the FINAL StreamResult{Err} and then closes the channel, so the
			// consumer learns why the stream ended.
			if IsRecoverable(err) && !errors.Is(err, ErrClosed) {
				runtime.Gosched()
				continue
			}
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
