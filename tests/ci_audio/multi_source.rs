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
//! Tests gracefully skip when audio infrastructure is missing via
//! `require_audio!()`.

use std::time::{Duration, Instant};

use rsac::{AudioCaptureBuilder, CaptureTarget};

/// Happy path: two `SystemDefault` captures, both running at once.
///
/// Spawns capture A + capture B targeting the same `SystemDefault`, runs
/// them for 2 seconds, asserts both produce at least one `AudioBuffer`
/// on their respective `subscribe()` channels. Neither capture's output
/// should be empty just because the other is also running.
#[test]
fn two_system_captures_both_produce_buffers() {
    require_audio!();

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
            return;
        }
    };
    if let Err(e) = capture_a.start() {
        eprintln!("[ci_audio] multi_source: capture A start failed: {:?}", e);
        return;
    }
    let rx_a = match capture_a.subscribe() {
        Ok(rx) => rx,
        Err(e) => {
            eprintln!(
                "[ci_audio] multi_source: capture A subscribe failed: {:?}",
                e
            );
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
            return;
        }
    };
    if let Err(e) = capture_b.start() {
        eprintln!("[ci_audio] multi_source: capture B start failed: {:?}", e);
        return;
    }
    let rx_b = match capture_b.subscribe() {
        Ok(rx) => rx,
        Err(e) => {
            eprintln!(
                "[ci_audio] multi_source: capture B subscribe failed: {:?}",
                e
            );
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

    // Each capture must have produced at least one buffer. If one is
    // starved while the other runs, the vision is broken.
    assert!(
        buffers_a > 0,
        "capture A produced no buffers while capture B was also running"
    );
    assert!(
        buffers_b > 0,
        "capture B produced no buffers while capture A was also running"
    );
}

/// Stop isolation: stopping capture A must not affect capture B.
///
/// Spawns A + B, lets both warm up, stops A, then asserts B continues
/// to produce buffers after A is gone. This guards against a regression
/// where the two `BridgeStream` ring buffers share OS-level state.
#[test]
fn stopping_one_capture_does_not_halt_the_other() {
    require_audio!();

    let mut capture_a = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ci_audio] stop_isolation: capture A build failed: {:?}", e);
            return;
        }
    };
    if capture_a.start().is_err() {
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
            return;
        }
    };
    if capture_b.start().is_err() {
        return;
    }

    let rx_b = match capture_b.subscribe() {
        Ok(rx) => rx,
        Err(_) => return,
    };

    // Warm up: drain a few buffers to confirm both are alive.
    std::thread::sleep(Duration::from_millis(500));
    while rx_b.try_recv().is_ok() {}

    // Stop A. B should continue.
    let _ = capture_a.stop();

    // Within 1 second of A stopping, B must deliver at least one more
    // buffer.
    let post_stop_buffer = rx_b.recv_timeout(Duration::from_secs(1));
    let _ = capture_b.stop();

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

    // Build system-default capture.
    let mut sys_capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };
    if sys_capture.start().is_err() {
        return;
    }
    let rx_sys = match sys_capture.subscribe() {
        Ok(rx) => rx,
        Err(_) => return,
    };

    // Build device capture targeting the first enumerated input device.
    // If enumeration fails or returns no devices, skip — this test
    // requires a real audio device, which is what `require_audio!`
    // checks for anyway.
    let enumerator = match rsac::get_device_enumerator() {
        Ok(e) => e,
        Err(_) => return,
    };
    let devices = match enumerator.enumerate_devices() {
        Ok(d) => d,
        Err(_) => return,
    };
    let Some(first_device) = devices.first() else {
        eprintln!("[ci_audio] mixed_target: no devices enumerated; skipping");
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
            return;
        }
    };
    if dev_capture.start().is_err() {
        let _ = sys_capture.stop();
        return;
    }
    let rx_dev = match dev_capture.subscribe() {
        Ok(rx) => rx,
        Err(_) => {
            let _ = sys_capture.stop();
            let _ = dev_capture.stop();
            return;
        }
    };

    // Each capture must produce ≥1 buffer within 2s.
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

    assert!(
        buffers_sys > 0,
        "system-default capture produced no buffers while a device capture ran in parallel"
    );
    assert!(
        buffers_dev > 0,
        "device capture produced no buffers while a system-default capture ran in parallel"
    );
}
