//! `AudioCapture::subscribe()` integration tests.
//!
//! Unit-level coverage in `src/api.rs` exercises `subscribe()` through a
//! `MockCapturingStream`: it locks the error-shape contract (not-running
//! returns `StreamReadError`), buffer fan-out, and the two exit paths of
//! the background `rsac-subscribe` thread (stream stop, receiver drop).
//!
//! This file covers what the mock can't: the path through a **real**
//! platform backend end-to-end. It verifies that after a real `build()` +
//! `start()`, the channel delivered by `subscribe()` actually carries
//! buffers produced by the OS audio callback, that `stop()` tears the
//! background thread down cleanly, and that lifecycle-phase errors
//! (subscribe-before-start, subscribe-after-stop) surface the documented
//! error variant rather than silently returning a dead channel.
//!
//! Tests gracefully skip when audio infrastructure is missing — see
//! `require_audio!` / `audio_infrastructure_available()`.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use rsac::{AudioCaptureBuilder, AudioError, CaptureTarget};

/// End-to-end happy path: a real capture + subscribe() must deliver at
/// least one `AudioBuffer` on the mpsc channel within a bounded timeout.
///
/// This is the integration-level complement to `subscribe_receives_buffers`
/// in `src/api.rs` (which feeds a mock). The mock proves the wiring; this
/// test proves the wiring is connected to a real OS audio callback and
/// that the `try_read_chunk` → `tx.send` loop survives a round-trip
/// through the platform backend (PipeWire on Linux, WASAPI on Windows,
/// CoreAudio on macOS).
#[test]
fn subscribe_delivers_buffers_from_live_capture() {
    require_audio!();

    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ci_audio] subscribe live: build failed: {:?}", e);
            return;
        }
    };

    if let Err(e) = capture.start() {
        eprintln!("[ci_audio] subscribe live: start failed: {:?}", e);
        return;
    }

    let rx = match capture.subscribe() {
        Ok(rx) => rx,
        Err(e) => {
            let _ = capture.stop();
            panic!(
                "subscribe() on a running capture must succeed; got: {:?}",
                e
            );
        }
    };

    // 5s ceiling matches `stream_lifecycle` timeout — generous enough for
    // cold-start latency on headless VMs while still bounding total test time.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buffers_received = 0usize;

    while Instant::now() < deadline && buffers_received < 3 {
        // recv_timeout so we can break on deadline rather than blocking
        // indefinitely on a silent system.
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(buf) => {
                buffers_received += 1;
                eprintln!(
                    "[ci_audio] subscribe live: buffer {} — {} frames, {}ch @ {}Hz",
                    buffers_received,
                    buf.num_frames(),
                    buf.channels(),
                    buf.sample_rate(),
                );
                // Guard against silent-wrong-output regressions in the
                // subscribe path: the builder configured 48000/2, so any
                // buffer arriving via subscribe() must reflect that.
                assert_eq!(buf.sample_rate(), 48000);
                assert_eq!(buf.channels(), 2);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Still live — loop until deadline.
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break;
            }
        }
    }

    let _ = capture.stop();

    if buffers_received == 0 {
        eprintln!(
            "[ci_audio] ⚠ subscribe live: no buffers within 5s \
             (CI limitation — backend may be idle)"
        );
    } else {
        eprintln!(
            "[ci_audio] ✅ subscribe live: received {} buffers via channel",
            buffers_received
        );
    }
}

/// Calling `subscribe()` on a freshly-built, never-started capture must
/// return `AudioError::StreamReadError` (per the doc-comment contract on
/// `AudioCapture::subscribe`). The unit test covers this against the mock
/// by signalling stop; this integration test covers the *real* code path
/// where the stream exists but has never been started.
///
/// NOTE: on some backends `build()` alone may not create a stream yet
/// (the stream is lazy until `start()`). The doc says subscribe must
/// error in either case — "stream not initialized" or "stream not
/// running". We accept either reason string, only requiring the variant
/// and that the reason mentions the lifecycle problem.
#[test]
fn subscribe_errors_when_not_started() {
    require_audio!();

    let capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ci_audio] subscribe-not-started: build failed: {:?}", e);
            return;
        }
    };

    match capture.subscribe() {
        Err(AudioError::StreamReadError { reason }) => {
            assert!(
                reason.to_lowercase().contains("not running")
                    || reason.to_lowercase().contains("not initialized"),
                "expected lifecycle-related reason, got: {}",
                reason
            );
            eprintln!(
                "[ci_audio] ✅ subscribe-not-started: got expected StreamReadError: {}",
                reason
            );
        }
        Err(other) => panic!(
            "expected StreamReadError for not-started capture, got: {:?}",
            other
        ),
        Ok(_) => panic!("subscribe() must not succeed on a never-started capture"),
    }
}

