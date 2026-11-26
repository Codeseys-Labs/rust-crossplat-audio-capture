//! Auto-recovery supervisor for desktop `SystemDefault` capture (feature
//! `hotplug-recovery`).
//!
//! [`RecoveryStream`] wraps the generation-0 stream `AudioCapture::start()`
//! opened and is itself a [`CapturingStream`], so every consumer of the handle
//! (pull reads, subscribe pumps, the callback pump, sink drains, the async
//! stream) is supervised without knowing about recovery.
//!
//! The supervisor's state machine is **poll-driven**: it advances inside
//! `try_read_chunk()` on the consumer thread — never on an OS real-time audio
//! callback thread (ADR-0001). Backoff, settle, and device-wait periods are
//! deadlines checked on each poll rather than sleeps, so a sticky
//! [`stop`](CapturingStream::stop) takes effect on the very next poll.
//!
//! # Backend work never runs on the read path
//!
//! Retiring the old stream and opening its replacement run on a short-lived,
//! single-flight helper thread (`rsac-recovery-open`, one per attempt; the
//! `Reopening` phase is the in-flight guard). A poll only *collects* the
//! helper's result and returns `Ok(None)` while an open is in flight. The
//! `machine` lock that serialises readers is therefore never held across a
//! backend `stop()`, a factory open, or the disposal of a rejected or late
//! stream, and [`stop`](CapturingStream::stop) wakes every reader without
//! waiting for a wedged backend open. A result that lands after `stop()` is
//! disposed (stopped and dropped) by the helper itself.
//!
//! # Recovery contract
//!
//! Recovery is break-before-make: the helper stops and releases the old stream
//! before the factory opens a replacement on the current default device. The
//! delivery format is gated twice, both on `(sample_rate, channels)`: a
//! replacement whose reported `format()` matches nothing generation 0 ever
//! reported is stopped unread, and **every buffer** a replacement delivers is
//! checked against the baseline the first delivered buffer established — the
//! only authoritative source, because a backend may still report its requested
//! format while negotiation is pending. Either mismatch ends the session with a
//! fatal [`AudioError::StreamEnded`] before any sample of the new format
//! reaches the caller. Delivered buffers are tagged with their stream
//! generation and, from generation 1 on, shifted onto one monotonic session
//! timeline: a session-position cursor advances by every delivered buffer's
//! duration (stamped or not), and the outage since the last audio — carried
//! forward across generations that die before delivering — is added when a
//! replacement is installed.
//!
//! Default-device changes proactively retire a running stream on Windows and
//! macOS only; on Linux the session manager (WirePlumber) re-routes
//! `SystemDefault` natively, so a default change merely wakes an absent-device
//! wait and recovery otherwise reacts to stream invalidation.

use crate::audio::get_device_enumerator;
use crate::core::buffer::AudioBuffer;
use crate::core::config::{AudioFormat, CaptureTarget, StreamConfig};
use crate::core::error::{AudioError, AudioResult};
use crate::core::interface::{
    CapturingStream, DeviceEvent, DeviceEventHandler, DeviceKind, DeviceWatcher,
};
use crate::core::recovery::{AutoRecoveryConfig, RecoveryEvent, RecoverySnapshot, RecoveryState};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Capacity of the bounded channel behind [`RecoveryHub::subscribe`].
const RECOVERY_EVENT_CHANNEL_CAPACITY: usize = 32;

/// Terminal reason when a replacement stream delivers a different format.
const FORMAT_CHANGED_REASON: &str = "reconnected device changed format";

/// Terminal reason when `stop()` lands while no stream is installed.
const STOPPED_DURING_RECOVERY_REASON: &str = "capture stopped while auto-recovery was reopening";

/// Poll interval of the blocking read path (and of the async poll waker).
const POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Fallback horizon for a deadline whose configured delay overflows `Instant`.
const FAR_FUTURE: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// Most queued buffers drained from a stream just before it is retired for a
/// proactive switch, or once `stop()` lands — one ring's worth, so a producer
/// that outpaces the consumer can never postpone the switch forever.
const TAIL_DRAIN_LIMIT: usize = 128;

/// Whether an output default-device change proactively retires a running
/// stream. Windows and macOS keep `SystemDefault` bound to the old endpoint, so
/// they must reopen; on Linux the session manager re-routes natively and a
/// reopen would only cost an outage and a generation change.
const PROACTIVE_DEFAULT_SWITCH: bool = cfg!(not(target_os = "linux"));

/// Opens a replacement backend stream on the device recovery follows.
type StreamFactory = Box<dyn Fn() -> AudioResult<Box<dyn CapturingStream>> + Send + Sync>;

/// Streams a poll decided to retire, stopped and dropped only after the
/// `machine` lock is released.
type Doomed = Vec<Arc<dyn CapturingStream>>;

/// Locks `mutex`, recovering the guard when a previous holder panicked: every
/// value guarded in this module is plain state that stays valid across a panic.
fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `now + delay`, saturating to a far-future deadline instead of panicking.
fn deadline_after(now: Instant, delay: Duration) -> Instant {
    now.checked_add(delay)
        .or_else(|| now.checked_add(FAR_FUTURE))
        .unwrap_or(now)
}

/// Errors a reopen cannot cure by retrying: permission, platform, and
/// configuration failures end recovery immediately instead of burning the
/// retry budget on an outcome that cannot change.
fn is_non_retryable(error: &AudioError) -> bool {
    matches!(
        error,
        AudioError::PermissionDenied { .. }
            | AudioError::PlatformNotSupported { .. }
            | AudioError::ConfigurationError { .. }
            | AudioError::InvalidParameter { .. }
            | AudioError::ApplicationNotFound { .. }
            | AudioError::UserConsentRequired { .. }
    )
}

// ── Builder preflight ────────────────────────────────────────────────────

/// Rejects auto-recovery on mobile: their capture paths are consent-gated and
/// have no replug semantics the supervisor could follow.
#[cfg(any(target_os = "android", target_os = "ios"))]
pub(super) fn preflight_auto_recovery(_target: &CaptureTarget) -> AudioResult<()> {
    Err(AudioError::PlatformNotSupported {
        feature: "capture auto-recovery (hotplug-recovery)".to_string(),
        platform: std::env::consts::OS.to_string(),
    })
}

/// Accepts only [`CaptureTarget::SystemDefault`]: following any other target
/// across a replug needs a stable replug key the backends do not provide yet.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub(super) fn preflight_auto_recovery(target: &CaptureTarget) -> AudioResult<()> {
    match target {
        CaptureTarget::SystemDefault => Ok(()),
        other => Err(AudioError::ConfigurationError {
            message: format!(
                "auto-recovery supports only CaptureTarget::SystemDefault in v0.5 (got \
                 {other}): device and application targets need stable replug keys the \
                 backends do not provide yet"
            ),
        }),
    }
}

// ── Wiring ───────────────────────────────────────────────────────────────

/// A device-change subscription armed **before** generation zero opens, so a
/// default switch during startup is recorded in the trigger (and reconciled by
/// the first poll) instead of being lost between open and registration.
/// Dropping it unregisters the listener.
pub(super) struct Supervision {
    trigger: Arc<RecoveryTrigger>,
    watcher: Option<DeviceWatcher>,
}

impl Supervision {
    /// Arms the device-change subscription; call before opening generation zero.
    pub(super) fn begin() -> Self {
        let trigger = Arc::new(RecoveryTrigger::default());
        let watcher = watch_default_device(&trigger);
        Self { trigger, watcher }
    }
}

/// Wraps the generation-0 stream `AudioCapture::start()` just opened in a
/// [`RecoveryStream`] that reopens the system default device under `hub`'s
/// policy, fed by the notifications `supervision` armed before the open.
pub(super) fn supervise(
    initial: Box<dyn CapturingStream>,
    stream_config: &StreamConfig,
    hub: &Arc<RecoveryHub>,
    supervision: Supervision,
) -> Box<dyn CapturingStream> {
    let Supervision { trigger, watcher } = supervision;
    let config = stream_config.clone();
    let factory: StreamFactory = Box::new(move || {
        get_device_enumerator()?
            .default_device()?
            .create_stream(&config)
    });
    Box::new(RecoveryStream::new(
        initial,
        factory,
        trigger,
        watcher,
        Arc::clone(hub),
    ))
}

/// Subscribes `trigger` to device-change notifications. Without them (the
/// backend returns `PlatformNotSupported`) recovery still reacts to stream
/// invalidation, just not proactively to default-device changes.
fn watch_default_device(trigger: &Arc<RecoveryTrigger>) -> Option<DeviceWatcher> {
    let handler_trigger = Arc::clone(trigger);
    let handler: DeviceEventHandler =
        Box::new(move |event: DeviceEvent| handler_trigger.on_device_event(&event));
    match get_device_enumerator().and_then(|enumerator| enumerator.watch(handler)) {
        Ok(watcher) => Some(watcher),
        Err(e) => {
            log::info!(
                "auto-recovery: device-change notifications unavailable ({e}); \
                 recovering from stream invalidation only"
            );
            None
        }
    }
}

// ── Hub: policy, events, snapshot ────────────────────────────────────────

/// Per-handle recovery state shared by an `AudioCapture` and every
/// [`RecoveryStream`] it starts: the policy, the single bounded event
/// subscription, and the published snapshot fields.
///
/// It outlives individual streams, so a subscription made before `start()`
/// keeps receiving events across stop/start restarts. Every `start()` takes a
/// fresh **session id**; a retired stream (one pump may still be mid-poll after
/// a restart) carries its old id and can neither mutate the snapshot nor emit
/// into the restarted session.
pub(super) struct RecoveryHub {
    config: AutoRecoveryConfig,
    events: Mutex<Option<mpsc::SyncSender<RecoveryEvent>>>,
    subscribed: AtomicBool,
    dropped_events: AtomicU64,
    status: Mutex<HubStatus>,
}

/// Snapshot fields other than `dropped_events`, updated on transitions only,
/// plus the id of the session allowed to update them.
struct HubStatus {
    session: u64,
    state: RecoveryState,
    generation: u64,
    attempt: u32,
    successful_recoveries: u64,
}

impl HubStatus {
    fn new(session: u64, state: RecoveryState) -> Self {
        Self {
            session,
            state,
            generation: 0,
            attempt: 0,
            successful_recoveries: 0,
        }
    }
}

impl RecoveryHub {
    /// Creates an idle hub for `config`; nothing is supervised until `start()`.
    pub(super) fn new(config: AutoRecoveryConfig) -> Self {
        Self {
            config,
            events: Mutex::new(None),
            subscribed: AtomicBool::new(false),
            dropped_events: AtomicU64::new(0),
            status: Mutex::new(HubStatus::new(0, RecoveryState::Idle)),
        }
    }

    /// Creates the handle's single event subscription: a bounded channel of
    /// [`RECOVERY_EVENT_CHANNEL_CAPACITY`] events, drop-newest when full.
    pub(super) fn subscribe(&self) -> AudioResult<mpsc::Receiver<RecoveryEvent>> {
        if self.subscribed.swap(true, Ordering::AcqRel) {
            return Err(AudioError::ConfigurationError {
                message: "recovery events already have a subscriber; \
                          subscribe_recovery_events() supports one subscriber per capture"
                    .to_string(),
            });
        }
        let (tx, rx) = mpsc::sync_channel(RECOVERY_EVENT_CHANNEL_CAPACITY);
        *lock(&self.events) = Some(tx);
        Ok(rx)
    }

    /// Reads the published state under one brief lock that is never held
    /// across device work.
    pub(super) fn snapshot(&self) -> RecoverySnapshot {
        let status = lock(&self.status);
        RecoverySnapshot {
            state: status.state.clone(),
            generation: status.generation,
            attempt: status.attempt,
            successful_recoveries: status.successful_recoveries,
            dropped_events: self.dropped_events.load(Ordering::Relaxed),
        }
    }

    /// Starts a capture session: a fresh monotonic session id, generation 0, a
    /// full retry budget, `Running`. Returns the id every later mutation and
    /// emission of that session must present.
    fn begin_session(&self) -> u64 {
        let mut status = lock(&self.status);
        let session = status.session.wrapping_add(1);
        *status = HubStatus::new(session, RecoveryState::Running);
        session
    }

    /// Publishes a supervisor transition of `session`. A stopped session is
    /// sealed so a transition racing `stop()` cannot overwrite `Stopped`, and a
    /// retired session is ignored outright.
    fn publish(&self, session: u64, state: RecoveryState, generation: u64, attempt: u32) {
        let mut status = lock(&self.status);
        if status.session == session && status.state != RecoveryState::Stopped {
            status.state = state;
            status.generation = generation;
            status.attempt = attempt;
        }
    }

