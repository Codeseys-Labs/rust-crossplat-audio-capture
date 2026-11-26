//! # Bounded Activity Probe
//!
//! A *recipe* (not a library API) for asking "is anything audible on this
//! target right now?" using only the public `rsac` API. It opens ONE capture
//! session, reads delivered buffers until a wall-clock window elapses, computes
//! peak and RMS over everything that arrived, stops the capture, prints one
//! line and exits.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example probe_activity --features cli -- system
//! cargo run --example probe_activity --features cli -- app:firefox 1000
//! cargo run --example probe_activity --features cli -- pid:4242 250
//! ```
//!
//! Arguments:
//!
//! - `<target>` — `system` (system default audio), `app:<name>` (an application
//!   by name, `CaptureTarget::ApplicationByName`) or `pid:<n>` (a process tree,
//!   `CaptureTarget::ProcessTree`).
//! - `[window_ms]` — how long to read, in milliseconds (default 500).
//!
//! Output is a single line starting with the verdict, and the exit code mirrors
//! it:
//!
//! | Verdict | Exit code | Meaning |
//! |---|---|---|
//! | `active` | 0 | buffers arrived and the peak is above the activity threshold |
//! | `silent` | 1 | buffers arrived and their peak is at or below the threshold — a verdict about the buffers actually observed, nothing more |
//! | `no audio delivered` | 2 | no buffer arrived within the window |
//!
//! Any error the public API returns (bad arguments, build/start/read failure)
//! exits 3 with the error on stderr. One failure can never reach the exit code:
//! `AudioCapture::stop()` logs and swallows a backend stop failure itself.
//!
//! ## Limits
//!
//! Read these before trusting a verdict:
//!
//! - **One session per probe.** Each run builds, starts and stops its own capture
//!   session; there is no persistent monitor and nothing is shared between runs.
//! - **Bounded by wall clock — the read loop only.** The window bounds the read
//!   polling; it does not cover `build()`, `start()` or `stop()` (the reported
//!   window is stamped before the capture is stopped, and each wait inside the
//!   loop is clamped to what remains of the window).
//! - **The first buffer can arrive late.** `no audio delivered` is therefore NOT
//!   proof of silence; try a longer window before concluding anything.
//! - **`silent` is only about what was observed.** An application that is running
//!   but rendering no audio at that moment delivers no buffers and reports
//!   `no audio delivered`, not `silent`; and silence is not proof that the target
//!   is absent.
//! - **App targets need application capture.** On platforms without it, `app:`
//!   and `pid:` targets fail with the library's capability error
//!   (`AudioError::PlatformNotSupported`) and the probe exits 3.
//!
//! The reported format is the one carried by the first *delivered* buffer, which
//! is authoritative; `capture.format()` right after `start()` can still be the
//! requested format.

use rsac::{AudioCaptureBuilder, AudioError, CaptureTarget};
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// Peak amplitude (linear, full scale = 1.0) above which the target counts as
/// active: -60 dBFS, matching the CI suite's non-silence threshold.
const ACTIVE_PEAK_THRESHOLD: f32 = 0.001;

/// Default probe window.
const DEFAULT_WINDOW_MS: u64 = 500;

/// Longest wait between two polls of the read loop.
const POLL_SLICE: Duration = Duration::from_millis(5);

/// What the probe measured over its window.
struct Measurement {
    /// Number of buffers delivered.
    buffers: u64,
    /// Total frames delivered.
    frames: u64,
    /// Largest absolute sample value seen.
    peak: f32,
    /// Sum of squared samples (accumulated in f64 to avoid drift).
    sum_squares: f64,
    /// Total samples seen (frames x channels).
    samples: u64,
    /// Sample rate and channel count of the first delivered buffer.
    format: Option<(u32, u16)>,
}

impl Measurement {
    fn rms(&self) -> f32 {
        if self.samples == 0 {
            0.0
        } else {
            (self.sum_squares / self.samples as f64).sqrt() as f32
        }
    }
}

fn main() -> ExitCode {
    env_logger::init();
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(3)
        }
    }
}

