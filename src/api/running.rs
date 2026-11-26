//! [`RunningCapture`]: the RAII guard returned by
//! [`AudioCaptureBuilder::start`](crate::api::AudioCaptureBuilder::start).

use super::capture::AudioCapture;
use super::delivery::DrainHandle;

use crate::core::error::AudioResult;
use std::fmt;
use std::ops::{Deref, DerefMut};

/// An RAII guard wrapping a started [`AudioCapture`].
///
/// Returned by [`AudioCaptureBuilder::start()`](crate::api::AudioCaptureBuilder::start).
/// It exists so that the common
/// "build, start, then use" path is a single call whose result also tears the
/// capture down deterministically when it goes out of scope.
///
/// # Deref
///
/// `RunningCapture` implements [`Deref`]`<Target = `[`AudioCapture`]`>` and
/// [`DerefMut`], so the full [`AudioCapture`] surface (reads, subscriptions,
/// stats, `stop`, …) is callable directly on the guard. There is no wrapper
/// boilerplate to keep in sync — new `AudioCapture` methods are reachable
/// automatically.
///
/// # Drop
///
/// Dropping the guard calls [`AudioCapture::stop`] once. `stop()` is idempotent
/// and `Drop`-safe (calling it explicitly *and* then dropping does not error
/// and does not double-stop the underlying stream — the second call is a no-op
/// once the stream is gone). The guard uses a plain [`Drop`] impl rather than
/// keeping the inner capture in a `ManuallyDrop` and reconstructing teardown by
/// hand: the wrapped `AudioCapture`'s own `Drop` already best-effort stops the
/// stream, and this guard's `Drop` simply makes the stop explicit and
/// authoritative on the ergonomic path.
///
/// Use [`into_inner`](Self::into_inner) to take ownership of the wrapped
/// [`AudioCapture`] without triggering the guard's stop.
pub struct RunningCapture(pub(super) AudioCapture);