    /// Publishes a successfully installed replacement generation of `session`.
    fn record_recovery(&self, session: u64, generation: u64, attempt: u32) {
        let mut status = lock(&self.status);
        if status.session == session && status.state != RecoveryState::Stopped {
            status.state = RecoveryState::Running;
            status.generation = generation;
            status.attempt = attempt;
            status.successful_recoveries += 1;
        }
    }

    fn mark_stopped(&self, session: u64) {
        let mut status = lock(&self.status);
        if status.session == session {
            status.state = RecoveryState::Stopped;
        }
    }

    /// Offers `event` of `session` to the subscriber without blocking: when the
    /// channel is full the event is dropped and counted (drop-newest, never
    /// stall). The session/stop check and the send happen under one `status`
    /// guard, so a retired or stopped session can never publish.
    fn emit(&self, session: u64, event: RecoveryEvent) {
        let status = lock(&self.status);
        if status.session != session || status.state == RecoveryState::Stopped {
            return;
        }
        let events = lock(&self.events);
        let Some(tx) = events.as_ref() else {
            return;
        };
        match tx.try_send(event) {
            Ok(()) | Err(mpsc::TrySendError::Disconnected(_)) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                self.dropped_events.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

// ── Trigger: device notifications → supervisor ───────────────────────────

/// Dirty flags raised by the device watcher's notification thread (or by a
/// test) and consumed by the supervisor's next poll.
#[derive(Debug)]
struct RecoveryTrigger {
    /// Proactive switch requested: the default output device changed.
    default_changed: AtomicBool,
    /// Wake-up for an absent-device wait: a device appeared (or, when not
    /// proactive, the default moved).
    device_arrived: AtomicBool,
    /// Whether a default-output change raises `default_changed` (Windows,
    /// macOS) or only wakes an absent-device wait (Linux).
    proactive_default: bool,
}

impl Default for RecoveryTrigger {
    fn default() -> Self {
        Self::new(PROACTIVE_DEFAULT_SWITCH)
    }
}

impl RecoveryTrigger {
    fn new(proactive_default: bool) -> Self {
        Self {
            default_changed: AtomicBool::new(false),
            device_arrived: AtomicBool::new(false),
            proactive_default,
        }
    }

    /// Requests a proactive switch, whatever the platform mode. This is the
    /// platform-neutral injection point tests use, and what a default-output
    /// change maps to when `proactive_default` is set.
    fn trigger_default_change(&self) {
        self.default_changed.store(true, Ordering::Release);
    }

    /// A device appeared: re-probe now if recovery is waiting for one.
    fn notify_device_arrived(&self) {
        self.device_arrived.store(true, Ordering::Release);
    }

    /// Maps a watcher event onto the flags. `SystemDefault` capture follows the
    /// default **output** endpoint (loopback), so only output defaults count.
    /// Without `proactive_default` a default change only wakes an absent-device
    /// wait and never retires a running stream.
    fn on_device_event(&self, event: &DeviceEvent) {
        match event {
            DeviceEvent::DefaultChanged {
                kind: DeviceKind::Output,
                ..
            } => {
                if self.proactive_default {
                    self.trigger_default_change();
                } else {
                    self.notify_device_arrived();
                }
            }
            DeviceEvent::DeviceAdded {
                kind: DeviceKind::Output,
                ..
            }
            | DeviceEvent::StateChanged {
                available: true, ..
            } => self.notify_device_arrived(),
            _ => {}
        }
    }

    fn take_default_changed(&self) -> bool {
        self.default_changed.swap(false, Ordering::AcqRel)
    }

    fn take_device_arrived(&self) -> bool {
        self.device_arrived.swap(false, Ordering::AcqRel)
    }
}

// ── Supervised stream ────────────────────────────────────────────────────

/// The installed backend stream plus the counters of the streams it replaced,
/// guarded together so cumulative counters never double-count a retirement.
struct ActiveSlot {
    stream: Option<Arc<dyn CapturingStream>>,
    retired: RetiredCounters,
}

/// Counters accumulated from every generation recovery has retired.
#[derive(Default)]
struct RetiredCounters {
    overruns: u64,
    captured: u64,
    pushed: u64,
    dropped: u64,
}

impl RetiredCounters {
    fn absorb(&mut self, stream: &dyn CapturingStream) {
        self.overruns = self.overruns.saturating_add(stream.overrun_count());
        self.captured = self.captured.saturating_add(stream.buffers_captured());
        self.pushed = self.pushed.saturating_add(stream.buffers_pushed());
        self.dropped = self.dropped.saturating_add(stream.buffers_dropped());
    }
}

/// The `(sample_rate, channels)` delivery contract of a session. Sample format
/// is not part of it: [`AudioBuffer`] payloads are always `f32`, and a backend
/// may report a provisional sample format until negotiation completes.
#[derive(Clone, Copy)]
struct FormatGate {
    /// What generation 0 reported when it opened — possibly the requested
    /// format, not yet the negotiated one.
    provisional: (u32, u16),
    /// What the first delivered buffer actually carried; the authority once
    /// known.
    authoritative: Option<(u32, u16)>,
}

impl FormatGate {
    fn signature(format: &AudioFormat) -> (u32, u16) {
        (format.sample_rate, format.channels)
    }

    /// Fail-fast screen for a replacement's *reported* format: admitted when it
    /// matches anything generation 0 reported or delivered. A mismatch here is
    /// certain; a match is only provisional until the first buffer is checked.
    fn admits(&self, format: &AudioFormat) -> bool {
        let reported = Self::signature(format);
        reported == self.provisional || self.authoritative == Some(reported)
    }

    fn expected(&self) -> (u32, u16) {
        self.authoritative.unwrap_or(self.provisional)
    }
}

/// State shared between the polling consumers and the reopen helper threads.
struct Core {
    factory: StreamFactory,
    slot: Mutex<ActiveSlot>,
    /// Sticky stop request. Checked under the `slot` lock before installing a
    /// replacement and under the `mailbox` lock before publishing a result, so
    /// a stream opened concurrently with `stop()` is never published.
    stop_requested: AtomicBool,
    /// Result of the single in-flight reopen, awaiting collection by a poll.
    mailbox: Mutex<Option<Reopened>>,
}

/// What a reopen helper produced, and when.
struct Reopened {
    outcome: Outcome,
    finished_at: Instant,
}

enum Outcome {
    /// A replacement whose reported format passed the fail-fast screen.
    Opened(Box<dyn CapturingStream>),
    /// A replacement whose reported format matches nothing generation 0
    /// reported; already stopped by the helper.
    FormatMismatch {
        detail: String,
    },
    Failed(AudioError),
}

/// Stops a stream nobody will read.
fn dispose(outcome: Outcome) {
    if let Outcome::Opened(stream) = outcome {
        if let Err(e) = stream.stop() {
            log::debug!("auto-recovery: stopping a discarded replacement failed: {e}");
        }
    }
}

impl Core {
    fn active_stream(&self) -> Option<Arc<dyn CapturingStream>> {
        lock(&self.slot).stream.clone()
    }

    /// Removes the installed stream from the slot, folding its counters into
    /// the retired totals. The caller stops it with no lock held.
    fn detach_active(&self) -> Option<Arc<dyn CapturingStream>> {
        let mut slot = lock(&self.slot);
        let stream = slot.stream.take()?;
        slot.retired.absorb(stream.as_ref());
        Some(stream)
    }

    /// Break-before-make: stops the installed stream and releases it, so the
    /// device is free before the factory opens a replacement. Runs on the
    /// reopen helper, never under a supervisor lock.
    fn retire_active(&self) {
        let Some(old) = self.active_stream() else {
            return;
        };
        if let Err(e) = old.stop() {
            log::debug!("auto-recovery: stopping the replaced stream failed: {e}");
        }
        {
            let mut slot = lock(&self.slot);
            if slot
                .stream
                .as_ref()
                .is_some_and(|installed| Arc::ptr_eq(installed, &old))
            {
                slot.retired.absorb(old.as_ref());
                slot.stream = None;
            }
        }
        // `old` drops here: the device is released before the open.
    }

    /// The single-flight reopen: retire the old stream, open a replacement,
    /// screen its reported format, and post the result. Disposes of anything a
    /// concurrent `stop()` made late.
    fn run_reopen(&self, gate: FormatGate) {
        self.retire_active();
        if self.stop_requested.load(Ordering::SeqCst) {
            // Never open a device just to close it again.
            return;
        }
        let opened = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.factory)()))
            .unwrap_or_else(|_| {
                Err(AudioError::InternalError {
                    message: "auto-recovery: the reopen factory panicked".to_string(),
                    source: None,
                })
            });
        let finished_at = Instant::now();
        let outcome = match opened {
            Ok(stream) => {
                let format = stream.format();
                if gate.admits(&format) {
                    Outcome::Opened(stream)
                } else {
                    // Stop it unread so no sample of the new format is ever delivered.
                    if let Err(e) = stream.stop() {
                        log::debug!(
                            "auto-recovery: stopping the mismatched replacement failed: {e}"
                        );
                    }
                    Outcome::FormatMismatch {
                        detail: format!(
                            "{FORMAT_CHANGED_REASON}: negotiated {:?} (rate, channels), \
                             replacement reports {:?}",
                            gate.expected(),
                            FormatGate::signature(&format)
                        ),
                    }
                }
            }
            Err(error) => Outcome::Failed(error),
        };
        let mut mailbox = lock(&self.mailbox);
        if self.stop_requested.load(Ordering::SeqCst) {
            drop(mailbox);
            dispose(outcome);
            return;
        }
        *mailbox = Some(Reopened {
            outcome,
            finished_at,
        });
    }

    /// Publishes `stream` unless `stop()` was requested. The flag is read under
    /// the `slot` lock that `stop()` takes after raising it, so either this
    /// install is visible to `stop()` or it is refused here (and the stream
    /// handed back through `doomed` for disposal outside every lock).
    fn install(&self, stream: Arc<dyn CapturingStream>, doomed: &mut Doomed) -> bool {
        let mut slot = lock(&self.slot);
        if self.stop_requested.load(Ordering::SeqCst) {
            drop(slot);
            doomed.push(stream);
            return false;
        }
        if let Some(stale) = slot.stream.replace(stream) {
            slot.retired.absorb(stale.as_ref());
            doomed.push(stale);
        }
        true
    }
}

/// Supervisor phase.
enum Phase {
    /// A stream is installed and delivering.
    Running,
    /// A default change was seen; the old stream keeps delivering until the
    /// notifications have been quiet for `settle_delay`, then its queued tail
    /// is drained before it is retired.
    Settling { deadline: Instant },
    /// Waiting out the backoff before the next reopen attempt.
    Backoff { until: Instant },
    /// No device is present; waiting for one without spending the budget.
    WaitingForDevice {
        deadline: Instant,
        next_probe: Instant,
    },
    /// One reopen is in flight on the helper thread. Polls return `Ok(None)`
    /// until its result is collected.
    Reopening {
        /// The device-wait deadline this probe belongs to, when it is one.
        wait_deadline: Option<Instant>,
        /// A proactive reopen: free when it succeeds, charged to the retry
        /// budget when it fails.
        uncharged: bool,
    },
    /// Recovery gave up; every read returns `StreamEnded { reason }`.
    Exhausted { reason: String },
}

/// Supervisor state, owned by whichever consumer thread is polling.
struct Machine {
    phase: Phase,
    generation: u64,
    /// Opens spent from the budget (the one in progress included).
    attempt: u32,
    /// Offset added to the current generation's timestamps.
    base_offset: Duration,
    /// Session-position cursor: end of the latest delivered audio on the
    /// session timeline. Advances by every buffer's duration whether or not it
    /// carries a timestamp.
    session_pos: Duration,
    gate: FormatGate,
    /// When the current outage began (old stream lost or released). Persists
    /// until the next generation delivers a buffer, so a generation that dies
    /// before delivering carries the outage forward.
    outage_start: Option<Instant>,
    /// When the current generation was installed; drives the budget reset.
    healthy_since: Option<Instant>,
    /// Buffers drained from the installed stream by `stop()`, served ahead of
    /// the (by then stopped) backend.
    tail: VecDeque<AudioBuffer>,
    /// Buffers drained from the old stream since its settle deadline expired.
    settle_drained: usize,
}

