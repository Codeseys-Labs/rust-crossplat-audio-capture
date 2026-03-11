//! AtomicStreamState — lock-free stream lifecycle state machine.
//!
//! Provides [`StreamState`] (the enum of possible states) and [`AtomicStreamState`]
//! (a lock-free atomic wrapper) for cross-thread stream lifecycle coordination.
//!
//! # State Machine
//!
//! ```text
//! Created → Running → Stopping → Stopped → Closed
//!                  ↘               ↗
//!                    Error ←────────
//! ```
//!
//! The OS callback thread (producer) and the consumer thread both read/write
//! the shared state through [`AtomicStreamState`] without locks.

use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};

/// Stream lifecycle states.
///
/// State machine transitions:
/// ```text
/// Created → Running → Stopping → Stopped → Closed
///                  ↘               ↗
///                    Error ←────────
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum StreamState {
    /// Stream created but not yet producing audio.
    Created = 0,
    /// Stream is actively capturing audio.
    Running = 1,
    /// Stop requested, waiting for OS callback to finish.
    Stopping = 2,
    /// Stream has stopped, no more audio data.
    Stopped = 3,
    /// Stream closed, all resources released.
    Closed = 4,
    /// Stream encountered an unrecoverable error.
    Error = 5,
}

impl StreamState {
    /// Convert a raw `u8` back to a [`StreamState`], returning `None` for invalid values.
    pub fn from_u8(value: u8) -> Option<StreamState> {
        match value {
            0 => Some(StreamState::Created),
            1 => Some(StreamState::Running),
            2 => Some(StreamState::Stopping),
            3 => Some(StreamState::Stopped),
            4 => Some(StreamState::Closed),
            5 => Some(StreamState::Error),
            _ => None,
        }
    }
}

impl fmt::Display for StreamState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StreamState::Created => write!(f, "Created"),
            StreamState::Running => write!(f, "Running"),
            StreamState::Stopping => write!(f, "Stopping"),
            StreamState::Stopped => write!(f, "Stopped"),
            StreamState::Closed => write!(f, "Closed"),
            StreamState::Error => write!(f, "Error"),
        }
    }
}

/// Lock-free atomic stream state for cross-thread state management.
///
/// Used by both the OS callback thread (producer) and the consumer thread
/// to coordinate stream lifecycle without locks.
///
/// # Examples
///
/// ```ignore
/// use rsac::bridge::state::{AtomicStreamState, StreamState};
///
/// let state = AtomicStreamState::new(StreamState::Created);
/// assert_eq!(state.get(), StreamState::Created);
///
/// // CAS transition: Created → Running
/// assert!(state.transition(StreamState::Created, StreamState::Running).is_ok());
/// assert!(state.is_running());
/// ```
pub struct AtomicStreamState {
    state: AtomicU8,
}

impl AtomicStreamState {
    /// Create a new [`AtomicStreamState`] with the given initial state.
    pub fn new(initial: StreamState) -> Self {
        Self {
            state: AtomicU8::new(initial as u8),
        }
    }

    /// Read the current state (Acquire ordering).
    pub fn get(&self) -> StreamState {
        let raw = self.state.load(Ordering::Acquire);
        // SAFETY: We only ever store valid StreamState discriminants.
        StreamState::from_u8(raw).expect("AtomicStreamState contains invalid discriminant")
    }

    /// Attempt a compare-and-swap transition from `from` to `to`.
    ///
    /// Uses `compare_exchange` with `AcqRel`/`Acquire` ordering.
    ///
    /// # Returns
    /// - `Ok(())` if the transition succeeded (current state was `from`, now `to`).
    /// - `Err(actual_state)` if the current state was not `from`.
    ///
    /// # Note
    /// This method does **not** validate whether `from → to` is a legal state
    /// transition. That responsibility lies with the caller (e.g., `BridgeStream`).
    /// This keeps `AtomicStreamState` simple and reusable.
    pub fn transition(&self, from: StreamState, to: StreamState) -> Result<(), StreamState> {
        match self
            .state
            .compare_exchange(from as u8, to as u8, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => Ok(()),
            Err(actual) => Err(StreamState::from_u8(actual)
                .expect("AtomicStreamState contains invalid discriminant")),
        }
    }

