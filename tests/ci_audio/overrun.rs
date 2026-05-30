//! Ring-buffer overrun integration test (G8 — `overrun_count()`).
//!
//! Unit-level coverage in `src/api.rs` proves `overrun_count()` reflects the
//! mock stream's counter. What the mock can't prove: that a *real* OS audio
//! callback, producing buffers faster than a deliberately-stalled consumer
//! drains them, actually drives the lock-free ring buffer to overflow and
//! increments the counter end-to-end.
//!
//! Strategy: start a `SystemDefault` capture, then NEVER read from it. The
//! producer (OS callback thread) keeps pushing `AudioBuffer`s into the
//! `rtrb` ring (default capacity ~64 buffers); once full,
//! `push_or_drop`/`push_samples_or_drop` start dropping and bumping the
//! overrun counter. After enough wall-clock time the count must be > 0.
//!
//! Determinism gate: under `RSAC_CI_AUDIO_DETERMINISTIC=1` (Linux null sink or
//! Windows VB-CABLE feeding a 440 Hz tone) the producer is guaranteed to be
//! running, so we HARD ASSERT `overrun_count() > 0`. On non-deterministic hosts
//! (a genuinely idle backend may never push enough buffers to overflow) we
//! soft-skip with a diagnostic.

use std::time::{Duration, Instant};

use rsac::{AudioCaptureBuilder, CaptureTarget};

use crate::helpers;

/// Stall the consumer and confirm the ring buffer overflows.
#[test]
fn overrun_count_increments_when_consumer_stalls() {
    require_system_capture!();

    // A test tone keeps the producer busy so the ring fills reliably.
    let wav_path = helpers::generate_test_wav(10.0, 48000, 2);
    let player = helpers::spawn_test_tone_player(&wav_path);

    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            // Under a deterministic source the backend is guaranteed present;
            // a build failure here is a real regression, not host flakiness, so
            // HARD-FAIL instead of silently masking it. Clean up first.
            if let Some(p) = player {
                helpers::stop_player(p);
            }
            cleanup(&wav_path);
            if helpers::deterministic_audio_env() {
                panic!(
                    "deterministic source: SystemDefault build failed — the backend \
                     must be available under RSAC_CI_AUDIO_DETERMINISTIC=1: {:?}",
                    e
                );
            }
            eprintln!(
                "[ci_audio] overrun: build failed (non-deterministic host): {:?}",
                e
            );
            return;
        }
    };

    if let Err(e) = capture.start() {
        // Same rationale as the build arm: a deterministic source must start.
        if let Some(p) = player {
            helpers::stop_player(p);
        }
        cleanup(&wav_path);
        if helpers::deterministic_audio_env() {
            panic!(
                "deterministic source: SystemDefault start failed — capture must \
                 start under RSAC_CI_AUDIO_DETERMINISTIC=1: {:?}",
                e
            );
        }
        eprintln!(
            "[ci_audio] overrun: start failed (non-deterministic host): {:?}",
            e
        );
        return;
    }

    assert!(
        capture.is_running(),
        "capture should be running after start"
    );

    // Deliberately do NOT read any buffers. Sleep long enough for the OS
    // callback to fill the ring (default ~64 buffers) and start dropping.
    // ~5s at any realistic buffer cadence overflows a 64-slot ring many
    // times over. We poll periodically so we can break early once overruns
    // appear, keeping the test fast on healthy hosts.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut overruns: u64 = 0;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        overruns = capture.overrun_count();
        if overruns > 0 {
            break;
        }
    }

    eprintln!("[ci_audio] overrun: observed overrun_count={}", overruns);

    let _ = capture.stop();
    if let Some(p) = player {
        helpers::stop_player(p);
    }
    cleanup(&wav_path);

    if helpers::deterministic_audio_env() {
        // Deterministic source: the producer is definitely pushing buffers,
        // and we never drained them, so the ring MUST have overflowed.
        assert!(
            overruns > 0,
            "deterministic source: overrun_count stayed 0 after stalling the \
             consumer for 5s — the ring buffer never overflowed, which means \
             either the producer isn't pushing or overrun bookkeeping is broken"
        );
        eprintln!("[ci_audio] ✅ overrun_count incremented under stalled consumer");
    } else if overruns == 0 {
        eprintln!(
            "[ci_audio] ⚠ overrun: count stayed 0 (non-deterministic host — \
             backend may be idle / not producing fast enough to overflow)"
        );
    } else {
        eprintln!("[ci_audio] ✅ overrun_count incremented ({overruns})");
    }
}

fn cleanup(wav_path: &std::path::Path) {
    let _ = std::fs::remove_file(wav_path);
    if let Some(parent) = wav_path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}
