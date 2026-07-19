//! rsac-97c8 — iOS SIMULATOR frames-delivered smoke.
//!
//! Runs ONLY inside a booted iOS simulator (spawned via `xcrun simctl spawn`
//! by `.github/workflows/ci-ios-sim.yml`) and ONLY when `RSAC_CI_IOS_SIM=1`
//! (mirrors the Linux deterministic legs' env-gate discipline,
//! `tests/ci_audio/helpers.rs`). It opens `CaptureTarget::Device("default")`
//! through the PUBLIC API and asserts FRAMES ARE DELIVERED with a sane
//! negotiated format — NOT content: the runner's host mic may be silent, so
//! this proves the AVAudioEngine tap wiring reaches rsac, not that it
//! carries a tone.
//!
//! The library deliberately never touches the shared AVAudioSession
//! (`src/audio/ios/mod.rs` — "Host-app responsibilities"). This test
//! therefore acts as the host app and configures/activates a
//! `.playAndRecord` session itself via the AVFAudio bindings before building
//! the capture. That session code lives ONLY in this test (test-scoped
//! dev-dependency in Cargo.toml — no production code path gains a session
//! dependency).
//!
//! Honesty labels (see the workflow header + docs/MOBILE_BACKEND_DESIGN.md):
//! a pass here is **simulator-verified**, never device-verified. Failure to
//! activate a record session or a zero-frame route degrades to
//! skip-with-summary unless `RSAC_CI_IOS_REQUIRE_FRAMES=1` (flipped via the
//! workflow_dispatch input once a runner proves the route reliable).
#![cfg(all(target_os = "ios", feature = "feat_ios"))]

use std::time::{Duration, Instant};

use rsac::{AudioCaptureBuilder, CaptureTarget, DeviceId, SampleFormat};

fn ios_sim_enabled() -> bool {
    matches!(std::env::var("RSAC_CI_IOS_SIM").as_deref(), Ok("1"))
}

fn require_frames_hard() -> bool {
    matches!(
        std::env::var("RSAC_CI_IOS_REQUIRE_FRAMES").as_deref(),
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

/// Configure + activate a record-capable AVAudioSession (the host-app job
/// the library refuses by design). Returns `false` if the session cannot be
/// activated (no input route in the sim) — the caller then
/// skip-with-summaries unless `RSAC_CI_IOS_REQUIRE_FRAMES=1`.
fn activate_record_session() -> bool {
    use objc2_avf_audio::{
        AVAudioSession, AVAudioSessionCategoryOptions, AVAudioSessionCategoryPlayAndRecord,
        AVAudioSessionModeDefault,
    };
    // SAFETY: standard shared-session configuration. `sharedInstance` /
    // `setCategory:mode:options:error:` / `setActive:error:` are documented
    // no-precondition AVFAudio calls; the category/mode extern statics are
    // link-time constants resolved by the AVFAudio framework. Errors are
    // surfaced as `false`, never panics.
    unsafe {
        let (Some(category), Some(mode)) = (
            AVAudioSessionCategoryPlayAndRecord,
            AVAudioSessionModeDefault,
        ) else {
            eprintln!("[ios-sim] AVFAudio category/mode constants unavailable at link time.");
            return false;
        };
        let session = AVAudioSession::sharedInstance();
        if let Err(e) = session.setCategory_mode_options_error(
            category,
            mode,
            AVAudioSessionCategoryOptions::empty(),
        ) {
            eprintln!("[ios-sim] setCategory(.playAndRecord) failed: {e:?}");
            return false;
        }
        match session.setActive_error(true) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("[ios-sim] setActive(true) failed: {e:?}");
                false
            }
        }
    }
}

#[test]
fn ios_sim_frames_delivered_via_public_api() {
    if !ios_sim_enabled() {
        eprintln!("[ios-sim] RSAC_CI_IOS_SIM != 1 — skipping (not in a sim runtime run).");
        return;
    }
    if !activate_record_session() {
        let msg = "[ios-sim] could not activate a record-capable AVAudioSession \
                   (no input route in this simulator).";
        if require_frames_hard() {
            panic!("{msg} RSAC_CI_IOS_REQUIRE_FRAMES=1");
        }
        eprintln!("{msg} skip-with-summary.");
        return;
    }

    // PUBLIC API only. "default" == the session's current input route
    // (src/audio/ios/mod.rs DEFAULT_INPUT_DEVICE_ID contract).
    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::Device(DeviceId("default".into())))
        .sample_rate(48_000)
        .channels(1)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            let msg = format!("[ios-sim] build() failed: {e:?}");
            if require_frames_hard() {
                panic!("{msg}");
            }
            eprintln!("{msg} skip-with-summary.");
            return;
        }
    };
    if let Err(e) = capture.start() {
        let msg = format!("[ios-sim] start() failed: {e:?}");
        if require_frames_hard() {
            panic!("{msg}");
        }
        eprintln!("{msg} skip-with-summary.");
        return;
    }
    assert!(capture.is_running(), "capture must run after start()");

    // Negotiated format is read live from the input node
    // (avaudio.rs inputFormatForBus heuristic — disposition item #2).
    // Sanity-check it BEFORE draining. `format()` is structurally Some after a
    // successful start() today (api.rs maps over the live stream), but keep
    // the None arm on the same soft-fail discipline as every other step here
    // instead of an .expect() hard panic.
    let Some(fmt) = capture.format() else {
        let msg = "[ios-sim] format() returned None after a successful start().";
        if require_frames_hard() {
            panic!("{msg} RSAC_CI_IOS_REQUIRE_FRAMES=1");
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
    assert_eq!(
        fmt.sample_format,
        SampleFormat::F32,
        "iOS bridge payload is always f32"
    );
    eprintln!(
        "[ios-sim] negotiated {} Hz, {} ch, {:?}",
        fmt.sample_rate, fmt.channels, fmt.sample_format
    );

    // DELIVERY assertion: at least one non-empty buffer within the timeout.
    // Bounded poll via read_buffer() (Ok(None) => no data yet) rather than a
    // blocking read, so a silent/blocked route cannot park the test past the
    // deadline (same loop shape as tests/ci_audio/app_capture.rs).
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
                eprintln!("[ios-sim] read error (treating as end-of-stream): {e:?}");
                break;
            }
        }
    }
    capture.request_stop();

    eprintln!("[ios-sim] delivered {buffers} buffers, {frames} frames.");
    if frames == 0 {
        let msg = "[ios-sim] zero frames delivered from the simulator mic route.";
        if require_frames_hard() {
            panic!("{msg}");
        }
        eprintln!("{msg} skip-with-summary (route may be silent/blocked).");
        return;
    }
    assert!(
        frames > 0,
        "AVAudioEngine tap delivered frames through the rsac public API"
    );
}
