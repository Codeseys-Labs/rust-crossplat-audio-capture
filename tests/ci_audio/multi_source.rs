//! Multi-simultaneous-capture integration tests.
//!
//! The vision (VISION.md § Multi-source): one process can spawn multiple
//! `AudioCapture` instances simultaneously, each with its own isolated
//! `BridgeStream` + ring buffer. No shared state means no interference
//! between captures — one stalled consumer can't starve another.
//!
//! This file asserts that contract end-to-end through a real platform
//! backend. The unit tests in `src/api.rs` cover the mock path (two
//! `MockCapturingStream`s run in the same process and produce distinct
//! buffers). This file covers what the mocks can't: two real captures,
//! both targeting live audio, both producing buffers from a real OS
//! audio callback, with no cross-talk.
//!
//! Skip policy:
//! - `require_audio!()` skips when no audio infrastructure is present
//! - If the test-tone player can't spawn, we skip (no audio to capture)
//! - If BOTH captures come back empty, we skip (environment has no
//!   working loopback — matches system_capture.rs philosophy)
//! - ONLY fail when ONE capture has buffers and the other doesn't:
//!   that's the actual multi-source isolation regression we're guarding
//!   against.

use std::time::{Duration, Instant};

use rsac::{AudioCaptureBuilder, CaptureTarget};

use crate::helpers;

/// Helper: stop a player if present. Consumes it.
fn stop(player: Option<std::process::Child>) {
    if let Some(p) = player {
        helpers::stop_player(p);
    }
}

/// Setup-failure policy: under a deterministic source (Linux null sink +
/// 440 Hz tone, `RSAC_CI_AUDIO_DETERMINISTIC=1`) the backend, player, and
/// capture pipeline are guaranteed to be present, so a build/start/subscribe
/// failure is a real regression and must HARD-FAIL. On non-deterministic
/// hosts the same failure is tolerated CI flakiness and we soft-skip.
///
/// `cleanup` runs before we decide, so resources are always released.
fn fail_or_skip(label: &str, detail: &str, cleanup: impl FnOnce()) {
    cleanup();
    if helpers::deterministic_audio_env() {
        panic!(
            "deterministic source: {label} failed ({detail}) — the multi-source \
             pipeline must work under RSAC_CI_AUDIO_DETERMINISTIC=1"
        );
    }
    eprintln!(
        "[ci_audio] multi_source: {label} failed (non-deterministic host): {detail}; skipping"
    );
}

/// Happy path: two `SystemDefault` captures, both running at once.
///
/// Spawns capture A + capture B targeting the same `SystemDefault`, runs
/// them for 2 seconds, asserts both produce at least one `AudioBuffer`
/// on their respective `subscribe()` channels. Neither capture's output
/// should be empty just because the other is also running.
#[test]
fn two_system_captures_both_produce_buffers() {
    require_system_capture!();

    // Play a test tone so SystemDefault loopback has audio to capture.
    let wav_path = helpers::generate_test_wav(5.0, 48000, 2);
    let player = helpers::spawn_test_tone_player(&wav_path);
    if player.is_none() {
        // A deterministic CI host ships pw-play/paplay; missing it is a setup
        // regression. Other hosts may genuinely lack a player — skip there.
        fail_or_skip(
            "no test-tone player",
            "spawn_test_tone_player returned None",
            || {},
        );
        return;
    }
    std::thread::sleep(Duration::from_millis(500));

    // Build + start capture A.
    let mut capture_a = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            fail_or_skip("capture A build", &format!("{e:?}"), || stop(player));
            return;
        }
    };
    if let Err(e) = capture_a.start() {
        fail_or_skip("capture A start", &format!("{e:?}"), || stop(player));
        return;
    }
    let rx_a = match capture_a.subscribe() {
        Ok(rx) => rx,
        Err(e) => {
            let _ = capture_a.stop();
            fail_or_skip("capture A subscribe", &format!("{e:?}"), || stop(player));
            return;
        }
    };

    // Build + start capture B.
    let mut capture_b = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = capture_a.stop();
            fail_or_skip("capture B build", &format!("{e:?}"), || stop(player));
            return;
        }
    };
    if let Err(e) = capture_b.start() {
        let _ = capture_a.stop();
        fail_or_skip("capture B start", &format!("{e:?}"), || stop(player));
        return;
    }
    let rx_b = match capture_b.subscribe() {
        Ok(rx) => rx,
        Err(e) => {
            let _ = capture_a.stop();
            let _ = capture_b.stop();
            fail_or_skip("capture B subscribe", &format!("{e:?}"), || stop(player));
            return;
        }
    };

    // Collect buffers from both for 2 seconds.
    let mut buffers_a = 0usize;
    let mut buffers_b = 0usize;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if rx_a.recv_timeout(Duration::from_millis(100)).is_ok() {
            buffers_a += 1;
        }
        if rx_b.recv_timeout(Duration::from_millis(100)).is_ok() {
            buffers_b += 1;
        }
    }

    let _ = capture_a.stop();
    let _ = capture_b.stop();
    stop(player);

    // Under a deterministic source both captures MUST see the 440 Hz tone:
    // the null sink is fed by a known player and neither consumer was stalled,
    // so zero buffers is a real regression, not "no loopback". Hard-assert.
    // On non-deterministic hosts we keep the graceful-skip philosophy.
    if buffers_a == 0 && buffers_b == 0 {
        assert!(
            !helpers::deterministic_audio_env(),
            "deterministic source: neither SystemDefault capture produced buffers \
             in 2s — the producer isn't pushing or multi-source fan-out is broken"
        );
        eprintln!(
            "[ci_audio] multi_source: neither capture produced buffers; \
             likely no functional audio loopback (non-deterministic host) — skipping"
        );
        return;
    }

    // If ONE is empty while the other has buffers, that IS the real
    // multi-source isolation regression we're guarding against.
    assert!(
        buffers_a > 0,
        "capture A produced no buffers while capture B produced {} — \
         multi-source isolation regression",
        buffers_b
    );
    assert!(
        buffers_b > 0,
        "capture B produced no buffers while capture A produced {} — \
         multi-source isolation regression",
        buffers_a
    );
}