impl Machine {
    /// Tags `buffer` with the current generation, moves its timestamp onto the
    /// session timeline (identity for generation 0), and advances the cursor.
    fn stamp(&mut self, buffer: AudioBuffer) -> AudioBuffer {
        let duration = buffer.duration();
        let shifted = self.generation > 0;
        let (buffer, start) = match buffer.timestamp() {
            Some(at) if shifted => {
                let start = self.base_offset.saturating_add(at);
                let format = buffer.format().clone();
                (
                    AudioBuffer::with_timestamp(buffer.into_data(), format, start),
                    start,
                )
            }
            Some(at) => (buffer, at),
            // No timestamp: the buffer continues the timeline, never earlier
            // than this generation's base.
            None => (buffer, self.session_pos.max(self.base_offset)),
        };
        self.session_pos = self.session_pos.max(start.saturating_add(duration));
        if shifted {
            // The first audio of a replacement closes the outage window.
            self.outage_start = None;
        }
        buffer.with_stream_generation(self.generation)
    }
}

/// Outcome of one non-blocking read of the installed stream.
enum Read {
    Buffer(AudioBuffer),
    Empty,
    /// The stream reached a fatal terminal; carries the diagnostic.
    Lost(String),
}

/// A [`CapturingStream`] that keeps a system-default capture alive across
/// default-device changes and backend invalidation; see the module docs.
struct RecoveryStream {
    hub: Arc<RecoveryHub>,
    /// This stream's session id on `hub`; see [`RecoveryHub`].
    session: u64,
    core: Arc<Core>,
    trigger: Arc<RecoveryTrigger>,
    /// Device-change subscription feeding `trigger`; released on stop or
    /// exhaustion.
    watcher: Mutex<Option<DeviceWatcher>>,
    /// Delivery format generation 0 reported; the fallback for `format()`
    /// while no stream is installed.
    expected_format: AudioFormat,
    /// Never held across backend work: see the module docs.
    machine: Mutex<Machine>,
    /// Set once stopped or exhausted; backs `is_running()`.
    terminal: AtomicBool,
    #[cfg(feature = "async-stream")]
    poll_waker: PollWaker,
}

impl RecoveryStream {
    fn new(
        initial: Box<dyn CapturingStream>,
        factory: StreamFactory,
        trigger: Arc<RecoveryTrigger>,
        watcher: Option<DeviceWatcher>,
        hub: Arc<RecoveryHub>,
    ) -> Self {
        let expected_format = initial.format();
        let session = hub.begin_session();
        Self {
            hub,
            session,
            core: Arc::new(Core {
                factory,
                slot: Mutex::new(ActiveSlot {
                    stream: Some(Arc::from(initial)),
                    retired: RetiredCounters::default(),
                }),
                stop_requested: AtomicBool::new(false),
                mailbox: Mutex::new(None),
            }),
            trigger,
            watcher: Mutex::new(watcher),
            machine: Mutex::new(Machine {
                phase: Phase::Running,
                generation: 0,
                attempt: 0,
                base_offset: Duration::ZERO,
                session_pos: Duration::ZERO,
                gate: FormatGate {
                    provisional: FormatGate::signature(&expected_format),
                    authoritative: None,
                },
                outage_start: None,
                healthy_since: Some(Instant::now()),
                tail: VecDeque::new(),
                settle_drained: 0,
            }),
            expected_format,
            terminal: AtomicBool::new(false),
            #[cfg(feature = "async-stream")]
            poll_waker: PollWaker::default(),
        }
    }

    fn publish(&self, state: RecoveryState, generation: u64, attempt: u32) {
        self.hub.publish(self.session, state, generation, attempt);
    }

    fn emit(&self, event: RecoveryEvent) {
        self.hub.emit(self.session, event);
    }

    /// Sums a counter over the retired generations and the installed stream.
    fn cumulative(
        &self,
        retired: fn(&RetiredCounters) -> u64,
        live: fn(&dyn CapturingStream) -> u64,
    ) -> u64 {
        let slot = lock(&self.core.slot);
        let current = slot
            .stream
            .as_ref()
            .map_or(0, |stream| live(stream.as_ref()));
        retired(&slot.retired).saturating_add(current)
    }

    /// Advances the state machine until it yields a buffer, `Ok(None)`, or an
    /// error. Runs on the polling consumer thread with the `machine` lock held,
    /// so it must never call backend `stop()`, the factory, or any other
    /// potentially slow backend operation: streams to retire go into `doomed`
    /// and the reopen runs on a helper thread.
    fn advance(&self, m: &mut Machine, doomed: &mut Doomed) -> AudioResult<Option<AudioBuffer>> {
        let settle = self.hub.config.settle_delay;
        loop {
            if self.core.stop_requested.load(Ordering::Acquire)
                && !matches!(m.phase, Phase::Exhausted { .. })
            {
                return self.read_after_stop(m, doomed);
            }
            let now = Instant::now();
            match m.phase {
                Phase::Exhausted { ref reason } => {
                    return Err(AudioError::StreamEnded {
                        reason: reason.clone(),
                    });
                }
                Phase::Running => {
                    if m.attempt > 0 && self.budget_reset_due(m, now) {
                        m.attempt = 0;
                        self.publish(RecoveryState::Running, m.generation, 0);
                    }
                    if self.trigger.take_default_changed() {
                        self.emit(RecoveryEvent::DefaultChanged {
                            generation: m.generation,
                        });
                        m.settle_drained = 0;
                        m.phase = Phase::Settling {
                            deadline: deadline_after(now, settle),
                        };
                        continue;
                    }
                    match self.read_active(m, doomed)? {
                        Read::Buffer(buffer) => return Ok(Some(buffer)),
                        Read::Empty => return Ok(None),
                        Read::Lost(reason) => self.on_lost(m, reason, now),
                    }
                }
                Phase::Settling { deadline } => {
                    // Every further notification restarts the quiet period.
                    let deadline = if self.trigger.take_default_changed() {
                        deadline_after(now, settle)
                    } else {
                        deadline
                    };
                    m.phase = Phase::Settling { deadline };
                    // Before the deadline the old stream simply keeps
                    // delivering. After it, keep draining what is already
                    // queued (bounded) so the readable tail is not discarded
                    // by the retirement.
                    if now < deadline || m.settle_drained < TAIL_DRAIN_LIMIT {
                        match self.read_active(m, doomed)? {
                            Read::Buffer(buffer) => {
                                if now >= deadline {
                                    m.settle_drained += 1;
                                }
                                return Ok(Some(buffer));
                            }
                            Read::Empty if now < deadline => return Ok(None),
                            Read::Empty => {}
                            Read::Lost(reason) => {
                                // The old device vanished mid-settle: a
                                // premature death, charged like any other loss.
                                // The reopen still waits for the deadline so it
                                // binds the new default, not a transient one.
                                self.on_lost(m, reason, now);
                                if let Phase::Backoff { ref mut until } = m.phase {
                                    *until = (*until).max(deadline);
                                }
                                continue;
                            }
                        }
                    }
                    // Quiet period over and the tail drained: break before make.
                    self.begin_outage(m, now);
                    self.start_reopen(m, None, true);
                    return Ok(None);
                }
                Phase::Backoff { until } => {
                    if now < until {
                        return Ok(None);
                    }
                    self.start_reopen(m, None, false);
                    return Ok(None);
                }
                Phase::WaitingForDevice {
                    deadline,
                    next_probe,
                } => {
                    let arrived = self.trigger.take_device_arrived();
                    let changed = self.trigger.take_default_changed();
                    let next_probe = if arrived || changed {
                        next_probe.min(deadline_after(now, settle))
                    } else {
                        next_probe
                    };
                    m.phase = Phase::WaitingForDevice {
                        deadline,
                        next_probe,
                    };
                    // Expiry wins over a due probe, so a zero `max_backoff` or a
                    // late poll can never open a device past the wait.
                    if now >= deadline {
                        let reason = format!(
                            "auto-recovery gave up: no capture device appeared within {:?}",
                            self.hub.config.device_wait_timeout
                        );
                        self.exhaust(m, reason.clone(), reason);
                        continue;
                    }
                    if now >= next_probe {
                        self.start_reopen(m, Some(deadline), false);
                    }
                    // One probe per poll: yield instead of spinning.
                    return Ok(None);
                }
                Phase::Reopening {
                    wait_deadline,
                    uncharged,
                } => {
                    let reopened = lock(&self.core.mailbox).take();
                    let Some(reopened) = reopened else {
                        // Still in flight: never block the reader.
                        return Ok(None);
                    };
                    self.finish_reopen(m, reopened, wait_deadline, uncharged, doomed);
                    match m.phase {
                        // Keep going: read the new stream, or surface the end.
                        Phase::Running | Phase::Exhausted { .. } => continue,
                        // An unsuccessful probe always yields.
                        _ => return Ok(None),
                    }
                }
            }
        }
    }

    /// After `stop()`: serve the tail `stop()` drained, then what the stopped
    /// stream still hands out, then surface its terminal. Never opens anything.
    fn read_after_stop(
        &self,
        m: &mut Machine,
        doomed: &mut Doomed,
    ) -> AudioResult<Option<AudioBuffer>> {
        if let Some(buffer) = m.tail.pop_front() {
            return self.accept(m, buffer, doomed).map(Some);
        }
        let stopped_during_recovery = || AudioError::StreamEnded {
            reason: STOPPED_DURING_RECOVERY_REASON.to_string(),
        };
        if !matches!(m.phase, Phase::Running | Phase::Settling { .. }) {
            return Err(stopped_during_recovery());
        }
        match self.core.active_stream() {
            Some(stream) => match stream.try_read_chunk()? {
                Some(buffer) => self.accept(m, buffer, doomed).map(Some),
                None => Ok(None),
            },
            None => Err(stopped_during_recovery()),
        }
    }

    fn read_active(&self, m: &mut Machine, doomed: &mut Doomed) -> AudioResult<Read> {
        // `Running`/`Settling` always have a stream installed; an empty slot
        // reads as empty.
        let Some(stream) = self.core.active_stream() else {
            return Ok(Read::Empty);
        };
        match stream.try_read_chunk() {
            Ok(Some(buffer)) => self.accept(m, buffer, doomed).map(Read::Buffer),
            Ok(None) => Ok(Read::Empty),
            Err(e) if e.is_fatal() => Ok(Read::Lost(e.to_string())),
            // Recoverable read errors pass through unchanged, as unsupervised.
            Err(e) => Err(e),
        }
    }

    /// Runtime format gate plus timeline placement for one delivered buffer.
    /// The first buffer delivered establishes the authoritative baseline; every
    /// buffer of a replacement generation must then match it. A mismatch stops
    /// the replacement and ends the session *before* the buffer is returned.
    fn accept(
        &self,
        m: &mut Machine,
        buffer: AudioBuffer,
        doomed: &mut Doomed,
    ) -> AudioResult<AudioBuffer> {
        let observed = (buffer.sample_rate(), buffer.channels());
        match m.gate.authoritative {
            None => m.gate.authoritative = Some(observed),
            Some(expected) if m.generation > 0 && expected != observed => {
                let detail = format!(
                    "{FORMAT_CHANGED_REASON}: negotiated {expected:?} (rate, channels), \
                     generation {} delivered {observed:?}",
                    m.generation
                );
                doomed.extend(self.core.detach_active());
                self.exhaust(m, detail, FORMAT_CHANGED_REASON.to_string());
                return Err(AudioError::StreamEnded {
                    reason: FORMAT_CHANGED_REASON.to_string(),
                });
            }
            Some(_) => {}
        }
        Ok(m.stamp(buffer))
    }

    fn budget_reset_due(&self, m: &Machine, now: Instant) -> bool {
        m.healthy_since.is_some_and(|since| {
            now.saturating_duration_since(since) >= self.hub.config.stable_period
        })
    }

    /// The installed stream reached a fatal terminal: schedule a retry,
    /// spending budget unless it had run healthy for `stable_period`. The dead
    /// stream is retired by the reopen helper right before the open.
    fn on_lost(&self, m: &mut Machine, reason: String, now: Instant) {
        log::warn!(
            "auto-recovery: generation {} stream invalidated: {reason}",
            m.generation
        );
        self.emit(RecoveryEvent::DeviceInvalidated {
            generation: m.generation,
            reason: reason.clone(),
        });
        if self.budget_reset_due(m, now) {
            m.attempt = 0;
        }
        self.begin_outage(m, now);
        self.schedule_retry(m, reason, now);
    }

