//! Terminal read-behavior & `request_stop()` integration tests.
//!
//! The unit suite in `src/api.rs` already exercises `request_stop()` and the
//! terminal-read contract against a `MockCapturingStream`:
//!   * `request_stop_signals_stream_once` / `_is_idempotent` / `_no_stream_is_noop`
//!   * `request_stop_unblocks_parked_blocking_read` (parked `read_buffer_blocking`
//!     returns the fatal `StreamEnded` once `request_stop` flips the mock terminal).
//!
//! What the mock cannot prove — and what this file locks in — is the same
//! contract through a **real** platform backend (PipeWire / WASAPI / CoreAudio):
//!
//!   1. `request_stop()` on a live, running capture drives the real bridge to a
//!      terminal state, so `is_running()` goes false and the *terminal-observable*
//!      reads (`read_chunk_nonblocking` / `read_buffer_blocking`) surface the
//!      **fatal** `AudioError::StreamEnded` after the drainable tail — not the
//!      recoverable `StreamReadError`, and not an infinite block.
//!   2. `request_stop()` is a no-op-safe unblock primitive: it keeps the stream
//!      handle alive (unlike `stop()`, which takes it), so it is the path a
//!      consumer uses to end a blocking read loop cleanly per ADR-0003.
//!   3. The `stop()` vs `request_stop()` distinction: after the owning `stop()`
//!      the handle no longer holds a stream, so the reads report
//!      "not initialized"/"not running" (recoverable) rather than `StreamEnded`.
//!      This is a deliberate API difference and downstream bindings depend on it.
//!
//! Gating: these run under `require_system_capture!()` (audio infra + macOS TCC
//! gate). On a deterministic source (`RSAC_CI_AUDIO_DETERMINISTIC=1`) the
//! setup path HARD-FAILS on build/start failure; elsewhere it soft-skips.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rsac::{AudioCaptureBuilder, AudioError, CaptureTarget};

use crate::helpers;

/// Shared build/start policy: hard-fail under a deterministic source (the
/// backend is guaranteed present), soft-skip on a non-deterministic host.
/// Returns `None` when the test should skip (setup failed on a tolerant host).
fn build_and_start_system() -> Option<rsac::AudioCapture> {
    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            if helpers::deterministic_audio_env() {
                panic!("deterministic source: SystemDefault build failed: {e:?}");
            }
            eprintln!(
                "[ci_audio] lifecycle_terminal: build failed (non-deterministic host): {e:?}"
            );
            return None;
        }
    };

    if let Err(e) = capture.start() {
        if helpers::deterministic_audio_env() {
            panic!("deterministic source: SystemDefault start failed: {e:?}");
        }
        eprintln!("[ci_audio] lifecycle_terminal: start failed (non-deterministic host): {e:?}");
        return None;
    }

    Some(capture)
}

