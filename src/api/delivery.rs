//! Background delivery pumps shared by [`AudioCapture`](crate::api::AudioCapture)
//! and the `compose` feature's `Composition`: the sink drain thread
//! ([`DrainHandle`]) and the bounded push-subscription pumps.

use crate::core::buffer::AudioBuffer;
use crate::core::error::{AudioError, AudioResult};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Capacity, in buffers, of the **bounded** channel behind
/// [`AudioCapture::subscribe`](crate::api::AudioCapture::subscribe),
/// [`AudioCapture::subscribe_with_errors`](crate::api::AudioCapture::subscribe_with_errors), and
/// the `compose` feature's `Composition::subscribe{,_with_errors}`.
///
/// 128 buffers is deliberately the same order of magnitude as the bridge ring
/// itself (the composed ring uses 128 slots; at the typical ~10 ms buffer
/// cadence this is ~1.3 s of audio). A subscriber that stalls longer than that
/// starts **losing** buffers — by design: the crate-wide backpressure policy is
/// *drop, don't block, and count it* (ADR-0007). The previous unbounded
/// `mpsc::channel()` instead grew without limit while `overrun_count()` /
/// `backpressure_report()` read healthy, because the pump drained the ring
/// promptly even when the subscriber never caught up (rsac-d6a8).
pub(crate) const SUBSCRIBE_CHANNEL_CAPACITY: usize = 128;

/// Minimum interval between two forwarded **same-variant** recoverable errors
/// on a [`AudioCapture::subscribe_with_errors`](crate::api::AudioCapture::subscribe_with_errors) channel.
///
/// The pump polls at ~1 ms, so a persistently-recoverable stream would
/// otherwise emit ~1000 identical advisory `Err` items per second, flooding
/// the bounded channel and crowding out audio. Repeated recoverable errors of
/// the same variant are therefore coalesced: forwarded at most once per this
/// interval, while an error of a *different* variant is always forwarded
/// immediately (rsac-d6a8).
pub(crate) const RECOVERABLE_ERROR_FORWARD_INTERVAL: Duration = Duration::from_secs(1);