    /// Opens the outage window. The earliest start is kept until a replacement
    /// actually delivers audio.
    fn begin_outage(&self, m: &mut Machine, now: Instant) {
        m.outage_start.get_or_insert(now);
        m.healthy_since = None;
        self.publish(
            RecoveryState::Recovering { attempt: m.attempt },
            m.generation,
            m.attempt,
        );
    }

    fn schedule_retry(&self, m: &mut Machine, reason: String, now: Instant) {
        let attempt = m.attempt.saturating_add(1);
        if attempt > self.hub.config.max_attempts {
            let reason = format!(
                "auto-recovery exhausted after {} failed attempt(s): {reason}",
                m.attempt
            );
            self.exhaust(m, reason.clone(), reason);
            return;
        }
        m.attempt = attempt;
        let delay = self.hub.config.backoff_for_attempt(attempt);
        m.phase = Phase::Backoff {
            until: deadline_after(now, delay),
        };
        log::warn!("auto-recovery: reopen attempt {attempt} in {delay:?} ({reason})");
        self.publish(RecoveryState::Recovering { attempt }, m.generation, attempt);
        self.emit(RecoveryEvent::RetryScheduled {
            generation: m.generation,
            attempt,
            delay,
        });
    }

    /// Starts the single in-flight reopen on a helper thread and enters
    /// `Reopening`. Only ever called from a phase with no open in flight.
    fn start_reopen(&self, m: &mut Machine, wait_deadline: Option<Instant>, uncharged: bool) {
        // The open binds whatever the default is now, so a change noted before
        // it started needs no second switch; later ones are kept.
        self.trigger.take_default_changed();
        m.phase = Phase::Reopening {
            wait_deadline,
            uncharged,
        };
        let core = Arc::clone(&self.core);
        let gate = m.gate;
        let spawned = std::thread::Builder::new()
            .name("rsac-recovery-open".into())
            .spawn(move || core.run_reopen(gate));
        if let Err(e) = spawned {
            *lock(&self.core.mailbox) = Some(Reopened {
                outcome: Outcome::Failed(AudioError::InternalError {
                    message: format!("auto-recovery: cannot spawn the reopen helper: {e}"),
                    source: None,
                }),
                finished_at: Instant::now(),
            });
        }
    }

    /// Applies a collected reopen result to the state machine.
    fn finish_reopen(
        &self,
        m: &mut Machine,
        reopened: Reopened,
        wait_deadline: Option<Instant>,
        uncharged: bool,
        doomed: &mut Doomed,
    ) {
        let Reopened {
            outcome,
            finished_at: now,
        } = reopened;
        match outcome {
            Outcome::Opened(stream) => self.adopt(m, stream, now, doomed),
            Outcome::FormatMismatch { detail } => {
                self.exhaust(m, detail, FORMAT_CHANGED_REASON.to_string());
            }
            Outcome::Failed(AudioError::DeviceNotFound { device_id }) => {
                // Nothing to open yet: wait for a device without spending
                // budget, re-probing on device events and every `max_backoff`.
                log::info!("auto-recovery: no capture device present ({device_id}); waiting");
                let deadline = wait_deadline
                    .unwrap_or_else(|| deadline_after(now, self.hub.config.device_wait_timeout));
                m.phase = Phase::WaitingForDevice {
                    deadline,
                    next_probe: deadline_after(now, self.hub.config.max_backoff),
                };
            }
            Outcome::Failed(error) if is_non_retryable(&error) => {
                // Retrying a permission, platform, or configuration failure
                // cannot succeed: end recovery now.
                let reason = format!(
                    "auto-recovery gave up: reopen failed with a non-retryable error: {error}"
                );
                self.exhaust(m, reason.clone(), reason);
            }
            Outcome::Failed(error) => {
                if uncharged {
                    // A failed proactive open is a real failed attempt.
                    m.attempt = m.attempt.saturating_add(1);
                }
                self.schedule_retry(m, error.to_string(), now);
            }
        }
    }

    /// Installs a freshly opened replacement as the next generation.
    fn adopt(
        &self,
        m: &mut Machine,
        stream: Box<dyn CapturingStream>,
        now: Instant,
        doomed: &mut Doomed,
    ) {
        if !self.core.install(Arc::from(stream), doomed) {
            return;
        }
        // The outage runs from the first loss until now, even across
        // generations that died before delivering any audio.
        let outage = m
            .outage_start
            .map_or(Duration::ZERO, |start| now.saturating_duration_since(start));
        m.base_offset = m.session_pos.saturating_add(outage);
        m.generation += 1;
        m.healthy_since = Some(now);
        m.phase = Phase::Running;
        log::info!(
            "auto-recovery: generation {} running after a {outage:?} outage",
            m.generation
        );
        self.hub
            .record_recovery(self.session, m.generation, m.attempt);
        self.emit(RecoveryEvent::Recovered {
            generation: m.generation,
            outage,
        });
    }

    fn exhaust(&self, m: &mut Machine, detail: String, terminal_reason: String) {
        log::error!(
            "auto-recovery: giving up at generation {}: {detail}",
            m.generation
        );
        m.phase = Phase::Exhausted {
            reason: terminal_reason,
        };
        self.terminal.store(true, Ordering::Release);
        self.publish(RecoveryState::Exhausted, m.generation, m.attempt);
        self.emit(RecoveryEvent::Exhausted {
            generation: m.generation,
            attempts: m.attempt,
            reason: detail,
        });
        // The watcher is released by the caller after the lock is dropped.
    }

    /// Moves the installed stream's already-queued buffers into the machine's
    /// tail before `stop()` halts it, so a backend that rejects reads once
    /// stopped cannot swallow them. Bounded by [`TAIL_DRAIN_LIMIT`].
    fn drain_tail_for_stop(&self) {
        let mut m = lock(&self.machine);
        if !matches!(m.phase, Phase::Running | Phase::Settling { .. }) {
            return;
        }
        let Some(stream) = self.core.active_stream() else {
            return;
        };
        while m.tail.len() < TAIL_DRAIN_LIMIT {
            match stream.try_read_chunk() {
                Ok(Some(buffer)) => m.tail.push_back(buffer),
                Ok(None) | Err(_) => break,
            }
        }
    }

    /// Drops the device-change subscription (joining its notify thread) with
    /// no supervisor lock held.
    fn release_watcher(&self) {
        let watcher = lock(&self.watcher).take();
        drop(watcher);
    }
}