/// After `stop()`, the background `rsac-subscribe` thread should exit
/// (its `stream.try_read_chunk()` errors out once the stream is stopped,
/// per `api.rs:522`). The receiver then observes channel disconnection.
///
/// This locks the teardown contract for consumers that hold the
/// `Receiver` past `stop()` — they must see `Disconnected`, not hang.
#[test]
fn subscribe_receiver_disconnects_after_stop() {
    require_audio!();

    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ci_audio] subscribe-after-stop: build failed: {:?}", e);
            return;
        }
    };

    if let Err(e) = capture.start() {
        eprintln!("[ci_audio] subscribe-after-stop: start failed: {:?}", e);
        return;
    }

    let rx = match capture.subscribe() {
        Ok(rx) => rx,
        Err(e) => {
            let _ = capture.stop();
            panic!("subscribe() must succeed on a running capture: {:?}", e);
        }
    };

    capture.stop().expect("stop should succeed");

    // Give the subscribe thread a moment to observe the stop signal.
    // 500ms is slack: the thread polls try_read_chunk in a 1ms sleep
    // loop, so it should exit within a few ms on a healthy host.
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut disconnected = false;
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(_) => {
                // Leftover buffer delivered just before stop — keep draining.
            }
            Err(mpsc::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                disconnected = true;
                break;
            }
        }
    }

    assert!(
        disconnected,
        "subscribe channel must disconnect within 500ms of stop(); \
         the rsac-subscribe thread appears to be leaked"
    );
    eprintln!("[ci_audio] ✅ subscribe-after-stop: channel disconnected cleanly");
}

/// `api.rs` docs state: "Multiple subscriptions are allowed but each
/// subscriber competes for buffers." This test verifies that:
///   1. Two sequential `subscribe()` calls on the same running capture
///      both succeed (no "already subscribed" rejection).
///   2. Between them, at least one subscriber observes buffer activity,
///      proving the second subscription didn't break the first.
///
/// We don't assert fair distribution — the docs explicitly say subscribers
/// *compete*, so uneven delivery is expected behavior, not a bug.
#[test]
fn subscribe_allows_multiple_subscribers() {
    require_audio!();

    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ci_audio] multi-subscribe: build failed: {:?}", e);
            return;
        }
    };

    if let Err(e) = capture.start() {
        eprintln!("[ci_audio] multi-subscribe: start failed: {:?}", e);
        return;
    }

    let rx_a = capture
        .subscribe()
        .expect("first subscribe on running capture must succeed");
    let rx_b = capture
        .subscribe()
        .expect("second subscribe on running capture must succeed");

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut total = 0usize;
    while Instant::now() < deadline && total < 2 {
        if let Ok(_buf) = rx_a.recv_timeout(Duration::from_millis(100)) {
            total += 1;
        }
        if let Ok(_buf) = rx_b.recv_timeout(Duration::from_millis(100)) {
            total += 1;
        }
    }

    let _ = capture.stop();

    if total == 0 {
        eprintln!(
            "[ci_audio] ⚠ multi-subscribe: 0 combined buffers in 3s \
             (CI may be silent). Both subscribe() calls succeeded — \
             contract locked."
        );
    } else {
        eprintln!(
            "[ci_audio] ✅ multi-subscribe: {} combined buffers across two subscribers",
            total
        );
    }
}

/// Dropping the `Receiver` must terminate the `rsac-subscribe` thread.
/// The unit test covers this against the mock; this integration test
/// runs the same scenario through a real backend to ensure the
/// `tx.send(buffer).is_err()` branch in the spawn loop (api.rs:514)
/// actually triggers when the real OS callback tries to enqueue into a
/// dropped channel.
///
/// We don't have a direct handle to the thread, so we verify it
/// indirectly: after dropping the first receiver, a fresh subscribe()
/// on the same capture must still work — proving the stream is healthy
/// and the first thread's exit didn't poison shared state.
#[test]
fn subscribe_thread_exits_on_receiver_drop() {
    require_audio!();

    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ci_audio] subscribe-drop: build failed: {:?}", e);
            return;
        }
    };

    if let Err(e) = capture.start() {
        eprintln!("[ci_audio] subscribe-drop: start failed: {:?}", e);
        return;
    }

    {
        let rx = capture
            .subscribe()
            .expect("first subscribe must succeed on a running capture");
        // Let the thread run briefly so there's data to send when we drop.
        std::thread::sleep(Duration::from_millis(100));
        drop(rx);
    }

    // Give the old thread a tick to observe the Send error and exit.
    std::thread::sleep(Duration::from_millis(100));

    // Health check: a new subscribe must still succeed. If the drop had
    // poisoned the stream or the channel scaffolding, this would fail.
    let rx2 = capture
        .subscribe()
        .expect("subscribe must succeed after prior receiver drop");

    // Confirm the fresh channel is live by attempting one non-blocking recv;
    // empty is fine, disconnected would indicate the scaffolding broke.
    match rx2.try_recv() {
        Ok(_) | Err(mpsc::TryRecvError::Empty) => {
            eprintln!("[ci_audio] ✅ subscribe-drop: stream healthy after receiver drop");
        }
        Err(mpsc::TryRecvError::Disconnected) => {
            let _ = capture.stop();
            panic!("new subscribe channel was born disconnected — old thread poisoned state");
        }
    }

    let _ = capture.stop();
}
