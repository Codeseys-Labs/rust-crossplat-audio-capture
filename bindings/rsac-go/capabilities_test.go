package rsac

import "testing"

// ── Platform Capabilities Tests (rsac-b624) ──────────────────────────────
//
// PlatformCapabilities() is a static, device-free query — see the function's
// own doc comment ("does not require an active capture session") — so these
// assertions are safe on every desktop OS the go-bindings CI matrix runs
// (Linux/Windows/macOS), with no headless-runner caveats.

func TestCapabilities(t *testing.T) {
	caps, err := PlatformCapabilities()
	if err != nil {
		t.Fatalf("PlatformCapabilities() error = %v", err)
	}

	if caps.BackendName == "" {
		t.Error("Capabilities.BackendName is empty")
	}
	if len(caps.SupportedSampleFormats) == 0 {
		t.Error("Capabilities.SupportedSampleFormats is empty")
	}
	if caps.MinSampleRate == 0 || caps.MaxSampleRate == 0 {
		t.Errorf("Capabilities sample-rate range is unset: min=%d max=%d", caps.MinSampleRate, caps.MaxSampleRate)
	}
	if caps.MinSampleRate > caps.MaxSampleRate {
		t.Errorf("Capabilities.MinSampleRate (%d) > MaxSampleRate (%d)", caps.MinSampleRate, caps.MaxSampleRate)
	}
	if len(caps.SupportedSampleRates) == 0 {
		t.Error("Capabilities.SupportedSampleRates is empty")
	}
	for _, r := range caps.SupportedSampleRates {
		if r < caps.MinSampleRate || r > caps.MaxSampleRate {
			t.Errorf("SupportedSampleRates entry %d falls outside [MinSampleRate=%d, MaxSampleRate=%d]", r, caps.MinSampleRate, caps.MaxSampleRate)
		}
	}
	if caps.MaxChannels <= 0 {
		t.Errorf("Capabilities.MaxChannels = %d, want > 0", caps.MaxChannels)
	}
}
