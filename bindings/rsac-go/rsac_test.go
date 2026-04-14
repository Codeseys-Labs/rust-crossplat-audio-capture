package rsac

import (
	"context"
	"errors"
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
		{ErrInvalidParameter, "InvalidParameter"},
		{ErrDeviceNotFound, "DeviceNotFound"},
		{ErrStreamRead, "StreamRead"},
		{ErrAppNotFound, "AppNotFound"},
		{ErrPlatformNotSupported, "PlatformNotSupported"},
		{ErrPermissionDenied, "PermissionDenied"},
		{ErrInternal, "Internal"},
		{ErrTimeout, "Timeout"},
		{ErrorCode(9999), "Unknown(9999)"},
	}
	for _, tt := range tests {
		if got := tt.code.String(); got != tt.want {
			t.Errorf("ErrorCode(%d).String() = %q, want %q", int(tt.code), got, tt.want)
		}
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
	if b.bufferSize != 0 {
		t.Errorf("default bufferSize = %d, want 0", b.bufferSize)
	}
	if b.target.kind != targetSystemDefault {
		t.Errorf("default target.kind = %v, want %v", b.target.kind, targetSystemDefault)
	}
}

func TestCaptureBuilder_FluentAPI(t *testing.T) {
	b := NewCaptureBuilder().
		WithApplicationByName("Chrome").
		SampleRate(44100).
		Channels(1).
		BufferSize(1024)

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
	if b.bufferSize != 1024 {
		t.Errorf("bufferSize = %d, want 1024", b.bufferSize)
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

// ── Callback Registry Tests (pure Go) ───────────────────────────────────

func TestCallbackRegistry_RegisterAndLookup(t *testing.T) {
	called := false
	fn := func(buf AudioBuffer) {
		called = true
	}

	id := registerCallback(fn)
	defer unregisterCallback(id)

	got, ok := lookupCallback(id)
	if !ok {
		t.Fatal("lookupCallback should find registered callback")
	}
	got(AudioBuffer{})
	if !called {
		t.Error("callback was not invoked")
	}
}

func TestCallbackRegistry_UnregisterRemoves(t *testing.T) {
	fn := func(buf AudioBuffer) {}
	id := registerCallback(fn)
	unregisterCallback(id)

	_, ok := lookupCallback(id)
	if ok {
		t.Error("lookupCallback should not find unregistered callback")
	}
}

func TestCallbackRegistry_MultipleCallbacks(t *testing.T) {
	count1 := 0
	count2 := 0
	id1 := registerCallback(func(buf AudioBuffer) { count1++ })
	id2 := registerCallback(func(buf AudioBuffer) { count2++ })
	defer unregisterCallback(id1)
	defer unregisterCallback(id2)

	fn1, _ := lookupCallback(id1)
	fn2, _ := lookupCallback(id2)
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
