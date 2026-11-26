//! Platform-independent part of the `hotplug-recovery` evidence tests
//! (issue #94): tone analysis, recovery-event collection, and the capture
//! session driver every platform's tests share.
//!
//! The evidence tests identify **which endpoint's audio** a capture delivers by
//! tone: one player renders 440 Hz (`A`) and another 997 Hz (`B`), each on its
//! own endpoint. Delivered buffers are mixed to mono and cut into consecutive,
//! non-overlapping [`WINDOW_MS`] windows. Each window gets its RMS and the
//! Goertzel power at both tone frequencies, computed at the buffer's own sample
//! rate, and a [`Verdict`]: A-dominant, B-dominant or neither. A window never
//! spans two stream generations (or two delivery formats): the partial window
//! in progress at such a boundary is discarded. A phase of the test "sees" a
//! tone once [`CONSECUTIVE`] windows in a row are dominant for it ([`Streak`]).
//!
//! # Writing a platform's evidence tests
//!
//! The platform module supplies everything that needs the real audio stack:
//! tone players bound to one endpoint each, a way to set the default endpoint
//! and read it back, the endpoint-loss mechanisms, restore guards for all of
//! them, and a [`Machine`] that describes the current defaults and the players
//! for failure dumps. The rest comes from here. A test:
//!
//! 1. takes [`machine_lock`] for its whole body, then returns early unless
//!    [`enabled`] (which prints the `HOTPLUG-SKIP` line);
//! 2. records the machine state in its restore guards, sets the defaults and
//!    starts the players;
//! 3. opens a [`Session`] with [`Session::open`], sets its policy flags
//!    ([`Session::forbid_exhausted`], [`Session::expect_terminal`],
//!    [`Session::forbid_a_after_b`]) and calls [`Session::start`];
//! 4. drives its phases with [`Session::expect_sees`],
//!    [`Session::expect_events_in_order`], [`Session::expect_stays_on_a`],
//!    [`Session::wait_for`] and [`Session::pump_for`], recording milestones
//!    with [`Session::mark`]; every failure goes through [`Session::fail`],
//!    which panics with the full dump ([`Session::report`]);
//! 5. formats its evidence with [`evidence_line`], calls [`Session::stop`],
//!    drops the session and the players, restores the machine (failing if
//!    that fails), and only then prints the evidence line on stdout.
//!
//! Recovery advances only while the capture is read, so every wait polls the
//! capture; a platform operation that itself waits (a default switch's
//! read-back) should call [`Session::pump_for`] between its attempts.
//!
//! Times are session time: the [`Duration`] since the session was opened
//! ([`Session::elapsed`]). Windows, events and milestones are all stamped with
//! it, and every deadline is expressed in it.

#![cfg(feature = "hotplug-recovery")]
// Each platform's tests use a different subset of the shared driver.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use rsac::{
    AudioBuffer, AudioCapture, AudioCaptureBuilder, AudioError, AutoRecoveryConfig, CaptureTarget,
    RecoveryEvent, RecoveryState,
};

/// Length of one analysis window.
pub const WINDOW_MS: u32 = 200;
/// A window quieter than this RMS is never dominant (silence, a muted path).
pub const MIN_RMS: f64 = 0.005;
/// How far one tone's power must exceed the other's for a dominant window.
pub const MIN_DOMINANCE_DB: f64 = 20.0;
/// Consecutive dominant windows that make a phase "see" a tone (1 s of audio).
pub const CONSECUTIVE: usize = 5;
/// Tone that identifies endpoint A.
pub const TONE_A_HZ: f64 = 440.0;
/// Tone that identifies endpoint B.
pub const TONE_B_HZ: f64 = 997.0;
/// Peak amplitude of every test tone, on every channel.
pub const TONE_AMPLITUDE: f32 = 0.05;

/// Floor for the competing tone's power in [`dominance_db`], so an empty bin
/// gives a large finite ratio instead of infinity.
const POWER_FLOOR: f64 = 1e-12;

// ── Machine-wide lock ────────────────────────────────────────────────────

static MACHINE_LOCK: Mutex<()> = Mutex::new(());

/// Takes the process-wide lock every evidence test holds for its whole body:
/// each one changes machine-wide audio state (default endpoints, endpoint
/// visibility), so two must never overlap even without `--test-threads=1`.
///
/// A test that fails panics while holding the lock and poisons it; the next
/// test must still run (and restore the machine on its own exit), so poisoning
/// is ignored.
pub fn machine_lock() -> MutexGuard<'static, ()> {
    MACHINE_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

// ── Per-window analysis ──────────────────────────────────────────────────

/// Which test tone, if either, a window carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 440 Hz dominates: endpoint A's audio.
    A,
    /// 997 Hz dominates: endpoint B's audio.
    B,
    /// Too quiet, or no tone dominates the other.
    Neither,
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Verdict::A => "A",
            Verdict::B => "B",
            Verdict::Neither => "neither",
        })
    }
}

/// Analysis of one [`WINDOW_MS`] window of delivered audio.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowReport {
    /// Stream generation of every frame in the window.
    pub generation: u64,
    /// Sample rate the window was analysed at.
    pub sample_rate: u32,
    /// Index of the window's first frame among the frames its generation delivered.
    pub start_frame: u64,
    /// RMS of the mono mix.
    pub rms: f64,
    /// Normalised Goertzel power at [`TONE_A_HZ`].
    pub power_a: f64,
    /// Normalised Goertzel power at [`TONE_B_HZ`].
    pub power_b: f64,
    /// Classification from the three values above.
    pub verdict: Verdict,
}

impl WindowReport {
    /// How far 440 Hz stands above 997 Hz.
    pub fn dominance_a_db(&self) -> f64 {
        dominance_db(self.power_a, self.power_b)
    }

    /// How far 997 Hz stands above 440 Hz.
    pub fn dominance_b_db(&self) -> f64 {
        dominance_db(self.power_b, self.power_a)
    }

    /// Dominance of the winning tone, or `None` for a window that is neither.
    pub fn winning_dominance_db(&self) -> Option<f64> {
        match self.verdict {
            Verdict::A => Some(self.dominance_a_db()),
            Verdict::B => Some(self.dominance_b_db()),
            Verdict::Neither => None,
        }
    }
}

/// `10 * log10(p_wanted / max(p_other, 1e-12))`.
pub fn dominance_db(p_wanted: f64, p_other: f64) -> f64 {
    10.0 * (p_wanted / p_other.max(POWER_FLOOR)).log10()
}