/// Runs the probe and returns the verdict's exit code (0, 1 or 2).
fn run() -> Result<u8, Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let target_arg = args
        .next()
        .ok_or("usage: probe_activity <system|app:<name>|pid:<n>> [window_ms]")?;
    let window_ms: u64 = match args.next() {
        Some(s) => s
            .parse()
            .map_err(|e| format!("invalid window '{s}' (milliseconds expected): {e}"))?,
        None => DEFAULT_WINDOW_MS,
    };
    if window_ms == 0 {
        return Err("window must be at least 1 ms".into());
    }
    if args.next().is_some() {
        return Err("too many arguments; expected <target> [window_ms]".into());
    }
    let target = parse_target(&target_arg)?;
    let window = Duration::from_millis(window_ms);

    // ONE session: build, start, read for the window, stop. The default request
    // (48 kHz, 2 channels) is only a request; the delivered format is read from
    // the buffers.
    let mut capture = AudioCaptureBuilder::new().with_target(target).build()?;
    capture.start()?;

    let started = Instant::now();
    let mut m = Measurement {
        buffers: 0,
        frames: 0,
        peak: 0.0,
        sum_squares: 0.0,
        samples: 0,
        format: None,
    };
    let mut read_error: Option<AudioError> = None;

    // The window bounds this read loop only — not build()/start()/stop().
    while started.elapsed() < window {
        // `read_chunk_nonblocking`, not `read_buffer`: the simple pull API
        // downgrades a dead stream to a *recoverable* error, which would let the
        // probe report a verdict after the capture failed. This read preserves
        // the terminal error, so any read failure exits 3 below.
        match capture.read_chunk_nonblocking() {
            Ok(Some(buffer)) => {
                m.format
                    .get_or_insert((buffer.sample_rate(), buffer.channels()));
                m.buffers += 1;
                m.frames += buffer.num_frames() as u64;
                for &s in buffer.data() {
                    m.peak = m.peak.max(s.abs());
                    m.sum_squares += f64::from(s) * f64::from(s);
                }
                m.samples += buffer.data().len() as u64;
            }
            // Nothing queued yet: a brief wait, clamped to what remains of the
            // window so the loop cannot overshoot it.
            Ok(None) => {
                let remaining = window.saturating_sub(started.elapsed());
                std::thread::sleep(remaining.min(POLL_SLICE));
            }
            // Any read failure — recoverable or fatal — is an error the public
            // API returns: stop and exit 3 with it.
            Err(e) => {
                read_error = Some(e);
                break;
            }
        }
    }

    // Stamp the end of observation BEFORE stopping, so the reported window
    // covers the read loop only, not the capture teardown.
    let observed_ms = started.elapsed().as_millis();

    // Stop before reporting so the session never outlives the probe, even when
    // the read loop ended on an error. A backend stop failure is logged and
    // swallowed inside stop() itself; it cannot reach the exit code.
    let stopped = capture.stop();
    if let Some(e) = read_error {
        return Err(e.into());
    }
    stopped?;

    let (verdict, code) = if m.buffers == 0 {
        ("no audio delivered", 2)
    } else if m.peak > ACTIVE_PEAK_THRESHOLD {
        ("active", 0)
    } else {
        ("silent", 1)
    };
    let format = match m.format {
        Some((rate, channels)) => format!("{rate} Hz {channels} ch"),
        None => "n/a".to_string(),
    };
    println!(
        "{verdict}: target={target_arg} peak={:.6} rms={:.6} buffers={} frames={} \
         window={observed_ms}ms format={format}",
        m.peak,
        m.rms(),
        m.buffers,
        m.frames,
    );
    Ok(code)
}

/// Parses `system`, `app:<name>` or `pid:<n>` into a [`CaptureTarget`].
///
/// This is deliberately NOT `CaptureTarget::from_str`: there `app:<id>` means an
/// application *id*, while this recipe's `app:<name>` means an application by
/// *name*.
fn parse_target(s: &str) -> Result<CaptureTarget, String> {
    if s.eq_ignore_ascii_case("system") {
        return Ok(CaptureTarget::SystemDefault);
    }
    match s.split_once(':') {
        Some((scheme, name)) if scheme.eq_ignore_ascii_case("app") && !name.is_empty() => {
            Ok(CaptureTarget::app(name))
        }
        Some((scheme, pid)) if scheme.eq_ignore_ascii_case("pid") => pid
            .parse::<u32>()
            .map(CaptureTarget::pid)
            .map_err(|e| format!("invalid pid '{pid}': {e}")),
        _ => Err(format!(
            "unknown target '{s}': expected 'system', 'app:<name>' or 'pid:<n>'"
        )),
    }
}
