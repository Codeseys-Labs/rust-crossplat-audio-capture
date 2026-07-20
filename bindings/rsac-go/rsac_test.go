package rsac

import (
	"context"
	"errors"
	"fmt"
	"math"
	"runtime"
	"runtime/cgo"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

// ── Error Type Tests ────────────────────────────────────────────────────

func TestErrorCode_String(t *testing.T) {
	tests := []struct {
		code ErrorCode
		want string
	}{
		{ErrOK, "OK"},
		{ErrNullPointer, "NullPointer"},
		{ErrInvalidParameter, "InvalidParameter"},
		{ErrDeviceNotFound, "DeviceNotFound"},
		{ErrStreamFailed, "StreamFailed"},
		{ErrStreamRead, "StreamRead"},
		{ErrConfiguration, "Configuration"},
		{ErrAppNotFound, "AppNotFound"},
		{ErrBackend, "Backend"},
		{ErrPlatformNotSupported, "PlatformNotSupported"},
		{ErrPermissionDenied, "PermissionDenied"},
		{ErrInternal, "Internal"},
		{ErrTimeout, "Timeout"},
		{ErrPanic, "Panic"},
		{ErrorCode(9999), "Unknown(9999)"},
	}
	for _, tt := range tests {
		if got := tt.code.String(); got != tt.want {
			t.Errorf("ErrorCode(%d).String() = %q, want %q", int(tt.code), got, tt.want)
		}
	}
}

// TestErrorCode_Discriminants pins the numeric values of every error code to
// the rsac_error_t discriminants in bindings/rsac-ffi/src/lib.rs. These MUST
// match the Rust enum exactly — a mismatch silently mislabels every error that
// crosses the FFI boundary. (Audit finding C1.)
func TestErrorCode_Discriminants(t *testing.T) {
	tests := []struct {
		code ErrorCode
		want int
	}{
		{ErrOK, 0},
		{ErrNullPointer, 1},
		{ErrInvalidParameter, 2},
		{ErrDeviceNotFound, 3},
		{ErrPlatformNotSupported, 4},
		{ErrStreamFailed, 5},
		{ErrStreamRead, 6},
		{ErrConfiguration, 7},
		{ErrAppNotFound, 8},
		{ErrBackend, 9},
		{ErrPermissionDenied, 10},
		{ErrTimeout, 11},
		{ErrInternal, 12},
		{ErrPanic, 99},
	}
	for _, tt := range tests {
		if int(tt.code) != tt.want {
			t.Errorf("ErrorCode discriminant = %d, want %d (%s)", int(tt.code), tt.want, tt.code)
		}
	}
}

// TestErrorCode_IsRecoverable pins the recoverability classification used by
// the stream loops to continue-on-recoverable / stop-on-fatal. It must mirror
// the rsac core (ADR-0003) as projected onto the FFI codes by map_rsac_error:
// STREAM_READ / TIMEOUT / BACKEND are recoverable; STREAM_FAILED (the terminal
// StreamEnded) and everything else are fatal. (BP-6.)
func TestErrorCode_IsRecoverable(t *testing.T) {
	recoverable := []ErrorCode{ErrStreamRead, ErrTimeout, ErrBackend}
	fatal := []ErrorCode{
		ErrOK, ErrNullPointer, ErrInvalidParameter, ErrDeviceNotFound,
		ErrPlatformNotSupported, ErrStreamFailed, ErrConfiguration,
		ErrAppNotFound, ErrPermissionDenied, ErrInternal, ErrPanic,
		ErrorCode(9999),
	}
	for _, c := range recoverable {
		if !c.IsRecoverable() {
			t.Errorf("%s should be recoverable", c)
		}
	}
	for _, c := range fatal {
		if c.IsRecoverable() {
			t.Errorf("%s should NOT be recoverable (fatal/terminal)", c)
		}
	}
	// The terminal StreamEnded crosses the FFI as ErrStreamFailed; it must be
	// classified fatal so the stream loops end (not retry) on natural end.
	if ErrStreamFailed.IsRecoverable() {
		t.Errorf("ErrStreamFailed (terminal StreamEnded) must be fatal")
	}
}

// TestIsRecoverable checks the package-level helper that unwraps an error to
// its *Error and classifies it. A nil or non-rsac error is not recoverable.
func TestIsRecoverable(t *testing.T) {
	if !IsRecoverable(&Error{Code: ErrStreamRead}) {
		t.Error("a StreamRead *Error should be recoverable")
	}
	if !IsRecoverable(&Error{Code: ErrTimeout}) {
		t.Error("a Timeout *Error should be recoverable")
	}
	if IsRecoverable(&Error{Code: ErrStreamFailed}) {
		t.Error("a StreamFailed *Error should be fatal")
	}
	if IsRecoverable(nil) {
		t.Error("nil error must not be classified recoverable")
	}
	if IsRecoverable(errors.New("plain non-rsac error")) {
		t.Error("a non-rsac error must not be classified recoverable")
	}
	// Wrapped rsac error still unwraps via errors.As.
	wrapped := fmt.Errorf("context: %w", &Error{Code: ErrBackend})
	if !IsRecoverable(wrapped) {
		t.Error("a wrapped recoverable *Error should still be recoverable")
	}
	// ErrClosed carries ErrStreamRead, so it is *code-recoverable*; the stream
	// loops special-case it with errors.Is(err, ErrClosed) to stop anyway. This
	// documents that IsRecoverable alone returns true for ErrClosed.
	if !IsRecoverable(ErrClosed) {
		t.Error("ErrClosed's code (ErrStreamRead) is recoverable; loops gate it via errors.Is")
	}
}

// TestErrClosed_IdentityMatch pins the fix for the bug where (*Error).Is matched
// purely by Code: because ErrClosed carries the recoverable ErrStreamRead code,
// a plain code-equality Is would make EVERY transient ErrStreamRead satisfy
// errors.Is(err, ErrClosed). The stream loops gate on
// `IsRecoverable(err) && !errors.Is(err, ErrClosed)`, so that bug would have made
// them STOP on a recoverable hiccup instead of retrying — violating the
// terminal-error contract. ErrClosed must match by identity only.
func TestErrClosed_IdentityMatch(t *testing.T) {
	// The sentinel matches itself.
	if !errors.Is(ErrClosed, ErrClosed) {
		t.Fatal("errors.Is(ErrClosed, ErrClosed) must be true")
	}
	// A *distinct* transient read error must NOT be mistaken for "closed",
	// even though it shares the ErrStreamRead code.
	transient := &Error{Code: ErrStreamRead, Message: "transient overrun"}
	if errors.Is(transient, ErrClosed) {
		t.Error("a transient ErrStreamRead must NOT satisfy errors.Is(err, ErrClosed)")
	}
	// Therefore the stream-loop guard retries the transient hiccup...
	if !(IsRecoverable(transient) && !errors.Is(transient, ErrClosed)) {
		t.Error("guard must RETRY a transient recoverable read error")
	}
	// ...and stops on the close sentinel.
	if IsRecoverable(ErrClosed) && !errors.Is(ErrClosed, ErrClosed) {
		t.Error("guard must STOP on ErrClosed (errors.Is short-circuits the retry)")
	}
	// A wrapped ErrClosed still matches by identity through the chain.
	wrapped := fmt.Errorf("close path: %w", ErrClosed)
	if !errors.Is(wrapped, ErrClosed) {
		t.Error("a wrapped ErrClosed must still satisfy errors.Is(err, ErrClosed)")
	}
	// Two different non-sentinel *Errors with the same code still match by code
	// (the generic contract is preserved for everything except the sentinel).
	a := &Error{Code: ErrTimeout, Message: "a"}
	b := &Error{Code: ErrTimeout, Message: "b"}
	if !errors.Is(a, b) {
		t.Error("non-sentinel *Errors of the same code should still match by code")
	}
}

func TestError_Error(t *testing.T) {
	e := &Error{Code: ErrDeviceNotFound, Message: "device foo not found"}
	got := e.Error()
	want := "rsac: DeviceNotFound: device foo not found"
	if got != want {
		t.Errorf("Error.Error() = %q, want %q", got, want)
	}

	e2 := &Error{Code: ErrInternal}
	got2 := e2.Error()
	want2 := "rsac: Internal"
	if got2 != want2 {
		t.Errorf("Error.Error() = %q, want %q", got2, want2)
	}
}

func TestError_Is(t *testing.T) {
	e1 := &Error{Code: ErrDeviceNotFound, Message: "foo"}
	e2 := &Error{Code: ErrDeviceNotFound, Message: "bar"}
	e3 := &Error{Code: ErrStreamRead, Message: "baz"}

	if !errors.Is(e1, e2) {
		t.Error("errors.Is should match same error code with different messages")
	}
	if errors.Is(e1, e3) {
		t.Error("errors.Is should not match different error codes")
	}
}

func TestError_As(t *testing.T) {
	var err error = &Error{Code: ErrPermissionDenied, Message: "need access"}
	var rsacErr *Error
	if !errors.As(err, &rsacErr) {
		t.Fatal("errors.As should succeed")
	}
	if rsacErr.Code != ErrPermissionDenied {
		t.Errorf("got code %v, want PermissionDenied", rsacErr.Code)
	}
}

// ── Capture Target Tests ────────────────────────────────────────────────

func TestCaptureTarget_SystemDefault(t *testing.T) {
	ct := SystemDefault()
	if ct.kind != targetSystemDefault {
		t.Errorf("SystemDefault().kind = %v, want %v", ct.kind, targetSystemDefault)
	}
}

func TestCaptureTarget_ApplicationByName(t *testing.T) {
	ct := ApplicationByName("Firefox")
	if ct.kind != targetApplicationByName {
		t.Errorf("kind = %v, want %v", ct.kind, targetApplicationByName)
	}
	if ct.name != "Firefox" {
		t.Errorf("name = %q, want %q", ct.name, "Firefox")
	}
}

func TestCaptureTarget_ProcessTree(t *testing.T) {
	ct := ProcessTree(1234)
	if ct.kind != targetProcessTree {
		t.Errorf("kind = %v, want %v", ct.kind, targetProcessTree)
	}
	if ct.pid != 1234 {
		t.Errorf("pid = %d, want %d", ct.pid, 1234)
	}
}

func TestCaptureTarget_Device(t *testing.T) {
	ct := Device("hw:0,0")
	if ct.kind != targetDevice {
		t.Errorf("kind = %v, want %v", ct.kind, targetDevice)
	}
	if ct.devID != "hw:0,0" {
		t.Errorf("devID = %q, want %q", ct.devID, "hw:0,0")
	}
}

func TestCaptureTarget_Application(t *testing.T) {
	ct := Application("com.app.music")
	if ct.kind != targetApplication {
		t.Errorf("kind = %v, want %v", ct.kind, targetApplication)
	}
	if ct.appID != "com.app.music" {
		t.Errorf("appID = %q, want %q", ct.appID, "com.app.music")
	}
}

// ── AudioBuffer Tests ───────────────────────────────────────────────────

func TestAudioBuffer_Empty(t *testing.T) {
	buf := AudioBuffer{}
	if !buf.IsEmpty() {
		t.Error("empty buffer should be empty")
	}
	if buf.NumFrames() != 0 {
		t.Error("empty buffer should have 0 frames")
	}
	if buf.NumSamples() != 0 {
		t.Error("empty buffer should have 0 samples")
	}
	if buf.Duration() != 0 {
		t.Error("empty buffer should have 0 duration")
	}
}

func TestAudioBuffer_WithData(t *testing.T) {
	data := make([]float32, 960)
	for i := range data {
		data[i] = float32(i) / 960.0
	}
	buf := AudioBuffer{
		data:       data,
		numFrames:  480,
		channels:   2,
		sampleRate: 48000,
	}

	if buf.NumFrames() != 480 {
		t.Errorf("NumFrames() = %d, want 480", buf.NumFrames())
	}
	if buf.Channels() != 2 {
		t.Errorf("Channels() = %d, want 2", buf.Channels())
	}
	if buf.SampleRate() != 48000 {
		t.Errorf("SampleRate() = %d, want 48000", buf.SampleRate())
	}
	if buf.NumSamples() != 960 {
		t.Errorf("NumSamples() = %d, want 960", buf.NumSamples())
	}
	if buf.IsEmpty() {
		t.Error("buffer with data should not be empty")
	}
	if got := buf.Duration(); got != 0.01 {
		t.Errorf("Duration() = %f, want 0.01", got)
	}
	if len(buf.Data()) != 960 {
		t.Errorf("len(Data()) = %d, want 960", len(buf.Data()))
	}
}

func TestAudioBuffer_Duration_ZeroSampleRate(t *testing.T) {
	buf := AudioBuffer{
		data:       []float32{1, 2, 3, 4},
		numFrames:  2,
		channels:   2,
		sampleRate: 0,
	}
	if buf.Duration() != 0 {
		t.Error("Duration should be 0 for sampleRate=0")
	}
}

// ── CaptureBuilder Tests ────────────────────────────────────────────────

func TestCaptureBuilder_Defaults(t *testing.T) {
	b := NewCaptureBuilder()
	if b.sampleRate != 48000 {
		t.Errorf("default sampleRate = %d, want 48000", b.sampleRate)
	}
	if b.channels != 2 {
		t.Errorf("default channels = %d, want 2", b.channels)
	}
	if b.target.kind != targetSystemDefault {
		t.Errorf("default target.kind = %v, want %v", b.target.kind, targetSystemDefault)
	}
}

func TestCaptureBuilder_FluentAPI(t *testing.T) {
	b := NewCaptureBuilder().
		WithApplicationByName("Chrome").
		SampleRate(44100).
		Channels(1)

	if b.target.kind != targetApplicationByName {
		t.Error("target kind should be applicationByName")
	}
	if b.target.name != "Chrome" {
		t.Errorf("target name = %q, want %q", b.target.name, "Chrome")
	}
	if b.sampleRate != 44100 {
		t.Errorf("sampleRate = %d, want 44100", b.sampleRate)
	}
	if b.channels != 1 {
		t.Errorf("channels = %d, want 1", b.channels)
	}
}

func TestCaptureBuilder_WithTarget(t *testing.T) {
	b := NewCaptureBuilder().WithTarget(ProcessTree(42))
	if b.target.kind != targetProcessTree {
		t.Error("WithTarget should set target kind")
	}
	if b.target.pid != 42 {
		t.Errorf("pid = %d, want 42", b.target.pid)
	}
}

func TestCaptureBuilder_WithSystemDefault(t *testing.T) {
	b := NewCaptureBuilder().
		WithApplicationByName("Firefox"). // set something else first
		WithSystemDefault()               // then override
	if b.target.kind != targetSystemDefault {
		t.Error("WithSystemDefault should override previous target")
	}
}

func TestCaptureBuilder_WithDevice(t *testing.T) {
	b := NewCaptureBuilder().WithDevice("built-in-speaker")
	if b.target.kind != targetDevice {
		t.Error("WithDevice should set target kind to device")
	}
	if b.target.devID != "built-in-speaker" {
		t.Errorf("devID = %q, want %q", b.target.devID, "built-in-speaker")
	}
}

func TestCaptureBuilder_WithApplication(t *testing.T) {
	b := NewCaptureBuilder().WithApplication("session-123")
	if b.target.kind != targetApplication {
		t.Error("WithApplication should set target kind to application")
	}
	if b.target.appID != "session-123" {
		t.Errorf("appID = %q, want %q", b.target.appID, "session-123")
	}
}

func TestCaptureBuilder_WithProcessTree(t *testing.T) {
	b := NewCaptureBuilder().WithProcessTree(9999)
	if b.target.kind != targetProcessTree {
		t.Error("WithProcessTree should set target kind to processTree")
	}
	if b.target.pid != 9999 {
		t.Errorf("pid = %d, want 9999", b.target.pid)
	}
}

// ── DeviceKind Tests ────────────────────────────────────────────────────

func TestDeviceKind_Constants(t *testing.T) {
	if DeviceInput != 0 {
		t.Errorf("DeviceInput = %d, want 0", DeviceInput)
	}
	if DeviceOutput != 1 {
		t.Errorf("DeviceOutput = %d, want 1", DeviceOutput)
	}
}

// ── Stream Context Cancellation (pure Go, no C dependency) ──────────────

func TestStream_ContextCancellation(t *testing.T) {
	// This test verifies that the stream loop exits when context is cancelled.
	// It does not require a real capture — we test the goroutine cleanup logic
	// by creating a mock scenario with a pre-cancelled context.
	ctx, cancel := context.WithCancel(context.Background())
	cancel() // cancel immediately

	// Create a channel and simulate the loop behavior.
	ch := make(chan AudioBuffer, 1)
	go func() {
		defer close(ch)
		select {
		case <-ctx.Done():
			return
		case <-time.After(5 * time.Second):
			t.Error("stream loop did not exit on context cancellation")
		}
	}()

	// The channel should be closed quickly.
	select {
	case _, ok := <-ch:
		if ok {
			t.Error("expected channel to be closed")
		}
	case <-time.After(1 * time.Second):
		t.Error("timed out waiting for channel close")
	}
}

// ── Callback Handle Bridge Tests ─────────────────────────────────────────

// The FFI callback closure is bridged across the C boundary via a
// runtime/cgo.Handle stored as the void* user_data. These tests cover the
// handle round-trip and the per-handle isolation that the SetCallback /
// goAudioCallback pair relies on.

func TestCallbackHandle_RoundTrip(t *testing.T) {
	called := false
	fn := func(buf AudioBuffer) {
		called = true
	}

	h := cgo.NewHandle(fn)
	defer h.Delete()

	got, ok := h.Value().(func(AudioBuffer))
	if !ok {
		t.Fatal("handle value should resolve to func(AudioBuffer)")
	}
	got(AudioBuffer{})
	if !called {
		t.Error("callback was not invoked")
	}
}

func TestCallbackHandle_MultipleHandles(t *testing.T) {
	count1 := 0
	count2 := 0
	h1 := cgo.NewHandle(func(buf AudioBuffer) { count1++ })
	h2 := cgo.NewHandle(func(buf AudioBuffer) { count2++ })
	defer h1.Delete()
	defer h2.Delete()

	if h1 == h2 {
		t.Fatal("distinct callbacks must get distinct handles")
	}

	fn1 := h1.Value().(func(AudioBuffer))
	fn2 := h2.Value().(func(AudioBuffer))
	fn1(AudioBuffer{})
	fn2(AudioBuffer{})
	fn1(AudioBuffer{})

	if count1 != 2 {
		t.Errorf("count1 = %d, want 2", count1)
	}
	if count2 != 1 {
		t.Errorf("count2 = %d, want 1", count2)
	}
}

// ── StreamResult Tests ──────────────────────────────────────────────────

func TestStreamResult_Values(t *testing.T) {
	sr := StreamResult{
		Buffer: AudioBuffer{numFrames: 100, channels: 2, sampleRate: 48000},
		Err:    nil,
	}
	if sr.Buffer.NumFrames() != 100 {
		t.Error("StreamResult should carry buffer")
	}
	if sr.Err != nil {
		t.Error("StreamResult.Err should be nil")
	}

	sr2 := StreamResult{
		Err: &Error{Code: ErrStreamRead, Message: "test"},
	}
	if sr2.Err == nil {
		t.Error("StreamResult.Err should be set")
	}
}

// ── AudioBuffer Metering Tests (pure Go, no C dependency) ────────────────

func TestAudioBuffer_Metering_FullScale(t *testing.T) {
	// A constant ±1.0 signal: RMS == 1.0, peak == 1.0, both 0.0 dBFS.
	buf := AudioBuffer{
		data:       []float32{1, -1, 1, -1},
		numFrames:  2,
		channels:   2,
		sampleRate: 48000,
	}
	if got := buf.RMS(); absf32(got-1.0) > 1e-6 {
		t.Errorf("RMS() = %v, want 1.0", got)
	}
	if got := buf.Peak(); absf32(got-1.0) > 1e-6 {
		t.Errorf("Peak() = %v, want 1.0", got)
	}
	if got := buf.RMSDbfs(); absf32(got) > 1e-4 {
		t.Errorf("RMSDbfs() = %v, want 0.0", got)
	}
	if got := buf.PeakDbfs(); absf32(got) > 1e-4 {
		t.Errorf("PeakDbfs() = %v, want 0.0", got)
	}
}

func TestAudioBuffer_Metering_HalfScale(t *testing.T) {
	// Constant 0.5 magnitude: RMS == 0.5, peak == 0.5, dBFS ≈ -6.0206.
	buf := AudioBuffer{
		data:       []float32{0.5, -0.5, 0.5, -0.5},
		numFrames:  2,
		channels:   2,
		sampleRate: 48000,
	}
	if got := buf.RMS(); absf32(got-0.5) > 1e-6 {
		t.Errorf("RMS() = %v, want 0.5", got)
	}
	if got := buf.Peak(); absf32(got-0.5) > 1e-6 {
		t.Errorf("Peak() = %v, want 0.5", got)
	}
	if got := buf.PeakDbfs(); absf32(got-(-6.0206)) > 1e-3 {
		t.Errorf("PeakDbfs() = %v, want ≈ -6.0206", got)
	}
}

func TestAudioBuffer_Metering_SilenceIsNegInf(t *testing.T) {
	buf := AudioBuffer{data: []float32{0, 0, 0, 0}, numFrames: 2, channels: 2, sampleRate: 48000}
	if got := buf.RMS(); got != 0 {
		t.Errorf("RMS() = %v, want 0", got)
	}
	if got := buf.Peak(); got != 0 {
		t.Errorf("Peak() = %v, want 0", got)
	}
	if got := buf.RMSDbfs(); !math.IsInf(float64(got), -1) {
		t.Errorf("RMSDbfs() = %v, want -Inf", got)
	}
	if got := buf.PeakDbfs(); !math.IsInf(float64(got), -1) {
		t.Errorf("PeakDbfs() = %v, want -Inf", got)
	}
}

func TestAudioBuffer_Metering_Empty(t *testing.T) {
	buf := AudioBuffer{}
	if got := buf.RMS(); got != 0 {
		t.Errorf("empty RMS() = %v, want 0", got)
	}
	if got := buf.Peak(); got != 0 {
		t.Errorf("empty Peak() = %v, want 0", got)
	}
	if got := buf.RMSDbfs(); !math.IsInf(float64(got), -1) {
		t.Errorf("empty RMSDbfs() = %v, want -Inf", got)
	}
}

func TestAudioBuffer_Metering_NaNSafe(t *testing.T) {
	// Non-finite samples are skipped: only the finite ±1.0 contribute.
	inf := float32(math.Inf(1))
	nan := float32(math.NaN())
	buf := AudioBuffer{data: []float32{1, nan, -1, inf}, numFrames: 2, channels: 2, sampleRate: 48000}
	if got := buf.RMS(); absf32(got-1.0) > 1e-6 {
		t.Errorf("NaN-safe RMS() = %v, want 1.0", got)
	}
	if got := buf.Peak(); absf32(got-1.0) > 1e-6 {
		t.Errorf("NaN-safe Peak() = %v, want 1.0", got)
	}
}

func absf32(v float32) float32 {
	if v < 0 {
		return -v
	}
	return v
}

// ── SampleFormat / AudioFormat / StreamStats value tests (pure Go) ───────

func TestSampleFormat_String(t *testing.T) {
	tests := []struct {
		f    SampleFormat
		want string
	}{
		{SampleFormatI16, "I16"},
		{SampleFormatI24, "I24"},
		{SampleFormatI32, "I32"},
		{SampleFormatF32, "F32"},
		{SampleFormat(42), "Unknown(42)"},
	}
	for _, tt := range tests {
		if got := tt.f.String(); got != tt.want {
			t.Errorf("SampleFormat(%d).String() = %q, want %q", int(tt.f), got, tt.want)
		}
	}
}

func TestSampleFormat_Discriminants(t *testing.T) {
	// Pin the Go constants to the rsac_sample_format_t discriminants.
	if SampleFormatI16 != 0 || SampleFormatI24 != 1 || SampleFormatI32 != 2 || SampleFormatF32 != 3 {
		t.Errorf("SampleFormat discriminants drifted: %d %d %d %d",
			SampleFormatI16, SampleFormatI24, SampleFormatI32, SampleFormatF32)
	}
}

func TestStreamStats_ZeroValue(t *testing.T) {
	var s StreamStats
	if s.BuffersCaptured != 0 || s.Overruns != 0 || s.IsRunning || s.UptimeSecs != 0 {
		t.Errorf("zero StreamStats should be all-zero/false, got %+v", s)
	}
}

func TestBackpressureReport_ZeroValue(t *testing.T) {
	var r BackpressureReport
	if r.Window != 0 || r.Pushed != 0 || r.Dropped != 0 || r.DropRate != 0 || r.IsUnderBackpressure {
		t.Errorf("zero BackpressureReport should be all-zero/false, got %+v", r)
	}
}

// TestBackpressureReport_Closed pins the closed-capture guard: like
// StreamStats(), BackpressureReport() must short-circuit to ErrClosed (and a
// zero-value report) before touching the freed/nil C handle. This is device-free
// — it constructs an already-closed AudioCapture (in-package), so it never opens
// a real stream.
func TestBackpressureReport_Closed(t *testing.T) {
	c := &AudioCapture{closed: true}
	r, err := c.BackpressureReport()
	if !errors.Is(err, ErrClosed) {
		t.Errorf("BackpressureReport on closed capture = %v, want ErrClosed", err)
	}
	if (r != BackpressureReport{}) {
		t.Errorf("closed BackpressureReport should be the zero value, got %+v", r)
	}
}

func TestAudioFormat_ZeroValue(t *testing.T) {
	var f AudioFormat
	if f.SampleRate != 0 || f.Channels != 0 || f.BitsPerSample != 0 || f.SampleFormat != SampleFormatI16 {
		t.Errorf("zero AudioFormat should be all-zero, got %+v", f)
	}
}

// ── Target String Tests (require the C library) ──────────────────────────

func TestParseTarget_ValidGrammar(t *testing.T) {
	valid := []string{
		"system",
		"name:Firefox",
		"app:1234",
		"device:hw:0,0",
		"tree:4321",
	}
	for _, spec := range valid {
		ct, err := ParseTarget(spec)
		if err != nil {
			t.Errorf("ParseTarget(%q) returned error: %v", spec, err)
			continue
		}
		if ct.kind != targetString {
			t.Errorf("ParseTarget(%q).kind = %v, want targetString", spec, ct.kind)
		}
		if ct.spec != spec {
			t.Errorf("ParseTarget(%q).spec = %q, want round-trip", spec, ct.spec)
		}
	}
}

func TestParseTarget_InvalidGrammar(t *testing.T) {
	_, err := ParseTarget("not-a-real-scheme:whatever")
	if err == nil {
		t.Fatal("ParseTarget should reject an unknown scheme")
	}
	var rsacErr *Error
	if !errors.As(err, &rsacErr) {
		t.Fatalf("ParseTarget error should be *Error, got %T", err)
	}
	if rsacErr.Code != ErrInvalidParameter {
		t.Errorf("ParseTarget invalid spec code = %v, want InvalidParameter", rsacErr.Code)
	}
}

func TestCaptureBuilder_SetTargetString(t *testing.T) {
	b := NewCaptureBuilder()
	if err := b.SetTargetString("name:Spotify"); err != nil {
		t.Fatalf("SetTargetString(valid) returned error: %v", err)
	}
	if b.target.kind != targetString || b.target.spec != "name:Spotify" {
		t.Errorf("SetTargetString did not store the spec: %+v", b.target)
	}

	// An invalid spec must not mutate the previously-set target.
	if err := b.SetTargetString("garbage::bad"); err == nil {
		t.Error("SetTargetString(invalid) should return an error")
	}
	if b.target.spec != "name:Spotify" {
		t.Errorf("SetTargetString(invalid) mutated target to %+v", b.target)
	}
}

func TestCaptureBuilder_WithTargetString(t *testing.T) {
	b := NewCaptureBuilder().WithTargetString("app:777")
	if b.target.kind != targetString {
		t.Errorf("WithTargetString kind = %v, want targetString", b.target.kind)
	}
	if b.target.spec != "app:777" {
		t.Errorf("WithTargetString spec = %q, want %q", b.target.spec, "app:777")
	}
}

// ── Mobile consent builder methods (rsac-c209) ──────────────────────────
//
// The desktop CI hosts (linux/windows/macos) are neither Android nor iOS, so
// Build() with a staged mobile consent artifact must surface the FFI's
// documented off-platform rejection. Per the Go error-projection contract
// (the FFI code projection is lossy), assert the CODE plus the DOCUMENTED
// MESSAGE — never IsRecoverable.

func TestCaptureBuilder_WithAndroidProjection_Staged(t *testing.T) {
	b := NewCaptureBuilder().WithAndroidProjection(42).SampleRate(44100)
	if !b.hasAndroidProjection || b.androidProjection != 42 {
		t.Errorf("WithAndroidProjection not staged: has=%v token=%d, want has=true token=42",
			b.hasAndroidProjection, b.androidProjection)
	}
	if b.sampleRate != 44100 {
		t.Error("WithAndroidProjection should be chainable with other setters")
	}
	// An unset artifact must never be sent (desktop Build paths unaffected).
	if NewCaptureBuilder().hasAndroidProjection {
		t.Error("fresh builder must not stage an Android projection token")
	}
}

func TestCaptureBuilder_WithIOSAppGroup_Staged(t *testing.T) {
	b := NewCaptureBuilder().WithIOSAppGroup("group.com.example.app.rsac").Channels(1)
	if !b.hasIOSAppGroup || b.iosAppGroup != "group.com.example.app.rsac" {
		t.Errorf("WithIOSAppGroup not staged: has=%v group=%q, want has=true group=group.com.example.app.rsac",
			b.hasIOSAppGroup, b.iosAppGroup)
	}
	if b.channels != 1 {
		t.Error("WithIOSAppGroup should be chainable with other setters")
	}
	if NewCaptureBuilder().hasIOSAppGroup {
		t.Error("fresh builder must not stage an iOS App Group")
	}
}

// mustBeOffPlatformError asserts err is the FFI's documented wrong-platform
// rejection: code ErrPlatformNotSupported carrying the exact documented
// message (asserted on the message per the lossy-projection contract).
func mustBeOffPlatformError(t *testing.T, err error, wantMsg string) {
	t.Helper()
	if err == nil {
		t.Fatal("Build with a wrong-platform consent artifact should error on a desktop host")
	}
	var rsacErr *Error
	if !errors.As(err, &rsacErr) {
		t.Fatalf("error should be *Error, got %T (%v)", err, err)
	}
	if rsacErr.Code != ErrPlatformNotSupported {
		t.Errorf("code = %v, want PlatformNotSupported", rsacErr.Code)
	}
	if rsacErr.Message != wantMsg {
		t.Errorf("message = %q, want %q", rsacErr.Message, wantMsg)
	}
}

func TestCaptureBuilder_WithAndroidProjection_ErrorsOffAndroid(t *testing.T) {
	if runtime.GOOS == "android" {
		t.Skip("wrong-platform path only exists off Android")
	}
	_, err := NewCaptureBuilder().WithAndroidProjection(42).Build()
	mustBeOffPlatformError(t, err,
		"rsac_builder_set_android_projection is only meaningful on Android")
}

func TestCaptureBuilder_WithIOSAppGroup_ErrorsOffIOS(t *testing.T) {
	if runtime.GOOS == "ios" {
		t.Skip("wrong-platform path only exists off iOS")
	}
	_, err := NewCaptureBuilder().WithIOSAppGroup("group.com.example.app.rsac").Build()
	mustBeOffPlatformError(t, err,
		"rsac_builder_set_ios_app_group is only meaningful on iOS")
}

// ── Concurrent Close-during-Read barrier (issue #28, H2) ─────────────────
//
// These tests exercise the read/Close use-after-free barrier WITHOUT a real
// audio device (CI is device-free), mirroring the device-free style of
// TestStream_ContextCancellation. The production AudioCapture keeps its handle
// alive across an in-flight C read via a sync.WaitGroup + a closing atomic.Bool,
// and Close() signals request_stop then drains reads.Wait() before freeing.
//
// closeHarness reproduces that exact ordering against a stub that stands in for
// the C handle + the rsac_capture_read / rsac_capture_request_stop / free FFI
// calls, so `go test -race` validates that the handle is never touched after it
// is "freed" while a read is in flight. It is a faithful copy of the real
// rsac.go logic (same field names, same lock discipline), not the real cgo path.
type closeHarness struct {
	mu      sync.Mutex
	closed  bool
	closing atomic.Bool
	reads   sync.WaitGroup

	// freed models the freed C handle: set true only after reads.Wait() drains.
	// The race detector flags any read of *handle after free; here we assert
	// logically that no in-flight read observes freed==true.
	freed atomic.Bool
	// terminal models the bridge's terminal flag that request_stop sets so a
	// parked stubRead returns promptly (StreamEnded-equivalent) rather than
	// spinning out a long timeout.
	terminal atomic.Bool
}

// stubRead stands in for C.rsac_capture_read: it parks (no data) until the
// stream goes terminal (request_stop) or a bounded fallback elapses, then
// returns ErrClosed. It asserts the "handle" is never freed while it runs.
func (h *closeHarness) stubRead() error {
	deadline := time.Now().Add(2 * time.Second)
	for {
		if h.freed.Load() {
			// The whole point of the barrier: this must never happen.
			return errors.New("USE-AFTER-FREE: read observed a freed handle")
		}
		if h.terminal.Load() {
			return ErrClosed // request_stop unblocked us → terminal stream
		}
		if time.Now().After(deadline) {
			return ErrClosed // safety net so a broken test can't hang CI
		}
		time.Sleep(time.Millisecond)
	}
}

// ReadBuffer mirrors AudioCapture.ReadBuffer's barrier exactly.
func (h *closeHarness) ReadBuffer() error {
	h.mu.Lock()
	if h.closed {
		h.mu.Unlock()
		return ErrClosed
	}
	h.reads.Add(1)
	h.mu.Unlock()
	defer h.reads.Done()

	if h.closing.Load() {
		return ErrClosed
	}
	return h.stubRead()
}

// Close mirrors AudioCapture.Close + closeLocked's drain-before-free ordering.
func (h *closeHarness) Close() error {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return nil
	}
	h.closing.Store(true)
	h.closed = true
	// request_stop: flip terminal so parked stubRead returns promptly.
	h.terminal.Store(true)
	// Drain in-flight reads with the lock released (matches closeLocked).
	h.mu.Unlock()
	h.reads.Wait()
	h.mu.Lock()
	// Only now is it safe to "free" — every in-flight read has returned.
	h.freed.Store(true)
	return nil
}

