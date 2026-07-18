//! rsac-e6d3 — Android EMULATOR frames-delivered smoke.
//!
//! Runs ONLY on an Android emulator (pushed + executed via `adb shell` by
//! `.github/workflows/ci-android-emu.yml`) and ONLY when
//! `RSAC_CI_ANDROID_EMU=1` (mirrors the iOS twin `tests/ios_sim_smoke.rs`
//! and the Linux deterministic legs' env-gate discipline). It opens
//! `CaptureTarget::Device("default")` through the PUBLIC API and asserts
//! FRAMES ARE DELIVERED with a sane negotiated format — NOT content: the
//! emulator microphone is synthetic (host-audio or silence), so this proves
//! the AAudio input wiring reaches rsac, not that it carries a tone.
//!
//! The binary runs as the `shell` user. Whether that uid can open an AAudio
//! INPUT stream without an app context is UNVERIFIED (platform.xml grants
//! shell only INTERNET; any access would come via shell's
//! privileged/debuggable status or audio-GID membership on the emulator
//! image) — so a refused input stream degrades to skip-with-summary unless
//! `RSAC_CI_ANDROID_REQUIRE_FRAMES=1` (flipped via the workflow_dispatch
//! input once a runner proves the route reliable). If the route proves
//! permission-blocked, the follow-up is an instrumented androidTest app
//! holding `RECORD_AUDIO` properly.
//!
//! Honesty labels (see the workflow header + docs/MOBILE_BACKEND_DESIGN.md):
//! a pass here is **emulator-verified**, never device-verified.
#![cfg(all(target_os = "android", feature = "feat_android"))]

use std::time::{Duration, Instant};

use rsac::{AudioCaptureBuilder, CaptureTarget, DeviceId};

fn android_emu_enabled() -> bool {
    matches!(std::env::var("RSAC_CI_ANDROID_EMU").as_deref(), Ok("1"))
}

fn require_frames_hard() -> bool {
    matches!(
        std::env::var("RSAC_CI_ANDROID_REQUIRE_FRAMES").as_deref(),
        Ok("1")
    )
}

fn capture_timeout() -> Duration {
    let secs = std::env::var("RSAC_TEST_CAPTURE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);
    Duration::from_secs(secs)
}

/// The honest-refusal contract is a hard assertion even when the mic route
/// soft-fails: `SystemDefault` without a MediaProjection consent token must
/// refuse at `build()` preflight with `UserConsentRequired` (ADR-0013) —
/// this needs no working audio route, only the compiled preflight logic, so
/// there is no reason to soften it.
#[test]
fn android_emu_system_default_refuses_without_consent() {
    if !android_emu_enabled() {
        eprintln!("[android-emu] RSAC_CI_ANDROID_EMU != 1 — skipping (not an emulator run).");
        return;
    }
    let err = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .build()
        .expect_err("SystemDefault without a projection token must refuse at preflight");
    let msg = format!("{err}");
    assert!(
        msg.contains("consent") || msg.contains("with_android_projection"),
        "refusal must be the documented UserConsentRequired guidance, got: {msg}"
    );
    eprintln!("[android-emu] honest refusal verified: {msg}");
}

#[test]
fn android_emu_frames_delivered_via_public_api() {
    if !android_emu_enabled() {
        eprintln!("[android-emu] RSAC_CI_ANDROID_EMU != 1 — skipping (not an emulator run).");
        return;
    }

    // PUBLIC API only. "default" == the default AAudio input route
    // (src/audio/android/mod.rs DEFAULT_INPUT_DEVICE_ID contract).
    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Device(DeviceId("default".into())))
        .sample_rate(48_000)
        .channels(1)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("[android-emu] build() failed: {e:?}");
            if require_frames_hard() {
                panic!("{msg}");
            }
            eprintln!("{msg} skip-with-summary.");
            return;
        }
    };
    if let Err(e) = capture.start() {
        let msg = format!("[android-emu] start() failed: {e:?}");
        if require_frames_hard() {
            panic!("{msg}");
        }
        eprintln!("{msg} skip-with-summary.");
        return;
    }
    assert!(capture.is_running(), "capture must run after start()");

    // Negotiated-format sanity (disposition: real AAudio negotiation, not the
    // requested values echoed back). Same soft-fail discipline as every other
    // step (mirrors the iOS twin post-review shape).
    let Some(fmt) = capture.format() else {
        let msg = "[android-emu] format() returned None after a successful start().";
        if require_frames_hard() {
            panic!("{msg} RSAC_CI_ANDROID_REQUIRE_FRAMES=1");
        }
        capture.request_stop();
        eprintln!("{msg} skip-with-summary.");
        return;
    };
    assert!(
        (8_000..=96_000).contains(&fmt.sample_rate),
        "sane negotiated rate, got {}",
        fmt.sample_rate
    );
    assert!(
        fmt.channels == 1 || fmt.channels == 2,
        "sane channel count, got {}",
        fmt.channels
    );
    // AAudio negotiates I16 or F32 natively; either is valid here — log it
    // rather than over-asserting (the delivered AudioBuffer payload is f32
    // regardless, via the bridge conversion).
    eprintln!(
        "[android-emu] negotiated {} Hz, {} ch, {:?}",
        fmt.sample_rate, fmt.channels, fmt.sample_format
    );

    // DELIVERY assertion: at least one non-empty buffer within the timeout.
    // Bounded non-blocking poll (Ok(None) => no data yet) so a dead route
    // cannot park the test past the deadline. The emulator mic is synthetic:
    // assert frames arrived, never their content.
    let deadline = Instant::now() + capture_timeout();
    let mut frames = 0usize;
    let mut buffers = 0usize;
    while Instant::now() < deadline {
        match capture.read_buffer() {
            Ok(Some(buf)) => {
                let n = buf.num_frames();
                if n > 0 {
                    buffers += 1;
                    frames += n;
                }
                if buffers >= 3 && frames > 0 {
                    break;
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                eprintln!("[android-emu] read error (treating as end-of-stream): {e:?}");
                break;
            }
        }
    }
    capture.request_stop();

    eprintln!("[android-emu] delivered {buffers} buffers, {frames} frames.");
    if frames == 0 {
        let msg = "[android-emu] zero frames delivered from the emulator mic route.";
        if require_frames_hard() {
            panic!("{msg}");
        }
        eprintln!("{msg} skip-with-summary (route may be silent/blocked).");
        return;
    }
    assert!(
        frames > 0,
        "AAudio input delivered frames through the rsac public API"
    );
}
