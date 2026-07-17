package rsac

import (
	"errors"
	"math"
	"testing"
)

// TestGroupLayout_Constants pins the Go GroupLayout discriminants to the
// rsac_group_layout_t values in the C ABI. A mismatch would silently mis-map
// every layout across the FFI boundary.
func TestGroupLayout_Constants(t *testing.T) {
	tests := []struct {
		layout GroupLayout
		want   int32
	}{
		{LayoutMono, 0},
		{LayoutStereo, 1},
		{LayoutKeepChannels, 2},
	}
	for _, tt := range tests {
		if int32(tt.layout) != tt.want {
			t.Errorf("GroupLayout %v = %d, want %d", tt.layout, int32(tt.layout), tt.want)
		}
	}
}

// TestGroup_LifecycleAndSources exercises the device-free group surface: create,
// set layout, add valid sources, and free. None of these touch a device.
func TestGroup_LifecycleAndSources(t *testing.T) {
	g, err := NewGroup("main")
	if err != nil {
		t.Fatalf("NewGroup: %v", err)
	}
	defer g.Free()

	if err := g.SetLayout(LayoutMono); err != nil {
		t.Errorf("SetLayout(Mono): %v", err)
	}
	// "system" always passes the capture builder's preflight on every platform.
	if err := g.AddSource("system"); err != nil {
		t.Errorf("AddSource(system): %v", err)
	}
	if err := g.AddSourceWithGain("system", 0.8); err != nil {
		t.Errorf("AddSourceWithGain(system, 0.8): %v", err)
	}
}

// TestGroup_InvalidGainRejected pins the eager gain validation: a non-finite or
// negative gain is rejected with ErrInvalidParameter and the group is unchanged
// (mirrors the C FFI invalid_gain_rejected_eagerly test).
func TestGroup_InvalidGainRejected(t *testing.T) {
	g, err := NewGroup("g")
	if err != nil {
		t.Fatalf("NewGroup: %v", err)
	}
	defer g.Free()

	for _, bad := range []float32{-0.5, float32(math.NaN()), float32(math.Inf(1)), float32(math.Inf(-1))} {
		err := g.AddSourceWithGain("system", bad)
		if err == nil {
			t.Errorf("AddSourceWithGain(system, %v) should be rejected", bad)
			continue
		}
		var e *Error
		if !errors.As(err, &e) || e.Code != ErrInvalidParameter {
			t.Errorf("AddSourceWithGain(system, %v) = %v, want ErrInvalidParameter", bad, err)
		}
	}
	// A valid gain still works on the same (unchanged) group.
	if err := g.AddSourceWithGain("system", 0.5); err != nil {
		t.Errorf("valid gain on unchanged group: %v", err)
	}
}

// TestGroup_BadSourceSpecRejected pins that an unknown target scheme is rejected
// with ErrInvalidParameter without touching a device.
func TestGroup_BadSourceSpecRejected(t *testing.T) {
	g, err := NewGroup("g")
	if err != nil {
		t.Fatalf("NewGroup: %v", err)
	}
	defer g.Free()

	if err := g.AddSource("not-a-real-scheme:whatever"); err == nil {
		t.Error("AddSource with a garbage scheme should be rejected")
	} else {
		var e *Error
		if !errors.As(err, &e) || e.Code != ErrInvalidParameter {
			t.Errorf("bad spec = %v, want ErrInvalidParameter", err)
		}
	}
}

// TestCompositionBuilder_Defaults builds a minimal valid device-free
// composition ("system" source) and verifies Build succeeds and consumes the
// builder (a second Build on the same builder returns ErrClosed).
func TestCompositionBuilder_Defaults(t *testing.T) {
	b, err := NewCompositionBuilder()
	if err != nil {
		t.Fatalf("NewCompositionBuilder: %v", err)
	}
	g := mustGroup(t, "main", "system")
	if err := b.AddGroup(g); err != nil {
		t.Fatalf("AddGroup: %v", err)
	}
	// AddGroup consumed the group handle; Free is now a no-op.
	if !g.consumed {
		t.Error("AddGroup should have marked the group consumed")
	}
	g.Free() // must not double-free

	comp, err := b.Build()
	if err != nil {
		t.Fatalf("Build (device-free, system source): %v", err)
	}
	defer comp.Close()

	// Build consumed the builder: a second Build fails with ErrClosed.
	if _, err := b.Build(); !errors.Is(err, ErrClosed) {
		t.Errorf("second Build = %v, want ErrClosed", err)
	}
}