// T1 + T2: N concurrent readers vs Close. Asserts no race, no UAF, Close
// returns promptly (well under the 1s blocking-read timeout the real FFI uses),
// and every in-flight read returns ErrClosed.
func TestCloseDuringRead_NoUseAfterFree(t *testing.T) {
	h := &closeHarness{}

	const n = 16
	var wg sync.WaitGroup
	errs := make([]error, n)
	for i := 0; i < n; i++ {
		wg.Add(1)
		go func(idx int) {
			defer wg.Done()
			errs[idx] = h.ReadBuffer()
		}(i)
	}

	// Let the readers park in stubRead.
	time.Sleep(20 * time.Millisecond)

	start := time.Now()
	if err := h.Close(); err != nil {
		t.Fatalf("Close returned error: %v", err)
	}
	elapsed := time.Since(start)
	// T2: request_stop must unblock parked readers, so Close returns fast.
	if elapsed > 500*time.Millisecond {
		t.Errorf("Close took %v; request_stop should unblock reads promptly", elapsed)
	}

	wg.Wait()
	for i, err := range errs {
		if err == nil {
			continue // a read that completed before Close is fine
		}
		if !errors.Is(err, ErrClosed) {
			t.Errorf("reader %d: unexpected error %v (want ErrClosed)", i, err)
		}
	}
	if !h.freed.Load() {
		t.Error("handle should be freed after Close drains in-flight reads")
	}
}