/// Goertzel power at `freq` over `samples`, normalised so that a sine of
/// amplitude `x` at exactly `freq` measures about `x²`.
///
/// The recurrence is evaluated at the exact frequency (not the nearest DFT
/// bin), so the measured tone's own power does not depend on whether the
/// window holds a whole number of its periods.
pub fn goertzel_power(samples: &[f64], sample_rate: u32, freq: f64) -> f64 {
    if samples.is_empty() || sample_rate == 0 {
        return 0.0;
    }
    let coeff = 2.0 * (std::f64::consts::TAU * freq / f64::from(sample_rate)).cos();
    let (mut s1, mut s2) = (0.0_f64, 0.0_f64);
    for &x in samples {
        let s0 = x + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let magnitude_sq = (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0);
    let n = samples.len() as f64;
    4.0 * magnitude_sq / (n * n)
}

/// Classifies one window from its RMS and the two tone powers.
pub fn classify(rms: f64, power_a: f64, power_b: f64) -> Verdict {
    if rms < MIN_RMS {
        Verdict::Neither
    } else if dominance_db(power_a, power_b) >= MIN_DOMINANCE_DB {
        Verdict::A
    } else if dominance_db(power_b, power_a) >= MIN_DOMINANCE_DB {
        Verdict::B
    } else {
        Verdict::Neither
    }
}

/// Frames in one window at `sample_rate` (`rate / 5` for 200 ms).
pub fn window_frames(sample_rate: u32) -> usize {
    (u64::from(sample_rate) * u64::from(WINDOW_MS) / 1000) as usize
}

fn analyze_window(
    mono: &[f64],
    sample_rate: u32,
    generation: u64,
    start_frame: u64,
) -> WindowReport {
    let rms = (mono.iter().map(|x| x * x).sum::<f64>() / mono.len() as f64).sqrt();
    let power_a = goertzel_power(mono, sample_rate, TONE_A_HZ);
    let power_b = goertzel_power(mono, sample_rate, TONE_B_HZ);
    WindowReport {
        generation,
        sample_rate,
        start_frame,
        rms,
        power_a,
        power_b,
        verdict: classify(rms, power_a, power_b),
    }
}

/// Cuts delivered buffers, in delivery order, into analysed windows.
#[derive(Debug, Default)]
pub struct ToneAnalyzer {
    /// `(generation, sample rate, channels)` of the window being filled.
    segment: Option<(u64, u32, u16)>,
    /// Mono samples of the window being filled.
    mono: Vec<f64>,
    /// Frames the current generation has delivered so far.
    generation_frames: u64,
    /// First frame of the window being filled, within its generation.
    window_start: u64,
    /// Frames of partial windows dropped at generation, format or phase boundaries.
    discarded_frames: u64,
    /// Buffers that could not be analysed (no channels, or no window length).
    skipped_buffers: u64,
}

impl ToneAnalyzer {
    /// An analyzer with no audio yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one delivered buffer and appends every window it completes to `out`.
    ///
    /// A buffer whose generation, rate or channel count differs from the
    /// window in progress discards that partial window; the next window starts
    /// at the buffer's first frame.
    pub fn push(&mut self, buffer: &AudioBuffer, out: &mut Vec<WindowReport>) {
        let generation = buffer.stream_generation();
        let sample_rate = buffer.sample_rate();
        let channels = buffer.channels();
        let window = window_frames(sample_rate);
        if channels == 0 || window == 0 {
            self.skipped_buffers += 1;
            return;
        }
        let segment = (generation, sample_rate, channels);
        if self.segment != Some(segment) {
            self.discard_partial();
            if self.segment.map(|(g, _, _)| g) != Some(generation) {
                self.generation_frames = 0;
            }
            self.segment = Some(segment);
            self.window_start = self.generation_frames;
        }
        let per_frame = f64::from(channels);
        for frame in buffer.data().chunks_exact(usize::from(channels)) {
            self.mono
                .push(frame.iter().map(|&s| f64::from(s)).sum::<f64>() / per_frame);
            self.generation_frames += 1;
            if self.mono.len() == window {
                out.push(analyze_window(
                    &self.mono,
                    sample_rate,
                    generation,
                    self.window_start,
                ));
                self.mono.clear();
                self.window_start = self.generation_frames;
            }
        }
    }

    // Preserve generation offsets when starting a fresh phase.
    fn discard_partial(&mut self) {
        self.discarded_frames += self.mono.len() as u64;
        self.mono.clear();
        self.window_start = self.generation_frames;
    }

    /// Frames dropped as partial windows at generation, format or phase boundaries.
    pub fn discarded_frames(&self) -> u64 {
        self.discarded_frames
    }

    /// Buffers that carried no analysable audio (zero channels or rate).
    pub fn skipped_buffers(&self) -> u64 {
        self.skipped_buffers
    }
}

/// Tracks whether a phase "sees" a tone: [`CONSECUTIVE`] windows in a row that
/// are dominant for it and, when `generation` is set, belong to that generation.
/// Any other window (neither, the other tone, another generation) restarts the
/// count. Once seen, it stays seen.
#[derive(Debug, Clone)]
pub struct Streak {
    wanted: Verdict,
    generation: Option<u64>,
    run: usize,
    seen: bool,
}

impl Streak {
    /// A streak for `wanted` (`A` or `B`), optionally restricted to one generation.
    pub fn new(wanted: Verdict, generation: Option<u64>) -> Self {
        Self {
            wanted,
            generation,
            run: 0,
            seen: false,
        }
    }

    /// Feeds the next window; returns whether the tone has been seen.
    pub fn observe(&mut self, window: &WindowReport) -> bool {
        if self.seen {
            return true;
        }
        let in_generation = self.generation.is_none_or(|g| g == window.generation);
        if window.verdict == self.wanted && in_generation {
            self.run += 1;
            self.seen = self.run >= CONSECUTIVE;
        } else {
            self.run = 0;
        }
        self.seen
    }

    /// Whether the tone has been seen.
    pub fn is_seen(&self) -> bool {
        self.seen
    }

    /// Length of the current run of matching windows.
    pub fn run(&self) -> usize {
        self.run
    }
}

/// Per-generation verdict counts, e.g. `gen0: A=12 B=0 neither=1`, one
/// generation per `; `-separated entry, in generation order.
pub fn verdict_summary<'a>(windows: impl IntoIterator<Item = &'a WindowReport>) -> String {
    let mut counts: BTreeMap<u64, [usize; 3]> = BTreeMap::new();
    for w in windows {
        let slot = match w.verdict {
            Verdict::A => 0,
            Verdict::B => 1,
            Verdict::Neither => 2,
        };
        counts.entry(w.generation).or_default()[slot] += 1;
    }
    if counts.is_empty() {
        return "no windows".to_string();
    }
    counts
        .iter()
        .map(|(g, [a, b, n])| format!("gen{g}: A={a} B={b} neither={n}"))
        .collect::<Vec<_>>()
        .join("; ")
}

// ── Event collection ─────────────────────────────────────────────────────

/// A value stamped with when the test observed it, relative to the test's origin.
#[derive(Debug, Clone, PartialEq)]
pub struct Timed<T> {
    /// Time since the origin when the value was observed.
    pub at: Duration,
    /// The observed value.
    pub value: T,
}

/// Formats a time since the origin as `+12.345s`.
pub fn fmt_at(at: Duration) -> String {
    format!("+{:.3}s", at.as_secs_f64())
}

/// Drains a receiver (the recovery-event subscription) into a timestamped list.
#[derive(Debug)]
pub struct EventLog<T> {
    rx: Receiver<T>,
    origin: Instant,
    events: Vec<Timed<T>>,
    disconnected: bool,
}

