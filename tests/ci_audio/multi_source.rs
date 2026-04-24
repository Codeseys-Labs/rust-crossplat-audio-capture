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

/// Happy path: two `SystemDefault` captures, both running at once.
///
/// Spawns capture A + capture B targeting the same `SystemDefault`, runs
/// them for 2 seconds, asserts both produce at least one `AudioBuffer`
/// on their respective `subscribe()` channels. Neither capture's output
/// should be empty just because the other is also running.
#[test]
fn two_system_captures_both_produce_buffers() {
    require_audio!();

    // Play a test tone so SystemDefault loopback has audio to capture.
    let wav_path = helpers::generate_test_wav(5.0, 48000, 2);
    let player = helpers::spawn_test_tone_player(&wav_path);
    if player.is_none() {
        eprintln!(
            "[ci_audio] multi_source: no test-tone player available; \
             skipping (environment cannot produce audio)"
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
            eprintln!("[ci_audio] multi_source: capture A build failed: {:?}", e);
            stop(player);
            return;
        }
    };
    if let Err(e) = capture_a.start() {
        eprintln!("[ci_audio] multi_source: capture A start failed: {:?}", e);
        stop(player);
        return;
    }
    let rx_a = match capture_a.subscribe() {
        Ok(rx) => rx,
        Err(e) => {
            eprintln!(
                "[ci_audio] multi_source: capture A subscribe failed: {:?}",
                e
            );
            let _ = capture_a.stop();
            stop(player);
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
            eprintln!("[ci_audio] multi_source: capture B build failed: {:?}", e);
            let _ = capture_a.stop();
            stop(player);
            return;
        }
    };
    if let Err(e) = capture_b.start() {
        eprintln!("[ci_audio] multi_source: capture B start failed: {:?}", e);
        let _ = capture_a.stop();
        stop(player);
        return;
    }
    let rx_b = match capture_b.subscribe() {
        Ok(rx) => rx,
        Err(e) => {
            eprintln!(
                "[ci_audio] multi_source: capture B subscribe failed: {:?}",
                e
            );
            let _ = capture_a.stop();
            let _ = capture_b.stop();
            stop(player);
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

    // Skip when the environment simply isn't producing audio for either
    // capture — this matches the graceful-skip philosophy of the other
    // ci_audio tests.
    if buffers_a == 0 && buffers_b == 0 {
        eprintln!(
            "[ci_audio] multi_source: neither capture produced buffers; \
             likely no functional audio loopback — skipping"
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
    require_audio!();

    let wav_path = helpers::generate_test_wav(5.0, 48000, 2);
    let player = helpers::spawn_test_tone_player(&wav_path);
    if player.is_none() {
        eprintln!("[ci_audio] stop_isolation: no test-tone player; skipping");
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
            eprintln!("[ci_audio] stop_isolation: capture A build failed: {:?}", e);
            stop(player);
            return;
        }
    };
    if capture_a.start().is_err() {
        stop(player);
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
            eprintln!("[ci_audio] stop_isolation: capture B build failed: {:?}", e);
            let _ = capture_a.stop();
            stop(player);
            return;
        }
    };
    if capture_b.start().is_err() {
        let _ = capture_a.stop();
        stop(player);
        return;
    }

    let rx_b = match capture_b.subscribe() {
        Ok(rx) => rx,
        Err(_) => {
            let _ = capture_a.stop();
            let _ = capture_b.stop();
            stop(player);
            return;
        }
    };

    // Warm up: drain a few buffers to confirm both are alive. If nothing
    // arrives during warmup, the environment isn't producing audio —
    // skip rather than fail.
    std::thread::sleep(Duration::from_millis(500));
    let mut warmup_seen = 0usize;
    while rx_b.try_recv().is_ok() {
        warmup_seen += 1;
    }
    if warmup_seen == 0 {
        eprintln!(
            "[ci_audio] stop_isolation: no buffers during warmup; \
             no functional loopback — skipping"
        );
        let _ = capture_a.stop();
        let _ = capture_b.stop();
        stop(player);
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
    require_audio!();

    let wav_path = helpers::generate_test_wav(5.0, 48000, 2);
    let player = helpers::spawn_test_tone_player(&wav_path);
    if player.is_none() {
        eprintln!("[ci_audio] mixed_target: no test-tone player; skipping");
        return;
    }
    std::thread::sleep(Duration::from_millis(500));

    // Build system-default capture.
    let mut sys_capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            stop(player);
            return;
        }
    };
    if sys_capture.start().is_err() {
        stop(player);
        return;
    }
    let rx_sys = match sys_capture.subscribe() {
        Ok(rx) => rx,
        Err(_) => {
            let _ = sys_capture.stop();
            stop(player);
            return;
        }
    };

    // Enumerate + pick first device. Skip cleanly if none.
    let enumerator = match rsac::get_device_enumerator() {
        Ok(e) => e,
        Err(_) => {
            let _ = sys_capture.stop();
            stop(player);
            return;
        }
    };
    let devices = match enumerator.enumerate_devices() {
        Ok(d) => d,
        Err(_) => {
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
                "[ci_audio] mixed_target: device capture build failed: {:?}",
                e
            );
            let _ = sys_capture.stop();
            stop(player);
            return;
        }
    };
    if dev_capture.start().is_err() {
        let _ = sys_capture.stop();
        stop(player);
        return;
    }
    let rx_dev = match dev_capture.subscribe() {
        Ok(rx) => rx,
        Err(_) => {
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

    // Skip when nothing is captured (no loopback environment).
    if buffers_sys == 0 && buffers_dev == 0 {
        eprintln!(
            "[ci_audio] mixed_target: neither capture produced buffers; \
             skipping"
        );
        return;
    }

    assert!(
        buffers_sys > 0,
        "system-default capture produced no buffers while device capture produced {} \
         — cross-target isolation regression",
        buffers_dev
    );
    assert!(
        buffers_dev > 0,
        "device capture produced no buffers while system-default capture produced {} \
         — cross-target isolation regression",
        buffers_sys
    );
}