/// Stop isolation: stopping capture A must not affect capture B.
///
/// Spawns A + B with a test tone playing, lets both warm up, stops A,
/// then asserts B continues to produce buffers after A is gone. Guards
/// against a regression where the two `BridgeStream` ring buffers share
/// OS-level state.
#[test]
fn stopping_one_capture_does_not_halt_the_other() {
    require_system_capture!();

    let wav_path = helpers::generate_test_wav(5.0, 48000, 2);
    let player = helpers::spawn_test_tone_player(&wav_path);
    if player.is_none() {
        fail_or_skip(
            "no test-tone player",
            "spawn_test_tone_player returned None",
            || {},
        );
        return;
    }
    std::thread::sleep(Duration::from_millis(500));

    let mut capture_a = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            fail_or_skip("capture A build", &format!("{e:?}"), || stop(player));
            return;
        }
    };
    if let Err(e) = capture_a.start() {
        fail_or_skip("capture A start", &format!("{e:?}"), || stop(player));
        return;
    }

    let mut capture_b = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = capture_a.stop();
            fail_or_skip("capture B build", &format!("{e:?}"), || stop(player));
            return;
        }
    };
    if let Err(e) = capture_b.start() {
        let _ = capture_a.stop();
        fail_or_skip("capture B start", &format!("{e:?}"), || stop(player));
        return;
    }

    let rx_b = match capture_b.subscribe() {
        Ok(rx) => rx,
        Err(e) => {
            let _ = capture_a.stop();
            let _ = capture_b.stop();
            fail_or_skip("capture B subscribe", &format!("{e:?}"), || stop(player));
            return;
        }
    };

    // Warm up: drain a few buffers to confirm both are alive. Under a
    // deterministic source the producer is guaranteed to be pushing, so a
    // silent warmup is a regression — hard-fail. On non-deterministic hosts
    // a silent warmup just means no functional loopback, so skip.
    std::thread::sleep(Duration::from_millis(500));
    let mut warmup_seen = 0usize;
    while rx_b.try_recv().is_ok() {
        warmup_seen += 1;
    }
    if warmup_seen == 0 {
        let deterministic = helpers::deterministic_audio_env();
        let _ = capture_a.stop();
        let _ = capture_b.stop();
        stop(player);
        assert!(
            !deterministic,
            "deterministic source: capture B saw no buffers during warmup — \
             the producer isn't pushing into the second capture"
        );
        eprintln!(
            "[ci_audio] stop_isolation: no buffers during warmup; \
             no functional loopback (non-deterministic host) — skipping"
        );
        return;
    }

    // Stop A. B should continue.
    let _ = capture_a.stop();

    // Within 1 second of A stopping, B must deliver at least one more
    // buffer.
    let post_stop_buffer = rx_b.recv_timeout(Duration::from_secs(1));
    let _ = capture_b.stop();
    stop(player);

    assert!(
        post_stop_buffer.is_ok(),
        "capture B stopped producing buffers after capture A was stopped \
         — the two captures should be isolated"
    );
}

