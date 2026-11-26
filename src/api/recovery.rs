//! Auto-recovery supervisor for desktop `SystemDefault` capture (feature
//! `hotplug-recovery`).
//!
//! [`start_supervised`] arms a device-change watcher and only then opens
//! generation zero, through the same factory every replacement uses: on the
//! default device current at `start()`, not the one `build()` resolved. A
//! default change before the open is therefore reflected in it, and one after
//! the watcher is armed is recorded and handled by the first poll. The
//! resulting [`RecoveryStream`] is itself a [`CapturingStream`], so every
//! consumer of the handle (pull reads, subscribe pumps, the callback pump, sink
//! drains, the async stream) is supervised without knowing about recovery.
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
//! helper's result and returns `Ok(None)` while an open is in flight. What a
//! poll itself retires — a replacement the runtime format gate rejects, a probe
//! result that came too late, an open that raced `stop()`, a stream still
//! installed when the session exhausts (one a failed reopen spawn never
//! retired), and the device-change watcher of a session that just ended — is
//! released (streams stopped, the watcher's notify thread joined) on a
//! short-lived `rsac-recovery-dispose` thread. When no such thread can be
//! spawned the resources are kept, and the capture's own `stop()` or drop
//! releases them;
//! the poll never does. The detach and that handoff happen in one critical
//! section of the `machine` lock, which `stop()` takes too, so `stop()` finds
//! every retired stream still installed, handed off, or kept — never in
//! between. A poll therefore never waits for platform teardown or
//! a factory open, the `machine` lock that serialises readers is never held
//! across either, and [`stop`](CapturingStream::stop) wakes every reader
//! without waiting for a wedged backend open. A result that lands after
//! `stop()`, or after an absent-device wait gave up on it, is disposed
//! (stopped and dropped) by the helper itself.
//!
//! # Recovery contract
//!
//! Recovery is break-before-make: the helper stops and releases the old stream
//! before the factory opens a replacement on the current default device. Before
//! a stream is retired — once a proactive switch's settle period ends, or when
//! `stop()` lands — its readable tail is drained: the backlog readable at that
//! cutoff as the stream counts it ([`CapturingStream::readable_buffers`]), or
//! one ring's worth when it cannot count. Everything readable at the cutoff is
//! delivered, and later traffic cannot postpone the switch; a generation found
//! dead at the cutoff, even with nothing left to drain, is a loss charged like
//! any other. After `stop()`, readers holding this stream receive that
//! preserved tail and then [`AudioError::StreamEnded`], without waiting for
//! the backend's teardown. The delivery format is gated
//! twice, both on `(sample_rate, channels)`: a replacement whose reported
//! `format()` matches nothing generation 0 ever reported is stopped unread, and
//! **every buffer** a replacement delivers is checked against the baseline the
//! first delivered buffer established — the only authoritative source, because
//! a backend may still report its requested format while negotiation is
//! pending. Either mismatch ends the session with a fatal
//! [`AudioError::StreamEnded`] before any sample of the new format reaches the
//! caller. Delivered buffers are tagged with their stream generation and, from
//! generation 1 on, shifted onto one monotonic session timeline: a
//! session-position cursor advances by every delivered buffer's duration
//! (stamped or not), and the outage since the last audio — carried forward
//! across generations that die before delivering — is added when a replacement
//! is installed.
//!
//! An absent-device wait's deadline also bounds a probe still in flight. A
//! probe completes when the helper publishes its result, timestamped under the
//! result mailbox's lock (its format screen runs before that, any cleanup
//! after): a result published before the deadline is applied even when
//! collected later, and one published after it — or none at all by the
//! deadline — ends the wait. The retry budget is restored only once a read
//! finds a generation still running `stable_period` after it was installed
//! (adopted from the helper by a poll; generation 0 when the session opened) —
//! a loss a reader notices late, or the tail a stopped generation still
//! drains, is no evidence that the generation lived that long. A
//! blocking [`read_chunk`](CapturingStream::read_chunk) keeps the backends'
//! ordinary read timeout while one running generation stays quiet, judged at
//! the instant each poll observed the stream (under the `machine` lock), never
//! when its reader resumes; recovery, a new generation (even one another
//! reader installed), or a stop restarts that clock, and once a stop is
//! requested a read ends instead of timing out.
//!
//! Default-device changes proactively retire a running stream on Windows and
//! macOS only; on Linux the session manager (WirePlumber) re-routes
//! `SystemDefault` natively, so a default change merely wakes an absent-device
//! wait and recovery otherwise reacts to stream invalidation.

use crate::audio::get_device_enumerator;
use crate::bridge::calculate_capacity;
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

/// Terminal reason when `stop()` lands while recovery is under way: no
/// generation is running or settling.
const STOPPED_DURING_RECOVERY_REASON: &str = "capture stopped while auto-recovery was reopening";

/// Terminal reason once the tail `stop()` preserved has been delivered.
const STOPPED_REASON: &str = "capture stopped";

/// Poll interval of the blocking read path (and of the async poll waker).
const POLL_INTERVAL: Duration = Duration::from_millis(1);

/// Fallback horizon for a deadline whose configured delay overflows `Instant`.
const FAR_FUTURE: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// Floor of the bridge ring capacity every desktop backend creates:
/// `calculate_capacity(StreamConfig::buffer_size, 4)`, see
/// [`StreamConfig::buffer_size`]. One ring's worth bounds a tail drain when
/// the stream cannot count its backlog.
const RING_MIN_SLOTS: usize = 4;

/// Blocking-read timeout of a quiet, running generation: the timeout every
/// backend gives its bridge stream, so a supervised
/// [`read_chunk`](CapturingStream::read_chunk) times out like an unsupervised
/// one.
const READ_TIMEOUT: Duration = Duration::from_secs(1);

/// Whether an output default-device change proactively retires a running
/// stream. Windows and macOS keep `SystemDefault` bound to the old endpoint, so
/// they must reopen; on Linux the session manager re-routes natively and a
/// reopen would only cost an outage and a generation change.
const PROACTIVE_DEFAULT_SWITCH: bool = cfg!(not(target_os = "linux"));

/// Opens a replacement backend stream on the device recovery follows.
type StreamFactory = Box<dyn Fn() -> AudioResult<Box<dyn CapturingStream>> + Send + Sync>;

/// Streams a poll decided to retire, handed off the read path before the
/// `machine` lock is released (see [`Retired`]).
type Doomed = Vec<Arc<dyn CapturingStream>>;

/// Moves what a poll retired off the read path, or hands it back when it
/// cannot (see [`spawn_disposal`]).
type Disposer = Box<dyn Fn(Retired) -> Result<(), Retired> + Send + Sync>;

/// Starts the helper thread that runs one reopen attempt (see
/// [`spawn_reopen_helper`]).
type ReopenSpawner = fn(Box<dyn FnOnce() + Send>) -> std::io::Result<()>;

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

/// Starts a supervised capture for `AudioCapture::start()`: arms the
/// device-change watcher, then opens generation zero through the same factory
/// every replacement uses — on the default device current now, not the one
/// `build()` resolved — and supervises it under `hub`'s policy.
///
/// Because the watcher is armed first, a default change before the open is
/// reflected in it, and one after is recorded in the trigger and handled by
/// the first poll (at worst a redundant switch to the device generation zero
/// already bound). Returns the factory's error when nothing can be opened.
pub(super) fn start_supervised(
    stream_config: &StreamConfig,
    hub: &Arc<RecoveryHub>,
) -> AudioResult<Box<dyn CapturingStream>> {
    let trigger = Arc::new(RecoveryTrigger::default());
    let watcher = watch_default_device(&trigger);
    // Every generation's ring holds at most this many buffers: the backends
    // size it with this same computation.
    let ring_slots = calculate_capacity(stream_config.buffer_size, RING_MIN_SLOTS);
    let stream = RecoveryStream::open(
        default_device_factory(stream_config.clone()),
        trigger,
        watcher,
        Arc::clone(hub),
        ring_slots,
    )?;
    Ok(Box::new(stream))
}

