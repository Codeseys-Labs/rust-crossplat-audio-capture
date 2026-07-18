//go:build rsac_audio_e2e

// Deterministic-route SystemDefault capture smoke for the rsac Go binding
// (rsac-3635). Build-tag gated (rsac_audio_e2e) so it is EXCLUDED from
// `make test-pure` (its -run filter never names TestSystemCaptureSmoke) AND
// from the default `go test ./...` / `go vet` in the existing go-bindings
// matrix (the tag is not passed). Only the linux-audio-bindings job's
// `make test-e2e` target passes -tags rsac_audio_e2e.
//
// Mirrors the Rust system-capture contract (tests/ci_audio/system_capture.rs +
// lifecycle_terminal.rs): build -> start -> non-silent buffer -> stop ->
// terminal-observable read -> clean drop via defer Close(). RMS floor 0.01
// copied from helpers::verify_rms_energy. Per rsac.go, ReadBuffer() (blocking)
// surfaces the terminal stream error / ErrClosed after Stop()/Close().

package rsac_test

import (
	"os"
	"strings"
	"testing"
	"time"

	rsac "github.com/Codeseys-Labs/rust-crossplat-audio-capture/bindings/rsac-go"
)

func TestSystemCaptureSmoke(t *testing.T) {
	if os.Getenv("RSAC_CI_AUDIO_DETERMINISTIC") != "1" {
		t.Skip("RSAC_CI_AUDIO_DETERMINISTIC != 1")
	}
	timeout := 15 * time.Second
	if s := os.Getenv("RSAC_TEST_CAPTURE_TIMEOUT_SECS"); s != "" {
		if d, err := time.ParseDuration(s + "s"); err == nil {
			timeout = d
		}
	}

	cap, err := rsac.NewCaptureBuilder().
		WithSystemDefault().
		SampleRate(48000).
		Channels(2).
		Build()
	if err != nil {
		t.Fatalf("build: %v", err)
	}
	defer cap.Close() // clean drop

	if err := cap.Start(); err != nil {
		t.Fatalf("start: %v", err)
	}
	if !cap.IsRunning() {
		t.Fatal("IsRunning must be true after Start()")
	}

	gotNonSilence := false
	deadline := time.Now().Add(timeout)
	for time.Now().Before(deadline) {
		buf, ok, rerr := cap.TryReadBuffer()
		if rerr != nil {
			continue
		}
		if !ok {
			time.Sleep(10 * time.Millisecond)
			continue
		}
		if buf.RMS() > 0.01 {
			gotNonSilence = true
			break
		}
	}
	if !gotNonSilence {
		t.Fatal("deterministic source: no non-silent buffer within timeout")
	}

	if err := cap.Stop(); err != nil {
		t.Fatalf("stop: %v", err)
	}
	if cap.IsRunning() {
		t.Fatal("IsRunning must be false after Stop()")
	}
	// Post-stop read returns a LIFECYCLE error, never a buffer. Core's
	// restart-by-recreation contract RELEASES the stream on Stop(), so a
	// sequential stop-then-read yields the NotInitialized lifecycle error
	// ("Stream is not initialized. Call start() first." — verified live,
	// CI run 29621951762); StreamEnded appears only while a terminal stream
	// is still PRESENT (racing/parked reads — the rsac-477d case). Note the
	// FFI maps NotInitialized onto ErrStreamRead (recoverable in the lossy
	// code projection), so we assert on the MESSAGE, not recoverability.
	if _, rerr := cap.ReadBuffer(); rerr == nil {
		t.Fatal("ReadBuffer() after Stop() must return a lifecycle error")
	} else {
		msg := strings.ToLower(rerr.Error())
		if !strings.Contains(msg, "not initialized") && !strings.Contains(msg, "stream ended") {
			t.Fatalf("ReadBuffer() after Stop() returned an off-contract error: %v "+
				"(expected the NotInitialized or StreamEnded lifecycle error)", rerr)
		}
	}
}