impl CapturingStream for RecoveryStream {
    /// Blocks by polling [`try_read_chunk`](Self::try_read_chunk) every
    /// millisecond, so recovery deadlines and `stop()` stay observable while a
    /// reader is parked here.
    fn read_chunk(&self) -> AudioResult<AudioBuffer> {
        loop {
            if let Some(buffer) = self.try_read_chunk()? {
                return Ok(buffer);
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    fn try_read_chunk(&self) -> AudioResult<Option<AudioBuffer>> {
        let mut doomed = Doomed::new();
        let result = {
            let mut machine = lock(&self.machine);
            self.advance(&mut machine, &mut doomed)
        };
        // Disposal and teardown run with no supervisor lock held.
        for stream in doomed {
            if let Err(e) = stream.stop() {
                log::debug!("auto-recovery: stopping a discarded stream failed: {e}");
            }
        }
        if self.terminal.load(Ordering::Acquire) {
            self.release_watcher();
        }
        result
    }

    /// Sticky stop: aborts any backoff, settle, device wait, or in-flight open
    /// at the next poll, stops the installed stream (its queued tail stays
    /// drainable), and guarantees no replacement is installed afterwards. Never
    /// waits for a reopen helper or for a reader.
    fn stop(&self) -> AudioResult<()> {
        self.core.stop_requested.store(true, Ordering::SeqCst);
        self.terminal.store(true, Ordering::Release);
        self.hub.mark_stopped(self.session);
        // A result the helper posted before it saw the flag is ours to dispose.
        let late = lock(&self.core.mailbox).take();
        if let Some(Reopened { outcome, .. }) = late {
            dispose(outcome);
        }
        self.drain_tail_for_stop();
        self.release_watcher();
        let result = match self.core.active_stream() {
            Some(stream) => stream.stop(),
            None => Ok(()),
        };
        // Last, and with no lock held: the helper's reentrant wake may call
        // `stop()` again.
        #[cfg(feature = "async-stream")]
        self.poll_waker.shutdown();
        result
    }

    fn format(&self) -> AudioFormat {
        self.core
            .active_stream()
            .map_or_else(|| self.expected_format.clone(), |stream| stream.format())
    }

    /// `true` until stopped or exhausted — including while recovering, so the
    /// pull API keeps polling (and thereby driving) the supervisor.
    fn is_running(&self) -> bool {
        !self.terminal.load(Ordering::Acquire)
    }

    fn overrun_count(&self) -> u64 {
        self.cumulative(|r| r.overruns, |s| s.overrun_count())
    }

    fn buffers_captured(&self) -> u64 {
        self.cumulative(|r| r.captured, |s| s.buffers_captured())
    }

    fn buffers_pushed(&self) -> u64 {
        self.cumulative(|r| r.pushed, |s| s.buffers_pushed())
    }

    fn buffers_dropped(&self) -> u64 {
        self.cumulative(|r| r.dropped, |s| s.buffers_dropped())
    }

    fn is_under_backpressure(&self) -> bool {
        lock(&self.core.slot)
            .stream
            .as_deref()
            .is_some_and(|s| s.is_under_backpressure())
    }

    fn drop_window_snapshot(&self) -> (u64, u64) {
        lock(&self.core.slot)
            .stream
            .as_deref()
            .map_or((0, 0), |s| s.drop_window_snapshot())
    }

    /// Always wakes the task after one poll interval instead of delegating to
    /// the installed stream: a backend waker cannot report settle deadlines,
    /// backoff expiry, or device notifications, and there is no backend stream
    /// at all during an outage.
    #[cfg(feature = "async-stream")]
    fn register_waker(&self, waker: &std::task::Waker) -> bool {
        self.poll_waker.wake_soon(waker)
    }

    #[cfg(feature = "async-stream")]
    fn is_stream_producing(&self) -> bool {
        self.is_running()
    }
}

// ── Async poll waker ─────────────────────────────────────────────────────

/// Wakes a parked async consumer after [`POLL_INTERVAL`], giving
/// `AsyncAudioStream` the same ~1 ms poll cadence the synchronous pumps use.
/// The helper thread starts on first use and parks while no task is waiting.
///
/// No mutex guard is ever held while a waker is invoked, dropped, or while the
/// helper is joined: a user waker may re-enter the capture (e.g.
/// `request_stop()`), and that must not deadlock against shutdown.
#[cfg(feature = "async-stream")]
#[derive(Default)]
struct PollWaker {
    shared: Arc<PollWakerShared>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

#[cfg(feature = "async-stream")]
#[derive(Default)]
struct PollWakerShared {
    pending: Mutex<Option<std::task::Waker>>,
    shutdown: AtomicBool,
}

#[cfg(feature = "async-stream")]
impl PollWakerShared {
    /// Takes the parked waker; the guard is released before the caller uses it.
    fn take_pending(&self) -> Option<std::task::Waker> {
        lock(&self.pending).take()
    }

    /// Parks `waker`, handing back the one it displaced for the caller to drop
    /// outside the lock.
    fn park(&self, waker: std::task::Waker) -> Option<std::task::Waker> {
        lock(&self.pending).replace(waker)
    }
}

#[cfg(feature = "async-stream")]
impl PollWaker {
    fn wake_soon(&self, waker: &std::task::Waker) -> bool {
        if self.shared.shutdown.load(Ordering::Acquire) {
            waker.wake_by_ref();
            return true;
        }
        let displaced = self.shared.park(waker.clone());
        drop(displaced);
        let mut thread = lock(&self.thread);
        if let Some(handle) = thread.as_ref() {
            handle.thread().unpark();
            return true;
        }
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name("rsac-recovery-wake".into())
            .spawn(move || {
                while !shared.shutdown.load(Ordering::Acquire) {
                    match shared.take_pending() {
                        Some(waker) => {
                            std::thread::sleep(POLL_INTERVAL);
                            waker.wake();
                        }
                        None => std::thread::park(),
                    }
                }
                // A waker parked while shutting down must still be woken.
                if let Some(waker) = shared.take_pending() {
                    waker.wake();
                }
            });
        match spawned {
            Ok(handle) => {
                *thread = Some(handle);
                true
            }
            Err(e) => {
                log::warn!("auto-recovery: cannot spawn the async poll waker: {e}");
                drop(thread);
                drop(self.shared.take_pending());
                false
            }
        }
    }

    fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::Release);
        // Take the handle in its own lock scope: the helper may be inside a
        // user waker that re-enters `shutdown()`, and must find `thread`
        // unlocked while it is joined.
        let handle = lock(&self.thread).take();
        if let Some(handle) = handle {
            handle.thread().unpark();
            if handle.thread().id() != std::thread::current().id() {
                let _ = handle.join();
            }
        }
        if let Some(waker) = self.shared.take_pending() {
            waker.wake();
        }
    }
}

#[cfg(feature = "async-stream")]
impl Drop for PollWaker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{AudioCapture, AudioCaptureBuilder};
    use crate::core::config::{AudioCaptureConfig, SampleFormat};
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;

    const RATE: u32 = 48_000;
    /// 10 ms of audio at [`RATE`].
    const FRAMES: usize = 480;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn stereo(sample_rate: u32) -> AudioFormat {
        AudioFormat {
            sample_rate,
            channels: 2,
            sample_format: SampleFormat::F32,
        }
    }

    /// A 10 ms stereo buffer stamped `at_ms` after its own stream started.
    fn buffer(at_ms: u64) -> AudioBuffer {
        AudioBuffer::with_timestamp(vec![0.25; FRAMES * 2], stereo(RATE), ms(at_ms))
    }

    fn busy() -> AudioError {
        AudioError::StreamCreationFailed {
            reason: "endpoint busy".to_string(),
            context: None,
        }
    }

    /// Policy with millisecond timings; `stable_period` keeps its 60 s default
    /// so no test resets the budget by accident.
    fn fast_policy() -> AutoRecoveryConfig {
        AutoRecoveryConfig::new()
            .with_initial_backoff(ms(5))
            .with_max_backoff(ms(40))
            .with_settle_delay(ms(5))
            .with_device_wait_timeout(ms(500))
    }

    enum Step {
        Buffer(AudioBuffer),
        /// Fatal terminal: this and every later read returns `StreamEnded`.
        End(&'static str),
    }

    #[derive(Default)]
    struct MockState {
        script: Mutex<VecDeque<Step>>,
        ended: Mutex<Option<String>>,
        stopped: AtomicBool,
        /// Reject reads once stopped even with steps still queued, like the
        /// production bridge (which marks a stopped stream non-readable).
        strict_stop: AtomicBool,
        delivered: AtomicUsize,
        overruns: AtomicU64,
    }

    /// Scripted backend stream: serves its steps in order, then stays live and
    /// empty (or ends, once stopped and drained).
    #[derive(Clone)]
    struct MockStream {
        format: AudioFormat,
        state: Arc<MockState>,
    }

    impl MockStream {
        fn new(format: AudioFormat, steps: Vec<Step>) -> Self {
            let state = MockState {
                script: Mutex::new(steps.into()),
                ..MockState::default()
            };
            Self {
                format,
                state: Arc::new(state),
            }
        }

        fn with_overruns(self, overruns: u64) -> Self {
            self.state.overruns.store(overruns, Ordering::SeqCst);
            self
        }

        /// Mirrors a backend that rejects reads as soon as it is stopped, even
        /// with buffers still queued.
        fn strict_stop(self) -> Self {
            self.state.strict_stop.store(true, Ordering::SeqCst);
            self
        }

        /// Ends the stream as if its device vanished.
        fn kill(&self, reason: &str) {
            *lock(&self.state.ended) = Some(reason.to_string());
        }

        fn stopped(&self) -> bool {
            self.state.stopped.load(Ordering::SeqCst)
        }

        fn delivered(&self) -> usize {
            self.state.delivered.load(Ordering::SeqCst)
        }

        fn boxed(&self) -> Box<dyn CapturingStream> {
            Box::new(self.clone())
        }
    }

    impl CapturingStream for MockStream {
        fn read_chunk(&self) -> AudioResult<AudioBuffer> {
            loop {
                if let Some(buffer) = self.try_read_chunk()? {
                    return Ok(buffer);
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        }

        fn try_read_chunk(&self) -> AudioResult<Option<AudioBuffer>> {
            if let Some(reason) = lock(&self.state.ended).clone() {
                return Err(AudioError::StreamEnded { reason });
            }
            if self.state.strict_stop.load(Ordering::SeqCst) && self.stopped() {
                return Err(AudioError::StreamEnded {
                    reason: "mock stopped".to_string(),
                });
            }
            match lock(&self.state.script).pop_front() {
                Some(Step::Buffer(buffer)) => {
                    self.state.delivered.fetch_add(1, Ordering::SeqCst);
                    Ok(Some(buffer))
                }
                Some(Step::End(reason)) => {
                    *lock(&self.state.ended) = Some(reason.to_string());
                    Err(AudioError::StreamEnded {
                        reason: reason.to_string(),
                    })
                }
                None if self.stopped() => Err(AudioError::StreamEnded {
                    reason: "mock stopped".to_string(),
                }),
                None => Ok(None),
            }
        }

        fn stop(&self) -> AudioResult<()> {
            self.state.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn format(&self) -> AudioFormat {
            self.format.clone()
        }

        fn is_running(&self) -> bool {
            !self.stopped() && lock(&self.state.ended).is_none()
        }

        fn overrun_count(&self) -> u64 {
            self.state.overruns.load(Ordering::SeqCst)
        }
    }

    /// Scripted replacement opener: one result per call, counting calls.
    struct Opener {
        script: Mutex<VecDeque<AudioResult<MockStream>>>,
        calls: AtomicUsize,
    }

    impl Opener {
        fn new(script: Vec<AudioResult<MockStream>>) -> Arc<Self> {
            Arc::new(Self {
                script: Mutex::new(script.into()),
                calls: AtomicUsize::new(0),
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn factory(self: &Arc<Self>) -> StreamFactory {
            let opener = Arc::clone(self);
            Box::new(move || {
                opener.calls.fetch_add(1, Ordering::SeqCst);
                match lock(&opener.script).pop_front() {
                    Some(result) => result.map(|stream| stream.boxed()),
                    None => Err(AudioError::StreamCreationFailed {
                        reason: "opener script exhausted".to_string(),
                        context: None,
                    }),
                }
            })
        }
    }

    struct Harness {
        stream: Arc<RecoveryStream>,
        hub: Arc<RecoveryHub>,
        trigger: Arc<RecoveryTrigger>,
        events: mpsc::Receiver<RecoveryEvent>,
    }

    impl Harness {
        fn new(policy: AutoRecoveryConfig, initial: &MockStream, factory: StreamFactory) -> Self {
            Self::with_trigger(policy, initial, factory, RecoveryTrigger::default())
        }

        fn with_trigger(
            policy: AutoRecoveryConfig,
            initial: &MockStream,
            factory: StreamFactory,
            trigger: RecoveryTrigger,
        ) -> Self {
            let hub = Arc::new(RecoveryHub::new(policy));
            let events = hub.subscribe().expect("first subscriber");
            let trigger = Arc::new(trigger);
            let stream = Arc::new(RecoveryStream::new(
                initial.boxed(),
                factory,
                Arc::clone(&trigger),
                None,
                Arc::clone(&hub),
            ));
            Self {
                stream,
                hub,
                trigger,
                events,
            }
        }

        fn events(&self) -> Vec<RecoveryEvent> {
            self.events.try_iter().collect()
        }
    }

    /// Polls until `count` buffers arrive; panics on an error or after 5 s.
    fn read_n(stream: &RecoveryStream, count: usize) -> Vec<AudioBuffer> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut buffers = Vec::new();
        while buffers.len() < count {
            assert!(
                Instant::now() < deadline,
                "timed out after {} of {count} buffers",
                buffers.len()
            );
            match stream.try_read_chunk() {
                Ok(Some(buffer)) => buffers.push(buffer),
                Ok(None) => std::thread::sleep(POLL_INTERVAL),
                Err(e) => panic!("unexpected error after {} buffers: {e}", buffers.len()),
            }
        }
        buffers
    }

    /// Polls until the stream errors, returning what it delivered first.
    fn read_until_err(stream: &RecoveryStream) -> (Vec<AudioBuffer>, AudioError) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut buffers = Vec::new();
        loop {
            assert!(Instant::now() < deadline, "stream never errored");
            match stream.try_read_chunk() {
                Ok(Some(buffer)) => buffers.push(buffer),
                Ok(None) => std::thread::sleep(POLL_INTERVAL),
                Err(e) => return (buffers, e),
            }
        }
    }

    fn wait_for(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "condition not met within 5 s");
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    #[test]
    fn default_changed_reopens_without_budget_and_keeps_a_monotonic_timeline() {
        let old = MockStream::new(
            stereo(RATE),
            vec![
                Step::Buffer(buffer(0)),
                Step::Buffer(buffer(10)),
                Step::Buffer(buffer(20)),
            ],
        )
        .with_overruns(2);
        let new = MockStream::new(
            stereo(RATE),
            vec![Step::Buffer(buffer(0)), Step::Buffer(buffer(10))],
        )
        .with_overruns(3);
        let old_at_open = old.clone();
        let released_before_open = Arc::new(AtomicBool::new(false));
        let released = Arc::clone(&released_before_open);
        let replacement = Mutex::new(Some(new.clone()));
        let factory: StreamFactory = Box::new(move || {
            released.store(old_at_open.stopped(), Ordering::SeqCst);
            Ok(lock(&replacement).take().expect("a single reopen").boxed())
        });
        let h = Harness::new(fast_policy(), &old, factory);

        let before = read_n(&h.stream, 3);
        // A burst of notifications coalesces into one reopen.
        for _ in 0..3 {
            h.trigger.trigger_default_change();
        }
        let after = read_n(&h.stream, 2);

        assert!(
            released_before_open.load(Ordering::SeqCst),
            "break-before-make: the old stream is stopped before the reopen"
        );
        let outage = match h.events().as_slice() {
            [RecoveryEvent::DefaultChanged { generation: 0 }, RecoveryEvent::Recovered {
                generation: 1,
                outage,
            }] => *outage,
            other => panic!("unexpected events: {other:?}"),
        };
        for (i, buffer) in before.iter().enumerate() {
            assert_eq!(buffer.stream_generation(), 0);
            assert_eq!(buffer.timestamp(), Some(ms(10 * i as u64)));
        }
        let resume = ms(30) + outage;
        assert_eq!(after[0].timestamp(), Some(resume), "last end + outage");
        assert_eq!(after[1].timestamp(), Some(resume + ms(10)));
        assert!(after.iter().all(|b| b.stream_generation() == 1));
        let stamps: Vec<Duration> = before
            .iter()
            .chain(&after)
            .map(|b| b.timestamp().expect("stamped"))
            .collect();
        assert!(
            stamps.windows(2).all(|pair| pair[0] < pair[1]),
            "timeline must stay monotonic: {stamps:?}"
        );

        let snapshot = h.hub.snapshot();
        assert_eq!(snapshot.state, RecoveryState::Running);
        assert_eq!(snapshot.generation, 1);
        assert_eq!(snapshot.attempt, 0, "a proactive reopen spends no budget");
        assert_eq!(snapshot.successful_recoveries, 1);
        assert_eq!(
            h.stream.overrun_count(),
            5,
            "overruns accumulate across generations"
        );
    }

    #[test]
    fn invalidation_backs_off_exponentially_and_recovers_on_the_third_attempt() {
        let policy = fast_policy();
        let old = MockStream::new(
            stereo(RATE),
            vec![
                Step::Buffer(buffer(0)),
                Step::Buffer(buffer(10)),
                Step::End("device unplugged"),
            ],
        );
        let new = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let opener = Opener::new(vec![Err(busy()), Err(busy()), Ok(new)]);
        let h = Harness::new(policy.clone(), &old, opener.factory());

        let buffers = read_n(&h.stream, 3);

        assert_eq!(opener.calls(), 3);
        match h.events().as_slice() {
            [RecoveryEvent::DeviceInvalidated {
                generation: 0,
                reason,
            }, RecoveryEvent::RetryScheduled {
                generation: 0,
                attempt: 1,
                delay: d1,
            }, RecoveryEvent::RetryScheduled {
                generation: 0,
                attempt: 2,
                delay: d2,
            }, RecoveryEvent::RetryScheduled {
                generation: 0,
                attempt: 3,
                delay: d3,
            }, RecoveryEvent::Recovered {
                generation: 1,
                outage,
            }] => {
                assert!(reason.contains("device unplugged"), "{reason}");
                assert_eq!(*d1, policy.backoff_for_attempt(1));
                assert_eq!(*d2, *d1 * 2);
                assert_eq!(*d3, *d1 * 4);
                assert!(
                    *outage >= *d1 + *d2 + *d3,
                    "outage {outage:?} must cover every backoff"
                );
                assert_eq!(buffers[2].timestamp(), Some(ms(20) + *outage));
            }
            other => panic!("unexpected events: {other:?}"),
        }
        assert!(buffers[..2].iter().all(|b| b.stream_generation() == 0));
        assert_eq!(buffers[2].stream_generation(), 1);
        let snapshot = h.hub.snapshot();
        assert_eq!(snapshot.state, RecoveryState::Running);
        assert_eq!(snapshot.generation, 1);
        assert_eq!(
            snapshot.attempt, 3,
            "the budget resets only after the stable period"
        );
        assert_eq!(snapshot.successful_recoveries, 1);
    }

    #[test]
    fn exhausted_budget_surfaces_a_fatal_stream_ended() {
        let old = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
        let spare = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let opener = Opener::new(vec![Err(busy()), Err(busy()), Ok(spare)]);
        let h = Harness::new(fast_policy().with_max_attempts(2), &old, opener.factory());

        let (buffers, err) = read_until_err(&h.stream);

        assert!(buffers.is_empty());
        assert!(err.is_fatal());
        match &err {
            AudioError::StreamEnded { reason } => assert!(reason.contains("exhausted"), "{reason}"),
            other => panic!("expected StreamEnded, got {other:?}"),
        }
        assert_eq!(opener.calls(), 2, "no open beyond max_attempts");
        match h.events().as_slice() {
            [RecoveryEvent::DeviceInvalidated { generation: 0, .. }, RecoveryEvent::RetryScheduled { attempt: 1, .. }, RecoveryEvent::RetryScheduled { attempt: 2, .. }, RecoveryEvent::Exhausted {
                generation: 0,
                attempts: 2,
                reason,
            }] => assert!(reason.contains("endpoint busy"), "{reason}"),
            other => panic!("unexpected events: {other:?}"),
        }
        assert_eq!(h.hub.snapshot().state, RecoveryState::Exhausted);
        assert!(!h.stream.is_running());
        assert!(
            matches!(
                h.stream.try_read_chunk(),
                Err(AudioError::StreamEnded { .. })
            ),
            "the terminal is sticky"
        );
    }

    #[test]
    fn a_replacement_in_another_format_ends_the_stream_before_any_sample() {
        let old = MockStream::new(
            stereo(RATE),
            vec![Step::Buffer(buffer(0)), Step::End("device unplugged")],
        );
        let foreign = AudioBuffer::with_timestamp(vec![0.5; 441 * 2], stereo(44_100), ms(0));
        let new = MockStream::new(stereo(44_100), vec![Step::Buffer(foreign)]);
        let opener = Opener::new(vec![Ok(new.clone())]);
        let h = Harness::new(fast_policy(), &old, opener.factory());

        let (buffers, err) = read_until_err(&h.stream);

        assert_eq!(buffers.len(), 1);
        assert!(buffers.iter().all(|b| b.sample_rate() == RATE));
        match err {
            AudioError::StreamEnded { reason } => {
                assert_eq!(reason, "reconnected device changed format")
            }
            other => panic!("expected StreamEnded, got {other:?}"),
        }
        assert!(new.stopped(), "the mismatched replacement is stopped");
        assert_eq!(new.delivered(), 0, "no sample of the new format was read");
        match h.events().as_slice() {
            [RecoveryEvent::DeviceInvalidated { .. }, RecoveryEvent::RetryScheduled { attempt: 1, .. }, RecoveryEvent::Exhausted {
                generation: 0,
                reason,
                ..
            }] => assert!(reason.contains(FORMAT_CHANGED_REASON), "{reason}"),
            other => panic!("unexpected events: {other:?}"),
        }
        let snapshot = h.hub.snapshot();
        assert_eq!(snapshot.state, RecoveryState::Exhausted);
        assert_eq!(snapshot.successful_recoveries, 0);
    }

    #[test]
    fn stop_during_backoff_returns_promptly_and_never_reopens() {
        let policy = fast_policy()
            .with_initial_backoff(Duration::from_secs(30))
            .with_max_backoff(Duration::from_secs(30));
        let old = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
        let spare = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let opener = Opener::new(vec![Ok(spare)]);
        let h = Harness::new(policy, &old, opener.factory());
        let reader_stream = Arc::clone(&h.stream);
        let reader = std::thread::spawn(move || reader_stream.read_chunk());

        wait_for(|| {
            matches!(
                h.hub.snapshot().state,
                RecoveryState::Recovering { attempt: 1 }
            )
        });
        let stopped_at = Instant::now();
        h.stream.stop().expect("stop");
        let result = reader.join().expect("reader thread");

        assert!(
            stopped_at.elapsed() < Duration::from_secs(2),
            "stop must abort the 30 s backoff"
        );
        assert!(
            matches!(result, Err(AudioError::StreamEnded { .. })),
            "{result:?}"
        );
        std::thread::sleep(ms(20));
        assert!(h.stream.try_read_chunk().is_err());
        assert_eq!(opener.calls(), 0, "no reopen after stop");
        let snapshot = h.hub.snapshot();
        assert_eq!(snapshot.state, RecoveryState::Stopped);
        assert_eq!(snapshot.generation, 0);
        assert!(!h.stream.is_running());
        assert!(!h
            .events()
            .iter()
            .any(|e| matches!(e, RecoveryEvent::Recovered { .. })));
    }

    #[test]
    fn stop_during_an_in_flight_open_never_installs_the_late_stream() {
        let old = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
        let late = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let gate = Mutex::new((entered_tx, release_rx));
        let late_for_open = late.clone();
        let factory: StreamFactory = Box::new(move || {
            let gate = lock(&gate);
            gate.0.send(()).expect("test is waiting");
            gate.1.recv().expect("test releases the open");
            Ok(late_for_open.boxed())
        });
        let h = Harness::new(fast_policy(), &old, factory);
        let reader_stream = Arc::clone(&h.stream);
        let reader = std::thread::spawn(move || read_until_err(&reader_stream));

        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the reopen started");
        h.stream.stop().expect("stop");
        release_tx.send(()).expect("the open is waiting");
        let (buffers, err) = reader.join().expect("reader thread");

        assert!(buffers.is_empty(), "the late stream never delivers");
        assert!(matches!(err, AudioError::StreamEnded { .. }), "{err:?}");
        // The helper disposes of the late stream once the opener is released.
        wait_for(|| late.stopped());
        assert_eq!(late.delivered(), 0);
        let snapshot = h.hub.snapshot();
        assert_eq!(snapshot.state, RecoveryState::Stopped);
        assert_eq!(
            (snapshot.generation, snapshot.successful_recoveries),
            (0, 0)
        );
        assert!(!h
            .events()
            .iter()
            .any(|e| matches!(e, RecoveryEvent::Recovered { .. })));
    }

    #[test]
    fn an_absent_device_is_awaited_without_spending_budget() {
        let policy = fast_policy()
            .with_max_backoff(Duration::from_secs(30))
            .with_device_wait_timeout(Duration::from_secs(5));
        let old = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
        let new = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let absent = AudioError::DeviceNotFound {
            device_id: "default_render".to_string(),
        };
        let opener = Opener::new(vec![Err(absent), Ok(new)]);
        let h = Harness::new(policy, &old, opener.factory());

        wait_for(|| {
            let _ = h.stream.try_read_chunk();
            opener.calls() == 1
        });
        for _ in 0..5 {
            assert!(matches!(h.stream.try_read_chunk(), Ok(None)));
            std::thread::sleep(POLL_INTERVAL);
        }
        assert_eq!(opener.calls(), 1, "no re-probe before a device event");
        h.trigger.notify_device_arrived();
        let buffers = read_n(&h.stream, 1);

        assert_eq!(buffers[0].stream_generation(), 1);
        assert_eq!(opener.calls(), 2);
        match h.events().as_slice() {
            [RecoveryEvent::DeviceInvalidated { .. }, RecoveryEvent::RetryScheduled { attempt: 1, .. }, RecoveryEvent::Recovered { generation: 1, .. }] =>
                {}
            other => panic!("waiting for a device must not schedule retries: {other:?}"),
        }
        assert_eq!(h.hub.snapshot().attempt, 1);
    }

    #[test]
    fn an_absent_device_exhausts_after_the_wait_timeout() {
        let policy = fast_policy()
            .with_max_backoff(Duration::from_secs(1))
            .with_device_wait_timeout(ms(30));
        let old = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
        let absent = AudioError::DeviceNotFound {
            device_id: "default_render".to_string(),
        };
        let opener = Opener::new(vec![Err(absent)]);
        let h = Harness::new(policy, &old, opener.factory());

        let (buffers, err) = read_until_err(&h.stream);

        assert!(buffers.is_empty());
        match err {
            AudioError::StreamEnded { reason } => {
                assert!(reason.contains("no capture device appeared"), "{reason}")
            }
            other => panic!("expected StreamEnded, got {other:?}"),
        }
        assert_eq!(opener.calls(), 1);
        match h.events().as_slice() {
            [RecoveryEvent::DeviceInvalidated { .. }, RecoveryEvent::RetryScheduled { attempt: 1, .. }, RecoveryEvent::Exhausted { attempts: 1, .. }] =>
                {}
            other => panic!("unexpected events: {other:?}"),
        }
    }

    #[test]
    fn a_healthy_stable_period_restores_the_budget() {
        let old = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
        let new = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let opener = Opener::new(vec![Ok(new)]);
        let h = Harness::new(
            fast_policy().with_stable_period(ms(20)),
            &old,
            opener.factory(),
        );

        read_n(&h.stream, 1);
        assert_eq!(
            h.hub.snapshot().attempt,
            1,
            "the recovery spent one attempt"
        );
        std::thread::sleep(ms(30));
        assert!(matches!(h.stream.try_read_chunk(), Ok(None)));
        assert_eq!(h.hub.snapshot().attempt, 0);
    }

    #[test]
    fn the_event_channel_is_bounded_drop_newest_with_a_single_subscriber() {
        let hub = RecoveryHub::new(AutoRecoveryConfig::new());
        let events = hub.subscribe().expect("first subscriber");
        assert!(matches!(
            hub.subscribe(),
            Err(AudioError::ConfigurationError { .. })
        ));
        let session = hub.begin_session();

        for generation in 0..40 {
            hub.emit(session, RecoveryEvent::DefaultChanged { generation });
        }

        let received: Vec<u64> = events
            .try_iter()
            .map(|event| match event {
                RecoveryEvent::DefaultChanged { generation } => generation,
                other => panic!("unexpected event {other:?}"),
            })
            .collect();
        assert_eq!(
            received,
            (0..32).collect::<Vec<u64>>(),
            "the first 32 events are kept; newer ones are dropped"
        );
        assert_eq!(hub.snapshot().dropped_events, 8);
    }

    fn capture_with(recovery: Option<Arc<RecoveryHub>>) -> AudioCapture {
        AudioCapture {
            config: AudioCaptureConfig {
                target: CaptureTarget::SystemDefault,
                stream_config: StreamConfig::default(),
            },
            device: None,
            stream: None,
            callback: Mutex::new(None),
            callback_pump: None,
            start_instant: None,
            subscriber_dropped: Arc::new(AtomicU64::new(0)),
            recovery,
        }
    }

    #[test]
    fn capture_recovery_accessors_require_auto_recovery() {
        let plain = capture_with(None);
        assert!(plain.recovery_snapshot().is_none());
        assert!(matches!(
            plain.subscribe_recovery_events(),
            Err(AudioError::ConfigurationError { .. })
        ));

        let hub = Arc::new(RecoveryHub::new(AutoRecoveryConfig::new()));
        let supervised = capture_with(Some(hub));
        assert_eq!(
            supervised.recovery_snapshot().map(|s| s.state),
            Some(RecoveryState::Idle)
        );
        let _events = supervised
            .subscribe_recovery_events()
            .expect("first subscriber");
        assert!(matches!(
            supervised.subscribe_recovery_events(),
            Err(AudioError::ConfigurationError { .. })
        ));
    }

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    #[test]
    fn preflight_rejects_every_target_but_system_default() {
        use crate::core::config::{ApplicationId, DeviceId, ProcessId};

        let targets = [
            CaptureTarget::Device(DeviceId("usb-mic".to_string())),
            CaptureTarget::Application(ApplicationId("1234".to_string())),
            CaptureTarget::ApplicationByName("VLC".to_string()),
            CaptureTarget::ProcessTree(ProcessId(42)),
        ];
        for target in targets {
            let builder = AudioCaptureBuilder::new()
                .with_target(target.clone())
                .with_auto_recovery(AutoRecoveryConfig::new());
            match builder.preflight() {
                Err(AudioError::ConfigurationError { message }) => {
                    assert!(message.contains("SystemDefault"), "{message}")
                }
                other => panic!("{target}: expected ConfigurationError, got {other:?}"),
            }
            assert!(
                matches!(builder.build(), Err(AudioError::ConfigurationError { .. })),
                "{target}: build() runs the same gate before any device work"
            );
        }
        let system = AudioCaptureBuilder::new().with_auto_recovery(AutoRecoveryConfig::new());
        assert!(system.preflight().is_ok());
    }

    #[cfg(any(target_os = "android", target_os = "ios"))]
    #[test]
    fn preflight_rejects_auto_recovery_on_mobile() {
        let err = AudioCaptureBuilder::new()
            .with_auto_recovery(AutoRecoveryConfig::new())
            .preflight()
            .expect_err("mobile targets are unsupported");
        assert!(
            matches!(err, AudioError::PlatformNotSupported { .. }),
            "{err:?}"
        );
    }

    // ── Regression tests: Stage 5 review findings ─────────────────────────

    fn not_found() -> AudioError {
        AudioError::DeviceNotFound {
            device_id: "default_render".to_string(),
        }
    }

    /// A 10 ms stereo buffer without a timestamp.
    fn untimed() -> AudioBuffer {
        AudioBuffer::with_format(vec![0.25; FRAMES * 2], stereo(RATE))
    }

    fn default_output_changed() -> DeviceEvent {
        DeviceEvent::DefaultChanged {
            id: crate::core::config::DeviceId("speakers".to_string()),
            kind: DeviceKind::Output,
        }
    }

    #[test]
    fn a_slow_reopen_never_blocks_readers_and_stop_wakes_them_without_the_opener() {
        let old = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
        let late = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let gate = Mutex::new((entered_tx, release_rx));
        let late_for_open = late.clone();
        let factory: StreamFactory = Box::new(move || {
            let gate = lock(&gate);
            gate.0.send(()).expect("test is waiting");
            gate.1.recv().expect("test releases the open");
            Ok(late_for_open.boxed())
        });
        let h = Harness::new(fast_policy(), &old, factory);

        // A blocking reader drives the supervisor into the wedged open and parks.
        let (parked_tx, parked_rx) = mpsc::channel();
        let parked_stream = Arc::clone(&h.stream);
        std::thread::spawn(move || {
            let _ = parked_tx.send(parked_stream.read_chunk());
        });
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the reopen started");

        // While the open is wedged, a poll from any other thread returns at once.
        let (polled_tx, polled_rx) = mpsc::channel();
        let polling_stream = Arc::clone(&h.stream);
        std::thread::spawn(move || {
            let all_empty = (0..50).all(|_| matches!(polling_stream.try_read_chunk(), Ok(None)));
            let _ = polled_tx.send(all_empty);
        });
        let all_empty = polled_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a poll must not wait for the wedged open");
        assert!(
            all_empty,
            "every poll returns Ok(None) while an open is in flight"
        );

        // stop() wakes the parked reader although the opener is still wedged.
        let stopped_at = Instant::now();
        h.stream.stop().expect("stop");
        let woken = parked_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("stop must wake the parked reader without waiting for the opener");
        assert!(
            matches!(woken, Err(AudioError::StreamEnded { .. })),
            "{woken:?}"
        );
        assert!(stopped_at.elapsed() < Duration::from_secs(2));
        assert!(!late.stopped(), "the opener has not even returned yet");

        // Releasing the opener afterwards: the helper disposes of the late stream.
        release_tx.send(()).expect("the open is waiting");
        wait_for(|| late.stopped());
        assert_eq!(late.delivered(), 0);
        let snapshot = h.hub.snapshot();
        assert_eq!(snapshot.state, RecoveryState::Stopped);
        assert_eq!(snapshot.successful_recoveries, 0);
    }

    #[cfg(feature = "async-stream")]
    #[test]
    fn a_reentrant_waker_calling_stop_cannot_deadlock_the_wake_helper() {
        use std::task::{Wake, Waker};

        /// A waker that re-enters the capture: it stops the stream from inside
        /// `wake()`, after waiting until a concurrent stopper is joining the
        /// helper that is running it.
        struct StopOnWake {
            stream: Mutex<Option<Arc<RecoveryStream>>>,
            entered: Mutex<mpsc::Sender<()>>,
            release: Mutex<mpsc::Receiver<()>>,
        }

        impl Wake for StopOnWake {
            fn wake(self: Arc<Self>) {
                self.wake_by_ref();
            }

            fn wake_by_ref(self: &Arc<Self>) {
                let _ = lock(&self.entered).send(());
                let _ = lock(&self.release).recv_timeout(Duration::from_secs(5));
                let stream = lock(&self.stream).take();
                if let Some(stream) = stream {
                    stream.stop().expect("reentrant stop");
                }
            }
        }

        let old = MockStream::new(stereo(RATE), vec![]);
        let h = Harness::new(fast_policy(), &old, Opener::new(vec![]).factory());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let wake = Arc::new(StopOnWake {
            stream: Mutex::new(Some(Arc::clone(&h.stream))),
            entered: Mutex::new(entered_tx),
            release: Mutex::new(release_rx),
        });
        let waker = Waker::from(Arc::clone(&wake));
        assert!(h.stream.register_waker(&waker));
        drop(waker);
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the helper is inside the waker");

        // A concurrent stopper joins the helper while the waker is mid-flight.
        let stopper_stream = Arc::clone(&h.stream);
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            stopper_stream.stop().expect("stop");
            let _ = done_tx.send(());
        });
        std::thread::sleep(ms(50));
        release_tx.send(()).expect("the waker is waiting");

        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("stop() must not deadlock against a waker that re-enters stop()");
        assert!(!h.stream.is_running());
    }

    #[test]
    fn a_runtime_format_mismatch_ends_the_stream_before_its_buffer_is_delivered() {
        let mono = AudioFormat {
            sample_rate: RATE,
            channels: 1,
            sample_format: SampleFormat::F32,
        };
        let foreign = vec![
            (
                "sample rate",
                AudioBuffer::with_timestamp(vec![0.5; 441 * 2], stereo(44_100), ms(0)),
            ),
            (
                "channel count",
                AudioBuffer::with_timestamp(vec![0.5; FRAMES], mono, ms(0)),
            ),
        ];
        for (what, foreign) in foreign {
            let old = MockStream::new(
                stereo(RATE),
                vec![Step::Buffer(buffer(0)), Step::End("device unplugged")],
            );
            // The replacement *reports* the baseline format (a provisional or
            // requested format that passes the open-time screen) but its first
            // delivered buffer carries something else.
            let new = MockStream::new(stereo(RATE), vec![Step::Buffer(foreign)]);
            let opener = Opener::new(vec![Ok(new.clone())]);
            let h = Harness::new(fast_policy(), &old, opener.factory());

            let (buffers, err) = read_until_err(&h.stream);

            assert_eq!(
                buffers.len(),
                1,
                "{what}: only generation 0's buffer is delivered"
            );
            match err {
                AudioError::StreamEnded { reason } => {
                    assert_eq!(reason, "reconnected device changed format", "{what}")
                }
                other => panic!("{what}: expected StreamEnded, got {other:?}"),
            }
            assert!(
                new.stopped(),
                "{what}: the mismatched replacement is stopped"
            );
            assert_eq!(
                new.delivered(),
                1,
                "{what}: read by recovery, never delivered"
            );
            match h.events().as_slice() {
                [RecoveryEvent::DeviceInvalidated { .. }, RecoveryEvent::RetryScheduled { attempt: 1, .. }, RecoveryEvent::Recovered { generation: 1, .. }, RecoveryEvent::Exhausted {
                    generation: 1,
                    reason,
                    ..
                }] => assert!(reason.contains(FORMAT_CHANGED_REASON), "{what}: {reason}"),
                other => panic!("{what}: unexpected events: {other:?}"),
            }
            assert_eq!(h.hub.snapshot().state, RecoveryState::Exhausted, "{what}");
            assert!(!h.stream.is_running(), "{what}");
            assert!(
                matches!(
                    h.stream.try_read_chunk(),
                    Err(AudioError::StreamEnded { .. })
                ),
                "{what}: the terminal is sticky"
            );
        }
    }

    #[test]
    fn a_proactive_switch_drains_the_queued_tail_before_retiring_the_old_stream() {
        let policy = fast_policy().with_settle_delay(Duration::ZERO);
        let old = MockStream::new(
            stereo(RATE),
            vec![
                Step::Buffer(buffer(0)),
                Step::Buffer(buffer(10)),
                Step::Buffer(buffer(20)),
            ],
        );
        let new = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let opener = Opener::new(vec![Ok(new)]);
        let h = Harness::new(policy, &old, opener.factory());
        h.trigger.trigger_default_change();

        let buffers = read_n(&h.stream, 4);

        let generations: Vec<u64> = buffers.iter().map(|b| b.stream_generation()).collect();
        assert_eq!(
            generations,
            vec![0, 0, 0, 1],
            "the queued tail is delivered before the switch"
        );
        assert_eq!(old.delivered(), 3);
        assert!(old.stopped(), "the drained old stream is then retired");
        assert!(buffers[3].timestamp().expect("stamped") >= ms(30));
    }

    #[test]
    fn stop_preserves_the_queued_tail_of_a_backend_that_rejects_reads_once_stopped() {
        let old = MockStream::new(
            stereo(RATE),
            vec![Step::Buffer(buffer(0)), Step::Buffer(buffer(10))],
        )
        .strict_stop();
        let opener = Opener::new(vec![]);
        let h = Harness::new(fast_policy(), &old, opener.factory());

        h.stream.stop().expect("stop");
        let (buffers, err) = read_until_err(&h.stream);

        assert!(old.stopped());
        assert_eq!(
            buffers.len(),
            2,
            "the tail queued before stop() survives it"
        );
        assert_eq!(buffers[0].timestamp(), Some(ms(0)));
        assert_eq!(buffers[1].timestamp(), Some(ms(10)));
        assert!(matches!(err, AudioError::StreamEnded { .. }), "{err:?}");
        assert_eq!(opener.calls(), 0);
    }

    #[test]
    fn a_retired_session_cannot_publish_into_a_restarted_session() {
        let hub = RecoveryHub::new(AutoRecoveryConfig::new());
        let events = hub.subscribe().expect("first subscriber");
        let retired = hub.begin_session();
        let current = hub.begin_session();
        assert!(current > retired, "session ids are monotonic");

        hub.publish(retired, RecoveryState::Recovering { attempt: 1 }, 7, 1);
        hub.record_recovery(retired, 8, 1);
        hub.emit(retired, RecoveryEvent::DefaultChanged { generation: 7 });
        hub.mark_stopped(retired);

        let snapshot = hub.snapshot();
        assert_eq!(snapshot.state, RecoveryState::Running);
        assert_eq!(
            (
                snapshot.generation,
                snapshot.attempt,
                snapshot.successful_recoveries,
                snapshot.dropped_events
            ),
            (0, 0, 0, 0)
        );
        assert!(
            events.try_recv().is_err(),
            "nothing leaked into the channel"
        );

        hub.emit(current, RecoveryEvent::DefaultChanged { generation: 0 });
        assert!(matches!(
            events.try_recv(),
            Ok(RecoveryEvent::DefaultChanged { generation: 0 })
        ));
        hub.publish(current, RecoveryState::Recovering { attempt: 1 }, 1, 1);
        assert_eq!(
            hub.snapshot().state,
            RecoveryState::Recovering { attempt: 1 }
        );
    }

    #[test]
    fn a_retired_stream_cannot_publish_into_a_restarted_streams_snapshot() {
        // Stream A is the first start; its handle is stopped and restarted, so
        // stream B takes a fresh session on the same hub while A is still polled.
        let lost = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
        let a = Harness::new(fast_policy(), &lost, Opener::new(vec![]).factory());
        let b_initial = MockStream::new(stereo(RATE), vec![]);
        let b = RecoveryStream::new(
            b_initial.boxed(),
            Opener::new(vec![]).factory(),
            Arc::new(RecoveryTrigger::default()),
            None,
            Arc::clone(&a.hub),
        );

        // A, now retired, notices its device loss.
        let _ = a.stream.try_read_chunk();

        let snapshot = a.hub.snapshot();
        assert_eq!(snapshot.state, RecoveryState::Running);
        assert_eq!((snapshot.generation, snapshot.attempt), (0, 0));
        assert!(a.events().is_empty(), "the retired stream emitted nothing");
        b.stop().expect("stop b");
        a.stream.stop().expect("stop a");
        assert_eq!(a.hub.snapshot().state, RecoveryState::Stopped);
    }

    #[test]
    fn a_zero_max_backoff_with_a_missing_device_yields_per_poll_and_exhausts_at_the_deadline() {
        let policy = fast_policy()
            .with_initial_backoff(Duration::ZERO)
            .with_max_backoff(Duration::ZERO)
            .with_device_wait_timeout(ms(40));
        let old = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
        let probes = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&probes);
        let factory: StreamFactory = Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            Err(not_found())
        });
        let h = Harness::new(policy, &old, factory);

        let started = Instant::now();
        let mut empty_polls = 0_usize;
        let err = loop {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "the wait never expired"
            );
            match h.stream.try_read_chunk() {
                Ok(None) => {
                    empty_polls += 1;
                    std::thread::sleep(POLL_INTERVAL);
                }
                Ok(Some(_)) => panic!("no device ever appeared"),
                Err(e) => break e,
            }
        };

        match err {
            AudioError::StreamEnded { reason } => {
                assert!(reason.contains("no capture device appeared"), "{reason}")
            }
            other => panic!("expected StreamEnded, got {other:?}"),
        }
        assert!(
            started.elapsed() >= ms(40),
            "expiry waits the full device_wait_timeout"
        );
        assert!(
            empty_polls > 1,
            "an unsuccessful probe yields to the caller instead of retrying in one poll"
        );
        assert!(probes.load(Ordering::SeqCst) >= 1);
        assert_eq!(h.hub.snapshot().state, RecoveryState::Exhausted);
    }

