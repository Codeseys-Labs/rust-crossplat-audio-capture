//! Policy and observability for automatic recovery of desktop default-device capture.

use std::time::Duration;

/// Policy controlling automatic recovery when a capture device changes or is invalidated.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct AutoRecoveryConfig {
    /// Maximum consecutive failed reopen attempts before entering [`RecoveryState::Exhausted`] (default: 5).
    pub max_attempts: u32,
    /// Initial backoff delay before the first retry (default: 100 ms).
    pub initial_backoff: Duration,
    /// Upper bound on the exponential backoff between reopen attempts (default: 2 s).
    pub max_backoff: Duration,
    /// How long a recovered stream must be seen alive after it is installed to reset
    /// the failed-attempt budget (default: 60 s). The period runs from the instant the
    /// supervisor installs the stream (generation 0: when the session opens) to a later
    /// read that finds it alive; a loss noticed late is not evidence of health.
    pub stable_period: Duration,
    /// Debounce window for coalescing rapid device-change notifications (default: 200 ms).
    pub settle_delay: Duration,
    /// Maximum time to wait for a device to reappear when none is present (default: 30 s).
    /// It also bounds a probe still in flight when it expires: a probe whose result is
    /// published after the deadline is discarded and the wait ends.
    pub device_wait_timeout: Duration,
}

impl Default for AutoRecoveryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(2),
            stable_period: Duration::from_secs(60),
            settle_delay: Duration::from_millis(200),
            device_wait_timeout: Duration::from_secs(30),
        }
    }
}

impl AutoRecoveryConfig {
    /// Returns the default recovery policy. Constructing a policy does not enable recovery.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the number of consecutive failed attempts the supervisor allows.
    #[must_use]
    pub fn with_max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = max_attempts;
        self
    }

    /// Sets the delay before the first retry, subject to [`Self::max_backoff`].
    #[must_use]
    pub fn with_initial_backoff(mut self, initial_backoff: Duration) -> Self {
        self.initial_backoff = initial_backoff;
        self
    }

    /// Caps the delay for every retry, including the first one.
    #[must_use]
    pub fn with_max_backoff(mut self, max_backoff: Duration) -> Self {
        self.max_backoff = max_backoff;
        self
    }

    /// Sets how long a recovered stream must be seen alive after it is installed to
    /// reset its retry budget.
    #[must_use]
    pub fn with_stable_period(mut self, stable_period: Duration) -> Self {
        self.stable_period = stable_period;
        self
    }

    /// Sets how long the supervisor waits for device-change notifications to settle.
    #[must_use]
    pub fn with_settle_delay(mut self, settle_delay: Duration) -> Self {
        self.settle_delay = settle_delay;
        self
    }

    /// Sets how long recovery waits for an absent device to become available.
    #[must_use]
    pub fn with_device_wait_timeout(mut self, device_wait_timeout: Duration) -> Self {
        self.device_wait_timeout = device_wait_timeout;
        self
    }

    /// Returns the delay for a one-based retry number. Attempt 1 uses the initial
    /// delay; each subsequent attempt doubles it, up to the configured maximum.
    /// Attempt 0 is treated as attempt 1. A zero initial delay stays zero.
    pub fn backoff_for_attempt(&self, attempt: u32) -> Duration {
        let mut delay = self.initial_backoff.min(self.max_backoff);
        if delay.is_zero() {
            return delay;
        }
        let mut doublings = attempt.saturating_sub(1);
        while doublings > 0 && delay < self.max_backoff {
            // Duration's multiplier is a u32. Scale in chunks so even u32::MAX
            // attempts reach the cap in at most four steps from one nanosecond.
            let shift = doublings.min(31);
            delay = delay.saturating_mul(1 << shift).min(self.max_backoff);
            doublings -= shift;
        }
        delay
    }
}

/// Lifecycle of a capture session supervised for automatic recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecoveryState {
    /// The session has not started capturing.
    Idle,
    /// A backend stream is active and no recovery is in progress.
    Running,
    /// The current stream is unavailable and the supervisor is arranging a replacement.
    Recovering {
        /// Current reopen attempt number.
        attempt: u32,
    },
    /// Automatic recovery ended without a replacement stream.
    Exhausted,
    /// The session was stopped; it will not publish another generation.
    Stopped,
}