impl RunningCapture {
    /// Drains captured audio into an [`AudioSink`](crate::sink::AudioSink) on a
    /// dedicated background thread, returning a [`DrainHandle`] that owns it.
    ///
    /// This is the library's first path that actually *drives* the
    /// [`AudioSink`](crate::sink::AudioSink) abstraction. It spawns a thread
    /// (named `rsac-drain`) that **owns** `sink`, reads buffers from the same
    /// ring the manual reads use via the *terminal-observable*
    /// [`try_read_chunk`](crate::core::interface::CapturingStream::try_read_chunk)
    /// path, and for each captured buffer calls
    /// [`sink.write(&buf)`](crate::sink::AudioSink::write). When the stream
    /// reaches a **fatal terminal** state (e.g.
    /// [`AudioError::StreamEnded`](crate::core::error::AudioError::StreamEnded)) the
    /// loop exits and the thread calls [`sink.flush()`](crate::sink::AudioSink::flush)
    /// then [`sink.close()`](crate::sink::AudioSink::close) so the sink is
    /// finalized exactly once.
    ///
    /// The pattern mirrors the callback pump (`spawn_callback_pump`): the sink
    /// runs on this dedicated thread, **never** the OS real-time audio thread, so
    /// a slow sink (disk I/O, encoding) only delays draining — it never stalls
    /// the audio callback (ADR-0001).
    ///
    /// # Error handling inside the drain loop
    ///
    /// - A **recoverable** read error (transient
    ///   [`AudioError::StreamReadError`](crate::core::error::AudioError::StreamReadError),
    ///   overrun/underrun) is logged and retried
    ///   — it never ends draining.
    /// - A **fatal** read error ends the loop, after which `flush()`/`close()`
    ///   still run so a file sink's header is finalized.
    /// - A **recoverable** `write()` error is logged and the loop continues; a
    ///   **fatal** `write()` error (e.g. a `WavFileSink` format mismatch, or
    ///   disk-full mapped to a fatal `AudioError`) ends the loop — but
    ///   `flush()`/`close()` are still attempted so the sink is left in the
    ///   most-finalized state possible.
    ///
    /// # Lifecycle
    ///
    /// The returned [`DrainHandle`] joins the thread on
    /// [`shutdown()`](DrainHandle::shutdown) or `Drop` (signal + join,
    /// self-join-guarded like the callback pump). Because the stream reaching a
    /// terminal state ends the loop on its own, you do not have to call
    /// `shutdown()` for the thread to exit — but doing so (or dropping the
    /// handle) joins it deterministically and guarantees `flush()`/`close()` have
    /// completed.
    ///
    /// # Competes with other readers
    ///
    /// The drain thread competes with [`read_buffer`](AudioCapture::read_buffer),
    /// [`subscribe`](AudioCapture::subscribe), and the callback pump for buffers
    /// from the same ring (a single logical consumer per buffer). **Do not** mix
    /// `drain_to` with manual reads on the same capture.
    ///
    /// # Accepted stream states (drain-the-tail; rsac-7aa2)
    ///
    /// Accepted whenever a stream exists: `Running`, the drainable `Stopping`
    /// window (a gracefully-ended stream's buffered tail is drained into the
    /// sink before finalization), or even an already-terminal stream — the
    /// drain thread then exits on its first read and still finalizes the sink
    /// (`flush()` then `close()`), so e.g. a WAV header is written for an empty
    /// capture. The gate is stream **presence** only, chosen over a
    /// `Running || Stopping` state check because (a) `CapturingStream` cannot
    /// distinguish `Stopping` from terminal without consuming a buffer, and
    /// (b) a state check here merely races the stream's own lifecycle — the
    /// drain loop already handles every state honestly, so rejecting at the
    /// gate could strand a drainable tail while accepting adds no hazard.
    /// `Composition::drain_to` (behind the `compose` feature) applies the same
    /// policy, so the two handles agree.
    ///
    /// # Errors
    ///
    /// Returns
    /// [`AudioError::StreamReadError`](crate::core::error::AudioError::StreamReadError)
    /// if the capture has no stream (never started, or stopped — `stop()`
    /// releases the stream), or
    /// [`AudioError::InternalError`](crate::core::error::AudioError::InternalError)
    /// if the drain thread cannot be spawned.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[cfg(feature = "sink-wav")]
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use rsac::{AudioCaptureBuilder, CaptureTarget};
    /// use rsac::core::config::{AudioFormat, SampleFormat};
    /// use rsac::sink::{AudioSink, WavFileSink};
    ///
    /// let capture = AudioCaptureBuilder::new()
    ///     .with_target(CaptureTarget::SystemDefault)
    ///     .start()?;
    /// // Build the sink from the first DELIVERED buffer. Right after `start()`
    /// // a backend may not have published its negotiated format yet, and
    /// // `WavFileSink` rejects a buffer that does not match its header.
    /// let first = capture.read_buffer_blocking()?;
    /// let format = AudioFormat {
    ///     sample_rate: first.sample_rate(),
    ///     channels: first.channels(),
    ///     sample_format: SampleFormat::F32,
    /// };
    /// let mut sink = WavFileSink::new("capture.wav", &format)?;
    /// sink.write(&first)?;
    /// let drain = capture.drain_to(sink)?;
    /// std::thread::sleep(std::time::Duration::from_secs(2));
    /// drain.shutdown(); // flushes + closes the sink, joins the thread
    /// # Ok(())
    /// # }
    /// # #[cfg(not(feature = "sink-wav"))]
    /// # fn main() {}
    /// ```
    pub fn drain_to<S>(&self, sink: S) -> AudioResult<DrainHandle>
    where
        S: crate::sink::AudioSink + 'static,
    {
        // Delegate to the inherent AudioCapture::drain_to so the driver (and its
        // presence-gate + finalization policy) lives in exactly one place and the
        // two surfaces can never drift. Read via `&self` (DerefMut not needed) so
        // we never form a `&mut` alias to the handle.
        self.0.drain_to(sink)
    }

    /// Consumes the guard and returns the wrapped [`AudioCapture`], **without**
    /// stopping it.
    ///
    /// Use this to escape the RAII lifecycle — e.g. to move the capture into a
    /// longer-lived owner or to manage `stop()` manually. Because the returned
    /// `AudioCapture` is moved out before the guard's [`Drop`] runs, the guard
    /// does **not** stop the capture; the caller becomes responsible for its
    /// lifecycle (the `AudioCapture`'s own `Drop` still best-effort stops it).
    pub fn into_inner(self) -> AudioCapture {
        // Move the AudioCapture out of the guard without running RunningCapture's
        // Drop (which would stop the still-running capture). We destructure via
        // ptr::read under ManuallyDrop so the guard's Drop is suppressed but the
        // AudioCapture itself is NOT dropped — it is returned to the caller.
        let this = std::mem::ManuallyDrop::new(self);
        // SAFETY: `this` is a ManuallyDrop, so RunningCapture::drop never runs.
        // We read the single field out exactly once and never touch `this.0`
        // again, so there is no double-move and no double-drop. `this` itself is
        // a ManuallyDrop wrapping a now-moved-from value, so it is never dropped.
        unsafe { std::ptr::read(&this.0) }
    }
}

impl Deref for RunningCapture {
    type Target = AudioCapture;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for RunningCapture {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for RunningCapture {
    fn drop(&mut self) {
        // Deterministic teardown. `stop()` is idempotent and Drop-safe, so an
        // explicit `stop()` before this drop (or the wrapped AudioCapture's own
        // Drop running afterwards) does not double-stop or error.
        let _ = self.0.stop();
    }
}

impl fmt::Debug for RunningCapture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RunningCapture").field(&self.0).finish()
    }
}