    #[test]
    fn a_non_retryable_reopen_error_exhausts_immediately_without_retrying() {
        let errors = vec![
            AudioError::PermissionDenied {
                operation: "system audio tap".to_string(),
                details: None,
            },
            AudioError::PlatformNotSupported {
                feature: "loopback capture".to_string(),
                platform: "test".to_string(),
            },
            AudioError::ConfigurationError {
                message: "bad configuration".to_string(),
            },
            AudioError::InvalidParameter {
                param: "sample_rate".to_string(),
                reason: "unsupported".to_string(),
            },
            AudioError::ApplicationNotFound {
                identifier: "pid 1".to_string(),
            },
        ];
        for error in errors {
            let label = format!("{error:?}");
            let old = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
            let spare = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
            let opener = Opener::new(vec![Err(error), Ok(spare)]);
            let h = Harness::new(fast_policy(), &old, opener.factory());

            let (buffers, err) = read_until_err(&h.stream);

            assert!(buffers.is_empty(), "{label}");
            match &err {
                AudioError::StreamEnded { reason } => {
                    assert!(reason.contains("non-retryable"), "{label}: {reason}")
                }
                other => panic!("{label}: expected StreamEnded, got {other:?}"),
            }
            assert!(err.is_fatal(), "{label}");
            assert_eq!(opener.calls(), 1, "{label}: no second open");
            match h.events().as_slice() {
                [RecoveryEvent::DeviceInvalidated { .. }, RecoveryEvent::RetryScheduled { attempt: 1, .. }, RecoveryEvent::Exhausted {
                    generation: 0,
                    attempts: 1,
                    reason,
                }] => assert!(reason.contains("non-retryable"), "{label}: {reason}"),
                other => panic!("{label}: unexpected events: {other:?}"),
            }
            assert_eq!(h.hub.snapshot().state, RecoveryState::Exhausted, "{label}");
        }
    }