// T2 (focused): Close does not deadlock against a single parked reader and
// returns promptly because request_stop (terminal flag) unblocks it.
func TestCloseDuringRead_NoDeadlock(t *testing.T) {
	h := &closeHarness{}
	done := make(chan error, 1)
	go func() { done <- h.ReadBuffer() }()

	time.Sleep(20 * time.Millisecond)

	closed := make(chan struct{})
	go func() {
		_ = h.Close()
		close(closed)
	}()
	select {
	case <-closed:
	case <-time.After(1 * time.Second):
		t.Fatal("Close deadlocked against a parked reader")
	}

	select {
	case err := <-done:
		if !errors.Is(err, ErrClosed) {
			t.Errorf("parked read returned %v, want ErrClosed", err)
		}
	case <-time.After(1 * time.Second):
		t.Fatal("parked reader did not return after Close")
	}
}

// T4: idempotent Close (twice → nil both times) and Close-then-Read → ErrClosed.
func TestClose_IdempotentAndReadAfterClose(t *testing.T) {
	h := &closeHarness{}
	if err := h.Close(); err != nil {
		t.Fatalf("first Close: %v", err)
	}
	if err := h.Close(); err != nil {
		t.Fatalf("second Close should be a no-op nil, got %v", err)
	}
	if err := h.ReadBuffer(); !errors.Is(err, ErrClosed) {
		t.Errorf("ReadBuffer after Close = %v, want ErrClosed", err)
	}
}

