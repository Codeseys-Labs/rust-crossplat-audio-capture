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

use crate::helpers;

/// Setup-failure policy shared by the live-capture tests in this file.
///
/// Under a deterministic source (`RSAC_CI_AUDIO_DETERMINISTIC=1`, Linux null
/// sink + 440 Hz tone) the backend is guaranteed present, so a build/start
/// failure is a real regression and must HARD-FAIL. On non-deterministic
/// hosts the same failure is tolerated CI flakiness and we soft-skip.
fn fail_or_skip(label: &str, detail: &str) {
    if helpers::deterministic_audio_env() {
        panic!(
            "deterministic source: {label} failed ({detail}) — capture must work \
             under RSAC_CI_AUDIO_DETERMINISTIC=1"
        );
    }
    eprintln!("[ci_audio] subscribe: {label} failed (non-deterministic host): {detail}; skipping");
}

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
    // This test captures CaptureTarget::SystemDefault, which on macOS is a
    // Process Tap behind the kTCCServiceAudioCapture gate — without the grant
    // the tap "succeeds" but delivers pure silence, so the #34 content checks
    // can never pass. Gate like system_capture.rs / lifecycle_terminal.rs do
    // (no-op on Linux/Windows, where macos_tcc_available() is always true).
    require_system_capture!();

    // #34: feed a known 440 Hz tone through SystemDefault loopback so the
    // subscribe path can be CONTENT-verified (not just liveness-verified) on a
    // deterministic source. Mirrors system_capture.rs. On a non-deterministic
    // host the player may be unavailable; that path still soft-skips below.
    let wav_path = helpers::generate_test_wav(5.0, 48000, 2);
    let player = helpers::spawn_test_tone_player(&wav_path);
    std::thread::sleep(Duration::from_millis(500));

    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            if let Some(p) = player {
                helpers::stop_player(p);
            }
            fail_or_skip("subscribe live build", &format!("{e:?}"));
            return;
        }
    };

    if let Err(e) = capture.start() {
        if let Some(p) = player {
            helpers::stop_player(p);
        }
        fail_or_skip("subscribe live start", &format!("{e:?}"));
        return;
    }

    let rx = match capture.subscribe() {
        Ok(rx) => rx,
        Err(e) => {
            let _ = capture.stop();
            if let Some(p) = player {
                helpers::stop_player(p);
            }
            panic!(
                "subscribe() on a running capture must succeed; got: {:?}",
                e
            );
        }
    };

    // 5s ceiling matches `stream_lifecycle` timeout — generous enough for
    // cold-start latency on headless VMs while still bounding total test time.
    // Collect for up to 5s. We need MORE than 3 buffers now: content checks
    // (RMS/tone) want enough audio to clear cold-start silence, so accumulate
    // until we have a confident content observation or the deadline hits.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buffers_received = 0usize;
    let mut got_non_silence = false;
    let mut got_tone = false;

    while Instant::now() < deadline && !(got_non_silence && got_tone) {
        // recv_timeout so we can break on deadline rather than blocking
        // indefinitely on a silent system.
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(buf) => {
                buffers_received += 1;
                if buffers_received <= 3 {
                    eprintln!(
                        "[ci_audio] subscribe live: buffer {} — {} frames, {}ch @ {}Hz",
                        buffers_received,
                        buf.num_frames(),
                        buf.channels(),
                        buf.sample_rate(),
                    );
                }
                // Guard against silent-wrong-output regressions in the
                // subscribe path. The builder *requested* 48000/2, but rsac
                // does not resample, so a shared-mode backend delivers the
                // device's negotiated format; equality with the request only
                // holds under a deterministic source. See
                // `helpers::assert_buffer_format`.
                helpers::assert_buffer_format(&buf, 48000, 2);

                // #34: CONTENT verification — the subscribe path must carry the
                // real 440 Hz tone, not just well-formed silence. Accumulate
                // observations; asserted (hard) under a deterministic source below.
                if !got_non_silence && helpers::verify_non_silence(&buf, 0.001) {
                    got_non_silence = true;
                }
                if !got_tone && helpers::verify_tone_present(&buf, 440.0) {
                    got_tone = true;
                }
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
    if let Some(p) = player {
        helpers::stop_player(p);
    }
    let _ = std::fs::remove_file(&wav_path);

    if buffers_received == 0 {
        // A deterministic source feeds the null sink a known tone, so the
        // subscribe channel MUST deliver buffers — zero means the
        // try_read_chunk → tx.send loop is broken end-to-end. Hard-fail.
        assert!(
            !helpers::deterministic_audio_env(),
            "deterministic source: subscribe() delivered 0 buffers within 5s — \
             the live capture → channel path is broken"
        );
        eprintln!(
            "[ci_audio] ⚠ subscribe live: no buffers within 5s \
             (non-deterministic host — backend may be idle)"
        );
        return;
    }

    eprintln!(
        "[ci_audio] ✅ subscribe live: received {} buffers (non_silence={}, tone={})",
        buffers_received, got_non_silence, got_tone
    );

    // #34: under a deterministic source the 440 Hz tone is guaranteed to flow,
    // so the subscribe path must deliver NON-SILENT audio carrying that tone.
    // Anything else is a silent-wrong-output regression (the channel delivers
    // empty/garbage buffers), not host flakiness. On a non-deterministic host
    // these stay soft (logged above) since real routing can be silent.
    if helpers::deterministic_audio_env() {
        assert!(
            got_non_silence,
            "deterministic source: subscribe() delivered only silence across {} buffers — \
             the live capture → channel path is dropping signal",
            buffers_received
        );
        assert!(
            got_tone,
            "deterministic source: subscribe() never carried the 440 Hz tone across {} buffers — \
             the channel is delivering wrong-content audio",
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
            fail_or_skip("subscribe-not-started build", &format!("{e:?}"));
            return;
        }
    };

    match capture.subscribe() {
        Err(AudioError::StreamReadError { reason }) => {
            assert!(
                reason.to_lowercase().contains("not running")
                    || reason.to_lowercase().contains("not initialized")
                    || reason.to_lowercase().contains("no active stream")
                    || reason.to_lowercase().contains("never started"),
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
            fail_or_skip("subscribe-after-stop build", &format!("{e:?}"));
            return;
        }
    };

    if let Err(e) = capture.start() {
        fail_or_skip("subscribe-after-stop start", &format!("{e:?}"));
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
            fail_or_skip("multi-subscribe build", &format!("{e:?}"));
            return;
        }
    };

    if let Err(e) = capture.start() {
        fail_or_skip("multi-subscribe start", &format!("{e:?}"));
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
        // Both subscribe() calls succeeded (asserted above). Under a
        // deterministic source at least one subscriber must also observe the
        // tone, proving the second subscription didn't starve the pipeline.
        assert!(
            !helpers::deterministic_audio_env(),
            "deterministic source: two competing subscribers received 0 combined \
             buffers in 3s — the fan-out path is broken"
        );
        eprintln!(
            "[ci_audio] ⚠ multi-subscribe: 0 combined buffers in 3s \
             (non-deterministic host may be silent). Both subscribe() calls \
             succeeded — contract locked."
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
            fail_or_skip("subscribe-drop build", &format!("{e:?}"));
            return;
        }
    };

    if let Err(e) = capture.start() {
        fail_or_skip("subscribe-drop start", &format!("{e:?}"));
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
            eprintln!("[ci_audio] subscribe-drop: fresh channel connected after receiver drop");
        }
        Err(mpsc::TryRecvError::Disconnected) => {
            let _ = capture.stop();
            panic!("new subscribe channel was born disconnected — old thread poisoned state");
        }
    }

    // Strengthen the "stream healthy" claim under a deterministic source: a
    // not-disconnected channel proves the scaffolding survived, but only an
    // actually-delivered buffer proves the new subscribe thread is reading
    // live audio (i.e. the prior thread's exit didn't break the producer or
    // leave a poisoned consumer). On non-deterministic hosts the stream may
    // be silent, so we keep the connected-only check there.
    if helpers::deterministic_audio_env() {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut delivered = false;
        while Instant::now() < deadline {
            match rx2.recv_timeout(Duration::from_millis(250)) {
                Ok(_) => {
                    delivered = true;
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let _ = capture.stop();
        assert!(
            delivered,
            "deterministic source: fresh subscribe() after a receiver drop \
             delivered no buffers within 5s — the prior thread's exit broke the \
             live capture pipeline"
        );
        eprintln!("[ci_audio] ✅ subscribe-drop: fresh subscriber received live audio");
        return;
    }

    eprintln!("[ci_audio] ✅ subscribe-drop: stream healthy after receiver drop");
    let _ = capture.stop();
}