    #[test]
    fn a_failed_proactive_reopen_spends_the_retry_budget() {
        let old = MockStream::new(stereo(RATE), vec![]);
        let spare = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let opener = Opener::new(vec![Err(busy()), Ok(spare)]);
        let h = Harness::new(fast_policy().with_max_attempts(1), &old, opener.factory());
        h.trigger.trigger_default_change();

        let (buffers, err) = read_until_err(&h.stream);

        assert!(buffers.is_empty());
        match &err {
            AudioError::StreamEnded { reason } => assert!(reason.contains("exhausted"), "{reason}"),
            other => panic!("expected StreamEnded, got {other:?}"),
        }
        assert_eq!(
            opener.calls(),
            1,
            "the failed proactive open used the only attempt"
        );
        match h.events().as_slice() {
            [RecoveryEvent::DefaultChanged { generation: 0 }, RecoveryEvent::Exhausted {
                generation: 0,
                attempts: 1,
                reason,
            }] => assert!(reason.contains("endpoint busy"), "{reason}"),
            other => panic!("unexpected events: {other:?}"),
        }
    }

    #[test]
    fn a_stream_dying_during_settle_is_charged_against_the_budget() {
        let policy = fast_policy().with_settle_delay(ms(60));
        let old = MockStream::new(stereo(RATE), vec![]);
        let new = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let opener = Opener::new(vec![Ok(new)]);
        let h = Harness::new(policy, &old, opener.factory());

        let started = Instant::now();
        h.trigger.trigger_default_change();
        assert!(matches!(h.stream.try_read_chunk(), Ok(None)));
        old.kill("device unplugged");
        let buffers = read_n(&h.stream, 1);

        assert_eq!(buffers[0].stream_generation(), 1);
        assert!(
            started.elapsed() >= ms(60),
            "the reopen still waits out the settle deadline"
        );
        match h.events().as_slice() {
            [RecoveryEvent::DefaultChanged { generation: 0 }, RecoveryEvent::DeviceInvalidated { generation: 0, .. }, RecoveryEvent::RetryScheduled { attempt: 1, .. }, RecoveryEvent::Recovered { generation: 1, .. }] =>
                {}
            other => panic!("unexpected events: {other:?}"),
        }
        assert_eq!(
            h.hub.snapshot().attempt,
            1,
            "a premature death spends budget"
        );
    }