// TestCompositionBuilder_EmptyRejected pins that a builder with no groups fails
// Build with ErrConfiguration (device-free).
func TestCompositionBuilder_EmptyRejected(t *testing.T) {
	b, err := NewCompositionBuilder()
	if err != nil {
		t.Fatalf("NewCompositionBuilder: %v", err)
	}
	_, err = b.Build()
	assertConfigErr(t, err, "empty builder Build")
}

// TestCompositionBuilder_KeepChannelsTwoSourcesRejected pins the keep-channels
// arity check: a keep-channels group with two sources fails Build with
// ErrConfiguration. This fires before any per-source capability check, so it is
// deterministic on every platform.
func TestCompositionBuilder_KeepChannelsTwoSourcesRejected(t *testing.T) {
	g := mustGroup(t, "keep", "system")
	if err := g.AddSource("system"); err != nil {
		t.Fatalf("second AddSource: %v", err)
	}
	if err := g.SetLayout(LayoutKeepChannels); err != nil {
		t.Fatalf("SetLayout(KeepChannels): %v", err)
	}
	b, err := NewCompositionBuilder()
	if err != nil {
		t.Fatalf("NewCompositionBuilder: %v", err)
	}
	if err := b.AddGroup(g); err != nil {
		t.Fatalf("AddGroup: %v", err)
	}
	_, err = b.Build()
	assertConfigErr(t, err, "keep-channels with two sources")
}

// TestCompositionBuilder_ZeroQuantumRejected pins that a zero quantum is
// rejected by Preflight (and Build) with ErrConfiguration; Preflight does NOT
// consume the builder, so the fix-and-retry workflow works.
func TestCompositionBuilder_ZeroQuantumRejected(t *testing.T) {
	b, err := NewCompositionBuilder()
	if err != nil {
		t.Fatalf("NewCompositionBuilder: %v", err)
	}
	g := mustGroup(t, "main", "system")
	if err := b.AddGroup(g); err != nil {
		t.Fatalf("AddGroup: %v", err)
	}
	if err := b.SetQuantumMs(0); err != nil {
		t.Fatalf("SetQuantumMs(0): %v", err)
	}
	assertConfigErr(t, b.Preflight(), "zero quantum preflight")

	// Preflight did not consume the builder: fix on the same builder and retry.
	if err := b.SetQuantumMs(10); err != nil {
		t.Fatalf("SetQuantumMs(10): %v", err)
	}
	if err := b.Preflight(); err != nil {
		t.Errorf("preflight after fixing quantum: %v", err)
	}
}

// TestCompositionBuilder_UnsupportedSampleRateRejected pins that an
// out-of-whitelist session rate is rejected with ErrInvalidParameter — the
// whitelist check precedes the group loop, so it is deterministic everywhere.
func TestCompositionBuilder_UnsupportedSampleRateRejected(t *testing.T) {
	b, err := NewCompositionBuilder()
	if err != nil {
		t.Fatalf("NewCompositionBuilder: %v", err)
	}
	g := mustGroup(t, "main", "system")
	if err := b.AddGroup(g); err != nil {
		t.Fatalf("AddGroup: %v", err)
	}
	if err := b.SetSampleRate(12345); err != nil {
		t.Fatalf("SetSampleRate(12345): %v", err)
	}
	err = b.Preflight()
	var e *Error
	if !errors.As(err, &e) || e.Code != ErrInvalidParameter {
		t.Errorf("preflight unsupported rate = %v, want ErrInvalidParameter", err)
	}
}