/// Mixed-target: one `SystemDefault` + one `Device` capture in parallel.
///
/// Targets different `CaptureTarget` variants, so we exercise the
/// cross-backend-code-path isolation (device enumeration + default
/// selection use different internal paths on most platforms). If the
/// two code paths share a non-reentrant resource (e.g. a COM/ObjC
/// static), this test will hang or fail.
#[test]
fn mixed_target_captures_run_independently() {
    require_system_capture!();

    let wav_path = helpers::generate_test_wav(5.0, 48000, 2);
    let player = helpers::spawn_test_tone_player(&wav_path);
    if player.is_none() {
        fail_or_skip(
            "no test-tone player",
            "spawn_test_tone_player returned None",
            || {},
        );
        return;
    }
    std::thread::sleep(Duration::from_millis(500));

    // Build system-default capture. The SystemDefault path is guaranteed on a
    // deterministic host, so its setup failures hard-fail; the Device path
    // below stays graceful because an input device may genuinely be absent.
    let mut sys_capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            fail_or_skip("system-default build", &format!("{e:?}"), || stop(player));
            return;
        }
    };
    if let Err(e) = sys_capture.start() {
        fail_or_skip("system-default start", &format!("{e:?}"), || stop(player));
        return;
    }
    let rx_sys = match sys_capture.subscribe() {
        Ok(rx) => rx,
        Err(e) => {
            let _ = sys_capture.stop();
            fail_or_skip("system-default subscribe", &format!("{e:?}"), || {
                stop(player)
            });
            return;
        }
    };

    // Enumerate + pick first device. The Device path is genuinely
    // heterogeneous — a capable input device may not exist even on a
    // deterministic null-sink host — so these stay graceful skips.
    let enumerator = match rsac::get_device_enumerator() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[ci_audio] mixed_target: device enumerator unavailable: {e:?}; skipping");
            let _ = sys_capture.stop();
            stop(player);
            return;
        }
    };
    let devices = match enumerator.enumerate_devices() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[ci_audio] mixed_target: enumerate_devices failed: {e:?}; skipping");
            let _ = sys_capture.stop();
            stop(player);
            return;
        }
    };
    let Some(first_device) = devices.first() else {
        eprintln!("[ci_audio] mixed_target: no devices enumerated; skipping");
        let _ = sys_capture.stop();
        stop(player);
        return;
    };

    let mut dev_capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Device(first_device.id()))
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "[ci_audio] mixed_target: device capture build failed: {:?}; skipping",
                e
            );
            let _ = sys_capture.stop();
            stop(player);
            return;
        }
    };
    if let Err(e) = dev_capture.start() {
        eprintln!("[ci_audio] mixed_target: device capture start failed: {e:?}; skipping");
        let _ = sys_capture.stop();
        stop(player);
        return;
    }
    let rx_dev = match dev_capture.subscribe() {
        Ok(rx) => rx,
        Err(e) => {
            eprintln!("[ci_audio] mixed_target: device capture subscribe failed: {e:?}; skipping");
            let _ = sys_capture.stop();
            let _ = dev_capture.stop();
            stop(player);
            return;
        }
    };

    // Collect for 2s.
    let mut buffers_sys = 0usize;
    let mut buffers_dev = 0usize;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && (buffers_sys == 0 || buffers_dev == 0) {
        if rx_sys.recv_timeout(Duration::from_millis(100)).is_ok() {
            buffers_sys += 1;
        }
        if rx_dev.recv_timeout(Duration::from_millis(100)).is_ok() {
            buffers_dev += 1;
        }
    }

    let _ = sys_capture.stop();
    let _ = dev_capture.stop();
    stop(player);

    // Skip when nothing is captured (no loopback environment). Under a
    // deterministic source the SystemDefault capture is guaranteed to receive
    // the tone, so both-zero implies a system-capture regression — hard-fail.
    if buffers_sys == 0 && buffers_dev == 0 {
        assert!(
            !helpers::deterministic_audio_env(),
            "deterministic source: neither system-default nor device capture \
             produced buffers — the SystemDefault path must capture the tone"
        );
        eprintln!(
            "[ci_audio] mixed_target: neither capture produced buffers \
             (non-deterministic host); skipping"
        );
        return;
    }

    // The isolation property under test — two captures coexisting for 2s
    // without one halting the other — is already proven by reaching here: both
    // built, started, subscribed, ran concurrently, and stopped cleanly with no
    // panic. The buffer *counts* prove liveness, but only the SystemDefault
    // target is guaranteed live under a deterministic source (the null sink /
    // VB-CABLE feeds it the tone). The Device target is `devices.first()` —
    // arbitrary ordering, and the comment above notes it may be an output or a
    // silent input with no loopback — so it stays best-effort on every host
    // (asserting it > 0 was the bug: it hard-failed on a host whose first
    // device cannot capture while system-default could).
    if helpers::deterministic_audio_env() {
        assert!(
            buffers_sys > 0,
            "deterministic source: system-default capture must receive the tone \
             (got 0 buffers; device target got {}) — the SystemDefault path is a regression",
            buffers_dev
        );
    }
    eprintln!(
        "[ci_audio] mixed_target: ran concurrently without mutual starvation \
         (system-default={} buffers, device={} buffers)",
        buffers_sys, buffers_dev
    );
}