    /// Returns `true` if the stream is currently in the [`StreamState::Running`] state.
    pub fn is_running(&self) -> bool {
        self.get() == StreamState::Running
    }

    /// Returns `true` if the stream is in [`StreamState::Stopped`] or [`StreamState::Closed`].
    pub fn is_stopped(&self) -> bool {
        matches!(self.get(), StreamState::Stopped | StreamState::Closed)
    }

    /// Returns `true` if the stream is in a terminal state:
    /// [`StreamState::Stopped`], [`StreamState::Closed`], or [`StreamState::Error`].
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.get(),
            StreamState::Stopped | StreamState::Closed | StreamState::Error
        )
    }

    /// Unconditionally set the state (Release ordering).
    ///
    /// Intended for error recovery paths where a CAS loop is not needed.
    pub fn force_set(&self, state: StreamState) {
        self.state.store(state as u8, Ordering::Release);
    }

    /// Returns `true` if data can be read from the stream.
    ///
    /// Data is readable when the stream is [`StreamState::Running`] or
    /// [`StreamState::Stopping`] (draining remaining buffered data).
    pub fn is_readable(&self) -> bool {
        matches!(self.get(), StreamState::Running | StreamState::Stopping)
    }
}

impl fmt::Debug for AtomicStreamState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AtomicStreamState")
            .field("state", &self.get())
            .finish()
    }
}

impl Default for AtomicStreamState {
    fn default() -> Self {
        Self::new(StreamState::Created)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_initial_state() {
        let state = AtomicStreamState::new(StreamState::Created);
        assert_eq!(state.get(), StreamState::Created);
    }

    #[test]
    fn test_successful_transition() {
        let state = AtomicStreamState::new(StreamState::Created);

        // Created → Running
        assert!(state
            .transition(StreamState::Created, StreamState::Running)
            .is_ok());
        assert_eq!(state.get(), StreamState::Running);

        // Running → Stopping
        assert!(state
            .transition(StreamState::Running, StreamState::Stopping)
            .is_ok());
        assert_eq!(state.get(), StreamState::Stopping);

        // Stopping → Stopped
        assert!(state
            .transition(StreamState::Stopping, StreamState::Stopped)
            .is_ok());
        assert_eq!(state.get(), StreamState::Stopped);

        // Stopped → Closed
        assert!(state
            .transition(StreamState::Stopped, StreamState::Closed)
            .is_ok());
        assert_eq!(state.get(), StreamState::Closed);
    }

    #[test]
    fn test_failed_transition() {
        let state = AtomicStreamState::new(StreamState::Created);

        // Try to transition from Running (but we're in Created)
        let result = state.transition(StreamState::Running, StreamState::Stopping);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), StreamState::Created);