impl<T: fmt::Debug> EventLog<T> {
    /// A log stamping events relative to `origin`.
    pub fn new(rx: Receiver<T>, origin: Instant) -> Self {
        Self {
            rx,
            origin,
            events: Vec::new(),
            disconnected: false,
        }
    }

    /// Moves every event queued now into the log; returns how many arrived.
    pub fn drain(&mut self) -> usize {
        let before = self.events.len();
        loop {
            match self.rx.try_recv() {
                Ok(value) => self.events.push(Timed {
                    at: self.origin.elapsed(),
                    value,
                }),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.disconnected = true;
                    break;
                }
            }
        }
        self.events.len() - before
    }

    /// Every event collected so far, in arrival order.
    pub fn events(&self) -> &[Timed<T>] {
        &self.events
    }

    /// Whether the sending side has gone away.
    pub fn is_disconnected(&self) -> bool {
        self.disconnected
    }

    /// One line per event (`  [+1.234s] Event`), for failure messages.
    pub fn render(&self) -> String {
        let mut out = String::new();
        if self.events.is_empty() {
            out.push_str("  (no events)\n");
        }
        for e in &self.events {
            let _ = writeln!(out, "  [{}] {:?}", fmt_at(e.at), e.value);
        }
        if self.disconnected {
            out.push_str("  (subscription disconnected)\n");
        }
        out
    }
}

/// One predicate of an ordered search (see [`find_in_order`]).
pub type Step<'a, T> = &'a dyn Fn(&T) -> bool;