/// Shared drain-thread spawner behind [`RunningCapture::drain_to`](crate::api::RunningCapture::drain_to) and the
/// `compose` feature's `Composition::drain_to`: reads the stream's
/// *terminal-observable* `try_read_chunk` path on a dedicated `rsac-drain`
/// thread that **owns** the sink, writing each buffer and finalizing
/// (`flush()` then `close()`) exactly once when the loop exits.
///
/// Policy (identical for both callers):
/// - recoverable read/write errors are logged and retried — they never end
///   draining;
/// - a **fatal** read error (e.g. the terminal
///   [`AudioError::StreamEnded`]) or a fatal write error ends the loop, after
///   which `flush()`/`close()` still run so a file sink's header is written;
/// - `Ok(None)` sleeps ~1 ms to avoid busy-spinning (mirrors the callback
///   pump's idle poll).
pub(crate) fn spawn_drain_thread<S>(
    stream: Arc<dyn crate::core::interface::CapturingStream>,
    mut sink: S,
) -> AudioResult<DrainHandle>
where
    S: crate::sink::AudioSink + 'static,
{
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_thread = Arc::clone(&stop_flag);
    let handle = std::thread::Builder::new()
        .name("rsac-drain".into())
        .spawn(move || {
            loop {
                if stop_flag_thread.load(Ordering::SeqCst) {
                    break;
                }
                match stream.try_read_chunk() {
                    // The drain thread OWNS `sink`, so no lock is held while
                    // user sink code runs (it may block on I/O without
                    // stalling anything else).
                    Ok(Some(buffer)) => {
                        if let Err(e) = sink.write(&buffer) {
                            if e.is_fatal() {
                                log::error!(
                                    "Drain sink write failed fatally; \
                                     stopping drain: {:?}",
                                    e
                                );
                                break;
                            }
                            // Recoverable write error: log and keep draining
                            // (mirrors the read-side recoverable policy).
                            log::warn!("Drain sink write error (continuing): {:?}", e);
                        }
                    }
                    Ok(None) => {
                        // No data right now — avoid busy-spinning. Mirrors
                        // spawn_callback_pump's idle poll.
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    // Only a FATAL read error (e.g. StreamEnded — terminal)
                    // ends the drain. A recoverable read error is logged and
                    // retried, mirroring the callback pump and subscribe().
                    Err(e) if e.is_fatal() => break,
                    Err(e) => {
                        log::warn!("Drain pump read error (retrying): {:?}", e);
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            }
            // Loop exited (terminal stream, fatal write, or stop signal):
            // finalize the sink. flush() then close() so a file sink's header
            // is written even on the error paths. Both are best-effort here
            // because the thread cannot return a Result; log any failure.
            if let Err(e) = sink.flush() {
                log::error!("Drain sink flush failed: {:?}", e);
            }
            if let Err(e) = sink.close() {
                log::error!("Drain sink close failed: {:?}", e);
            }
        })
        .map_err(|e| AudioError::InternalError {
            message: format!("Failed to spawn drain thread: {}", e),
            source: None,
        })?;

    Ok(DrainHandle {
        stop_flag,
        handle: Some(handle),
    })
}

/// Shared push-subscription pump behind [`AudioCapture::subscribe`](crate::api::AudioCapture::subscribe) and the
/// `compose` feature's `Composition::subscribe`: a background `rsac-subscribe`
/// thread reads the stream's terminal-observable `try_read_chunk` path and
/// forwards each buffer over a **bounded** [`mpsc`] channel
/// ([`SUBSCRIBE_CHANNEL_CAPACITY`] buffers).
///
/// Policy (identical for both callers; mirrors the callback pump and the
/// iterator):
/// - buffers are forwarded with a **non-blocking** `try_send`: when the
///   subscriber has stalled long enough to fill the channel, the buffer is
///   **dropped and counted** in `dropped` — the crate-wide "drop, don't
///   block, and count it" backpressure policy. An unbounded channel would
///   instead grow memory without limit while the ring-side `overrun_count()`
///   read healthy (rsac-d6a8);
/// - only a **fatal** terminal (e.g. [`AudioError::StreamEnded`]) ends the
///   subscription — the channel then disconnects, which the receiver observes
///   as a [`RecvError`](std::sync::mpsc::RecvError);
/// - a **recoverable** read error is logged and retried, never ending delivery
///   (FH-1/BP-6);
/// - a momentarily-empty ring sleeps ~1 ms (the documented latency floor);
/// - a dropped receiver ends the thread (observed on every send attempt,
///   including the `Full` path's `Disconnected` variant).
pub(crate) fn spawn_subscribe_thread(
    stream: Arc<dyn crate::core::interface::CapturingStream>,
    dropped: Arc<AtomicU64>,
) -> AudioResult<mpsc::Receiver<AudioBuffer>> {
    let (tx, rx) = mpsc::sync_channel(SUBSCRIBE_CHANNEL_CAPACITY);

    std::thread::Builder::new()
        .name("rsac-subscribe".into())
        .spawn(move || loop {
            match stream.try_read_chunk() {
                Ok(Some(buffer)) => match tx.try_send(buffer) {
                    Ok(()) => {}
                    // Subscriber slower than real time: the bounded channel is
                    // full. Drop the buffer (never block the pump, never queue
                    // unbounded memory) and count the loss so it is visible via
                    // subscriber_dropped_count() — the ring-side counters can't
                    // see it, because the pump drained the ring successfully.
                    Err(mpsc::TrySendError::Full(_)) => {
                        dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => break, // Receiver dropped
                },
                Ok(None) => {
                    // No data available, sleep briefly to avoid busy-spinning
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                // Only a FATAL terminal (e.g. StreamEnded) ends the
                // subscription — the channel then disconnects, which the
                // receiver observes as a RecvError. A recoverable read error
                // (transient StreamReadError, BufferOverrun/Underrun) must
                // NOT kill delivery; log and retry after a brief pause,
                // mirroring the callback pump (spawn_callback_pump) and the
                // iterator. This closes the prior bug where ANY error broke
                // the loop and silently ended the subscription (FH-1/BP-6).
                Err(e) if e.is_fatal() => break,
                Err(e) => {
                    log::warn!("Subscribe thread read error (retrying): {:?}", e);
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        })
        .map_err(|e| AudioError::InternalError {
            message: format!("Failed to spawn subscribe thread: {}", e),
            source: None,
        })?;

    Ok(rx)
}

/// Error-carrying sibling of [`spawn_subscribe_thread`] behind
/// [`AudioCapture::subscribe_with_errors`](crate::api::AudioCapture::subscribe_with_errors) and the `compose` feature's
/// `Composition::subscribe_with_errors`: each item is an
/// [`AudioResult<AudioBuffer>`], and the **fatal terminal** error is delivered
/// as the final channel item *before* the disconnect, so the consumer never
/// has to race a bare `RecvError` to learn why the stream ended.
///
/// On top of [`spawn_subscribe_thread`]'s bounded-channel policy (`try_send`
/// buffers; on `Full` drop-and-count into `dropped`):
/// - repeated **recoverable** errors are coalesced: a same-variant error is
///   forwarded at most once per [`RECOVERABLE_ERROR_FORWARD_INTERVAL`] (a
///   persistently-recoverable stream errors once per ~1 ms poll and would
///   otherwise flood the channel with identical advisory items); an error of a
///   *different* variant is always forwarded immediately. A recoverable error
///   item that meets a full channel is simply not forwarded that round (it is
///   advisory; it is **not** counted as a dropped buffer);
/// - the **fatal terminal** is sent with a **blocking** `send` so it can never
///   be lost to the `Full` path: the pump exits right after, so blocking on
///   this one final item is acceptable, and delivery is guaranteed unless the
///   receiver is already gone (in which case nobody is listening anyway).
pub(crate) fn spawn_subscribe_with_errors_thread(
    stream: Arc<dyn crate::core::interface::CapturingStream>,
    dropped: Arc<AtomicU64>,
) -> AudioResult<mpsc::Receiver<AudioResult<AudioBuffer>>> {
    let (tx, rx) = mpsc::sync_channel(SUBSCRIBE_CHANNEL_CAPACITY);

    std::thread::Builder::new()
        .name("rsac-subscribe-err".into())
        .spawn(move || {
            // Coalescing state: the variant of the last forwarded recoverable
            // error and when it was forwarded. Variant identity (discriminant)
            // rather than full equality keeps the hot path allocation-free.
            let mut last_recoverable: Option<(std::mem::Discriminant<AudioError>, Instant)> = None;
            loop {
                match stream.try_read_chunk() {
                    Ok(Some(buffer)) => match tx.try_send(Ok(buffer)) {
                        Ok(()) => {}
                        // Full channel: drop the buffer, count it (see
                        // spawn_subscribe_thread — same policy).
                        Err(mpsc::TrySendError::Full(_)) => {
                            dropped.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(mpsc::TrySendError::Disconnected(_)) => break, // Receiver dropped
                    },
                    Ok(None) => {
                        // No data available, sleep briefly to avoid busy-spinning
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    // Fatal terminal: forward the error as the FINAL item, THEN
                    // exit. This is deliberately the BLOCKING send: the channel
                    // may be full of unread buffers, and the terminal must never
                    // be silently lost to the Full path (the consumer would then
                    // see a bare disconnect and never learn why the stream
                    // ended). Blocking here is fine — the pump exits right after
                    // this send, and send() errors out (ignored) if the receiver
                    // is already gone.
                    Err(e) if e.is_fatal() => {
                        let _ = tx.send(Err(e));
                        break;
                    }
                    // Recoverable: surface it (best-effort, coalesced) AND keep
                    // delivering — a transient hiccup must not end the
                    // subscription.
                    Err(e) => {
                        let variant = std::mem::discriminant(&e);
                        let now = Instant::now();
                        let forward = match last_recoverable {
                            Some((last_variant, last_at)) => {
                                variant != last_variant
                                    || now.duration_since(last_at)
                                        >= RECOVERABLE_ERROR_FORWARD_INTERVAL
                            }
                            None => true,
                        };
                        if forward {
                            // Advisory item: non-blocking. If the channel is
                            // full the error is not forwarded this round (and
                            // NOT counted as a dropped buffer); a disconnect
                            // still ends the pump.
                            match tx.try_send(Err(e)) {
                                Ok(()) => {
                                    // rsac-0055: arm the coalescing cooldown
                                    // only on a DELIVERED error — a Full drop
                                    // must not suppress the next report.
                                    last_recoverable = Some((variant, now));
                                }
                                Err(mpsc::TrySendError::Full(_)) => {}
                                Err(mpsc::TrySendError::Disconnected(_)) => {
                                    break; // Receiver dropped
                                }
                            }
                        }
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            }
        })
        .map_err(|e| AudioError::InternalError {
            message: format!("Failed to spawn subscribe thread: {}", e),
            source: None,
        })?;

    Ok(rx)
}

/// Handle to the background thread spawned by
/// [`RunningCapture::drain_to`](crate::api::RunningCapture::drain_to).
///
/// The thread owns the [`AudioSink`](crate::sink::AudioSink) and drains captured
/// buffers into it until the stream reaches a terminal state (or this handle is
/// shut down / dropped). On exit the thread flushes and closes the sink, so a
/// file sink's header is finalized exactly once.
///
/// Modeled on the callback pump's handle: it holds a stop flag and the thread's
/// [`JoinHandle`](std::thread::JoinHandle), and joins on
/// [`shutdown`](Self::shutdown) / [`Drop`] (self-join-guarded, so a join from the
/// drain thread itself is skipped rather than dead-locking). Because the loop
/// also exits on its own when the stream becomes terminal, you do not strictly
/// have to call `shutdown()` for the thread to end — but doing so joins it
/// deterministically and guarantees the sink's `flush()`/`close()` have run.
pub struct DrainHandle {
    stop_flag: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl DrainHandle {
    /// Signal the drain thread to stop and join it. Idempotent.
    ///
    /// Sets the stop flag (so the loop breaks on its next pass, after which the
    /// thread flushes + closes the sink) and joins the thread so the finalize
    /// has completed by the time this returns. If called from the drain thread
    /// itself the join is skipped — a thread cannot join itself — and the
    /// `JoinHandle` is retained so a later `shutdown()`/`Drop` from another
    /// thread can still join it (mirrors `CallbackPump::shutdown`).
    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    /// Internal shutdown shared by [`shutdown`](Self::shutdown) and [`Drop`].
    /// Takes `&mut self` so `Drop` can call it; the public `shutdown` consumes
    /// the handle so it cannot be used after finalizing.
    fn shutdown_inner(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            if handle.thread().id() == std::thread::current().id() {
                // Re-entrant teardown from the drain thread: don't join self.
                // The stop flag is set; the loop will break on its next pass.
                // Put the handle back so a later shutdown()/Drop from another
                // thread can still join it deterministically.
                self.handle = Some(handle);
            } else {
                let _ = handle.join();
            }
        }
    }
}

impl Drop for DrainHandle {
    fn drop(&mut self) {
        // Deterministic teardown: signal + join (self-join-guarded). Ensures the
        // drain thread is never leaked and the sink's flush()/close() have run.
        self.shutdown_inner();
    }
}

impl fmt::Debug for DrainHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DrainHandle")
            .field("stopping", &self.stop_flag.load(Ordering::Relaxed))
            .field("joined", &self.handle.is_none())
            .finish()
    }
}