        // State should be unchanged
        assert_eq!(state.get(), StreamState::Created);
    }

    #[test]
    fn test_is_running() {
        let state = AtomicStreamState::new(StreamState::Created);
        assert!(!state.is_running());

        state.force_set(StreamState::Running);
        assert!(state.is_running());

        state.force_set(StreamState::Stopping);
        assert!(!state.is_running());

        state.force_set(StreamState::Stopped);
        assert!(!state.is_running());

        state.force_set(StreamState::Closed);
        assert!(!state.is_running());

        state.force_set(StreamState::Error);
        assert!(!state.is_running());
    }

    #[test]
    fn test_is_stopped() {
        let state = AtomicStreamState::new(StreamState::Created);
        assert!(!state.is_stopped());

        state.force_set(StreamState::Running);
        assert!(!state.is_stopped());

        state.force_set(StreamState::Stopping);
        assert!(!state.is_stopped());

        state.force_set(StreamState::Stopped);
        assert!(state.is_stopped());

        state.force_set(StreamState::Closed);
        assert!(state.is_stopped());

        state.force_set(StreamState::Error);
        assert!(!state.is_stopped());
    }

    #[test]
    fn test_is_terminal() {
        let state = AtomicStreamState::new(StreamState::Created);
        assert!(!state.is_terminal());

        state.force_set(StreamState::Running);
        assert!(!state.is_terminal());

        state.force_set(StreamState::Stopping);
        assert!(!state.is_terminal());

        state.force_set(StreamState::Stopped);
        assert!(state.is_terminal());

        state.force_set(StreamState::Closed);
        assert!(state.is_terminal());

        state.force_set(StreamState::Error);
        assert!(state.is_terminal());
    }

    #[test]
    fn test_is_readable() {
        let state = AtomicStreamState::new(StreamState::Created);
        assert!(!state.is_readable());

        state.force_set(StreamState::Running);
        assert!(state.is_readable());

        state.force_set(StreamState::Stopping);
        assert!(state.is_readable());

        state.force_set(StreamState::Stopped);
        assert!(!state.is_readable());

        state.force_set(StreamState::Closed);
        assert!(!state.is_readable());

        state.force_set(StreamState::Error);
        assert!(!state.is_readable());
    }

    #[test]
    fn test_force_set() {
        let state = AtomicStreamState::new(StreamState::Created);

        // Force to Error from Created (skipping normal transitions)
        state.force_set(StreamState::Error);
        assert_eq!(state.get(), StreamState::Error);

        // Force back to Running (error recovery)
        state.force_set(StreamState::Running);
        assert_eq!(state.get(), StreamState::Running);
    }

    #[test]
    fn test_default() {
        let state = AtomicStreamState::default();
        assert_eq!(state.get(), StreamState::Created);
    }

    #[test]
    fn test_display() {
        assert_eq!(format!("{}", StreamState::Created), "Created");
        assert_eq!(format!("{}", StreamState::Running), "Running");
        assert_eq!(format!("{}", StreamState::Stopping), "Stopping");
        assert_eq!(format!("{}", StreamState::Stopped), "Stopped");
        assert_eq!(format!("{}", StreamState::Closed), "Closed");
        assert_eq!(format!("{}", StreamState::Error), "Error");
    }

    #[test]
    fn test_stream_state_from_u8() {
        // Valid round-trips
        assert_eq!(StreamState::from_u8(0), Some(StreamState::Created));
        assert_eq!(StreamState::from_u8(1), Some(StreamState::Running));
        assert_eq!(StreamState::from_u8(2), Some(StreamState::Stopping));
        assert_eq!(StreamState::from_u8(3), Some(StreamState::Stopped));
        assert_eq!(StreamState::from_u8(4), Some(StreamState::Closed));
        assert_eq!(StreamState::from_u8(5), Some(StreamState::Error));

        // Round-trip: enum → u8 → enum
        for &s in &[
            StreamState::Created,
            StreamState::Running,
            StreamState::Stopping,
            StreamState::Stopped,
            StreamState::Closed,
            StreamState::Error,
        ] {
            assert_eq!(StreamState::from_u8(s as u8), Some(s));
        }

        // Invalid values
        assert_eq!(StreamState::from_u8(6), None);
        assert_eq!(StreamState::from_u8(255), None);
        assert_eq!(StreamState::from_u8(100), None);
    }

    #[test]
    fn test_concurrent_transitions() {
        use std::sync::atomic::AtomicUsize;
        use std::thread;

        let state = Arc::new(AtomicStreamState::new(StreamState::Created));
        let success_count = Arc::new(AtomicUsize::new(0));
        let num_threads = 10;

        let mut handles = Vec::with_capacity(num_threads);

        for _ in 0..num_threads {
            let state = Arc::clone(&state);
            let success_count = Arc::clone(&success_count);

            handles.push(thread::spawn(move || {
                if state
                    .transition(StreamState::Created, StreamState::Running)
                    .is_ok()
                {
                    success_count.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }

        for handle in handles {
            handle.join().expect("Thread panicked");
        }

        // Exactly one thread should have succeeded with the CAS
        assert_eq!(success_count.load(Ordering::Relaxed), 1);
        assert_eq!(state.get(), StreamState::Running);
    }

    #[test]
    fn test_debug_format() {
        let state = AtomicStreamState::new(StreamState::Running);
        let debug_str = format!("{:?}", state);
        assert!(debug_str.contains("AtomicStreamState"));
        assert!(debug_str.contains("Running"));
    }
}
