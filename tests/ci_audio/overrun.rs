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

//! Backpressure coverage (`backpressure_report()`) lives here too: it is the
//! windowed sibling of `overrun_count()` — same stalled-consumer setup, but it
//! exposes the bridge's per-window drop ring (pushed/dropped/drop_rate over an
//! estimated wall-clock span) rather than a lifetime overrun tally. No unit test
//! feeds those windowed counters from a real OS callback; this module does.

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

/// Windowed backpressure: a stalled consumer must drive `backpressure_report()`
/// to observe drops, a positive `drop_rate`, and (when a `buffer_size` is
/// configured so the span is attributable) a non-zero `window`.
///
/// This is the integration-level novelty over the `api.rs` unit tests: it proves
/// the bridge's windowed drop ring is fed by a *real* OS callback (unit tests
/// only inject counts). `estimate_window_span()` returns `Duration::ZERO` unless
/// a `buffer_size` is set AND a usable sample rate exists, so we set an explicit
/// buffer size to exercise the non-zero-window path.
#[test]
fn backpressure_report_reflects_stalled_consumer() {
    require_system_capture!();

    // A test tone keeps the producer busy so drops accrue reliably.
    let wav_path = helpers::generate_test_wav(10.0, 48000, 2);
    let player = helpers::spawn_test_tone_player(&wav_path);

    // Explicit buffer size so `estimate_window_span` can attribute a non-zero
    // window span (0ns without it — see rsac-cfe4 evidence). Public setter
    // confirmed: `AudioCaptureBuilder::buffer_size(Option<usize>)`.
    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .buffer_size(Some(1024))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            if let Some(p) = player {
                helpers::stop_player(p);
            }
            cleanup(&wav_path);
            if helpers::deterministic_audio_env() {
                panic!(
                    "deterministic source: SystemDefault build failed — the backend must be \
                     available under RSAC_CI_AUDIO_DETERMINISTIC=1: {:?}",
                    e
                );
            }
            eprintln!(
                "[ci_audio] backpressure: build failed (non-deterministic host): {:?}",
                e
            );
            return;
        }
    };

    if let Err(e) = capture.start() {
        if let Some(p) = player {
            helpers::stop_player(p);
        }
        cleanup(&wav_path);
        if helpers::deterministic_audio_env() {
            panic!(
                "deterministic source: SystemDefault start failed — capture must start under \
                 RSAC_CI_AUDIO_DETERMINISTIC=1: {:?}",
                e
            );
        }
        eprintln!(
            "[ci_audio] backpressure: start failed (non-deterministic host): {:?}",
            e
        );
        return;
    }

    assert!(
        capture.is_running(),
        "capture should be running after start"
    );

    // Stall the consumer: NEVER call read_buffer(). Poll the windowed report
    // until drops appear or a 5s deadline (mirrors the overrun poll loop).
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut report = capture.backpressure_report();
    let mut prev_pushed = report.pushed;
    let mut prev_dropped = report.dropped;
    let mut monotonic = true;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
        report = capture.backpressure_report();
        // pushed/dropped should not go backwards across successive reads within
        // a run (strengthens the windowed-vs-lifetime claim).
        if report.pushed < prev_pushed || report.dropped < prev_dropped {
            monotonic = false;
        }
        prev_pushed = report.pushed;
        prev_dropped = report.dropped;
        if report.dropped > 0 {
            break;
        }
    }

    eprintln!(
        "[ci_audio] backpressure: pushed={}, dropped={}, drop_rate={:.4}, window={:?}, \
         is_under_backpressure={} (legacy bool, timing-dependent)",
        report.pushed,
        report.dropped,
        report.drop_rate,
        report.window,
        report.is_under_backpressure
    );

    let _ = capture.stop();
    if let Some(p) = player {
        helpers::stop_player(p);
    }
    cleanup(&wav_path);

    if helpers::deterministic_audio_env() {
        // Deterministic source: the producer is definitely pushing and we never
        // drained, so the windowed drop ring MUST show loss.
        assert!(
            report.dropped > 0,
            "deterministic source: backpressure_report().dropped stayed 0 after stalling the \
             consumer for 5s — the windowed drop ring is not being fed"
        );
        assert!(
            report.drop_rate > 0.0,
            "deterministic source: drop_rate must be > 0 once buffers are dropped, got {}",
            report.drop_rate
        );
        assert!(
            report.pushed + report.dropped > 0,
            "deterministic source: pushed + dropped must be > 0 under an active producer"
        );
        // Non-zero window is only assertable because we configured buffer_size.
        assert_ne!(
            report.window,
            Duration::ZERO,
            "windowed span must be attributed when buffer_size is configured"
        );
        assert!(
            monotonic,
            "deterministic source: pushed/dropped tallies must be non-decreasing across \
             successive reads within a run"
        );
        eprintln!("[ci_audio] ✅ backpressure_report reflected the stalled consumer");
    } else if report.dropped == 0 {
        eprintln!(
            "[ci_audio] ⚠ backpressure: dropped stayed 0 (non-deterministic host — backend \
             may be idle / not producing fast enough to overflow)"
        );
    } else {
        eprintln!(
            "[ci_audio] ✅ backpressure_report observed drops (dropped={}, drop_rate={:.4})",
            report.dropped, report.drop_rate
        );
    }
}

fn cleanup(wav_path: &std::path::Path) {
    let _ = std::fs::remove_file(wav_path);
    if let Some(parent) = wav_path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}