/// Opens a stream on whatever the system default device is when called.
fn default_device_factory(config: StreamConfig) -> StreamFactory {
    Box::new(move || {
        get_device_enumerator()?
            .default_device()?
            .create_stream(&config)
    })
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
/// keeps receiving events across stop/start restarts. Each newly opened
/// capture session (a `start()` that opens a stream) takes a fresh **session
/// id**; a retired stream (one pump may still be mid-poll after a restart)
/// carries its old id and can neither mutate the snapshot nor emit into the
/// restarted session. Events and the snapshot carry the id too, so events a
/// previous session left queued before a restart stay distinguishable from the
/// new session's.
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
            session: status.session,
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

/// Backend counters accumulated from every generation recovery has retired.
#[derive(Default)]
struct RetiredCounters {
    overruns: u64,
    pushed: u64,
    dropped: u64,
}

impl RetiredCounters {
    fn absorb(&mut self, stream: &dyn CapturingStream) {
        self.overruns = self.overruns.saturating_add(stream.overrun_count());
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
    /// Sticky: an absent-device wait gave up on its in-flight probe. Raised
    /// under the `mailbox` lock (see [`Core::collect_reopen`]) and read under
    /// it before publishing, so the probe's late result is disposed by the
    /// helper instead of being stranded in the mailbox.
    abandoned: AtomicBool,
    /// Result of the single in-flight reopen, awaiting collection by a poll.
    mailbox: Mutex<Option<Reopened>>,
}

/// What a reopen helper produced, and when it published it.
struct Reopened {
    outcome: Outcome,
    /// Taken under the `mailbox` lock as the result is stored: the probe's one
    /// completion point. A poll that abandons an in-flight probe holds the same
    /// lock, so a result is late exactly when it is published after the
    /// deadline, whichever side gets the lock first.
    published_at: Instant,
}

enum Outcome {
    /// A replacement whose reported format passed the fail-fast screen.
    Opened(Box<dyn CapturingStream>),
    /// A replacement whose reported format matches nothing generation 0
    /// reported; stopped by the helper right after publishing this.
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

/// Resources a poll retired: streams to stop and, once the session ended, its
/// device-change subscription. Releasing them can block on platform teardown
/// (a backend `stop()`, a watcher's notify-thread join or dispatch-queue
/// drain), so a poll never releases them itself.
#[derive(Default)]
struct Retired {
    streams: Doomed,
    watcher: Option<DeviceWatcher>,
}

impl Retired {
    fn is_empty(&self) -> bool {
        self.streams.is_empty() && self.watcher.is_none()
    }

    /// Adds `other`'s resources. A session has a single watcher, so at most
    /// one of the two holds it.
    fn absorb(&mut self, other: Retired) {
        self.streams.extend(other.streams);
        self.watcher = self.watcher.take().or(other.watcher);
    }

    /// Stops every stream, then drops the streams and the watcher. Blocking.
    fn release(self) {
        let Self { streams, watcher } = self;
        for stream in streams {
            if let Err(e) = stream.stop() {
                log::debug!("auto-recovery: stopping a discarded stream failed: {e}");
            }
        }
        drop(watcher);
    }
}

/// Releases `retired` on a short-lived `rsac-recovery-dispose` thread. When no
/// thread can be spawned, hands `retired` back untouched: the caller must not
/// release it on the read path.
fn spawn_disposal(retired: Retired) -> Result<(), Retired> {
    // Shared, so the resources are recoverable when the spawn fails and drops
    // the closure.
    let pending = Arc::new(Mutex::new(Some(retired)));
    let worker = Arc::clone(&pending);
    let spawned = std::thread::Builder::new()
        .name("rsac-recovery-dispose".into())
        .spawn(move || {
            let retired = lock(&worker).take();
            if let Some(retired) = retired {
                retired.release();
            }
        });
    match spawned {
        Ok(_) => Ok(()),
        Err(e) => {
            log::warn!(
                "auto-recovery: cannot spawn the disposal helper ({e}); keeping the retired \
                 resources for the capture's stop()"
            );
            let retired = lock(&pending).take();
            retired.map_or(Ok(()), Err)
        }
    }
}

/// Runs one reopen attempt on a short-lived `rsac-recovery-open` thread.
fn spawn_reopen_helper(attempt: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("rsac-recovery-open".into())
        .spawn(attempt)
        .map(drop)
}

impl Core {
    fn active_stream(&self) -> Option<Arc<dyn CapturingStream>> {
        lock(&self.slot).stream.clone()
    }

    /// Removes the installed stream from the slot, folding its counters into
    /// the retired totals. The caller hands it to disposal off the read path.
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

    /// Whether nobody will ever collect a result: `stop()` was requested, or
    /// an absent-device wait gave up on the in-flight probe.
    fn unwanted(&self) -> bool {
        self.stop_requested.load(Ordering::SeqCst) || self.abandoned.load(Ordering::SeqCst)
    }

    /// The single-flight reopen: retire the old stream, open a replacement,
    /// screen its reported format, and publish the result. Disposes of anything
    /// a concurrent `stop()` or an expired device wait made late.
    fn run_reopen(&self, gate: FormatGate) {
        self.retire_active();
        if self.unwanted() {
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
        // Screen the reported format (a cheap read) before publishing; stopping
        // a rejected stream is cleanup and waits until after publication.
        let (outcome, rejected) = match opened {
            Ok(stream) => {
                let format = stream.format();
                if gate.admits(&format) {
                    (Outcome::Opened(stream), None)
                } else {
                    let detail = format!(
                        "{FORMAT_CHANGED_REASON}: negotiated {:?} (rate, channels), \
                         replacement reports {:?}",
                        gate.expected(),
                        FormatGate::signature(&format)
                    );
                    (Outcome::FormatMismatch { detail }, Some(stream))
                }
            }
            Err(error) => (Outcome::Failed(error), None),
        };
        // Publication is the probe's one completion point (see `Reopened`).
        let discarded = {
            let mut mailbox = lock(&self.mailbox);
            if self.unwanted() {
                Some(outcome)
            } else {
                *mailbox = Some(Reopened {
                    outcome,
                    published_at: Instant::now(),
                });
                None
            }
        };
        // Stop the rejected stream unread, so no sample of its format is ever
        // delivered.
        if let Some(stream) = rejected {
            if let Err(e) = stream.stop() {
                log::debug!("auto-recovery: stopping the mismatched replacement failed: {e}");
            }
        }
        if let Some(outcome) = discarded {
            dispose(outcome);
        }
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

    /// Takes the in-flight reopen's result. With `abandon` and no result yet,
    /// gives up on it instead. Both happen under the `mailbox` lock the helper
    /// publishes under, so a result is either returned here or disposed by the
    /// helper — never stranded in the mailbox — and a result abandoned here
    /// would have been published after this call.
    fn collect_reopen(&self, abandon: bool) -> Option<Reopened> {
        let mut mailbox = lock(&self.mailbox);
        let reopened = mailbox.take();
        if reopened.is_none() && abandon {
            self.abandoned.store(true, Ordering::SeqCst);
        }
        reopened
    }
}

/// Supervisor phase.
enum Phase {
    /// A stream is installed and delivering.
    Running,
    /// A default change was seen; the old stream keeps delivering until the
    /// notifications have been quiet for `settle_delay`, then the backlog it
    /// holds at that cutoff is drained before it is retired.
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
        /// The device-wait deadline this probe belongs to, when it is one. It
        /// keeps running while the probe is in flight; an ordinary reopen has
        /// no deadline.
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
    /// When the current generation was installed: generation 0 when the
    /// session opened, a replacement when a poll adopted it from the reopen
    /// helper. A read that finds it alive `stable_period` later restores the
    /// retry budget.
    healthy_since: Option<Instant>,
    /// Buffers `stop()` moved out of the installed stream, served ahead of
    /// the end of the session.
    tail: VecDeque<AudioBuffer>,
    /// Set once `stop()` has preserved the tail: from then on the backend is
    /// never read again, so reads end with the tail instead of waiting for (or
    /// timing out on) the backend's teardown. `terminal` is raised in the same
    /// critical section.
    tail_preserved: bool,
    /// Buffers of the old stream still to deliver after the settle cutoff:
    /// the backlog readable at that cutoff. `None` before the cutoff.
    settle_tail: Option<usize>,
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
    /// Device-change subscription feeding `trigger`; released by `stop()`, or
    /// handed off the read path by the poll that ends the session.
    watcher: Mutex<Option<DeviceWatcher>>,
    /// Delivery format generation 0 reported; the fallback for `format()`
    /// while no stream is installed.
    expected_format: AudioFormat,
    /// Never held across backend work: see the module docs.
    machine: Mutex<Machine>,
    /// Set once stopped or exhausted, always under the machine lock: by
    /// `begin_stop` right after the tail is preserved, or by `exhaust`. Backs
    /// `is_running()`.
    terminal: AtomicBool,
    /// Buffers handed to the caller, counted where a poll returns one; backs
    /// `buffers_captured()`.
    delivered: AtomicU64,
    /// Ring capacity of every generation: the most buffers one can hold, and
    /// so the bound of a tail drain from a stream that cannot count its
    /// backlog.
    ring_slots: usize,
    /// Quiet time after which a blocking read of a running generation fails
    /// with [`AudioError::Timeout`]; [`READ_TIMEOUT`] outside tests.
    read_timeout: Duration,
    /// How a poll moves what it retired off the read path: [`spawn_disposal`]
    /// outside tests. Called with the machine lock held.
    disposer: Disposer,
    /// How `start_reopen` starts its helper thread: [`spawn_reopen_helper`]
    /// outside tests.
    spawn_reopen: ReopenSpawner,
    /// What a poll retired but could not hand off; released by `stop()` or on
    /// drop, never by a poll.
    retained: Mutex<Retired>,
    #[cfg(feature = "async-stream")]
    poll_waker: PollWaker,
}

impl RecoveryStream {
    /// Opens generation zero through `factory` — the open every replacement
    /// uses — and supervises it. `trigger` must already be armed, so a default
    /// change during this open is kept for the first poll rather than lost.
    fn open(
        factory: StreamFactory,
        trigger: Arc<RecoveryTrigger>,
        watcher: Option<DeviceWatcher>,
        hub: Arc<RecoveryHub>,
        ring_slots: usize,
    ) -> AudioResult<Self> {
        let initial = factory()?;
        Ok(Self::new(
            initial, factory, trigger, watcher, hub, ring_slots,
        ))
    }

    fn new(
        initial: Box<dyn CapturingStream>,
        factory: StreamFactory,
        trigger: Arc<RecoveryTrigger>,
        watcher: Option<DeviceWatcher>,
        hub: Arc<RecoveryHub>,
        ring_slots: usize,
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
                abandoned: AtomicBool::new(false),
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
                tail_preserved: false,
                settle_tail: None,
            }),
            expected_format,
            terminal: AtomicBool::new(false),
            delivered: AtomicU64::new(0),
            ring_slots,
            read_timeout: READ_TIMEOUT,
            disposer: Box::new(spawn_disposal),
            spawn_reopen: spawn_reopen_helper,
            retained: Mutex::new(Retired::default()),
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
    /// (stopped off the read path) and the reopen runs on a helper thread.
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
                    if self.trigger.take_default_changed() {
                        self.emit(RecoveryEvent::DefaultChanged {
                            session: self.session,
                            generation: m.generation,
                        });
                        m.settle_tail = None;
                        m.phase = Phase::Settling {
                            deadline: deadline_after(now, settle),
                        };
                        continue;
                    }
                    match self.read_active(m, now, doomed)? {
                        Read::Buffer(buffer) => return Ok(Some(buffer)),
                        Read::Empty => return Ok(None),
                        Read::Lost(reason) => self.on_lost(m, reason, now),
                    }
                }
                Phase::Settling { deadline } => {
                    // Every further notification restarts the quiet period,
                    // and with it the cutoff that follows it.
                    let deadline = if self.trigger.take_default_changed() {
                        m.settle_tail = None;
                        deadline_after(now, settle)
                    } else {
                        deadline
                    };
                    m.phase = Phase::Settling { deadline };
                    // Before the deadline the old stream simply keeps
                    // delivering. Its passing is the cutoff: the backlog
                    // readable then is the tail the retirement must not
                    // discard — all of it, and nothing that arrives later.
                    let tail_left = if now < deadline {
                        None
                    } else {
                        let backlog = m.settle_tail.unwrap_or_else(|| self.readable_backlog());
                        m.settle_tail = Some(backlog);
                        Some(backlog)
                    };
                    if tail_left != Some(0) {
                        match self.read_active(m, now, doomed)? {
                            Read::Buffer(buffer) => {
                                if let Some(left) = m.settle_tail.as_mut() {
                                    *left = left.saturating_sub(1);
                                }
                                return Ok(Some(buffer));
                            }
                            Read::Empty if tail_left.is_none() => return Ok(None),
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
                    } else if !self
                        .core
                        .active_stream()
                        .is_some_and(|stream| stream.is_running())
                    {
                        // Nothing left to drain, but the old stream died during
                        // the settle window: a loss like any other, told from
                        // an empty live stream without reading audio that
                        // arrived after the cutoff.
                        let reason = format!(
                            "generation {} stream ended during the settle window",
                            m.generation
                        );
                        self.on_lost(m, reason, now);
                        continue;
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
                        self.give_up_waiting(m);
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
                    // An absent-device wait's deadline keeps running while its
                    // probe is in flight; an ordinary reopen has none.
                    let expired = wait_deadline.is_some_and(|deadline| now >= deadline);
                    match self.core.collect_reopen(expired) {
                        // Still in flight: never block the reader.
                        None if !expired => return Ok(None),
                        // The wait ran out first; the helper disposes of
                        // whatever the probe still produces.
                        None => {
                            self.give_up_waiting(m);
                            continue;
                        }
                        // A result published past the deadline is as late as
                        // none at all, however soon it is collected.
                        Some(late)
                            if wait_deadline
                                .is_some_and(|deadline| late.published_at >= deadline) =>
                        {
                            if let Outcome::Opened(stream) = late.outcome {
                                doomed.push(Arc::from(stream));
                            }
                            self.give_up_waiting(m);
                            continue;
                        }
                        Some(reopened) => {
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
        }
    }

    /// After `stop()` is requested: serve the tail `stop()` preserved, then end
    /// the session. The stop's own `StreamEnded` comes only once `begin_stop`
    /// has preserved the tail and raised `terminal` (one critical section of
    /// the machine lock held here); until then the still-installed stream
    /// serves reads and a poll with nothing to deliver returns `Ok(None)`. A
    /// buffer the format gate rejects in that window still exhausts the
    /// session at once; `exhaust` raises `terminal` itself, so every
    /// `StreamEnded` a poll returns finds `is_running()` already false. Never
    /// opens anything, and never waits for the backend's teardown.
    fn read_after_stop(
        &self,
        m: &mut Machine,
        doomed: &mut Doomed,
    ) -> AudioResult<Option<AudioBuffer>> {
        if let Some(buffer) = m.tail.pop_front() {
            return self.accept(m, buffer, doomed).map(Some);
        }
        let live = matches!(m.phase, Phase::Running | Phase::Settling { .. });
        if m.tail_preserved {
            let reason = if live {
                STOPPED_REASON
            } else {
                STOPPED_DURING_RECOVERY_REASON
            };
            return Err(AudioError::StreamEnded {
                reason: reason.to_string(),
            });
        }
        // `stop()` has not reached the tail yet: the session has not ended.
        if !live {
            return Ok(None);
        }
        let Some(stream) = self.core.active_stream() else {
            return Ok(None);
        };
        match stream.try_read_chunk() {
            Ok(Some(buffer)) => self.accept(m, buffer, doomed).map(Some),
            Ok(None) => Ok(None),
            // A loss racing the stop is not reported: the stop ends the
            // session, and `begin_stop` publishes that end.
            Err(e) if e.is_fatal() => Ok(None),
            // Recoverable read errors pass through, as on the live path.
            Err(e) => Err(e),
        }
    }

    /// One non-blocking read of the installed stream. A read the stream answers
    /// (with a buffer or empty) while it still reports itself running is
    /// evidence the generation is alive at `now`; a stopped generation's
    /// draining tail is delivered but proves nothing.
    fn read_active(&self, m: &mut Machine, now: Instant, doomed: &mut Doomed) -> AudioResult<Read> {
        // `Running`/`Settling` always have a stream installed; an empty slot
        // reads as empty.
        let Some(stream) = self.core.active_stream() else {
            return Ok(Read::Empty);
        };
        let read = match stream.try_read_chunk() {
            Ok(Some(buffer)) => Read::Buffer(self.accept(m, buffer, doomed)?),
            Ok(None) => Read::Empty,
            Err(e) if e.is_fatal() => return Ok(Read::Lost(e.to_string())),
            // Recoverable read errors pass through unchanged, as unsupervised.
            Err(e) => return Err(e),
        };
        if stream.is_running() {
            self.note_alive(m, now);
        }
        Ok(read)
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

    /// The installed generation answered a read at `now` and still reports
    /// itself running. Seen so a full `stable_period` after it was installed,
    /// it has proven stable and the retry budget is restored. Only such a read
    /// counts: a loss a reader notices late, or a tail a stopped generation
    /// still drains, is no evidence that the generation lived that long.
    fn note_alive(&self, m: &mut Machine, now: Instant) {
        let stable = m.healthy_since.is_some_and(|since| {
            now.saturating_duration_since(since) >= self.hub.config.stable_period
        });
        if m.attempt > 0 && stable {
            m.attempt = 0;
            self.publish(RecoveryState::Running, m.generation, 0);
        }
    }

    /// The installed stream reached a fatal terminal: schedule a retry,
    /// spending budget (a generation seen alive through `stable_period` already
    /// had the budget restored by [`note_alive`](Self::note_alive)). The dead
    /// stream is retired by the reopen helper right before the open.
    fn on_lost(&self, m: &mut Machine, reason: String, now: Instant) {
        log::warn!(
            "auto-recovery: generation {} stream invalidated: {reason}",
            m.generation
        );
        self.emit(RecoveryEvent::DeviceInvalidated {
            session: self.session,
            generation: m.generation,
            reason: reason.clone(),
        });
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
            session: self.session,
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
        let spawned = (self.spawn_reopen)(Box::new(move || core.run_reopen(gate)));
        if let Err(e) = spawned {
            let mut mailbox = lock(&self.core.mailbox);
            *mailbox = Some(Reopened {
                outcome: Outcome::Failed(AudioError::InternalError {
                    message: format!("auto-recovery: cannot spawn the reopen helper: {e}"),
                    source: None,
                }),
                published_at: Instant::now(),
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
            published_at: now,
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

    /// Installs a freshly opened replacement as the next generation. `opened`
    /// is when the helper published it: the end of the outage. Its stable
    /// period runs from this installation.
    fn adopt(
        &self,
        m: &mut Machine,
        stream: Box<dyn CapturingStream>,
        opened: Instant,
        doomed: &mut Doomed,
    ) {
        if !self.core.install(Arc::from(stream), doomed) {
            return;
        }
        // The outage runs from the first loss until the replacement opened,
        // even across generations that died before delivering any audio.
        let outage = m.outage_start.map_or(Duration::ZERO, |start| {
            opened.saturating_duration_since(start)
        });
        m.base_offset = m.session_pos.saturating_add(outage);
        m.generation += 1;
        m.healthy_since = Some(Instant::now());
        m.phase = Phase::Running;
        log::info!(
            "auto-recovery: generation {} running after a {outage:?} outage",
            m.generation
        );
        self.hub
            .record_recovery(self.session, m.generation, m.attempt);
        self.emit(RecoveryEvent::Recovered {
            session: self.session,
            generation: m.generation,
            outage,
        });
    }

    /// Ends an absent-device wait whose deadline passed.
    fn give_up_waiting(&self, m: &mut Machine) {
        let reason = format!(
            "auto-recovery gave up: no capture device appeared within {:?}",
            self.hub.config.device_wait_timeout
        );
        self.exhaust(m, reason.clone(), reason);
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
            session: self.session,
            generation: m.generation,
            attempts: m.attempt,
            reason: detail,
        });
        // `poll` hands the watcher off the read path before it releases the
        // machine lock.
    }

    /// Buffers queued in the installed stream right now: its own count when it
    /// can tell, else one ring's worth — the most it can hold.
    fn readable_backlog(&self) -> usize {
        self.core.active_stream().map_or(0, |stream| {
            stream.readable_buffers().unwrap_or(self.ring_slots)
        })
    }

    /// The part of `stop()` that runs under the machine lock: preserves the
    /// tail and raises `terminal` in one critical section, so a poll that
    /// finds the tail preserved also finds the stream stopped, and no poll can
    /// end the session while `is_running()` is still true. Then takes what
    /// `stop()` must release — the installed stream, whatever polls kept
    /// because no disposal thread could be spawned, and the watcher. A poll
    /// detaches and hands off (or keeps) what it retires under this same lock,
    /// so nothing is caught in between; whatever a poll retires afterwards is
    /// the installed stream returned here, which `stop()` stops. Idempotent.
    fn begin_stop(&self) -> (Option<Arc<dyn CapturingStream>>, Retired) {
        let mut machine = lock(&self.machine);
        let active = self.core.active_stream();
        self.preserve_tail(&mut machine, active.as_deref());
        self.terminal.store(true, Ordering::Release);
        let mut teardown = std::mem::take(&mut *lock(&self.retained));
        let watcher = lock(&self.watcher).take();
        teardown.absorb(Retired {
            streams: Doomed::new(),
            watcher,
        });
        (active, teardown)
    }

    /// Moves the backlog the installed stream holds as `stop()` lands into the
    /// machine's tail before `stop()` halts the backend, so a backend that
    /// rejects reads once stopped cannot swallow it and readers need not wait
    /// for the teardown. Runs once.
    fn preserve_tail(&self, m: &mut Machine, active: Option<&dyn CapturingStream>) {
        if m.tail_preserved {
            return;
        }
        m.tail_preserved = true;
        if !matches!(m.phase, Phase::Running | Phase::Settling { .. }) {
            return;
        }
        let Some(stream) = active else {
            return;
        };
        let backlog = stream.readable_buffers().unwrap_or(self.ring_slots);
        for _ in 0..backlog {
            match stream.try_read_chunk() {
                Ok(Some(buffer)) => m.tail.push_back(buffer),
                Ok(None) | Err(_) => break,
            }
        }
    }

    /// Hands what a poll retired to the `disposer`, or keeps it when no
    /// disposal thread can start: a poll never releases it itself. `_held`
    /// is the poll's machine guard, proof that the handoff happens in the same
    /// critical section as the detach (see [`begin_stop`](Self::begin_stop)).
    fn retire(&self, _held: &mut Machine, retired: Retired) {
        if retired.is_empty() {
            return;
        }
        if let Err(retired) = (self.disposer)(retired) {
            lock(&self.retained).absorb(retired);
        }
    }

    /// One supervised poll. Teardown never runs here: the streams it retired,
    /// and the watcher of a session it ended, leave the read path — handed off
    /// before the machine lock is released, so `stop()` never misses them.
    fn poll(&self) -> Polled {
        let mut machine = lock(&self.machine);
        let mut doomed = Doomed::new();
        let result = self.advance(&mut machine, &mut doomed);
        // An exhausted session keeps no backend: a stream still installed —
        // one a failed reopen spawn never retired — leaves with the rest.
        if matches!(machine.phase, Phase::Exhausted { .. }) {
            doomed.extend(self.core.detach_active());
        }
        // The one place a buffer reaches the caller (`try_read_chunk` and
        // `read_chunk` both return this poll's result).
        if matches!(result, Ok(Some(_))) {
            self.delivered.fetch_add(1, Ordering::Relaxed);
        }
        let observed_at = Instant::now();
        let running = matches!(machine.phase, Phase::Running)
            && !self.core.stop_requested.load(Ordering::Acquire);
        let watcher = if self.terminal.load(Ordering::Acquire) {
            lock(&self.watcher).take()
        } else {
            None
        };
        let quiet = running.then_some(machine.generation);
        self.retire(
            &mut machine,
            Retired {
                streams: doomed,
                watcher,
            },
        );
        Polled {
            result,
            quiet,
            observed_at,
        }
    }
}

/// What one supervised poll produced.
struct Polled {
    result: AudioResult<Option<AudioBuffer>>,
    /// The generation the poll found running, when it did and no stop was
    /// requested: the only polls a blocking read's quiet clock counts. `None`
    /// while recovering (settling, backing off, waiting for a device,
    /// reopening), once stopped, and once exhausted.
    quiet: Option<u64>,
    /// When the poll observed the stream, taken under the machine lock: the
    /// instant a quiet clock judges this poll at, however late its reader
    /// resumes.
    observed_at: Instant,
}

/// The quiet clock of one blocking [`read_chunk`](CapturingStream::read_chunk):
/// how long the running generation it last saw has stayed empty.
struct QuietClock {
    timeout: Duration,
    /// The generation last seen running and empty, and since when.
    since: Option<(u64, Instant)>,
}

impl QuietClock {
    fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            since: None,
        }
    }

    /// Records a poll that delivered nothing, judged at `observed_at` — the
    /// instant that poll observed the stream, never when its reader resumed —
    /// with the [`Polled::quiet`] it reported. True once one running
    /// generation has stayed empty for the whole timeout. Recovery or a stop
    /// (`None`) and a different running generation — even one another reader
    /// installed while this one was descheduled — restart the clock.
    fn expired(&mut self, quiet: Option<u64>, observed_at: Instant) -> bool {
        let Some(generation) = quiet else {
            self.since = None;
            return false;
        };
        let since = match self.since {
            Some((seen, since)) if seen == generation => since,
            _ => {
                self.since = Some((generation, observed_at));
                observed_at
            }
        };
        observed_at.saturating_duration_since(since) >= self.timeout
    }
}

impl CapturingStream for RecoveryStream {
    /// Blocks by polling [`try_read_chunk`](Self::try_read_chunk) every
    /// millisecond, so recovery deadlines and `stop()` stay observable while a
    /// reader is parked here. Like an unsupervised read, it fails with
    /// [`AudioError::Timeout`] once one running generation has stayed quiet
    /// for the backends' blocking-read timeout (see [`QuietClock`]); recovery
    /// restarts that clock, and once a stop is requested the read ends with
    /// the preserved tail and [`AudioError::StreamEnded`] instead.
    fn read_chunk(&self) -> AudioResult<AudioBuffer> {
        let mut quiet = QuietClock::new(self.read_timeout);
        loop {
            let polled = self.poll();
            if let Some(buffer) = polled.result? {
                return Ok(buffer);
            }
            // Judged at the poll's own observation, so a reader descheduled
            // after its poll returned never charges later recovery time to a
            // generation; a stop that lands after the poll still wins.
            if quiet.expired(polled.quiet, polled.observed_at)
                && !self.core.stop_requested.load(Ordering::SeqCst)
            {
                return Err(AudioError::Timeout {
                    operation: "read_chunk".to_string(),
                    duration: self.read_timeout,
                });
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    fn try_read_chunk(&self) -> AudioResult<Option<AudioBuffer>> {
        self.poll().result
    }

    /// Sticky stop: aborts any backoff, settle, device wait, or in-flight open
    /// at the next poll, preserves the installed stream's queued tail for
    /// readers, stops it, and guarantees no replacement is installed
    /// afterwards. Readers end once the tail is delivered, without waiting for
    /// this teardown. Never waits for a reopen helper or for a reader.
    ///
    /// The stream reports itself stopped (`is_running()`, and so
    /// `is_stream_producing()`, turn false) only once `begin_stop` has
    /// preserved the tail, and the stop does not end a read before that: from
    /// the request until then, a poll with nothing to deliver returns
    /// `Ok(None)`. An async reader that just found the backend empty therefore
    /// keeps polling, receives that tail, and then sees the stop as the end of
    /// the stream, never as an error.
    fn stop(&self) -> AudioResult<()> {
        self.core.stop_requested.store(true, Ordering::SeqCst);
        self.hub.mark_stopped(self.session);
        let (active, teardown) = self.begin_stop();
        // A result the helper posted before it saw the flag is ours to dispose.
        let late = lock(&self.core.mailbox).take();
        if let Some(Reopened { outcome, .. }) = late {
            dispose(outcome);
        }
        let result = match active {
            Some(stream) => stream.stop(),
            None => Ok(()),
        };
        teardown.release();
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

    /// Buffers this stream handed to its caller, across every generation.
    /// Counted at delivery, not when a backend gave a buffer up: neither a
    /// tail `stop()` preserved that no read has returned yet, nor a buffer the
    /// format gate rejected, counts.
    fn buffers_captured(&self) -> u64 {
        self.delivered.load(Ordering::Relaxed)
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

impl Drop for RecoveryStream {
    /// Releases what a poll kept because no disposal thread could be spawned
    /// after `stop()` ran (only the stream `stop()` already stopped), or all of
    /// it when the capture is dropped without `stop()`.
    fn drop(&mut self) {
        let retained = std::mem::take(
            self.retained
                .get_mut()
                .unwrap_or_else(PoisonError::into_inner),
        );
        retained.release();
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

    /// The blocked side of a one-shot gate: reports reaching it, then waits to
    /// be let through — by the controller's `open()`, or by the controller
    /// being dropped. It never opens by itself; the watchdog is on the
    /// observing side ([`GateControl::wait_reached`]).
    struct Gate {
        reached: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    impl Gate {
        fn pass(self) {
            let _ = self.reached.send(());
            let _ = self.release.recv();
        }
    }

    /// The test's side of a [`Gate`].
    struct GateControl {
        reached: mpsc::Receiver<()>,
        release: mpsc::Sender<()>,
    }

    impl GateControl {
        /// Blocks until the gated operation is held at the gate.
        fn wait_reached(&self) {
            self.reached
                .recv_timeout(Duration::from_secs(5))
                .expect("the gated operation reached its gate");
        }

        fn open(&self) {
            let _ = self.release.send(());
        }
    }

    fn gate() -> (Gate, GateControl) {
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        (
            Gate {
                reached: reached_tx,
                release: release_rx,
            },
            GateControl {
                reached: reached_rx,
                release: release_tx,
            },
        )
    }

    /// A device-change subscription whose teardown passes `gate` (when given),
    /// as a real backend joins its notify thread, then raises the returned flag.
    fn mock_watcher(gate: Option<Gate>) -> (DeviceWatcher, Arc<AtomicBool>) {
        let released = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&released);
        let watcher = DeviceWatcher::from_teardown(Box::new(move || {
            if let Some(gate) = gate {
                gate.pass();
            }
            flag.store(true, Ordering::SeqCst);
        }));
        (watcher, released)
    }

    #[derive(Default)]
    struct MockState {
        script: Mutex<VecDeque<Step>>,
        ended: Mutex<Option<String>>,
        stopped: AtomicBool,
        /// The thread whose `stop()` stopped it first.
        stopped_by: Mutex<Option<std::thread::ThreadId>>,
        /// Reject reads once stopped even with steps still queued, like the
        /// production bridge (which marks a stopped stream non-readable).
        strict_stop: AtomicBool,
        delivered: AtomicUsize,
        overruns: AtomicU64,
        /// Held by the first `stop()`, like a wedged platform teardown.
        stop_gate: Mutex<Option<Gate>>,
        /// Held by the first `format()` call.
        format_gate: Mutex<Option<Gate>>,
        /// Report the queued backlog through `readable_buffers`, like the
        /// bridge.
        counts_backlog: AtomicBool,
        /// A producer that keeps up: every buffer read is queued again.
        refills: AtomicBool,
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

        /// Holds the first `stop()` at the returned gate.
        fn gated_stop(self) -> (Self, GateControl) {
            let (gate, control) = gate();
            *lock(&self.state.stop_gate) = Some(gate);
            (self, control)
        }

        /// Holds the first `format()` call at the returned gate.
        fn gated_format(self) -> (Self, GateControl) {
            let (gate, control) = gate();
            *lock(&self.state.format_gate) = Some(gate);
            (self, control)
        }

        /// Reports its queued backlog through `readable_buffers`.
        fn counted(self) -> Self {
            self.state.counts_backlog.store(true, Ordering::SeqCst);
            self
        }

        /// Queues every buffer read again, as a producer that keeps pace.
        fn refilled(self) -> Self {
            self.state.refills.store(true, Ordering::SeqCst);
            self
        }

        /// Ends the stream as if its device vanished.
        fn kill(&self, reason: &str) {
            *lock(&self.state.ended) = Some(reason.to_string());
        }

        fn stopped(&self) -> bool {
            self.state.stopped.load(Ordering::SeqCst)
        }

        fn stopped_by(&self) -> Option<std::thread::ThreadId> {
            *lock(&self.state.stopped_by)
        }

        /// Queues `step` behind whatever is still scripted.
        fn push(&self, step: Step) {
            lock(&self.state.script).push_back(step);
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
            let mut script = lock(&self.state.script);
            match script.pop_front() {
                Some(Step::Buffer(buffer)) => {
                    self.state.delivered.fetch_add(1, Ordering::SeqCst);
                    if self.state.refills.load(Ordering::SeqCst) {
                        script.push_back(Step::Buffer(buffer.clone()));
                    }
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
            let gate = lock(&self.state.stop_gate).take();
            if let Some(gate) = gate {
                gate.pass();
            }
            lock(&self.state.stopped_by).get_or_insert_with(|| std::thread::current().id());
            self.state.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn format(&self) -> AudioFormat {
            let gate = lock(&self.state.format_gate).take();
            if let Some(gate) = gate {
                gate.pass();
            }
            self.format.clone()
        }

        fn readable_buffers(&self) -> Option<usize> {
            self.state.counts_backlog.load(Ordering::SeqCst).then(|| {
                lock(&self.state.script)
                    .iter()
                    .filter(|step| matches!(step, Step::Buffer(_)))
                    .count()
            })
        }

        fn is_running(&self) -> bool {
            !self.stopped() && lock(&self.state.ended).is_none()
        }

        fn overrun_count(&self) -> u64 {
            self.state.overruns.load(Ordering::SeqCst)
        }

        /// Counts pops, as the bridge does.
        fn buffers_captured(&self) -> u64 {
            self.delivered() as u64
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

    /// Ring depth of the scripted streams: the backends' default.
    const RING_SLOTS: usize = 64;

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
            Self::build(policy, initial, factory, trigger, |_| {})
        }

        /// `tweak` adjusts the supervisor before it is shared: its ring depth,
        /// read timeout, or disposer.
        fn build(
            policy: AutoRecoveryConfig,
            initial: &MockStream,
            factory: StreamFactory,
            trigger: RecoveryTrigger,
            tweak: impl FnOnce(&mut RecoveryStream),
        ) -> Self {
            let hub = Arc::new(RecoveryHub::new(policy));
            let events = hub.subscribe().expect("first subscriber");
            let trigger = Arc::new(trigger);
            let mut stream = RecoveryStream::new(
                initial.boxed(),
                factory,
                Arc::clone(&trigger),
                None,
                Arc::clone(&hub),
                RING_SLOTS,
            );
            tweak(&mut stream);
            Self {
                stream: Arc::new(stream),
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
            [RecoveryEvent::DefaultChanged { generation: 0, .. }, RecoveryEvent::Recovered {
                generation: 1,
                outage,
                ..
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
                ..
            }, RecoveryEvent::RetryScheduled {
                generation: 0,
                attempt: 1,
                delay: d1,
                ..
            }, RecoveryEvent::RetryScheduled {
                generation: 0,
                attempt: 2,
                delay: d2,
                ..
            }, RecoveryEvent::RetryScheduled {
                generation: 0,
                attempt: 3,
                delay: d3,
                ..
            }, RecoveryEvent::Recovered {
                generation: 1,
                outage,
                ..
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
                ..
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
        // The helper stops it unread, right after publishing the verdict.
        wait_for(|| new.stopped());
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
            hub.emit(
                session,
                RecoveryEvent::DefaultChanged {
                    session,
                    generation,
                },
            );
        }

        let received: Vec<u64> = events
            .try_iter()
            .map(|event| match event {
                RecoveryEvent::DefaultChanged { generation, .. } => generation,
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
            // Stopped by the disposal helper, off the read path.
            wait_for(|| new.stopped());
            assert_eq!(
                new.delivered(),
                1,
                "{what}: read by recovery, never delivered"
            );
            assert_eq!(
                h.stream.buffers_captured(),
                1,
                "{what}: the rejected buffer was never handed to the caller"
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
        hub.emit(
            retired,
            RecoveryEvent::DefaultChanged {
                session: retired,
                generation: 7,
            },
        );
        hub.mark_stopped(retired);

        let snapshot = hub.snapshot();
        assert_eq!(snapshot.session, current);
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

        hub.emit(
            current,
            RecoveryEvent::DefaultChanged {
                session: current,
                generation: 0,
            },
        );
        assert!(matches!(
            events.try_recv(),
            Ok(RecoveryEvent::DefaultChanged { session, generation: 0 }) if session == current
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
            RING_SLOTS,
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
                    ..
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
            [RecoveryEvent::DefaultChanged { generation: 0, .. }, RecoveryEvent::Exhausted {
                generation: 0,
                attempts: 1,
                reason,
                ..
            }] => assert!(reason.contains("endpoint busy"), "{reason}"),
            other => panic!("unexpected events: {other:?}"),
        }
    }

    #[test]
    fn a_stream_dying_during_settle_is_charged_against_the_budget() {
        // Uncounted, the death is seen while reading. Counted with an empty
        // ring, the cutoff finds nothing to drain: the first poll after the
        // death comes after the settle deadline and must still see the loss.
        for counted in [false, true] {
            let policy = fast_policy().with_settle_delay(ms(60));
            let old = MockStream::new(stereo(RATE), vec![]);
            let old = if counted { old.counted() } else { old };
            let new = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
            let opener = Opener::new(vec![Ok(new)]);
            let h = Harness::new(policy, &old, opener.factory());

            let started = Instant::now();
            h.trigger.trigger_default_change();
            assert!(
                matches!(h.stream.try_read_chunk(), Ok(None)),
                "counted: {counted}"
            );
            old.kill("device unplugged");
            if counted {
                let deadline = match lock(&h.stream.machine).phase {
                    Phase::Settling { deadline } => deadline,
                    _ => panic!("counted: the switch is settling"),
                };
                wait_for(|| Instant::now() >= deadline);
            }
            let buffers = read_n(&h.stream, 1);

            assert_eq!(buffers[0].stream_generation(), 1, "counted: {counted}");
            assert!(
                started.elapsed() >= ms(60),
                "counted: {counted}: the reopen still waits out the settle deadline"
            );
            match h.events().as_slice() {
                [RecoveryEvent::DefaultChanged { generation: 0, .. }, RecoveryEvent::DeviceInvalidated { generation: 0, .. }, RecoveryEvent::RetryScheduled { attempt: 1, .. }, RecoveryEvent::Recovered { generation: 1, .. }] =>
                    {}
                other => panic!("counted: {counted}: unexpected events: {other:?}"),
            }
            assert_eq!(
                h.hub.snapshot().attempt,
                1,
                "counted: {counted}: a premature death spends budget"
            );
        }
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

    // ── Regression tests: CodeRabbit round 1 ──────────────────────────────

    /// `count` back-to-back 10 ms stereo buffers.
    fn backlog(count: u64) -> Vec<Step> {
        (0..count).map(|i| Step::Buffer(buffer(i * 10))).collect()
    }

    /// Polls until an absent-device probe is in flight, then stops polling and
    /// returns the wait deadline that probe runs under.
    fn poll_until_probing(stream: &RecoveryStream) -> Instant {
        let mut deadline = None;
        wait_for(|| {
            let _ = stream.try_read_chunk();
            deadline = match lock(&stream.machine).phase {
                Phase::Reopening { wait_deadline, .. } => wait_deadline,
                _ => None,
            };
            deadline.is_some()
        });
        deadline.expect("an absent-device probe is in flight")
    }

    /// A factory whose first open finds no device and whose second is held at
    /// the returned gate before it opens `stream`; every later open finds no
    /// device.
    fn gated_second_probe(stream: &MockStream) -> (StreamFactory, GateControl) {
        let (gate, control) = gate();
        let gate = Mutex::new(Some(gate));
        let calls = AtomicUsize::new(0);
        let stream = stream.clone();
        let factory: StreamFactory = Box::new(move || {
            if calls.fetch_add(1, Ordering::SeqCst) != 1 {
                return Err(not_found());
            }
            let held = lock(&gate).take();
            if let Some(held) = held {
                held.pass();
            }
            Ok(stream.boxed())
        });
        (factory, control)
    }

    /// An absent-device wait that never expires on its own: the tests below
    /// move the deadline themselves, under the machine lock.
    fn absence_policy() -> AutoRecoveryConfig {
        fast_policy()
            .with_max_backoff(ms(10))
            .with_device_wait_timeout(Duration::from_secs(60))
    }

    /// Moves the in-flight probe's device-wait deadline to `deadline`, so the
    /// test decides when it passes instead of racing a clock.
    fn set_wait_deadline(stream: &RecoveryStream, deadline: Instant) {
        let mut m = lock(&stream.machine);
        match &mut m.phase {
            Phase::Reopening {
                wait_deadline: Some(wait),
                ..
            } => *wait = deadline,
            _ => panic!("no absent-device probe is in flight"),
        }
    }

    /// A 10 ms mono buffer: a channel count no stereo session accepts.
    fn mono_buffer() -> AudioBuffer {
        let mono = AudioFormat {
            sample_rate: RATE,
            channels: 1,
            sample_format: SampleFormat::F32,
        };
        AudioBuffer::with_timestamp(vec![0.5; FRAMES], mono, ms(0))
    }

    /// A replacement that reports the baseline format but delivers mono: the
    /// runtime format gate rejects it on its first buffer.
    fn mono_impostor() -> MockStream {
        MockStream::new(stereo(RATE), vec![Step::Buffer(mono_buffer())])
    }

    #[test]
    fn generation_zero_is_opened_by_the_reopen_factory_after_the_watcher_is_armed() {
        let hub = Arc::new(RecoveryHub::new(fast_policy()));
        let events = hub.subscribe().expect("first subscriber");
        let trigger = Arc::new(RecoveryTrigger::new(true));
        let current = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let next = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let opener = Opener::new(vec![Ok(current.clone()), Ok(next)]);
        let open = opener.factory();
        let counted = Arc::clone(&opener);
        // The default moves while generation zero is opening, after the trigger
        // was armed: the change must survive the open.
        let during_open = Arc::clone(&trigger);
        let factory: StreamFactory = Box::new(move || {
            let opened = open();
            if counted.calls() == 1 {
                during_open.trigger_default_change();
            }
            opened
        });

        let stream = RecoveryStream::open(
            factory,
            Arc::clone(&trigger),
            None,
            Arc::clone(&hub),
            RING_SLOTS,
        )
        .expect("generation zero opens");
        assert_eq!(
            opener.calls(),
            1,
            "generation zero came from the reopen factory"
        );
        assert_eq!(hub.snapshot().state, RecoveryState::Running);
        let buffers = read_n(&stream, 2);

        let generations: Vec<u64> = buffers.iter().map(|b| b.stream_generation()).collect();
        assert_eq!(generations, vec![0, 1], "the startup switch is acted on");
        assert!(current.stopped(), "the switch retired generation zero");
        let events: Vec<RecoveryEvent> = events.try_iter().collect();
        assert!(
            matches!(
                events.as_slice(),
                [
                    RecoveryEvent::DefaultChanged { generation: 0, .. },
                    RecoveryEvent::Recovered { generation: 1, .. }
                ]
            ),
            "{events:?}"
        );

        // Nothing to open: start() fails and the hub never begins a session.
        let idle = Arc::new(RecoveryHub::new(fast_policy()));
        let absent: StreamFactory = Box::new(|| Err(not_found()));
        let failed = RecoveryStream::open(
            absent,
            Arc::new(RecoveryTrigger::new(true)),
            None,
            Arc::clone(&idle),
            RING_SLOTS,
        );
        assert!(matches!(failed, Err(AudioError::DeviceNotFound { .. })));
        let snapshot = idle.snapshot();
        assert_eq!((snapshot.session, snapshot.state), (0, RecoveryState::Idle));
    }

    #[test]
    fn a_proactive_switch_drains_a_backlog_beyond_128_buffers_and_stops_at_one_ring() {
        // (buffers queued at the cutoff, buffers generation 0 delivers)
        for (queued, delivered) in [(200, 200), (300, 256)] {
            let old = MockStream::new(stereo(RATE), backlog(queued));
            let new = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
            let h = Harness::build(
                fast_policy().with_settle_delay(Duration::ZERO),
                &old,
                Opener::new(vec![Ok(new)]).factory(),
                RecoveryTrigger::default(),
                // A stream that cannot count its backlog: one ring's worth.
                |stream| stream.ring_slots = 256,
            );
            h.trigger.trigger_default_change();

            let buffers = read_n(&h.stream, delivered + 1);

            let generation_zero = buffers
                .iter()
                .take_while(|b| b.stream_generation() == 0)
                .count();
            assert_eq!(
                generation_zero, delivered,
                "{queued} queued: the whole ring's tail, never more"
            );
            assert_eq!(buffers[delivered].stream_generation(), 1, "{queued} queued");
            assert_eq!(old.delivered(), delivered, "{queued} queued");
            assert!(old.stopped(), "{queued} queued: drained, then retired");
        }
    }

    #[test]
    fn stop_preserves_a_backlog_beyond_128_buffers_up_to_one_ring() {
        // (buffers queued when stop() lands, buffers still delivered after it)
        for (queued, preserved) in [(200, 200), (300, 256)] {
            let old = MockStream::new(stereo(RATE), backlog(queued)).strict_stop();
            let h = Harness::build(
                fast_policy(),
                &old,
                Opener::new(vec![]).factory(),
                RecoveryTrigger::default(),
                // A stream that cannot count its backlog: one ring's worth.
                |stream| stream.ring_slots = 256,
            );

            h.stream.stop().expect("stop");
            let (buffers, err) = read_until_err(&h.stream);

            assert_eq!(buffers.len(), preserved, "{queued} queued");
            assert!(matches!(err, AudioError::StreamEnded { .. }), "{err:?}");
        }
    }

    #[test]
    fn a_switch_or_stop_drains_the_backlog_readable_at_the_cutoff_not_what_arrives_later() {
        // Three buffers queued in a 256-slot ring, each replaced as it is read:
        // a producer that keeps pace with the drain.
        let old = MockStream::new(stereo(RATE), backlog(3))
            .counted()
            .refilled();
        let new = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let h = Harness::build(
            fast_policy().with_settle_delay(Duration::ZERO),
            &old,
            Opener::new(vec![Ok(new)]).factory(),
            RecoveryTrigger::default(),
            |stream| stream.ring_slots = 256,
        );
        h.trigger.trigger_default_change();

        let generations: Vec<u64> = read_n(&h.stream, 4)
            .iter()
            .map(|b| b.stream_generation())
            .collect();

        assert_eq!(
            generations,
            vec![0, 0, 0, 1],
            "the switch delivers the backlog readable at the cutoff, then switches"
        );
        assert_eq!(old.delivered(), 3);

        let old = MockStream::new(stereo(RATE), backlog(3))
            .counted()
            .refilled()
            .strict_stop();
        let h = Harness::build(
            fast_policy(),
            &old,
            Opener::new(vec![]).factory(),
            RecoveryTrigger::default(),
            |stream| stream.ring_slots = 256,
        );
        h.stream.stop().expect("stop");
        let (buffers, err) = read_until_err(&h.stream);

        assert_eq!(
            buffers.len(),
            3,
            "stop preserves the backlog it found, not what arrives during the drain"
        );
        assert!(matches!(err, AudioError::StreamEnded { .. }), "{err:?}");
    }

    #[test]
    fn events_a_previous_session_left_queued_carry_its_session_id() {
        // Session A recovers to generation 1; its subscriber reads nothing
        // before the handle is restarted.
        let lost_a = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
        let new_a = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let a = Harness::new(
            fast_policy(),
            &lost_a,
            Opener::new(vec![Ok(new_a)]).factory(),
        );
        assert_eq!(read_n(&a.stream, 1)[0].stream_generation(), 1);
        a.stream.stop().expect("stop a");
        let session_a = a.hub.snapshot().session;

        // Session B, on the same hub and subscription, recovers to generation 1 too.
        let lost_b = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
        let new_b = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let b = RecoveryStream::new(
            lost_b.boxed(),
            Opener::new(vec![Ok(new_b)]).factory(),
            Arc::new(RecoveryTrigger::default()),
            None,
            Arc::clone(&a.hub),
            RING_SLOTS,
        );
        assert_eq!(read_n(&b, 1)[0].stream_generation(), 1);
        let session_b = a.hub.snapshot().session;
        assert!(
            session_a > 0 && session_b > session_a,
            "{session_a} then {session_b}"
        );

        let recovered: Vec<(u64, u64)> = a
            .events()
            .iter()
            .filter_map(|event| match event {
                RecoveryEvent::Recovered {
                    session,
                    generation,
                    ..
                } => Some((*session, *generation)),
                _ => None,
            })
            .collect();
        assert_eq!(
            recovered,
            vec![(session_a, 1), (session_b, 1)],
            "the same generation of two sessions is told apart by the session id"
        );
        b.stop().expect("stop b");
    }

    #[test]
    fn a_probe_still_in_flight_at_the_device_wait_deadline_ends_the_wait() {
        let old = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
        let late = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let (factory, open) = gated_second_probe(&late);
        let h = Harness::new(absence_policy(), &old, factory);

        // The second probe wedges inside the open, and then its deadline passes.
        poll_until_probing(&h.stream);
        open.wait_reached();
        set_wait_deadline(&h.stream, Instant::now());
        let (buffers, err) = read_until_err(&h.stream);

        assert!(buffers.is_empty());
        match err {
            AudioError::StreamEnded { reason } => {
                assert!(reason.contains("no capture device appeared"), "{reason}")
            }
            other => panic!("expected StreamEnded, got {other:?}"),
        }
        assert!(!late.stopped(), "the wedged open has not returned yet");
        let snapshot = h.hub.snapshot();
        assert_eq!(snapshot.state, RecoveryState::Exhausted);
        assert_eq!(
            (snapshot.generation, snapshot.successful_recoveries),
            (0, 0)
        );

        // When the open finally returns, the helper disposes of the late stream.
        open.open();
        wait_for(|| late.stopped());
        assert_eq!(late.delivered(), 0);
        assert!(
            lock(&h.stream.core.mailbox).is_none(),
            "the abandoned probe is disposed, never published"
        );
        assert!(matches!(
            h.stream.try_read_chunk(),
            Err(AudioError::StreamEnded { .. })
        ));
    }

    #[test]
    fn a_probe_is_late_when_published_after_the_deadline_however_early_its_open_returned() {
        // Whether the deadline poll runs before the helper publishes (and
        // abandons the probe) or after it (and finds the result), the verdict
        // is the same: publication is the probe's one completion point.
        for poll_first in [true, false] {
            let old = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
            let (late, screen) =
                MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]).gated_format();
            let opener = Opener::new(vec![Err(not_found()), Ok(late.clone())]);
            let h = Harness::new(absence_policy(), &old, opener.factory());

            // The open has returned and the helper is screening its format,
            // not yet published, when the deadline passes.
            poll_until_probing(&h.stream);
            screen.wait_reached();
            set_wait_deadline(&h.stream, Instant::now());
            let (buffers, err) = if poll_first {
                let read = read_until_err(&h.stream);
                screen.open();
                read
            } else {
                screen.open();
                wait_for(|| lock(&h.stream.core.mailbox).is_some());
                read_until_err(&h.stream)
            };

            assert!(buffers.is_empty(), "poll first: {poll_first}");
            assert!(
                matches!(err, AudioError::StreamEnded { ref reason } if reason.contains("no capture device appeared")),
                "poll first: {poll_first}: {err:?}"
            );
            wait_for(|| late.stopped());
            assert_eq!(late.delivered(), 0, "poll first: {poll_first}");
            assert_eq!(
                h.hub.snapshot().successful_recoveries,
                0,
                "poll first: {poll_first}"
            );
        }
    }

    #[test]
    fn a_probe_published_before_the_deadline_is_applied_even_when_collected_after_it() {
        let old = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
        let timely = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let opener = Opener::new(vec![Err(not_found()), Ok(timely)]);
        let h = Harness::new(absence_policy(), &old, opener.factory());

        // Published; the deadline passes right after, before any poll collects
        // the result.
        poll_until_probing(&h.stream);
        wait_for(|| lock(&h.stream.core.mailbox).is_some());
        let published = lock(&h.stream.core.mailbox)
            .as_ref()
            .map(|reopened| reopened.published_at)
            .expect("the probe is published");
        let deadline = published + Duration::from_nanos(1);
        set_wait_deadline(&h.stream, deadline);
        wait_for(|| Instant::now() > deadline);
        let buffers = read_n(&h.stream, 1);

        assert_eq!(
            buffers[0].stream_generation(),
            1,
            "a probe published in time is applied"
        );
        assert_eq!(h.hub.snapshot().successful_recoveries, 1);
    }

    #[test]
    fn a_generation_that_died_early_never_restores_the_budget_however_late_the_loss_is_seen() {
        let stable = ms(20);
        let policy = fast_policy()
            .with_max_attempts(1)
            .with_stable_period(stable);
        let short_lived = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let spare = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let opener = Opener::new(vec![Ok(spare)]);
        let h = Harness::new(policy, &short_lived, opener.factory());

        // As if `short_lived` were a replacement that spent the only attempt:
        // installed now, it dies before any read finds it alive.
        let installed = {
            let mut m = lock(&h.stream.machine);
            m.attempt = 1;
            let installed = Instant::now();
            m.healthy_since = Some(installed);
            installed
        };
        short_lived.kill("device unplugged again");
        // The reader comes back only after its stable period would have passed;
        // a longer pause only makes the loss later.
        wait_for(|| Instant::now() >= installed + stable * 2);
        let (buffers, err) = read_until_err(&h.stream);

        assert!(buffers.is_empty());
        match err {
            AudioError::StreamEnded { reason } => assert!(reason.contains("exhausted"), "{reason}"),
            other => panic!("expected StreamEnded, got {other:?}"),
        }
        assert_eq!(
            opener.calls(),
            0,
            "no reopen: the generation never proved stable"
        );
        let snapshot = h.hub.snapshot();
        assert_eq!(snapshot.state, RecoveryState::Exhausted);
        assert_eq!(snapshot.attempt, 1);
    }

    #[test]
    fn a_quiet_running_stream_times_out_like_an_unsupervised_blocking_read() {
        let quiet = MockStream::new(stereo(RATE), vec![]);
        let h = Harness::build(
            fast_policy(),
            &quiet,
            Opener::new(vec![]).factory(),
            RecoveryTrigger::default(),
            |stream| stream.read_timeout = ms(30),
        );
        let reader = Arc::clone(&h.stream);
        let (tx, rx) = mpsc::channel();
        let started = Instant::now();
        std::thread::spawn(move || {
            let _ = tx.send(reader.read_chunk());
        });

        let result = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("a quiet, running stream must time out instead of blocking forever");

        match result {
            Err(AudioError::Timeout {
                operation,
                duration,
            }) => {
                assert_eq!(operation, "read_chunk");
                assert_eq!(duration, ms(30));
            }
            other => panic!("expected Timeout, got {other:?}"),
        }
        assert!(started.elapsed() >= ms(30));
        assert!(h.stream.is_running(), "a timeout is not terminal");
        assert_eq!(h.hub.snapshot().state, RecoveryState::Running);
    }

    #[test]
    fn a_recovery_wait_longer_than_the_read_timeout_is_not_cut_short() {
        let policy = fast_policy()
            .with_initial_backoff(ms(60))
            .with_max_backoff(ms(60));
        let old = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
        let new = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let h = Harness::build(
            policy,
            &old,
            Opener::new(vec![Ok(new)]).factory(),
            RecoveryTrigger::default(),
            |stream| stream.read_timeout = ms(20),
        );

        let started = Instant::now();
        let recovered = h
            .stream
            .read_chunk()
            .expect("the recovered generation's audio, not a timeout");

        assert_eq!(recovered.stream_generation(), 1);
        assert!(
            started.elapsed() >= ms(60),
            "the reader waited out the whole backoff"
        );
    }

    #[test]
    fn a_parked_readers_quiet_clock_restarts_when_another_reader_installs_a_generation() {
        let old = MockStream::new(stereo(RATE), vec![]);
        let new = MockStream::new(stereo(RATE), vec![]);
        let h = Harness::new(fast_policy(), &old, Opener::new(vec![Ok(new)]).factory());
        let timeout = h.stream.read_timeout;

        // Reader A finds generation 0 running and quiet, then is descheduled.
        let mut a = QuietClock::new(timeout);
        let first = h.stream.poll();
        assert!(matches!(first.result, Ok(None)));
        assert_eq!(first.quiet, Some(0));
        assert!(!a.expired(first.quiet, first.observed_at));

        // Reader B notices the loss and drives the recovery to a quiet
        // generation 1.
        old.kill("device unplugged");
        wait_for(|| {
            assert!(matches!(h.stream.try_read_chunk(), Ok(None)));
            h.hub.snapshot().generation == 1
        });

        // A's next poll finds generation 1. Even judged a full timeout after
        // A's clock started, generation 1 has not been quiet that long.
        let polled = h.stream.poll();
        assert!(matches!(polled.result, Ok(None)));
        assert_eq!(polled.quiet, Some(1));
        let later = first.observed_at + timeout;
        assert!(
            !a.expired(polled.quiet, later),
            "recovery another reader drove is not quiet time of the new generation"
        );
        assert!(
            a.expired(polled.quiet, later + timeout),
            "generation 1's own quiet period still times out"
        );

        // Once stopped, no poll is quiet.
        h.stream.stop().expect("stop");
        let polled = h.stream.poll();
        assert!(matches!(polled.result, Err(AudioError::StreamEnded { .. })));
        assert_eq!(polled.quiet, None);
        assert!(!a.expired(polled.quiet, later + timeout * 2));
    }

    #[test]
    fn a_quiet_poll_is_judged_when_it_observed_the_stream_not_when_its_reader_resumes() {
        // A long backoff keeps the recovery running while reader A is away.
        let policy = fast_policy()
            .with_initial_backoff(Duration::from_secs(30))
            .with_max_backoff(Duration::from_secs(30));
        let old = MockStream::new(stereo(RATE), vec![]);
        let h = Harness::new(policy, &old, Opener::new(vec![]).factory());
        let timeout = h.stream.read_timeout;
        let mut a = QuietClock::new(timeout);

        // Reader A's clock starts on a quiet generation 0; A polls it quiet
        // once more and is descheduled right after that poll returns.
        let first = h.stream.poll();
        assert!(!a.expired(first.quiet, first.observed_at));
        let last = h.stream.poll();
        assert_eq!(last.quiet, Some(0));

        // Reader B sees the loss and starts a recovery.
        old.kill("device unplugged");
        assert!(matches!(h.stream.try_read_chunk(), Ok(None)));
        assert!(matches!(
            h.hub.snapshot().state,
            RecoveryState::Recovering { .. }
        ));

        // A resumes after its timeout. Judged at the resume instant, as before,
        // its stale poll would time out on a stream that is recovering...
        let resumed = first.observed_at + timeout;
        let mut judged_at_resume = QuietClock {
            timeout,
            since: a.since,
        };
        assert!(judged_at_resume.expired(last.quiet, resumed));
        // ...but it is judged at the instant it observed the stream, which was
        // quiet for far less than the timeout.
        assert!(
            !a.expired(last.quiet, last.observed_at),
            "a stale poll cannot charge later recovery time to the generation"
        );
        // A's next poll sees the recovery, which restarts the clock.
        let next = h.stream.poll();
        assert_eq!(next.quiet, None);
        assert!(!a.expired(next.quiet, next.observed_at));
        h.stream.stop().expect("stop");
    }

    #[test]
    fn a_stop_whose_teardown_outlasts_the_read_timeout_ends_blocking_reads_after_the_tail() {
        let (old, teardown) = MockStream::new(
            stereo(RATE),
            vec![Step::Buffer(buffer(0)), Step::Buffer(buffer(10))],
        )
        .gated_stop();
        let h = Harness::build(
            fast_policy(),
            &old,
            Opener::new(vec![]).factory(),
            RecoveryTrigger::default(),
            |stream| stream.read_timeout = ms(20),
        );
        let stopper = Arc::clone(&h.stream);
        let stopping = std::thread::spawn(move || stopper.stop());
        // stop() preserved the tail and is now wedged in the backend's
        // teardown, and stays so across every read below.
        teardown.wait_reached();

        // Every read runs on a worker and is awaited with a timeout, so a read
        // that waits for the teardown fails the test instead of hanging it.
        let reader = Arc::clone(&h.stream);
        let (reads_tx, reads_rx) = mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..3 {
                if reads_tx.send(reader.read_chunk()).is_err() {
                    return;
                }
            }
        });
        let mut reads = Vec::new();
        for _ in 0..3 {
            match reads_rx.recv_timeout(Duration::from_secs(2)) {
                Ok(read) => reads.push(read),
                Err(_) => {
                    teardown.open();
                    panic!("a read after stop() waited for the backend's teardown");
                }
            }
        }
        let still_wedged = !old.stopped();
        teardown.open();

        let mut reads = reads.into_iter();
        let tail: Vec<AudioBuffer> = reads
            .by_ref()
            .take(2)
            .map(|read| read.expect("the preserved tail"))
            .collect();
        let end = reads.next().expect("a third read");
        assert_eq!(tail[0].timestamp(), Some(ms(0)));
        assert_eq!(tail[1].timestamp(), Some(ms(10)));
        assert!(
            matches!(end, Err(AudioError::StreamEnded { .. })),
            "a stopped read ends, it never times out: {end:?}"
        );
        assert!(
            still_wedged,
            "the backend's teardown was still wedged across the reads"
        );
        stopping.join().expect("stopper thread").expect("stop");
        assert!(old.stopped());
    }

    #[test]
    fn buffers_captured_counts_what_the_caller_receives_not_what_stop_preserved() {
        let old = MockStream::new(stereo(RATE), backlog(3)).strict_stop();
        let h = Harness::new(fast_policy(), &old, Opener::new(vec![]).factory());
        read_n(&h.stream, 1);
        assert_eq!(h.stream.buffers_captured(), 1);

        h.stream.stop().expect("stop");

        assert_eq!(
            old.delivered(),
            3,
            "stop() took the queued tail off the backend"
        );
        assert_eq!(
            h.stream.buffers_captured(),
            1,
            "the preserved tail is not counted before a read returns it"
        );
        let (tail, err) = read_until_err(&h.stream);
        assert_eq!(tail.len(), 2);
        assert!(matches!(err, AudioError::StreamEnded { .. }), "{err:?}");
        assert_eq!(
            h.stream.buffers_captured(),
            3,
            "every buffer the caller received, and only those"
        );
    }

    #[test]
    fn a_terminal_poll_leaves_the_rejected_stream_and_the_watcher_to_a_disposal_thread() {
        let old = MockStream::new(
            stereo(RATE),
            vec![Step::Buffer(buffer(0)), Step::End("device unplugged")],
        );
        // The rejected replacement's stop() and the watcher's teardown both
        // wedge until released.
        let (new, stop) = mono_impostor().gated_stop();
        let (teardown, unsubscribe) = gate();
        let (watcher, unsubscribed) = mock_watcher(Some(teardown));
        let h = Harness::new(
            fast_policy(),
            &old,
            Opener::new(vec![Ok(new.clone())]).factory(),
        );
        *lock(&h.stream.watcher) = Some(watcher);
        let reader = Arc::clone(&h.stream);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(read_until_err(&reader));
        });

        let (buffers, err) = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("the terminal poll must not wait for a stop() or a watcher teardown");

        assert_eq!(buffers.len(), 1, "only generation 0's buffer is delivered");
        match err {
            AudioError::StreamEnded { reason } => assert_eq!(reason, FORMAT_CHANGED_REASON),
            other => panic!("expected StreamEnded, got {other:?}"),
        }
        assert_eq!(new.delivered(), 1, "read by recovery, never delivered");
        assert!(
            !new.stopped(),
            "its stop() is wedged on the disposal thread"
        );
        assert!(
            !unsubscribed.load(Ordering::SeqCst),
            "the watcher is not torn down yet"
        );
        assert!(
            lock(&h.stream.watcher).is_none(),
            "the terminal poll handed the watcher off"
        );
        stop.open();
        wait_for(|| new.stopped());
        unsubscribe.open();
        wait_for(|| unsubscribed.load(Ordering::SeqCst));
        assert_eq!(h.hub.snapshot().state, RecoveryState::Exhausted);
    }

    #[test]
    fn what_a_poll_cannot_hand_off_is_kept_for_stop_or_drop_and_never_released_inline() {
        for by_stop in [true, false] {
            let old = MockStream::new(
                stereo(RATE),
                vec![Step::Buffer(buffer(0)), Step::End("device unplugged")],
            );
            let new = mono_impostor();
            let (watcher, unsubscribed) = mock_watcher(None);
            let h = Harness::build(
                fast_policy(),
                &old,
                Opener::new(vec![Ok(new.clone())]).factory(),
                RecoveryTrigger::default(),
                // As if no disposal thread could be spawned.
                |stream| stream.disposer = Box::new(Err::<(), Retired>),
            );
            *lock(&h.stream.watcher) = Some(watcher);

            let (buffers, err) = read_until_err(&h.stream);

            assert_eq!(buffers.len(), 1, "by stop: {by_stop}");
            assert!(
                matches!(err, AudioError::StreamEnded { ref reason } if reason == FORMAT_CHANGED_REASON),
                "by stop: {by_stop}: {err:?}"
            );
            assert!(
                !new.stopped(),
                "by stop: {by_stop}: the poll stopped nothing itself"
            );
            assert!(
                !unsubscribed.load(Ordering::SeqCst),
                "by stop: {by_stop}: nor tore the watcher down"
            );
            if by_stop {
                h.stream.stop().expect("stop");
            } else {
                let Harness { stream, .. } = h;
                drop(stream);
            }
            assert!(
                new.stopped(),
                "by stop: {by_stop}: released with the capture"
            );
            assert!(
                unsubscribed.load(Ordering::SeqCst),
                "by stop: {by_stop}: released with the capture"
            );
        }
    }

    #[test]
    fn a_failed_handoff_racing_stop_is_released_by_stop_never_by_the_reader() {
        // Handoff first: the reader's poll is inside a failing disposal
        // handoff when stop() starts.
        {
            let old = MockStream::new(
                stereo(RATE),
                vec![Step::Buffer(buffer(0)), Step::End("device unplugged")],
            );
            let new = mono_impostor();
            let (handoff, in_handoff) = gate();
            let handoff = Mutex::new(Some(handoff));
            let h = Harness::build(
                fast_policy(),
                &old,
                Opener::new(vec![Ok(new.clone())]).factory(),
                RecoveryTrigger::default(),
                // No disposal thread can be spawned, and finding that out is
                // held at a gate.
                move |stream| {
                    stream.disposer = Box::new(move |retired: Retired| -> Result<(), Retired> {
                        let held = lock(&handoff).take();
                        if let Some(held) = held {
                            held.pass();
                        }
                        Err(retired)
                    })
                },
            );
            let reader = Arc::clone(&h.stream);
            let reading =
                std::thread::spawn(move || (std::thread::current().id(), read_until_err(&reader)));
            in_handoff.wait_reached();
            let stopper = Arc::clone(&h.stream);
            let stopping =
                std::thread::spawn(move || (std::thread::current().id(), stopper.stop()));
            wait_for(|| h.stream.core.stop_requested.load(Ordering::SeqCst));
            assert!(
                !stopping.is_finished(),
                "handoff first: stop() waits for the handoff in progress"
            );
            in_handoff.open();
            let (stopper_id, stopped) = stopping.join().expect("stopper thread");
            stopped.expect("stop");

            assert!(
                new.stopped(),
                "handoff first: stopped by the time stop() returned"
            );
            assert_eq!(new.stopped_by(), Some(stopper_id), "handoff first");
            let (reader_id, (buffers, err)) = reading.join().expect("reader thread");
            assert_ne!(
                new.stopped_by(),
                Some(reader_id),
                "handoff first: never by the reader"
            );
            assert_eq!(buffers.len(), 1, "handoff first");
            assert!(
                matches!(err, AudioError::StreamEnded { ref reason } if reason == FORMAT_CHANGED_REASON),
                "handoff first: {err:?}"
            );
        }

        // Stop first: stop() lands while the rejected replacement is still
        // installed, and the reader's failing handoff only comes after it.
        {
            let old = MockStream::new(
                stereo(RATE),
                vec![Step::Buffer(buffer(0)), Step::End("device unplugged")],
            );
            // Reports the baseline format; its mono buffer arrives once it is
            // installed.
            let new = MockStream::new(stereo(RATE), vec![]);
            let h = Harness::build(
                fast_policy(),
                &old,
                Opener::new(vec![Ok(new.clone())]).factory(),
                RecoveryTrigger::default(),
                |stream| stream.disposer = Box::new(Err::<(), Retired>),
            );
            assert_eq!(read_n(&h.stream, 1)[0].stream_generation(), 0);
            wait_for(|| {
                assert!(matches!(h.stream.try_read_chunk(), Ok(None)));
                h.hub.snapshot().generation == 1
            });
            new.push(Step::Buffer(mono_buffer()));
            let stopper = Arc::clone(&h.stream);
            let (stopper_id, stopped) =
                std::thread::spawn(move || (std::thread::current().id(), stopper.stop()))
                    .join()
                    .expect("stopper thread");
            stopped.expect("stop");
            assert!(
                new.stopped(),
                "stop first: stopped by the time stop() returned"
            );
            assert_eq!(new.stopped_by(), Some(stopper_id), "stop first");

            // The reader now meets the mono buffer stop() preserved, rejects
            // it, and fails to hand the detached stream off.
            let (buffers, err) = read_until_err(&h.stream);

            assert!(buffers.is_empty(), "stop first");
            assert!(
                matches!(err, AudioError::StreamEnded { ref reason } if reason == FORMAT_CHANGED_REASON),
                "stop first: {err:?}"
            );
            assert_eq!(
                new.stopped_by(),
                Some(stopper_id),
                "stop first: never by the reader"
            );
        }
    }

    #[test]
    fn a_session_exhausted_by_a_failed_reopen_spawn_retires_the_stream_still_installed() {
        // A proactive switch whose reopen helper cannot be spawned, with no
        // budget left after it: no helper ever retires the old, still live
        // stream.
        let old = MockStream::new(stereo(RATE), vec![]);
        let opener = Opener::new(vec![]);
        let h = Harness::build(
            fast_policy()
                .with_max_attempts(1)
                .with_settle_delay(Duration::ZERO),
            &old,
            opener.factory(),
            RecoveryTrigger::default(),
            |stream| {
                stream.spawn_reopen = |_| Err(std::io::Error::other("no thread for the reopen"))
            },
        );
        h.trigger.trigger_default_change();

        let (buffers, err) = read_until_err(&h.stream);

        assert!(buffers.is_empty());
        match err {
            AudioError::StreamEnded { reason } => assert!(reason.contains("exhausted"), "{reason}"),
            other => panic!("expected StreamEnded, got {other:?}"),
        }
        assert_eq!(opener.calls(), 0, "no reopen helper ever ran");
        assert_eq!(h.hub.snapshot().state, RecoveryState::Exhausted);
        // Without any stop(): the exhausted session hands the stream to the
        // disposal thread.
        wait_for(|| old.stopped());
        assert_ne!(
            old.stopped_by(),
            Some(std::thread::current().id()),
            "stopped off the read path"
        );
    }

    #[test]
    fn a_stopped_but_draining_generation_delivers_its_tail_without_restoring_the_budget() {
        let stable = ms(20);
        let policy = fast_policy()
            .with_max_attempts(1)
            .with_stable_period(stable);
        let draining = MockStream::new(
            stereo(RATE),
            vec![Step::Buffer(buffer(0)), Step::Buffer(buffer(10))],
        );
        let spare = MockStream::new(stereo(RATE), vec![Step::Buffer(buffer(0))]);
        let opener = Opener::new(vec![Ok(spare)]);
        let h = Harness::new(policy, &draining, opener.factory());

        // As if `draining` were a replacement that spent the only attempt.
        let installed = {
            let mut m = lock(&h.stream.machine);
            m.attempt = 1;
            let installed = Instant::now();
            m.healthy_since = Some(installed);
            installed
        };
        // The backend stops on its own (a `Stopping` bridge) with two buffers
        // still queued, and is read only after its stable period.
        draining.stop().expect("backend stop");
        assert!(!draining.is_running());
        wait_for(|| Instant::now() >= installed + stable * 2);

        let tail = read_n(&h.stream, 2);

        assert_eq!(tail[0].timestamp(), Some(ms(0)), "the tail is delivered");
        assert_eq!(tail[1].timestamp(), Some(ms(10)));
        assert_eq!(
            lock(&h.stream.machine).attempt,
            1,
            "draining a stopped generation is no evidence it stayed healthy"
        );
        let (buffers, err) = read_until_err(&h.stream);
        assert!(buffers.is_empty());
        match err {
            AudioError::StreamEnded { reason } => assert!(reason.contains("exhausted"), "{reason}"),
            other => panic!("expected StreamEnded, got {other:?}"),
        }
        assert_eq!(opener.calls(), 0, "the spent budget was not restored");
    }

    #[cfg(feature = "async-stream")]
    #[test]
    fn an_async_reader_receives_the_tail_a_stop_preserves_after_its_empty_read() {
        use crate::bridge::AsyncAudioStream;
        use futures_core::Stream;
        use std::pin::Pin;
        use std::task::{Context, Poll, Waker};

        let live = MockStream::new(stereo(RATE), vec![]);
        let h = Harness::new(fast_policy(), &live, Opener::new(vec![]).factory());
        let mut cx = Context::from_waker(Waker::noop());
        let mut reader = AsyncAudioStream::new(h.stream.as_ref());

        // stop() is requested and then held before it preserves the tail:
        // its next step takes the hub's status lock, held here.
        let held = lock(&h.hub.status);
        let stopper = Arc::clone(&h.stream);
        let stopping = std::thread::spawn(move || stopper.stop());
        wait_for(|| h.stream.core.stop_requested.load(Ordering::SeqCst));

        // The async reader finds the backend empty. The stream does not report
        // itself stopped yet, so the reader keeps polling instead of ending.
        assert!(matches!(
            Pin::new(&mut reader).poll_next(&mut cx),
            Poll::Pending
        ));
        // A buffer arrives before stop() reaches the tail; then stop() runs on.
        live.push(Step::Buffer(buffer(0)));
        drop(held);
        stopping.join().expect("stopper thread").expect("stop");

        match Pin::new(&mut reader).poll_next(&mut cx) {
            Poll::Ready(Some(Ok(preserved))) => assert_eq!(preserved.timestamp(), Some(ms(0))),
            other => panic!("expected the preserved buffer, got {other:?}"),
        }
        assert!(matches!(
            Pin::new(&mut reader).poll_next(&mut cx),
            Poll::Ready(None)
        ));
    }

    /// A supervisor that lost generation 0 and is now in a 30 s backoff.
    fn backing_off() -> Harness {
        let policy = fast_policy()
            .with_initial_backoff(Duration::from_secs(30))
            .with_max_backoff(Duration::from_secs(30));
        let lost = MockStream::new(stereo(RATE), vec![Step::End("device unplugged")]);
        let h = Harness::new(policy, &lost, Opener::new(vec![]).factory());
        assert!(matches!(h.stream.try_read_chunk(), Ok(None)));
        assert!(matches!(
            lock(&h.stream.machine).phase,
            Phase::Backoff { .. }
        ));
        h
    }

    #[test]
    fn a_read_racing_a_stop_during_recovery_ends_only_once_the_stream_reports_itself_stopped() {
        let h = backing_off();

        // stop() is requested and then held before it ends the session: its
        // next step takes the hub's status lock, held here.
        let held = lock(&h.hub.status);
        let stopper = Arc::clone(&h.stream);
        let stopping = std::thread::spawn(move || stopper.stop());
        wait_for(|| h.stream.core.stop_requested.load(Ordering::SeqCst));

        assert!(
            matches!(h.stream.try_read_chunk(), Ok(None)),
            "no end is reported while the stream still reports itself running"
        );
        assert!(h.stream.is_running());
        drop(held);
        stopping.join().expect("stopper thread").expect("stop");

        match h.stream.try_read_chunk() {
            Err(AudioError::StreamEnded { reason }) => {
                assert_eq!(reason, STOPPED_DURING_RECOVERY_REASON)
            }
            other => panic!("expected StreamEnded, got {other:?}"),
        }
        assert!(!h.stream.is_running());
    }

    #[cfg(feature = "async-stream")]
    #[test]
    fn an_async_reader_sees_a_stop_during_recovery_as_the_end_of_the_stream_not_an_error() {
        use crate::bridge::AsyncAudioStream;
        use futures_core::Stream;
        use std::pin::Pin;
        use std::task::{Context, Poll, Waker};

        let h = backing_off();
        let mut cx = Context::from_waker(Waker::noop());
        let mut reader = AsyncAudioStream::new(h.stream.as_ref());

        // stop() is requested and then held before it ends the session: its
        // next step takes the hub's status lock, held here.
        let held = lock(&h.hub.status);
        let stopper = Arc::clone(&h.stream);
        let stopping = std::thread::spawn(move || stopper.stop());
        wait_for(|| h.stream.core.stop_requested.load(Ordering::SeqCst));

        // The stream still reports itself running, so an end read here would
        // reach the reader as an error item.
        match Pin::new(&mut reader).poll_next(&mut cx) {
            Poll::Pending => {}
            other => panic!("expected Pending while stop() is in progress, got {other:?}"),
        }
        drop(held);
        stopping.join().expect("stopper thread").expect("stop");

        assert!(matches!(
            Pin::new(&mut reader).poll_next(&mut cx),
            Poll::Ready(None)
        ));
    }
}
