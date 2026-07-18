//go:build rsac_audio_e2e

// REAL-device cgo-boundary race coverage for AudioCapture.Close() racing
// ReadBuffer/TryReadBuffer and Stream(ctx) cancellation (rsac-a8bd).
//
// The 4 TestCloseDuringRead* tests in rsac_test.go exercise a hand-copied
// closeHarness stub (same mu/reads/closing fields, same lock ordering) —
// faithful to the Go-side logic, but NEVER calls into the real cgo path
// (C.rsac_capture_read / C.rsac_capture_request_stop / C.rsac_capture_free).
// This file closes that gap: it drives the actual AudioCapture against a
// live SystemDefault stream so `go test -race` can catch a real memory-safety
// bug at the cgo boundary that a Go-only stub structurally cannot exercise
// (e.g. a race between the FFI's own internal locking and Go's WaitGroup
// barrier, or a use-after-free of the *C.RsacCapture handle itself).
//
// Gating: this needs a LIVE audio device continuously producing buffers (not
// necessarily a routed deterministic tone — no content assertion is made
// here, only lifecycle correctness). Two opt-ins, either satisfies:
//   - RSAC_CI_AUDIO_DETERMINISTIC=1 — the linux-audio-bindings CI route gate
//     (mirrors capture_smoke_test.go).
//   - RSAC_RACE_ALLOW_REAL_DEVICE=1 — local hardware opt-in for a developer
//     box with a real mic/speaker (deliberately a distinct var from the
//     smoke's, since this test makes no RMS/non-silence assertion and so
//     doesn't need the deterministic route's tone-loop infrastructure).
//
// Bounded: whole test <30s combined; every reader goroutine exits on
// ErrClosed/a terminal error, never spins unbounded.
package rsac_test

import (
	"context"
	"os"
	"sync"
	"testing"
	"time"

	rsac "github.com/Codeseys-Labs/rust-crossplat-audio-capture/bindings/rsac-go"
)

func raceTestAllowed() bool {
	return os.Getenv("RSAC_CI_AUDIO_DETERMINISTIC") == "1" ||
		os.Getenv("RSAC_RACE_ALLOW_REAL_DEVICE") == "1"
}

func newRaceCapture(t *testing.T) *rsac.AudioCapture {
	t.Helper()
	cap, err := rsac.NewCaptureBuilder().
		WithSystemDefault().
		SampleRate(48000).
		Channels(2).
		Build()
	if err != nil {
		t.Fatalf("build: %v", err)
	}
	if err := cap.Start(); err != nil {
		t.Fatalf("start: %v", err)
	}
	return cap
}

// TestRealDevice_CloseDuringRead_NoRace: N goroutines hammer ReadBuffer
// (blocking) / TryReadBuffer (non-blocking) against a REAL cgo handle while
// Close() runs concurrently. Under -race, any unsynchronized access at the
// cgo boundary (Go WaitGroup barrier vs. the C handle lifetime) surfaces
// here — the stub tests structurally cannot reach this bug class.
func TestRealDevice_CloseDuringRead_NoRace(t *testing.T) {
	if !raceTestAllowed() {
		t.Skip("no real audio route (RSAC_CI_AUDIO_DETERMINISTIC=1 or RSAC_RACE_ALLOW_REAL_DEVICE=1)")
	}
	cap := newRaceCapture(t)

	const n = 12
	var wg sync.WaitGroup
	for i := 0; i < n; i++ {
		wg.Add(1)
		go func(idx int) {
			defer wg.Done()
			for {
				if idx%2 == 0 {
					if _, err := cap.ReadBuffer(); err != nil {
						return // ErrClosed / terminal — reader exits, bounded
					}
				} else {
					_, ok, err := cap.TryReadBuffer()
					if err != nil {
						return
					}
					if !ok {
						time.Sleep(time.Millisecond)
					}
				}
			}
		}(i)
	}

	time.Sleep(100 * time.Millisecond) // let readers park in the real C call
	start := time.Now()
	if err := cap.Close(); err != nil {
		t.Fatalf("close: %v", err)
	}
	if elapsed := time.Since(start); elapsed > 2*time.Second {
		t.Errorf("Close() took %v against real device; request_stop should unblock parked reads promptly", elapsed)
	}

	done := make(chan struct{})
	go func() { wg.Wait(); close(done) }()
	select {
	case <-done:
	case <-time.After(10 * time.Second):
		t.Fatal("reader goroutines did not exit within 10s of Close()")
	}
}

// TestRealDevice_StreamCancelDuringClose_NoRace: exercises Stream(ctx)'s
// consumer goroutine racing ctx-cancel against a concurrent Close() on a REAL
// capture — covers the streamLoop/TryReadBuffer path the smoke test's
// blocking-read-only coverage does not reach.
func TestRealDevice_StreamCancelDuringClose_NoRace(t *testing.T) {
	if !raceTestAllowed() {
		t.Skip("no real audio route (RSAC_CI_AUDIO_DETERMINISTIC=1 or RSAC_RACE_ALLOW_REAL_DEVICE=1)")
	}
	cap := newRaceCapture(t)

	ctx, cancel := context.WithCancel(context.Background())
	ch := cap.Stream(ctx)
	drained := make(chan struct{})
	go func() {
		for range ch {
		}
		close(drained)
	}()

	time.Sleep(100 * time.Millisecond)
	var wg sync.WaitGroup
	wg.Add(2)
	go func() { defer wg.Done(); cancel() }()
	go func() { defer wg.Done(); _ = cap.Close() }()
	wg.Wait()

	select {
	case <-drained:
	case <-time.After(10 * time.Second):
		t.Fatal("Stream() channel did not close within 10s of cancel+Close")
	}
}