/// `request_stop()` on a live capture must flip the real bridge terminal:
/// `is_running()` becomes false and a subsequent *terminal-observable* read
/// (`read_chunk_nonblocking`) yields the FATAL `StreamEnded` once the buffered
/// tail is drained — never an endless `Ok(None)` and never the recoverable
/// `StreamReadError`.
///
/// This is the end-to-end complement to the mock-driven
/// `request_stop_unblocks_parked_blocking_read` unit test: it proves the real
/// backend honors ADR-0003's terminal-error contract, not just the mock.
#[test]
fn request_stop_drives_real_stream_terminal() {
    require_system_capture!();

    let capture = match build_and_start_system() {
        Some(c) => c,
        None => return,
    };

    assert!(
        capture.is_running(),
        "capture must be running after start()"
    );

    // Best-effort unblock: signals the stream terminal WITHOUT taking the
    // handle, so the terminal-observable reads below still have a stream to
    // consult (that is the whole point of request_stop vs stop).
    capture.request_stop();

    assert!(
        !capture.is_running(),
        "request_stop() must flip is_running() false on the real bridge"
    );

    // Drain the tail: try_read_chunk (via read_chunk_nonblocking) may yield a
    // few buffered `Ok(Some)` / `Ok(None)` before the ring empties, then MUST
    // surface the fatal terminal StreamEnded. Bound the loop so a broken
    // terminal signal fails as a timeout rather than hanging.
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut saw_terminal = false;
    let mut saw_recoverable_not_running = false;

    while Instant::now() < deadline {
        match capture.read_chunk_nonblocking() {
            Ok(Some(_buf)) => {
                // Drainable tail while Stopping — keep draining.
            }
            Ok(None) => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(AudioError::StreamEnded { .. }) => {
                saw_terminal = true;
                break;
            }
            Err(AudioError::StreamReadError { reason }) => {
                // Some backends may momentarily report the recoverable
                // "not running" before the ring drains to terminal; record it
                // but keep polling for the terminal signal.
                saw_recoverable_not_running = true;
                eprintln!("[ci_audio] lifecycle_terminal: transient StreamReadError: {reason}");
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(other) => panic!(
                "read_chunk_nonblocking after request_stop must return StreamEnded \
                 (fatal) or a transient StreamReadError, got: {other:?}"
            ),
        }
    }

    assert!(
        saw_terminal,
        "read_chunk_nonblocking must eventually surface the fatal StreamEnded \
         after request_stop() (saw_recoverable_not_running={saw_recoverable_not_running}); \
         the real backend is not honoring ADR-0003's terminal contract"
    );

    // The terminal error must classify as fatal so a `while !is_fatal()` read
    // loop actually breaks (the exact busy-wait ADR-0003 fixes).
    match capture.read_chunk_nonblocking() {
        Err(e) => assert!(
            e.is_fatal(),
            "the terminal read error must be fatal (is_fatal()==true) so consumer \
             loops break; got a non-fatal {e:?}"
        ),
        Ok(_) => panic!("a terminal stream must keep returning the terminal error, not Ok"),
    }

    eprintln!("[ci_audio] ✅ request_stop() drove the real stream terminal (StreamEnded, fatal)");
}

/// `request_stop()` must unblock a reader parked in `read_chunk_blocking()`
/// on a real backend PROMPTLY (well under the blocking-read timeout), returning
/// the fatal `StreamEnded`. This is the real-backend version of the mock
/// `request_stop_unblocks_parked_blocking_read` unit test and guards the #28
/// unblock-primitive contract the C/Go bindings rely on.
///
/// Uses `read_chunk_blocking` — the terminal-PRESERVING blocking read — not
/// `read_buffer_blocking`, whose `is_running()` compatibility guard only
/// surfaces `StreamEnded` when the terminal transition lands *during* an
/// already-parked read (documented on the method). On a data-flowing stream
/// (e.g. the Linux null-sink monitor, which emits continuous silence frames
/// once the graph actually links — rsac-b106) the reader is usually mid-drain
/// when the terminal lands, so the race fires deterministically: this test
/// originally used `read_buffer_blocking` and only ever passed on backends
/// whose idle loopback kept the reader parked in-flight (evidence: runs
/// 28906084214 / 28907189823 / 28907740898).
#[test]
fn request_stop_unblocks_parked_blocking_read_real() {
    require_system_capture!();

    let capture = match build_and_start_system() {
        Some(c) => Arc::new(c),
        None => return,
    };

    let reader = {
        let capture = Arc::clone(&capture);
        std::thread::spawn(move || {
            // Loop through the drainable tail: data reads and transient
            // errors retry, only the fatal terminal ends the loop. Bounded so
            // a genuine hang fails the join-timeout below rather than
            // spinning forever.
            let deadline = Instant::now() + Duration::from_secs(8);
            loop {
                match capture.read_chunk_blocking() {
                    Ok(_buf) => {
                        // Real audio arrived before we stopped — keep reading.
                    }
                    Err(AudioError::StreamEnded { .. }) => return Ok(()),
                    Err(AudioError::StreamReadError { .. }) | Err(AudioError::Timeout { .. }) => {
                        // Transient: a recoverable read hiccup, or the bounded
                        // blocking-read window elapsing before the terminal
                        // lands. Retry until the deadline.
                        if Instant::now() > deadline {
                            return Err("timed out without StreamEnded".to_string());
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(other) => return Err(format!("unexpected error: {other:?}")),
                }
            }
        })
    };

    // Let the reader get into the blocking read, then signal the unblock.
    std::thread::sleep(Duration::from_millis(200));
    let signalled = Instant::now();
    capture.request_stop();

    let result = reader.join().expect("reader thread joins without panic");
    let elapsed = signalled.elapsed();

    match result {
        Ok(()) => {
            eprintln!(
                "[ci_audio] ✅ request_stop() unblocked parked read in {:?} (StreamEnded)",
                elapsed
            );
        }
        Err(e) => panic!("parked read_buffer_blocking was not unblocked cleanly: {e}"),
    }

    // The unblock must be prompt — request_stop exists precisely so a blocked
    // reader does not wait out the full blocking-read timeout. Generous ceiling
    // (2s) to tolerate headless-VM scheduling jitter while still catching a
    // "waited out the timeout" regression.
    assert!(
        elapsed < Duration::from_secs(2),
        "request_stop() should unblock a parked read promptly; took {elapsed:?}"
    );
}

/// The `stop()` vs `request_stop()` API distinction, on a real backend:
/// the owning `stop()` TAKES the stream handle, so afterwards the terminal-
/// observable reads report the recoverable "not initialized"/"not running"
/// (`StreamReadError`) — NOT `StreamEnded` (there is no stream left to be
/// terminal). Downstream bindings depend on this difference; lock it in.
#[test]
fn stop_takes_stream_reads_report_not_initialized() {
    require_system_capture!();

    let mut capture = match build_and_start_system() {
        Some(c) => c,
        None => return,
    };

    assert!(capture.is_running());

    capture.stop().expect("stop() should succeed");
    assert!(!capture.is_running(), "not running after stop()");
    assert!(capture.uptime().is_none(), "uptime cleared after stop()");

    // stop() dropped the stream handle, so the terminal-observable read reports
    // the recoverable not-initialized/not-running StreamReadError, not the
    // fatal StreamEnded.
    match capture.read_chunk_nonblocking() {
        Err(AudioError::StreamReadError { reason }) => {
            let low = reason.to_lowercase();
            assert!(
                low.contains("not initialized") || low.contains("not running"),
                "expected a lifecycle StreamReadError after stop(), got reason: {reason}"
            );
            eprintln!("[ci_audio] ✅ read after stop() → recoverable StreamReadError: {reason}");
        }
        other => panic!(
            "read after owning stop() must be a recoverable StreamReadError \
             (stream handle was taken), got: {other:?}"
        ),
    }
}

/// `stop()` is idempotent on a real backend and safe to interleave with
/// `request_stop()`: signalling a terminal via `request_stop()` and then
/// calling the owning `stop()` must both succeed without panic, and a second
/// `stop()` is a clean no-op.
#[test]
fn request_stop_then_stop_is_clean_and_idempotent() {
    require_system_capture!();

    let mut capture = match build_and_start_system() {
        Some(c) => c,
        None => return,
    };

    // Best-effort unblock first (does not take the handle)...
    capture.request_stop();
    // ...then the owning stop must still succeed and be authoritative.
    capture
        .stop()
        .expect("stop() after request_stop() must succeed");
    assert!(!capture.is_running());

    // Second stop is idempotent (no stream left) — must not panic.
    capture.stop().expect("second stop() must be a clean no-op");

    // request_stop after everything is torn down is also a no-op.
    capture.request_stop();

    eprintln!("[ci_audio] ✅ request_stop()/stop() interleave cleanly and idempotently");
}