// T3-ish: many concurrent Close + Read interleavings stress the barrier under
// the race detector (mirrors the GC-finalizer/explicit-Close coexistence: both
// route through the same closing/closed-guarded path).
func TestCloseDuringRead_ConcurrentClosersAndReaders(t *testing.T) {
	h := &closeHarness{}
	var wg sync.WaitGroup

	for i := 0; i < 8; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			_ = h.ReadBuffer()
		}()
	}
	for i := 0; i < 4; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			_ = h.Close()
		}()
	}
	wg.Wait()

	if !h.freed.Load() {
		t.Error("handle should be freed after all Close calls")
	}
	// A post-drain read must cleanly report closed, never touch a freed handle.
	if err := h.ReadBuffer(); !errors.Is(err, ErrClosed) {
		t.Errorf("post-close ReadBuffer = %v, want ErrClosed", err)
	}
}

// ── Sentinel Error Tests ────────────────────────────────────────────────

func TestErrClosed_Is(t *testing.T) {
	var err error = ErrClosed
	var rsacErr *Error
	if !errors.As(err, &rsacErr) {
		t.Fatal("ErrClosed should be *Error")
	}
	if rsacErr.Code != ErrStreamRead {
		t.Errorf("ErrClosed.Code = %v, want StreamRead", rsacErr.Code)
	}
}