/// Finds `steps` as an ordered subsequence of `items` (other items may sit
/// between): the index of the first item matching `steps[0]`, then the first
/// later item matching `steps[1]`, and so on. `None` when a step has no match.
pub fn find_in_order<T>(items: &[T], steps: &[Step<'_, T>]) -> Option<Vec<usize>> {
    let mut found = Vec::with_capacity(steps.len());
    let mut next = 0;
    for step in steps {
        let offset = items[next..].iter().position(step)?;
        found.push(next + offset);
        next += offset + 1;
    }
    Some(found)
}

// ── Evidence-test parameters ─────────────────────────────────────────────
//
// Values the evidence contract fixes for every platform.

/// Sample rate every evidence capture requests ([`Session::open`]).
pub const CAPTURE_RATE: u32 = 48_000;
/// Channel count every evidence capture requests ([`Session::open`]).
pub const CAPTURE_CHANNELS: u16 = 2;
/// How long a phase may take to see its tone, from `start()` or from the
/// `Recovered` event that should move it.
pub const SEE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a default switch may take to produce its `Recovered` event.
pub const RECOVER_TIMEOUT: Duration = Duration::from_secs(15);
/// Delivered audio [`Session::expect_stays_on_a`] inspects after a switch that
/// must not move the capture (the negative control, the communications role).
pub const STEADY_AUDIO_SECS: f64 = 3.0;
/// Complete, consecutive A-dominant windows required by the steady control.
pub const STEADY_MIN_A_WINDOWS: usize = STEADY_AUDIO_SECS as usize * 1000 / WINDOW_MS as usize;
/// Wall-clock bound for [`Session::expect_stays_on_a`] to receive that audio.
pub const STEADY_WALL_TIMEOUT: Duration = Duration::from_secs(15);
/// Read-back of a default switch: poll interval.
pub const READBACK_INTERVAL: Duration = Duration::from_millis(50);
/// Read-back of a default switch: how long the new default may take to show.
pub const READBACK_TIMEOUT: Duration = Duration::from_secs(2);
/// A tone player must deliver its first buffer within this.
pub const PLAYER_FIRST_BUFFER_TIMEOUT: Duration = Duration::from_secs(5);
/// A tone player whose endpoint went away retries opening it this often.
pub const PLAYER_REOPEN_INTERVAL: Duration = Duration::from_millis(200);
/// A re-enabled endpoint must be active again within this.
pub const ENDPOINT_ACTIVE_TIMEOUT: Duration = Duration::from_secs(10);
/// `device_wait_timeout` of the exhaustion test.
pub const EXHAUST_DEVICE_WAIT: Duration = Duration::from_secs(3);
/// Margin on top of [`EXHAUST_DEVICE_WAIT`] for `Exhausted` to arrive.
pub const EXHAUST_MARGIN: Duration = Duration::from_secs(10);
/// `device_wait_timeout` of the loss-then-return test.
pub const RETURN_DEVICE_WAIT: Duration = Duration::from_secs(20);
/// How long the loss-then-return test keeps the endpoint away.
pub const RETURN_OUTAGE: Duration = Duration::from_secs(2);
/// How long after re-enabling the return test waits for recovery and the tone.
pub const RETURN_RECOVER_TIMEOUT: Duration = Duration::from_secs(20);
/// Pause between capture polls.
pub const POLL_INTERVAL: Duration = Duration::from_millis(5);
/// Most buffers one poll reads before the event queue is drained again.
pub const MAX_READS_PER_POLL: usize = 256;

// ── Skip and evidence lines ──────────────────────────────────────────────

/// Whether the test's enabling variable `var` is `1`. Otherwise prints
/// `HOTPLUG-SKIP test=<test> reason=<var> not set` on stdout and returns
/// `false`; the test then returns without touching the machine.
pub fn enabled(test: &str, var: &str) -> bool {
    if std::env::var(var).as_deref() == Ok("1") {
        return true;
    }
    println!("HOTPLUG-SKIP test={test} reason={var} not set");
    false
}

/// The evidence line of a passing test:
/// `HOTPLUG-EVIDENCE test=<test> result=pass <fields>`, with `fields` the
/// test's space-separated `key=value` pairs. Print it on stdout only after
/// every check passed and the machine was restored; the caller counts these
/// lines, so nothing may print one on a failure path.
pub fn evidence_line(test: &str, fields: &str) -> String {
    format!("HOTPLUG-EVIDENCE test={test} result=pass {fields}")
}

/// Name of an [`AudioError`] variant, e.g. `StreamEnded`, for evidence fields.
pub fn variant_name(error: &AudioError) -> String {
    format!("{error:?}")
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

// ── Recovery-event predicates ────────────────────────────────────────────

/// Whether the event is `DefaultChanged`.
pub fn is_default_changed(e: &Timed<RecoveryEvent>) -> bool {
    matches!(e.value, RecoveryEvent::DefaultChanged { .. })
}

/// Whether the event is `Exhausted`.
pub fn is_exhausted(e: &Timed<RecoveryEvent>) -> bool {
    matches!(e.value, RecoveryEvent::Exhausted { .. })
}

/// The published generation when the event is `Recovered`.
pub fn recovered_generation(e: &Timed<RecoveryEvent>) -> Option<u64> {
    match e.value {
        RecoveryEvent::Recovered { generation, .. } => Some(generation),
        _ => None,
    }
}

// ── Capture session driver ───────────────────────────────────────────────

/// Platform facts a failure dump prints, supplied by the platform module.
/// Both methods run while a failure message is being built, so they must not
/// panic: report a value that cannot be read as text instead.
pub trait Machine {
    /// The current default endpoint(s) on one line, for example
    /// `current defaults: eConsole=<id> eMultimedia=<id>`.
    fn defaults(&self) -> String;

    /// One line per tone player: what it plays where, its counters, and its
    /// last error.
    fn players(&self) -> Vec<String>;
}

/// What one stream generation delivered.
#[derive(Debug, Default)]
pub struct GenerationStats {
    /// Buffers delivered.
    pub buffers: u64,
    /// Frames delivered.
    pub frames: u64,
    /// When the generation's first buffer was read.
    pub first_at: Duration,
    /// Sample rate of the latest buffer.
    pub sample_rate: u32,
    /// Channel count of the latest buffer.
    pub channels: u16,
}

/// Where a session stood when a step began ([`Session::checkpoint`]): the
/// time, how many windows and recovery events it had, and how much audio it
/// had delivered.
#[derive(Debug, Clone, Copy)]
pub struct Checkpoint {
    /// Session time.
    pub at: Duration,
    /// Length of [`Session::windows`].
    pub windows: usize,
    /// Length of [`Session::events`].
    pub events: usize,
    /// [`Session::audio_secs`].
    pub audio_secs: f64,
}

/// A capture plus everything observed from it: analysed windows, recovery
/// events, per-generation buffer counts, read errors and milestones.
///
/// Every poll ([`Session::pump`]) reads all buffers available now, feeds them
/// to a [`ToneAnalyzer`], drains the recovery events, and then checks the
/// standing rules the policy flags enable; a broken rule fails the test at
/// once. A non-recoverable read error ends reading: it is stored in
/// [`Session::terminal`] and fails the test unless [`Session::expect_terminal`]
/// is set. Every failure goes through [`Session::fail`], which panics with
/// [`Session::report`].
pub struct Session {
    origin: Instant,
    /// The capture under test. Read it (for example `recovery_snapshot()`);
    /// start and stop it through [`Session::start`] and [`Session::stop`].
    pub capture: AudioCapture,
    events: Option<EventLog<RecoveryEvent>>,
    analyzer: ToneAnalyzer,
    scratch: Vec<WindowReport>,
    /// Every analysed window in delivery order, stamped with when the buffer
    /// that completed it was read.
    pub windows: Vec<Timed<WindowReport>>,
    /// What each stream generation delivered, keyed by generation.
    pub generations: BTreeMap<u64, GenerationStats>,
    /// Seconds of audio delivered so far: the sum of the buffers' durations.
    pub audio_secs: f64,
    recoverable_errors: u64,
    last_recoverable_error: Option<String>,
    /// The first non-recoverable read error, after which nothing more is read.
    pub terminal: Option<Timed<AudioError>>,
    /// A terminal read is expected (the exhaustion test); otherwise it fails
    /// the test on the poll that sees it.
    pub expect_terminal: bool,
    /// Any `Exhausted` event fails the test.
    pub forbid_exhausted: bool,
    /// While set, an A-dominant window after the first B-dominant one (seen
    /// while set) fails the test.
    pub forbid_a_after_b: bool,
    first_b_at: Option<Duration>,
    machine: Box<dyn Machine>,
    context: String,
    marks: Vec<Timed<String>>,
}

impl Session {
    /// Builds a `SystemDefault` capture at [`CAPTURE_RATE`] and
    /// [`CAPTURE_CHANNELS`], with auto-recovery when `recovery` is set, and
    /// subscribes to its recovery events before anything starts, so no event
    /// is missed. The capture is not started: set the policy flags, then call
    /// [`Session::start`]. A build or subscribe failure panics with `test`,
    /// the error and [`Machine::defaults`]. `context` is the first line of
    /// every failure dump (for example the test name and endpoint ids).
    pub fn open(
        test: &str,
        recovery: Option<AutoRecoveryConfig>,
        machine: Box<dyn Machine>,
        context: String,
    ) -> Self {
        let mut builder = AudioCaptureBuilder::new()
            .with_target(CaptureTarget::SystemDefault)
            .sample_rate(CAPTURE_RATE)
            .channels(CAPTURE_CHANNELS);
        let supervised = recovery.is_some();
        if let Some(config) = recovery {
            builder = builder.with_auto_recovery(config);
        }
        let capture = builder.build().unwrap_or_else(|e| {
            panic!(
                "{test}: building the SystemDefault capture failed: {e:?}\n{}",
                machine.defaults()
            )
        });
        let events = supervised.then(|| {
            capture.subscribe_recovery_events().unwrap_or_else(|e| {
                panic!(
                    "{test}: subscribe_recovery_events() failed: {e:?}\n{}",
                    machine.defaults()
                )
            })
        });
        Self::new(capture, events, machine, context)
    }

    /// Wraps an already built capture; `events` is its recovery-event
    /// subscription, if any. Session time starts now.
    pub fn new(
        capture: AudioCapture,
        events: Option<Receiver<RecoveryEvent>>,
        machine: Box<dyn Machine>,
        context: String,
    ) -> Self {
        let origin = Instant::now();
        Self {
            origin,
            capture,
            events: events.map(|rx| EventLog::new(rx, origin)),
            analyzer: ToneAnalyzer::new(),
            scratch: Vec::new(),
            windows: Vec::new(),
            generations: BTreeMap::new(),
            audio_secs: 0.0,
            recoverable_errors: 0,
            last_recoverable_error: None,
            terminal: None,
            expect_terminal: false,
            forbid_exhausted: false,
            forbid_a_after_b: false,
            first_b_at: None,
            machine,
            context,
            marks: Vec::new(),
        }
    }

    /// Session time now.
    pub fn elapsed(&self) -> Duration {
        self.origin.elapsed()
    }

    /// Records a milestone for the failure dump.
    pub fn mark(&mut self, text: impl Into<String>) {
        let at = self.elapsed();
        self.marks.push(Timed {
            at,
            value: text.into(),
        });
    }

    /// Recovery events received so far, in arrival order; empty without
    /// auto-recovery.
    pub fn events(&self) -> &[Timed<RecoveryEvent>] {
        match &self.events {
            Some(log) => log.events(),
            None => &[],
        }
    }

    /// Starts the capture; a failure fails the test.
    pub fn start(&mut self) {
        if let Err(e) = self.capture.start() {
            self.fail(&format!("AudioCapture::start() failed: {e:?}"));
        }
        self.mark("capture started");
    }

    /// Stops the capture; a failure fails the test.
    pub fn stop(&mut self) {
        if let Err(e) = self.capture.stop() {
            self.fail(&format!("AudioCapture::stop() failed: {e:?}"));
        }
    }

    /// Where the session stands now, for a later step to count from.
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            at: self.elapsed(),
            windows: self.windows.len(),
            events: self.events().len(),
            audio_secs: self.audio_secs,
        }
    }

    /// Drains currently readable audio and events, then discards the partial
    /// window. Count from the returned window index for fresh, complete
    /// windows after a verified switch or recovery.
    pub fn fresh_checkpoint(&mut self) -> Checkpoint {
        if !self.poll() {
            self.fail(
                "could not drain queued capture buffers to establish a fresh window boundary",
            );
        }
        self.analyzer.discard_partial();
        self.checkpoint()
    }

    /// Returns the elapsed time between milestones, failing if they are out of order.
    pub fn elapsed_between(&self, what: &str, from: Duration, to: Duration) -> Duration {
        to.checked_sub(from).unwrap_or_else(|| {
            self.fail(&format!("{what}: {} predates {}", fmt_at(to), fmt_at(from)))
        })
    }

    /// One poll: reads every buffer available now (at most
    /// [`MAX_READS_PER_POLL`]), drains the recovery events and checks the
    /// standing rules.
    pub fn pump(&mut self) {
        self.poll();
    }

    // True only when the ring was observed empty, not when the read cap or
    // an error stopped the poll with unknown backlog.
    fn poll(&mut self) -> bool {
        let mut empty = false;
        let first_new = self.windows.len();
        for _ in 0..MAX_READS_PER_POLL {
            if self.terminal.is_some() {
                break;
            }
            match self.capture.read_chunk_nonblocking() {
                Ok(Some(buffer)) => self.accept(&buffer),
                Ok(None) => {
                    empty = true;
                    break;
                }
                Err(e) if !e.is_recoverable() => {
                    let at = self.elapsed();
                    self.terminal = Some(Timed { at, value: e });
                }
                Err(e) => {
                    self.recoverable_errors += 1;
                    let at = fmt_at(self.elapsed());
                    self.last_recoverable_error = Some(format!("[{at}] {e:?}"));
                    break;
                }
            }
        }
        if let Some(log) = self.events.as_mut() {
            log.drain();
        }
        self.check_rules(first_new);
        empty
    }

    fn accept(&mut self, buffer: &AudioBuffer) {
        let at = self.elapsed();
        let stats = self
            .generations
            .entry(buffer.stream_generation())
            .or_insert_with(|| GenerationStats {
                first_at: at,
                ..GenerationStats::default()
            });
        stats.buffers += 1;
        stats.frames += buffer.num_frames() as u64;
        stats.sample_rate = buffer.sample_rate();
        stats.channels = buffer.channels();
        if buffer.sample_rate() > 0 {
            self.audio_secs += buffer.num_frames() as f64 / f64::from(buffer.sample_rate());
        }
        self.analyzer.push(buffer, &mut self.scratch);
        for window in self.scratch.drain(..) {
            self.windows.push(Timed { at, value: window });
        }
    }

    fn check_rules(&mut self, first_new: usize) {
        if let Some(t) = &self.terminal {
            if !self.expect_terminal {
                self.fail(&format!(
                    "the capture ended at {} with {:?}",
                    fmt_at(t.at),
                    t.value
                ));
            }
        }
        if self.forbid_exhausted {
            if let Some(e) = self.events().iter().find(|e| is_exhausted(e)) {
                self.fail(&format!(
                    "recovery gave up (Exhausted at {}); this test allows no Exhausted event",
                    fmt_at(e.at)
                ));
            }
        }
        if self.forbid_a_after_b {
            for w in &self.windows[first_new..] {
                match (w.value.verdict, self.first_b_at) {
                    (Verdict::B, None) => self.first_b_at = Some(w.at),
                    (Verdict::A, Some(first_b)) => self.fail(&format!(
                        "an A-dominant window (generation {}, {}, {:.1} dB) arrived after the \
                         first B-dominant window ({}) and before the default was switched back",
                        w.value.generation,
                        fmt_at(w.at),
                        w.value.dominance_a_db(),
                        fmt_at(first_b)
                    )),
                    _ => {}
                }
            }
        }
    }

    /// Keeps polling for `pause`; the standing rules still apply.
    pub fn pump_for(&mut self, pause: Duration) {
        let until = Instant::now() + pause;
        loop {
            self.pump();
            if Instant::now() >= until {
                return;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// Polls until `step` returns `Ok(true)`, observed by `deadline` (session
    /// time). Use [`Session::wait_for_observation`] for timestamped evidence.
    /// The step receives the session and the index of the first new window.
    /// `Err(violation)` fails immediately with the diagnostic dump.
    pub fn wait_for(
        &mut self,
        what: &str,
        deadline: Duration,
        mut step: impl FnMut(&Session, usize) -> Result<bool, String>,
    ) {
        self.wait_for_observation(what, deadline, |s, first| {
            Ok(step(s, first)?.then(|| s.elapsed()))
        });
    }

    /// Like [`Session::wait_for`], but the step returns the qualifying
    /// observation's own timestamp, or `None` while waiting. Observations
    /// stamped after `deadline` fail even if the predicate matches.
    pub fn wait_for_observation(
        &mut self,
        what: &str,
        deadline: Duration,
        mut step: impl FnMut(&Session, usize) -> Result<Option<Duration>, String>,
    ) {
        loop {
            let first_new = self.windows.len();
            self.pump();
            match step(self, first_new) {
                Ok(Some(at)) if at <= deadline => return,
                Ok(Some(at)) => self.fail(&format!(
                    "{what}: observed at {}, after deadline {}",
                    fmt_at(at),
                    fmt_at(deadline)
                )),
                Ok(None) => {}
                Err(violation) => self.fail(&format!("{what}: {violation}")),
            }
            if self.elapsed() >= deadline {
                self.fail(&format!("{what}: not reached by {}", fmt_at(deadline)));
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// Waits until the capture sees `wanted` ([`CONSECUTIVE`] dominant windows
    /// in a row) in windows of `generation`, counting windows from index
    /// `from` of [`Session::windows`] on, by `deadline`. Records a milestone
    /// and returns when the tone was seen (the last window's time).
    pub fn expect_sees(
        &mut self,
        what: &str,
        wanted: Verdict,
        generation: u64,
        from: usize,
        deadline: Duration,
    ) -> Duration {
        let mut streak = Streak::new(wanted, Some(generation));
        let mut next = from;
        let mut seen_at = None;
        self.wait_for_observation(what, deadline, |s, _| {
            while next < s.windows.len() {
                let w = &s.windows[next];
                next += 1;
                if streak.observe(&w.value) {
                    seen_at = Some(w.at);
                    return Ok(seen_at);
                }
            }
            Ok(None)
        });
        let seen_at = seen_at.expect("wait_for returns only once the tone was seen");
        self.mark(format!("{what}: seen at {}", fmt_at(seen_at)));
        seen_at
    }

    /// Waits until the recovery events from index `from` on contain `steps`
    /// in order, other events allowed between (see [`find_in_order`]), by
    /// `deadline`. Returns the matched events' indices into
    /// [`Session::events`].
    pub fn expect_events_in_order(
        &mut self,
        what: &str,
        from: usize,
        deadline: Duration,
        steps: &[Step<'_, Timed<RecoveryEvent>>],
    ) -> Vec<usize> {
        let mut found = None;
        self.wait_for_observation(what, deadline, |s, _| {
            found = find_in_order(&s.events()[from..], steps);
            Ok(found.as_ref().map(|indices| {
                indices
                    .last()
                    .map_or_else(|| s.elapsed(), |&i| s.events()[from + i].at)
            }))
        });
        found
            .expect("wait_for returns only once the events were found")
            .into_iter()
            .map(|i| i + from)
            .collect()
    }

    /// Requires the first fifteen complete 200 ms windows after a verified
    /// switch to all be A-dominant (3 s of audio, no B or neither), within
    /// [`STEADY_WALL_TIMEOUT`] of `since`. Every delivered buffer must be
    /// generation 0. A fresh boundary is taken on entry, discarding the
    /// queued audio and partial window; `also` runs after every poll to cover
    /// other observations, including events collected during read-back.
    /// Returns the number of qualifying A windows.
    pub fn expect_stays_on_a(
        &mut self,
        what: &str,
        since: Checkpoint,
        mut also: impl FnMut(&Session) -> Result<(), String>,
    ) -> usize {
        let mut checked = self.fresh_checkpoint().windows;
        let mut a_windows = 0;
        self.wait_for_observation(what, since.at + STEADY_WALL_TIMEOUT, |s, _| {
            also(s)?;
            if let Some(generation) = s.generations.keys().find(|&&g| g != 0) {
                return Err(format!(
                    "a buffer of generation {generation} arrived; every buffer must stay \
                     generation 0"
                ));
            }
            for w in &s.windows[checked..] {
                if w.value.verdict != Verdict::A {
                    return Err(format!(
                        "expected A in every fresh window; window {} was {} at {} \
                         (440 Hz over 997 Hz: {:.1} dB, RMS {:.4})",
                        a_windows + 1,
                        w.value.verdict,
                        fmt_at(w.at),
                        w.value.dominance_a_db(),
                        w.value.rms
                    ));
                }
                a_windows += 1;
                if a_windows == STEADY_MIN_A_WINDOWS {
                    return Ok(Some(w.at));
                }
            }
            checked = s.windows.len();
            Ok(None)
        });
        a_windows
    }

    /// Fails unless `recovery_snapshot()` reports `Running` at `generation`
    /// with `successful_recoveries` replacements published. Does not poll.
    pub fn expect_running_snapshot(&self, generation: u64, successful_recoveries: u64) {
        match self.capture.recovery_snapshot() {
            Some(snap)
                if matches!(snap.state, RecoveryState::Running)
                    && snap.generation == generation
                    && snap.successful_recoveries == successful_recoveries => {}
            other => self.fail(&format!(
                "recovery_snapshot() should be Running at generation {generation} with \
                 {successful_recoveries} successful recoveries, got {other:?}"
            )),
        }
    }

    /// Whether `recovery_snapshot()` reports `Exhausted`.
    pub fn snapshot_is_exhausted(&self) -> bool {
        self.capture
            .recovery_snapshot()
            .is_some_and(|snap| matches!(snap.state, RecoveryState::Exhausted))
    }

    /// Distinct generations that delivered audio, e.g. `0,1,2`.
    pub fn generation_list(&self) -> String {
        self.generations
            .keys()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Dominance of the weakest dominant window so far.
    pub fn min_dominance_db(&self) -> f64 {
        self.windows
            .iter()
            .filter_map(|w| w.value.winning_dominance_db())
            .fold(f64::INFINITY, f64::min)
    }

    /// Fails the test: panics with `message` and the full [`Session::report`].
    /// Restore guards still run while the panic unwinds.
    pub fn fail(&self, message: &str) -> ! {
        panic!("HOTPLUG FAILURE: {message}\n{}", self.report())
    }

    /// The failure dump: context, [`Machine::defaults`], milestones, recovery
    /// events and snapshot, window verdicts per generation, buffers per
    /// generation, delivered audio, read errors, the last 15 windows and
    /// [`Machine::players`].
    pub fn report(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "--- hotplug diagnostics at {} ---",
            fmt_at(self.elapsed())
        );
        let _ = writeln!(out, "context: {}", self.context);
        let _ = writeln!(out, "{}", self.machine.defaults());
        out.push_str("milestones:\n");
        for m in &self.marks {
            let _ = writeln!(out, "  [{}] {}", fmt_at(m.at), m.value);
        }
        match &self.events {
            Some(log) => {
                out.push_str("recovery events:\n");
                out.push_str(&log.render());
            }
            None => out.push_str("recovery events: auto-recovery not enabled\n"),
        }
        let _ = writeln!(
            out,
            "recovery snapshot: {:?}",
            self.capture.recovery_snapshot()
        );
        let _ = writeln!(
            out,
            "window verdicts: {}",
            verdict_summary(self.windows.iter().map(|w| &w.value))
        );
        let _ = writeln!(
            out,
            "windows: {} total; {} partial-window frames discarded at boundaries; {} \
             unanalysable buffers",
            self.windows.len(),
            self.analyzer.discarded_frames(),
            self.analyzer.skipped_buffers()
        );
        out.push_str("buffers per generation:\n");
        for (generation, s) in &self.generations {
            let _ = writeln!(
                out,
                "  gen{generation}: {} buffers, {} frames, {} Hz x {} ch, first at {}",
                s.buffers,
                s.frames,
                s.sample_rate,
                s.channels,
                fmt_at(s.first_at)
            );
        }
        let _ = writeln!(
            out,
            "audio delivered: {:.2} s; ring overruns: {}",
            self.audio_secs,
            self.capture.overrun_count()
        );
        let _ = writeln!(
            out,
            "read errors: {} recoverable (last: {}); terminal: {}",
            self.recoverable_errors,
            self.last_recoverable_error.as_deref().unwrap_or("none"),
            self.terminal.as_ref().map_or_else(
                || "none".to_string(),
                |t| format!("[{}] {:?}", fmt_at(t.at), t.value)
            )
        );
        out.push_str("last windows:\n");
        let skip = self.windows.len().saturating_sub(15);
        for w in &self.windows[skip..] {
            let _ = writeln!(
                out,
                "  [{}] gen{} frame {} {} rms={:.4} 440Hz-over-997Hz={:.1} dB",
                fmt_at(w.at),
                w.value.generation,
                w.value.start_frame,
                w.value.verdict,
                w.value.rms,
                w.value.dominance_a_db()
            );
        }
        out.push_str("players:\n");
        for p in self.machine.players() {
            let _ = writeln!(out, "  {p}");
        }
        out
    }
}

// ── Self-tests ───────────────────────────────────────────────────────────
//
// Expectations, worked out once for every test below. A window is 200 ms, so
// its Goertzel resolution is 5 Hz at both 48 kHz (9600 frames) and 44.1 kHz
// (8820 frames). A sine of amplitude 0.05 measures power 0.05² = 2.5e-3 at its
// own frequency. The other tone sits 557 Hz = 111.4 resolution steps away;
// rectangular-window leakage there is about |sin(0.4π)| / (π · 111.4) ≈ 2.7e-3
// of the amplitude, plus about 1.1e-3 from the negative-frequency image, so at
// worst 3.8e-3 → about −48 dB. A pure tone therefore dominates by roughly
// +48 dB or more, far above the 20 dB threshold.

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    const RATES: [u32; 2] = [48_000, 44_100];

    #[test]
    fn fresh_boundary_does_not_credit_a_partial_pre_switch_window() {
        let rate = 48_000;
        let mut analyzer = ToneAnalyzer::new();
        let half = window_frames(rate) / 2;
        let mut out = Vec::new();
        let pre_switch = AudioBuffer::new(vec![0.0; half], 1, rate);
        analyzer.push(&pre_switch, &mut out);
        analyzer.discard_partial();
        analyzer.push(&pre_switch, &mut out);
        assert!(
            out.is_empty(),
            "half a fresh window must not qualify: {out:?}"
        );
        analyzer.push(&pre_switch, &mut out);
        assert_eq!(out.len(), 1, "two fresh halves complete a window: {out:?}");
        assert_eq!(out[0].start_frame, half as u64);
    }

    /// `seconds` of mono audio: each `(frequency, amplitude)` sine summed.
    fn signal(rate: u32, seconds: f64, tones: &[(f64, f32)]) -> Vec<f32> {
        let frames = (f64::from(rate) * seconds).round() as usize;
        let omega = std::f64::consts::TAU / f64::from(rate);
        (0..frames)
            .map(|n| {
                let n = n as f64;
                tones
                    .iter()
                    .map(|&(freq, amp)| f64::from(amp) * (omega * freq * n).sin())
                    .sum::<f64>() as f32
            })
            .collect()
    }

    /// Delivers `mono` as stereo buffers of 10 ms, like a WASAPI period, so
    /// windows span many buffers.
    fn deliver(
        analyzer: &mut ToneAnalyzer,
        mono: &[f32],
        rate: u32,
        generation: u64,
    ) -> Vec<WindowReport> {
        let mut out = Vec::new();
        for chunk in mono.chunks((rate / 100) as usize) {
            let stereo: Vec<f32> = chunk.iter().flat_map(|&s| [s, s]).collect();
            let buffer = AudioBuffer::new(stereo, 2, rate).with_stream_generation(generation);
            analyzer.push(&buffer, &mut out);
        }
        out
    }

    fn windows_of(rate: u32, tones: &[(f64, f32)]) -> Vec<WindowReport> {
        deliver(&mut ToneAnalyzer::new(), &signal(rate, 1.0, tones), rate, 0)
    }

    fn report(verdict: Verdict, generation: u64) -> WindowReport {
        WindowReport {
            generation,
            sample_rate: 48_000,
            start_frame: 0,
            rms: 0.035,
            power_a: 2.5e-3,
            power_b: 2.5e-3,
            verdict,
        }
    }

    #[test]
    fn pure_440_is_a_dominant_in_every_window() {
        // rms = 0.05/√2 ≈ 0.035 ≥ 0.005; dominance ≈ +48 dB or more.
        for rate in RATES {
            let windows = windows_of(rate, &[(TONE_A_HZ, TONE_AMPLITUDE)]);
            assert_eq!(windows.len(), 5, "1 s at {rate} Hz is 5 windows");
            for w in &windows {
                assert_eq!(w.verdict, Verdict::A, "{rate} Hz: {w:?}");
                assert_eq!(w.sample_rate, rate, "analysed at the buffer's own rate");
                assert!((w.power_a - 2.5e-3).abs() < 1e-4, "{rate} Hz: {w:?}");
                assert!(w.dominance_a_db() > 40.0, "{rate} Hz: {w:?}");
                assert_eq!(w.winning_dominance_db(), Some(w.dominance_a_db()));
            }
        }
    }

    #[test]
    fn pure_997_is_b_dominant_in_every_window() {
        // Symmetric to 440 Hz: rms ≈ 0.035, dominance ≈ +48 dB or more.
        for rate in RATES {
            let windows = windows_of(rate, &[(TONE_B_HZ, TONE_AMPLITUDE)]);
            assert_eq!(windows.len(), 5, "1 s at {rate} Hz is 5 windows");
            for w in &windows {
                assert_eq!(w.verdict, Verdict::B, "{rate} Hz: {w:?}");
                assert!(w.dominance_b_db() > 40.0, "{rate} Hz: {w:?}");
                assert_eq!(w.winning_dominance_db(), Some(w.dominance_b_db()));
            }
        }
    }

    #[test]
    fn equal_mix_is_dominant_neither_way() {
        // Both bins measure ≈ 2.5e-3 (leakage moves each by under 1 dB), so the
        // dominance is ≈ 0 dB both ways, far below 20 dB.
        for rate in RATES {
            let mix = [(TONE_A_HZ, TONE_AMPLITUDE), (TONE_B_HZ, TONE_AMPLITUDE)];
            let windows = windows_of(rate, &mix);
            assert_eq!(windows.len(), 5);
            for w in &windows {
                assert_eq!(w.verdict, Verdict::Neither, "{rate} Hz: {w:?}");
                assert!(w.dominance_a_db().abs() < 3.0, "{rate} Hz: {w:?}");
                assert_eq!(w.winning_dominance_db(), None);
            }
        }
    }

    #[test]
    fn a_minus_26_db_competitor_keeps_a_dominant_but_minus_14_db_does_not() {
        // −26 dB: 997 Hz at 0.05·10^(−26/20) ≈ 2.51e-3. The 440 Hz leakage into
        // that bin (≤ 3.8e-3 · 0.05 ≈ 1.9e-4) shifts it by at most ±0.7 dB, so
        // the dominance is 25.3…26.7 dB ≥ 20: A-dominant.
        // −14 dB: 997 Hz at 0.05·10^(−14/20) ≈ 9.98e-3, dominance ≈ 14 dB < 20:
        // not A-dominant (and −14 dB the other way: not B either).
        for rate in RATES {
            let weak = TONE_AMPLITUDE * 10f32.powf(-26.0 / 20.0);
            for w in &windows_of(rate, &[(TONE_A_HZ, TONE_AMPLITUDE), (TONE_B_HZ, weak)]) {
                assert_eq!(w.verdict, Verdict::A, "{rate} Hz, −26 dB: {w:?}");
                assert!(
                    (w.dominance_a_db() - 26.0).abs() < 1.5,
                    "{rate} Hz, −26 dB: {w:?}"
                );
            }
            let strong = TONE_AMPLITUDE * 10f32.powf(-14.0 / 20.0);
            for w in &windows_of(rate, &[(TONE_A_HZ, TONE_AMPLITUDE), (TONE_B_HZ, strong)]) {
                assert_eq!(w.verdict, Verdict::Neither, "{rate} Hz, −14 dB: {w:?}");
                assert!(
                    (w.dominance_a_db() - 14.0).abs() < 1.5,
                    "{rate} Hz, −14 dB: {w:?}"
                );
            }
        }
    }

    #[test]
    fn silence_and_a_faint_tone_are_never_dominant() {
        // Silence: rms 0. A 0.001 sine: rms ≈ 7.1e-4 < 0.005, although its
        // dominance alone would pass.
        for rate in RATES {
            for w in &windows_of(rate, &[]) {
                assert_eq!(w.verdict, Verdict::Neither, "{rate} Hz silence: {w:?}");
                assert!(w.rms.abs() < 1e-12, "{rate} Hz silence: {w:?}");
            }
            for w in &windows_of(rate, &[(TONE_A_HZ, 0.001)]) {
                assert_eq!(w.verdict, Verdict::Neither, "{rate} Hz faint: {w:?}");
                assert!(
                    w.rms < MIN_RMS && w.dominance_a_db() > MIN_DOMINANCE_DB,
                    "{w:?}"
                );
            }
        }
    }

    #[test]
    fn a_generation_change_discards_the_partial_window() {
        // Generation 0 delivers 1.5 windows of 440 Hz: one window, then a
        // half-window partial that the change to generation 1 discards.
        // Generation 1 delivers two windows of 997 Hz. Had the partial been
        // kept, generation 1's first window would hold half 440 Hz and half
        // 997 Hz (≈ 0 dB: neither); started afresh it is purely B.
        for rate in RATES {
            let window = window_frames(rate);
            let mut analyzer = ToneAnalyzer::new();
            let gen0 = signal(rate, 0.3, &[(TONE_A_HZ, TONE_AMPLITUDE)]);
            assert_eq!(gen0.len(), window * 3 / 2);
            let first = deliver(&mut analyzer, &gen0, rate, 0);
            assert_eq!(first.len(), 1);
            assert_eq!((first[0].generation, first[0].verdict), (0, Verdict::A));

            let gen1 = signal(rate, 0.4, &[(TONE_B_HZ, TONE_AMPLITUDE)]);
            let second = deliver(&mut analyzer, &gen1, rate, 1);
            assert_eq!(analyzer.discarded_frames(), (window / 2) as u64);
            assert_eq!(second.len(), 2, "{rate} Hz: {second:?}");
            for (i, w) in second.iter().enumerate() {
                assert_eq!(w.generation, 1);
                assert_eq!(w.verdict, Verdict::B, "{rate} Hz window {i}: {w:?}");
                assert_eq!(w.start_frame, (i * window) as u64, "window {i} start");
            }
            assert_eq!(analyzer.skipped_buffers(), 0);
        }
    }

    #[test]
    fn a_buffer_without_channels_is_skipped() {
        let mut analyzer = ToneAnalyzer::new();
        let mut out = Vec::new();
        analyzer.push(&AudioBuffer::new(vec![0.0; 8], 0, 48_000), &mut out);
        assert!(out.is_empty());
        assert_eq!(analyzer.skipped_buffers(), 1);
    }

    #[test]
    fn five_consecutive_windows_see_a_phase_and_neither_resets() {
        let mut streak = Streak::new(Verdict::A, Some(0));
        for _ in 0..4 {
            assert!(!streak.observe(&report(Verdict::A, 0)));
        }
        assert_eq!(streak.run(), 4);
        assert!(!streak.is_seen(), "4 windows are not enough");
        assert!(
            streak.observe(&report(Verdict::A, 0)),
            "the 5th window sees A"
        );
        assert!(
            streak.observe(&report(Verdict::Neither, 0)),
            "seen stays seen"
        );

        let mut streak = Streak::new(Verdict::A, Some(0));
        for verdict in [Verdict::A, Verdict::A, Verdict::A, Verdict::Neither] {
            streak.observe(&report(verdict, 0));
        }
        assert_eq!(streak.run(), 0, "a neither window resets the count");
        for _ in 0..4 {
            assert!(!streak.observe(&report(Verdict::A, 0)));
        }
        assert!(streak.observe(&report(Verdict::A, 0)));

        let mut streak = Streak::new(Verdict::B, Some(1));
        for _ in 0..4 {
            streak.observe(&report(Verdict::B, 1));
        }
        streak.observe(&report(Verdict::B, 0));
        assert_eq!(streak.run(), 0, "another generation resets the count");
        streak.observe(&report(Verdict::A, 1));
        assert_eq!(streak.run(), 0, "the other tone resets the count");

        let mut any = Streak::new(Verdict::B, None);
        for generation in 0..5 {
            any.observe(&report(Verdict::B, generation));
        }
        assert!(
            any.is_seen(),
            "without a generation filter any generation counts"
        );
    }

    #[test]
    fn verdict_summary_counts_per_generation() {
        let windows = [
            report(Verdict::A, 0),
            report(Verdict::A, 0),
            report(Verdict::Neither, 0),
            report(Verdict::B, 1),
        ];
        assert_eq!(
            verdict_summary(&windows),
            "gen0: A=2 B=0 neither=1; gen1: A=0 B=1 neither=0"
        );
        assert_eq!(verdict_summary(std::iter::empty()), "no windows");
    }

    #[test]
    fn dominance_floors_an_empty_competitor() {
        assert!((dominance_db(1e-2, 1e-4) - 20.0).abs() < 1e-9);
        assert!((dominance_db(1e-3, 0.0) - 90.0).abs() < 1e-9);
        assert_eq!(classify(0.01, 1e-3, 0.0), Verdict::A);
        assert!(goertzel_power(&[], 48_000, TONE_A_HZ).abs() < 1e-12);
    }

    #[test]
    fn event_log_drains_in_order_with_timestamps() {
        let (tx, rx) = mpsc::channel();
        let mut log = EventLog::new(rx, Instant::now());
        assert_eq!(log.drain(), 0);
        assert!(log.render().contains("(no events)"));
        tx.send(1u32).unwrap();
        tx.send(2).unwrap();
        assert_eq!(log.drain(), 2);
        tx.send(3).unwrap();
        drop(tx);
        assert_eq!(log.drain(), 1);
        assert!(log.is_disconnected());
        let values: Vec<u32> = log.events().iter().map(|e| e.value).collect();
        assert_eq!(values, [1, 2, 3]);
        assert!(log.events().windows(2).all(|w| w[0].at <= w[1].at));
        let rendered = log.render();
        assert!(rendered.contains("] 3\n"), "{rendered}");
        assert!(rendered.contains("disconnected"), "{rendered}");
        assert_eq!(fmt_at(Duration::from_millis(1234)), "+1.234s");
    }

    #[test]
    fn find_in_order_matches_a_subsequence() {
        let items = [5, 1, 7, 2, 1, 3];
        let one = |x: &i32| *x == 1;
        let two = |x: &i32| *x == 2;
        let three = |x: &i32| *x == 3;
        assert_eq!(
            find_in_order(&items, &[&one, &two, &three]),
            Some(vec![1, 3, 5])
        );
        assert_eq!(find_in_order(&items, &[&two, &one]), Some(vec![3, 4]));
        assert_eq!(
            find_in_order(&items, &[&three, &one]),
            None,
            "order matters"
        );
        assert_eq!(find_in_order(&items, &[]), Some(vec![]));
    }

    #[test]
    fn machine_lock_survives_a_test_that_panicked_holding_it() {
        let poisoner = std::thread::spawn(|| {
            let _guard = machine_lock();
            panic!("intentional: a failed evidence test panics while holding the lock");
        });
        assert!(poisoner.join().is_err());
        drop(machine_lock());
    }
}