/// Point-in-time view of recovery state and counters for one capture session.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RecoverySnapshot {
    /// Capture session the snapshot describes: `0` before any session opened, then a
    /// new id for each newly opened capture session — an `AudioCapture::start()` that
    /// opens a stream. A `start()` on a running handle is a no-op and a failed open
    /// starts no session; neither changes the id. Events carry the id of the session
    /// that emitted them, so events a previous session left queued before a restart
    /// can be told apart from the current session's.
    pub session: u64,
    /// Supervisor state when the snapshot was taken.
    pub state: RecoveryState,
    /// Identifier of the most recently published stream generation; restarts at 0
    /// with every session.
    pub generation: u64,
    /// Current attempt within the supervisor's retry budget.
    pub attempt: u32,
    /// Number of replacement streams successfully published for this session.
    pub successful_recoveries: u64,
    /// Number of recovery events discarded because the subscriber's queue was full.
    pub dropped_events: u64,
}

/// Notification of a device change or recovery decision within a capture session.
///
/// Every event carries the `session` that emitted it (see [`RecoverySnapshot::session`]):
/// the subscription spans stop/start restarts, and generation numbers restart at 0 with
/// each session, so the session id is what distinguishes events a previous session left
/// queued from the current session's.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecoveryEvent {
    /// The system's default capture target changed.
    DefaultChanged {
        /// Capture session that observed the change.
        session: u64,
        /// Generation that was current when the change was observed.
        generation: u64,
    },
    /// The backend can no longer deliver data from the current device.
    DeviceInvalidated {
        /// Capture session whose stream became invalid.
        session: u64,
        /// Generation whose backend stream became invalid.
        generation: u64,
        /// Diagnostic description of the invalidation.
        reason: String,
    },
    /// The supervisor scheduled another attempt to open a replacement stream.
    RetryScheduled {
        /// Capture session the attempt belongs to.
        session: u64,
        /// Generation the supervisor is attempting to replace.
        generation: u64,
        /// One-based number of the scheduled attempt.
        attempt: u32,
        /// Backoff before the scheduled attempt.
        delay: Duration,
    },
    /// A replacement stream is ready to supply audio.
    Recovered {
        /// Capture session the replacement belongs to.
        session: u64,
        /// Newly published stream generation, also carried by its audio buffers.
        generation: u64,
        /// Measured duration of the interruption before this generation was published.
        outage: Duration,
    },
    /// Recovery ended without a replacement; no further attempts will be made.
    Exhausted {
        /// Capture session whose recovery ended.
        session: u64,
        /// Last published generation before recovery ended.
        generation: u64,
        /// Number of attempts spent before recovery ended.
        attempts: u32,
        /// Diagnostic description of the terminal failure.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy() {
        assert_eq!(
            AutoRecoveryConfig::new(),
            AutoRecoveryConfig {
                max_attempts: 5,
                initial_backoff: Duration::from_millis(100),
                max_backoff: Duration::from_secs(2),
                stable_period: Duration::from_secs(60),
                settle_delay: Duration::from_millis(200),
                device_wait_timeout: Duration::from_secs(30),
            }
        );
    }

    #[test]
    fn backoff_doubles_until_capped() {
        let policy = AutoRecoveryConfig::new();
        for (attempt, millis) in [(0, 100), (1, 100), (2, 200), (5, 1600), (6, 2000)] {
            assert_eq!(
                policy.backoff_for_attempt(attempt),
                Duration::from_millis(millis)
            );
        }
        let capped = policy.with_max_backoff(Duration::from_millis(30));
        assert_eq!(capped.backoff_for_attempt(1), Duration::from_millis(30));
    }

    #[test]
    fn extreme_attempts_and_durations_do_not_overflow() {
        let policy = AutoRecoveryConfig::new()
            .with_initial_backoff(Duration::from_nanos(1))
            .with_max_backoff(Duration::MAX);
        assert_eq!(
            policy.backoff_for_attempt(64),
            Duration::from_nanos(1 << 63)
        );
        assert_eq!(policy.backoff_for_attempt(u32::MAX), Duration::MAX);
        let zero = policy.with_initial_backoff(Duration::ZERO);
        assert_eq!(zero.backoff_for_attempt(u32::MAX), Duration::ZERO);
    }
}