// TestComposition_NotStartedContract builds a device-free composition and
// asserts the not-started contract without touching any device: no layout, no
// engine, idempotent stop.
func TestComposition_NotStartedContract(t *testing.T) {
	b, err := NewCompositionBuilder()
	if err != nil {
		t.Fatalf("NewCompositionBuilder: %v", err)
	}
	g := mustGroup(t, "main", "system")
	if err := b.AddGroup(g); err != nil {
		t.Fatalf("AddGroup: %v", err)
	}
	comp, err := b.Build()
	if err != nil {
		t.Fatalf("Build: %v", err)
	}
	defer comp.Close()

	if comp.IsRunning() {
		t.Error("IsRunning should be false before Start")
	}
	if got := comp.ChannelCount(); got != 0 {
		t.Errorf("ChannelCount before Start = %d, want 0", got)
	}
	if got := comp.OverrunCount(); got != 0 {
		t.Errorf("OverrunCount before Start = %d, want 0", got)
	}
	if _, ok := comp.ChannelGroup(0); ok {
		t.Error("ChannelGroup(0) should be (\"\", false) before Start")
	}
	if _, ok := comp.SourceGroup(0); ok {
		t.Error("SourceGroup(0) should be (\"\", false) before Start")
	}
	// Stats before start reports the recoverable not-started error.
	if _, err := comp.Stats(); err == nil {
		t.Error("Stats before Start should return a not-started error")
	} else {
		var e *Error
		if !errors.As(err, &e) || e.Code != ErrStreamRead {
			t.Errorf("Stats before Start = %v, want ErrStreamRead", err)
		}
	}
	if _, err := comp.SourceStats(0); err == nil {
		t.Error("SourceStats(0) before Start should return a not-started error")
	}
	// Stop before start is an idempotent no-op.
	if err := comp.Stop(); err != nil {
		t.Errorf("Stop before Start = %v, want nil", err)
	}
	if err := comp.Stop(); err != nil {
		t.Errorf("second Stop = %v, want nil", err)
	}
}

// TestComposition_CloseIdempotent pins idempotent Close on a device-free
// composition.
func TestComposition_CloseIdempotent(t *testing.T) {
	b, err := NewCompositionBuilder()
	if err != nil {
		t.Fatalf("NewCompositionBuilder: %v", err)
	}
	g := mustGroup(t, "main", "system")
	if err := b.AddGroup(g); err != nil {
		t.Fatalf("AddGroup: %v", err)
	}
	comp, err := b.Build()
	if err != nil {
		t.Fatalf("Build: %v", err)
	}
	if err := comp.Close(); err != nil {
		t.Errorf("Close: %v", err)
	}
	if err := comp.Close(); err != nil {
		t.Errorf("second Close: %v", err)
	}
	// Operations after close are safe no-ops / ErrClosed.
	if comp.IsRunning() {
		t.Error("IsRunning after Close should be false")
	}
	if _, err := comp.Stats(); !errors.Is(err, ErrClosed) {
		t.Errorf("Stats after Close = %v, want ErrClosed", err)
	}
}

// ── helpers ─────────────────────────────────────────────────────────────

// mustGroup builds a group with one "system" source, failing the test on error.
func mustGroup(t *testing.T, name, spec string) *Group {
	t.Helper()
	g, err := NewGroup(name)
	if err != nil {
		t.Fatalf("NewGroup(%q): %v", name, err)
	}
	if err := g.AddSource(spec); err != nil {
		t.Fatalf("AddSource(%q): %v", spec, err)
	}
	return g
}

// assertConfigErr fails the test unless err is an *Error with code
// ErrConfiguration.
func assertConfigErr(t *testing.T, err error, what string) {
	t.Helper()
	var e *Error
	if !errors.As(err, &e) || e.Code != ErrConfiguration {
		t.Errorf("%s = %v, want ErrConfiguration", what, err)
	}
}