    #[test]
    fn a_panicking_open_is_a_failed_attempt_not_a_hang() {
        let old = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
        let factory: StreamFactory = Box::new(|| panic!("simulated factory panic"));
        let h = Harness::new(fast_policy().with_max_attempts(1), &old, factory);

        let (buffers, err) = read_until_err(&h.stream);

        assert!(buffers.is_empty());
        match err {
            AudioError::StreamEnded { reason } => assert!(reason.contains("exhausted"), "{reason}"),
            other => panic!("expected StreamEnded, got {other:?}"),
        }
    }

    #[test]
    fn device_events_map_onto_the_platform_mode() {
        let proactive = RecoveryTrigger::new(true);
        proactive.on_device_event(&default_output_changed());
        assert!(proactive.take_default_changed());
        assert!(!proactive.take_device_arrived());

        let wait_only = RecoveryTrigger::new(false);
        wait_only.on_device_event(&default_output_changed());
        assert!(
            !wait_only.take_default_changed(),
            "wait-only mode never asks to retire a running stream"
        );
        assert!(
            wait_only.take_device_arrived(),
            "it wakes an absent-device wait instead"
        );

        // Input defaults never matter to a loopback capture, in either mode.
        let input = DeviceEvent::DefaultChanged {
            id: crate::core::config::DeviceId("mic".to_string()),
            kind: DeviceKind::Input,
        };
        proactive.on_device_event(&input);
        wait_only.on_device_event(&input);
        assert!(!proactive.take_default_changed() && !proactive.take_device_arrived());
        assert!(!wait_only.take_default_changed() && !wait_only.take_device_arrived());

        // The injection point is platform neutral.
        wait_only.trigger_default_change();
        assert!(wait_only.take_default_changed());
        assert_eq!(
            RecoveryTrigger::default().proactive_default,
            cfg!(not(target_os = "linux")),
            "only Windows and macOS switch proactively"
        );
    }

    #[test]
    fn wait_only_default_changes_never_retire_a_running_stream() {
        let old = MockStream::new(
            stereo(RATE),
            vec![Step::Buffer(buffer(0)), Step::Buffer(buffer(10))],
        );
        let opener = Opener::new(vec![]);
        let h = Harness::with_trigger(
            fast_policy(),
            &old,
            opener.factory(),
            RecoveryTrigger::new(false),
        );

        h.trigger.on_device_event(&default_output_changed());
        let buffers = read_n(&h.stream, 2);
        // Well past the settle delay: a proactive mode would have switched.
        std::thread::sleep(ms(30));
        assert!(matches!(h.stream.try_read_chunk(), Ok(None)));

        assert!(buffers.iter().all(|b| b.stream_generation() == 0));
        assert_eq!(opener.calls(), 0, "no proactive reopen");
        assert!(!old.stopped());
        assert_eq!(h.hub.snapshot().generation, 0);
        assert!(h.events().is_empty());
    }

    #[test]
    fn a_wait_only_default_change_wakes_an_absent_device_wait() {
        let policy = fast_policy()
            .with_max_backoff(Duration::from_secs(30))
            .with_device_wait_timeout(Duration::from_secs(5));
        let old = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
        let new = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let opener = Opener::new(vec![Err(not_found()), Ok(new)]);
        let h = Harness::with_trigger(policy, &old, opener.factory(), RecoveryTrigger::new(false));

        wait_for(|| {
            let _ = h.stream.try_read_chunk();
            opener.calls() == 1
        });
        for _ in 0..5 {
            assert!(matches!(h.stream.try_read_chunk(), Ok(None)));
            std::thread::sleep(POLL_INTERVAL);
        }
        assert_eq!(opener.calls(), 1, "no re-probe before a device event");

        h.trigger.on_device_event(&default_output_changed());
        let buffers = read_n(&h.stream, 1);

        assert_eq!(buffers[0].stream_generation(), 1);
        assert_eq!(opener.calls(), 2);
    }

    #[test]
    fn a_generation_that_dies_before_delivering_carries_the_outage_forward() {
        let policy = fast_policy();
        let old = MockStream::new(
            stereo(RATE),
            vec![
                Step::Buffer(buffer(0)),
                Step::Buffer(buffer(10)),
                Step::End("device unplugged"),
            ],
        );
        let flaky = MockStream::new(stereo(RATE), vec![Step::End("flaky endpoint")]);
        let healthy = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let opener = Opener::new(vec![Ok(flaky), Ok(healthy)]);
        let h = Harness::new(policy.clone(), &old, opener.factory());

        let buffers = read_n(&h.stream, 3);

        let outages: Vec<Duration> = h
            .events()
            .iter()
            .filter_map(|event| match event {
                RecoveryEvent::Recovered { outage, .. } => Some(*outage),
                _ => None,
            })
            .collect();
        let [first, second] = outages.as_slice() else {
            panic!("expected two recoveries, got {outages:?}");
        };
        assert!(
            *second >= *first + policy.backoff_for_attempt(2),
            "the outage runs from the first loss: {first:?} then {second:?}"
        );
        assert_eq!(buffers[2].stream_generation(), 2);
        assert_eq!(
            buffers[2].timestamp(),
            Some(ms(20) + *second),
            "the silent generation's outage is carried into the timeline"
        );
    }

    #[test]
    fn untimed_buffers_still_advance_the_session_timeline() {
        let old = MockStream::new(
            stereo(RATE),
            vec![
                Step::Buffer(untimed()),
                Step::Buffer(untimed()),
                Step::Buffer(untimed()),
                Step::End("device unplugged"),
            ],
        );
        let new = MockStream::new(
            stereo(RATE),
            vec![Step::Buffer(buffer(0)), Step::Buffer(untimed())],
        );
        let opener = Opener::new(vec![Ok(new)]);
        let h = Harness::new(fast_policy(), &old, opener.factory());

        let buffers = read_n(&h.stream, 5);

        assert!(buffers[..3]
            .iter()
            .all(|b| b.timestamp().is_none() && b.stream_generation() == 0));
        let outage = match h.events().as_slice() {
            [RecoveryEvent::DeviceInvalidated { .. }, RecoveryEvent::RetryScheduled { .. }, RecoveryEvent::Recovered { outage, .. }] => {
                *outage
            }
            other => panic!("unexpected events: {other:?}"),
        };
        assert_eq!(
            buffers[3].timestamp(),
            Some(ms(30) + outage),
            "30 ms of untimed audio plus the outage, not just the outage"
        );
        assert_eq!(buffers[3].stream_generation(), 1);
        assert!(buffers[4].timestamp().is_none(), "untimed stays untimed");
    }
}
