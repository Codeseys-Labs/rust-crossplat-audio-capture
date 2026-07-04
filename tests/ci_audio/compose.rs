//! Composed multi-source capture integration tests (`compose` feature,
//! ADR-0011).
//!
//! Exercises the `CompositionBuilder → Composition` pipeline against a real
//! platform backend: two groups over the same `SystemDefault` source family —
//! one mono mixdown group and one keep-channels group — composed into a single
//! multi-channel stream.
//!
//! Skip policy (mirrors multi_source.rs):
//! - `require_system_capture!()` skips when no audio infrastructure exists.
//! - Format/layout assertions (channel count, session rate, map shape) are
//!   HARD — they are pure data-plane properties, deterministic everywhere.
//! - Content (non-silence) assertions are hard only under
//!   `RSAC_CI_AUDIO_DETERMINISTIC=1`, soft-logged elsewhere (helpers policy).

#![cfg(feature = "compose")]

use std::time::{Duration, Instant};

use rsac::compose::{CompositionBuilder, Group, GroupLayout};
use rsac::CaptureTarget;

use crate::helpers;

/// Setup-failure policy: identical to multi_source.rs — hard-fail under the
/// deterministic env, soft-skip elsewhere.
fn fail_or_skip(label: &str, detail: &str, cleanup: impl FnOnce()) {
    cleanup();
    if helpers::deterministic_audio_env() {
        panic!(
            "deterministic source: {label} failed ({detail}) — the compose \
             pipeline must work under RSAC_CI_AUDIO_DETERMINISTIC=1"
        );
    }
    eprintln!("[ci_audio] compose: {label} failed (non-deterministic host): {detail}; skipping");
}

/// Happy path: a mono group + a keep-channels group over system audio compose
/// into one stream whose layout is exactly `1 + native` channels, delivering
/// interleaved f32 at the session rate.
#[test]
fn composed_system_capture_layout_and_delivery() {
    require_system_capture!();

    // Play a test tone so the system loopback has audio to compose.
    let wav_path = helpers::generate_test_wav(6.0, 48000, 2);
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

    // ── Build + start the composition ────────────────────────────────
    let mut session = match CompositionBuilder::new()
        .sample_rate(48000)
        .group(
            Group::new("mono_mix")
                .source(CaptureTarget::SystemDefault)
                .mixdown(GroupLayout::Mono),
        )
        .group(
            Group::new("native")
                .source(CaptureTarget::SystemDefault)
                .keep_channels(),
        )
        .build()
    {
        Ok(s) => s,
        Err(e) => {
            fail_or_skip("composition build", &format!("{e:?}"), || {
                stop(player);
            });
            return;
        }
    };
    if let Err(e) = session.start() {
        fail_or_skip("composition start", &format!("{e:?}"), || {
            stop(player);
        });
        return;
    }

    // ── HARD layout assertions (deterministic data-plane contract) ───
    let map = session
        .channel_map()
        .expect("channel_map after start")
        .clone();
    let native_width = map
        .group_range("native")
        .expect("native group present")
        .len();
    assert!(native_width >= 1, "keep-channels width must be >= 1");
    assert_eq!(
        usize::from(map.channels()),
        1 + native_width,
        "composed width = mono(1) + native({native_width})"
    );
    assert_eq!(
        map.group_range("mono_mix"),
        Some(0..1),
        "mono group owns channel 0 (declaration order)"
    );

    // ── Read composed audio for ~2 s ─────────────────────────────────
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut buffers = 0u64;
    let mut nonsilent = false;
    while Instant::now() < deadline {
        match session.read_buffer() {
            Ok(Some(buffer)) => {
                buffers += 1;
                // HARD: every composed buffer matches the resolved layout.
                assert_eq!(buffer.channels(), map.channels(), "buffer channel count");
                assert_eq!(buffer.sample_rate(), 48000, "buffer session rate");
                assert_eq!(
                    buffer.data().len() % usize::from(map.channels()),
                    0,
                    "whole interleaved frames only"
                );
                if buffer.data().iter().any(|s| s.abs() > 1e-4) {
                    nonsilent = true;
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(1)),
            Err(e) if e.is_fatal() => break,
            Err(e) => {
                eprintln!("[ci_audio] compose: transient read error (retrying): {e:?}");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    // Stats sanity (HARD: counters exist and are coherent).
    let stats = session.stats().expect("stats after start");
    assert_eq!(stats.sources.len(), 2, "one stat entry per source");

    let _ = session.stop();
    stop(player);

    // ── Delivery assertions ──────────────────────────────────────────
    if buffers == 0 {
        if helpers::deterministic_audio_env() {
            panic!("deterministic source: composition produced 0 buffers");
        }
        eprintln!("[ci_audio] compose: 0 composed buffers (no working loopback?); soft-skip");
        return;
    }
    // Content is hard only under the deterministic env (helpers policy).
    if helpers::deterministic_audio_env() {
        assert!(
            nonsilent,
            "deterministic source: composed stream must carry the test tone"
        );
    } else if !nonsilent {
        eprintln!("[ci_audio] compose: composed audio was silent (soft warning)");
    }
}

/// Stopping a composition ends the stream terminally: an explicit `stop()`
/// ends readability immediately (any buffered composed tail is discarded —
/// the same contract as `AudioCapture::stop`; drain-before-terminal applies
/// only to the composition's *natural* end), so reads must surface the fatal
/// `StreamEnded` promptly.
#[test]
fn composed_capture_stop_is_terminal() {
    require_system_capture!();

    let mut session = match CompositionBuilder::new()
        .group(
            Group::new("sys")
                .source(CaptureTarget::SystemDefault)
                .mixdown(GroupLayout::Stereo),
        )
        .build()
    {
        Ok(s) => s,
        Err(e) => {
            fail_or_skip("composition build", &format!("{e:?}"), || {});
            return;
        }
    };
    if let Err(e) = session.start() {
        fail_or_skip("composition start", &format!("{e:?}"), || {});
        return;
    }

    std::thread::sleep(Duration::from_millis(300));
    session.stop().expect("stop is Ok");

    // After stop, reads must reach the fatal terminal within a bounded window
    // (the tail, if any, is discarded by explicit stop — see Composition::stop).
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match session.read_buffer() {
            Ok(Some(_)) => continue, // a race-window buffer is tolerated
            Ok(None) => {
                assert!(
                    Instant::now() < deadline,
                    "stopped composition never reached the terminal StreamEnded"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                assert!(
                    e.is_fatal(),
                    "post-stop read error must be the fatal terminal, got {e:?}"
                );
                break;
            }
        }
    }
    assert!(!session.is_running(), "stopped composition is not running");
}

/// Helper: stop a player if present. Consumes it.
fn stop(player: Option<std::process::Child>) {
    if let Some(p) = player {
        helpers::stop_player(p);
    }
}
