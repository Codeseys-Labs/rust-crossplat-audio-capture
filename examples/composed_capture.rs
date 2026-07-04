//! # Composed Multi-Source Capture (ADR-0011)
//!
//! Demonstrates the `compose` feature: capture an application and the system
//! mix **simultaneously**, composed into one multi-channel stream —
//!
//! - group "app"    → the named application, mixed down to **1 mono channel**
//! - group "system" → the system default output, native channels kept as-is
//!
//! The composed stream is interleaved f32 at the session rate; the
//! [`ChannelMap`] reports which output channel belongs to which group. Sources
//! delivering a different sample rate are resampled transparently.
//!
//! Run with:
//! `cargo run --example composed_capture --features compose -- <app-name>`
//! (falls back to a system-only composition when no app name is given).

use rsac::compose::{CompositionBuilder, Group, GroupLayout};
use rsac::CaptureTarget;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app_name = std::env::args().nth(1);

    // ── Declare the composition ─────────────────────────────────────
    let mut builder = CompositionBuilder::new().sample_rate(48_000);
    if let Some(name) = &app_name {
        builder = builder.group(
            Group::new("app")
                .source(CaptureTarget::ApplicationByName(name.clone()))
                .mixdown(GroupLayout::Mono), // → 1 composed channel
        );
        println!("Group 'app'   : application '{name}' → mono");
    }
    builder = builder.group(
        Group::new("system")
            .source(CaptureTarget::SystemDefault)
            .keep_channels(), // → the endpoint's native channels
    );
    println!("Group 'system': system default → native channels");

    let mut session = builder.build()?;
    session.start()?;

    // ── Inspect the resolved layout ─────────────────────────────────
    let map = session.channel_map().expect("started").clone();
    println!("\nComposed layout: {} channels @ 48 kHz", map.channels());
    for (i, origin) in map.entries().iter().enumerate() {
        println!(
            "  channel {i}: group '{}' (channel {} within group)",
            origin.group, origin.channel_in_group
        );
    }

    // ── Read composed audio for a few seconds ───────────────────────
    println!("\nCapturing 5 seconds of composed audio...");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buffers = 0u64;
    let mut frames = 0u64;
    while Instant::now() < deadline {
        match session.read_buffer() {
            Ok(Some(buffer)) => {
                buffers += 1;
                frames += buffer.num_frames() as u64;
                if buffers.is_multiple_of(100) {
                    // Per-group RMS via the channel map.
                    let mut levels = String::new();
                    for entry in map.entries() {
                        let ch = map
                            .group_range(&entry.group)
                            .map(|r| r.start)
                            .unwrap_or_default();
                        if entry.channel_in_group == 0 {
                            let rms = buffer.channel_rms(ch as u16).unwrap_or(0.0);
                            levels.push_str(&format!("{}={rms:.4} ", entry.group));
                        }
                    }
                    println!("  {buffers} buffers, {frames} frames | rms: {levels}");
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(1)),
            Err(e) if e.is_fatal() => {
                println!("Composition ended: {e}");
                break;
            }
            Err(e) => eprintln!("transient read error (retrying): {e}"),
        }
    }

    // ── Stats + teardown ────────────────────────────────────────────
    if let Some(stats) = session.stats() {
        println!(
            "\nStats: {} ticks ({} wall-clock fallback)",
            stats.ticks, stats.fallback_ticks
        );
        for s in &stats.sources {
            println!(
                "  [{}] {} — buffers={} padded={} trimmed={} resampling={} ended={}",
                s.group,
                s.target,
                s.buffers_received,
                s.padded_frames,
                s.trimmed_frames,
                s.resampling,
                s.ended
            );
        }
    }
    session.stop()?;
    println!("Done: {buffers} composed buffers, {frames} frames total.");
    Ok(())
}
