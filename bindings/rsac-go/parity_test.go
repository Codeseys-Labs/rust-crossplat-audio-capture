package rsac

// Builder-surface lockstep guard (rsac-c209).
//
// The Go package mirrors the C ABI twice: rsac.h here is a byte-level mirror
// of bindings/rsac-ffi/include/rsac.h (the two headers' symbol sets are
// diffed by scripts/gate-bindings.sh and ci.yml's check-bindings job), and
// CaptureBuilder mirrors the rsac_builder_* function family as Go methods.
// The second mirror had no mechanical check: the Android projection setter
// existed in the FFI for two releases without a Go binding and nobody
// noticed. This test closes that gap — every rsac_builder_* symbol declared
// in the local rsac.h must be accounted for in the map below, which names the
// Go API that binds it. Adding a builder function to the C ABI now forces a
// conscious Go-surface decision here instead of silent drift.

import (
	"os"
	"regexp"
	"testing"
)

// builderSymbolBindings maps every rsac_builder_* C symbol to the Go API that
// binds it. A symbol whose binding is intentionally internal-only (consumed by
// the Go layer itself, not re-exported) still gets an entry naming that use.
var builderSymbolBindings = map[string]string{
	"rsac_builder_new":                     "CaptureBuilder.Build (handle creation)",
	"rsac_builder_free":                    "CaptureBuilder.Build (error-path cleanup)",
	"rsac_builder_build":                   "CaptureBuilder.Build",
	"rsac_builder_set_target_system":       "CaptureBuilder.WithSystemDefault / WithTarget",
	"rsac_builder_set_target_device":       "CaptureBuilder.WithDevice",
	"rsac_builder_set_target_app_by_name":  "CaptureBuilder.WithApplicationByName",
	"rsac_builder_set_target_app_by_id":    "CaptureBuilder.WithApplication",
	"rsac_builder_set_target_process_tree": "CaptureBuilder.WithProcessTree",
	"rsac_builder_set_target_str":          "CaptureBuilder.WithTargetString / SetTargetString",
	"rsac_builder_set_sample_rate":         "CaptureBuilder.SampleRate",
	"rsac_builder_set_channels":            "CaptureBuilder.Channels",
	"rsac_builder_set_android_projection":  "CaptureBuilder.WithAndroidProjection",
	"rsac_builder_set_ios_app_group":       "CaptureBuilder.WithIOSAppGroup",
}

// TestCaptureBuilder_HeaderSymbolParity fails when rsac.h declares a
// rsac_builder_* function this package has not consciously bound (or drops
// one it still claims to bind). The name matches make test-pure's
// TestCaptureBuilder run filter so the guard executes on every CI leg.
func TestCaptureBuilder_HeaderSymbolParity(t *testing.T) {
	header, err := os.ReadFile("rsac.h")
	if err != nil {
		t.Fatalf("cannot read the mirrored rsac.h: %v", err)
	}

	// Match declarations, not prose: a builder symbol immediately followed by
	// its argument list "(". Doc comments mention symbols as "name()" too, so
	// collect from the whole file and de-duplicate — a declaration and its doc
	// mention reduce to the same name.
	re := regexp.MustCompile(`\brsac_builder_[a-z0-9_]*\(`)
	declared := map[string]bool{}
	for _, m := range re.FindAll(header, -1) {
		declared[string(m[:len(m)-1])] = true
	}
	if len(declared) == 0 {
		t.Fatal("no rsac_builder_* symbols found in rsac.h — extractor or header format broken")
	}

	for sym := range declared {
		if _, ok := builderSymbolBindings[sym]; !ok {
			t.Errorf("rsac.h declares %s but rsac-go has no recorded binding for it — add the Go builder method (or record the internal use) in builderSymbolBindings", sym)
		}
	}
	for sym, boundBy := range builderSymbolBindings {
		if !declared[sym] {
			t.Errorf("builderSymbolBindings claims %s (bound by %s) but rsac.h no longer declares it — remove the stale Go surface or fix the header mirror", sym, boundBy)
		}
	}
}

// TestCaptureBuilder_HeaderMirrorLockstep fails when the local rsac.h mirror's
// function-symbol set drifts from the source-of-truth curated header at
// bindings/rsac-ffi/include/rsac.h. This is exactly how the Android projection
// setter went missing here: the FFI grew the symbol, the mirror was never
// updated, and no check compared the two. Doc layout may differ freely — only
// the declared rsac_* function set must match. Skips when the FFI header is
// absent (rsac-go vendored standalone outside the repo); in-repo CI always
// has it.
func TestCaptureBuilder_HeaderMirrorLockstep(t *testing.T) {
	const ffiHeader = "../rsac-ffi/include/rsac.h"
	source, err := os.ReadFile(ffiHeader)
	if err != nil {
		t.Skipf("FFI curated header not present (%v) — standalone checkout; in-repo CI runs this", err)
	}
	mirror, err := os.ReadFile("rsac.h")
	if err != nil {
		t.Fatalf("cannot read the mirrored rsac.h: %v", err)
	}

	fnSymbols := func(header []byte) map[string]bool {
		re := regexp.MustCompile(`\brsac_[a-z0-9_]*\(`)
		set := map[string]bool{}
		for _, m := range re.FindAll(header, -1) {
			set[string(m[:len(m)-1])] = true
		}
		return set
	}

	want := fnSymbols(source)
	got := fnSymbols(mirror)
	if len(want) == 0 || len(got) == 0 {
		t.Fatal("symbol extraction yielded an empty set — header format changed; fix the extractor")
	}
	for sym := range want {
		if !got[sym] {
			t.Errorf("FFI curated header declares %s but the rsac-go rsac.h mirror does not — update the mirror (never hand-drift it from bindings/rsac-ffi/include/rsac.h)", sym)
		}
	}
	for sym := range got {
		if !want[sym] {
			t.Errorf("rsac-go rsac.h mirror declares %s but the FFI curated header does not — the mirror has drifted ahead of the source of truth", sym)
		}
	}
}
