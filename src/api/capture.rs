//! [`AudioCapture`]: the lifecycle handle — start/stop, pull reads, push
//! delivery (subscriptions and the callback pump), and runtime introspection.

use super::delivery::{
    spawn_drain_thread, spawn_subscribe_thread, spawn_subscribe_with_errors_thread, DrainHandle,
};
#[cfg(feature = "hotplug-recovery")]
use super::recovery::RecoveryHub;

use crate::core::buffer::AudioBuffer;
use crate::core::config::{AudioCaptureConfig, AudioFormat, SampleFormat};
#[cfg(feature = "async-stream")]
use crate::core::error::REASON_CAPTURE_NOT_STARTED;
use crate::core::error::{
    AudioError, AudioResult, REASON_NOT_INITIALIZED, REASON_NOT_RUNNING, REASON_NO_ACTIVE_STREAM,
};
use crate::core::introspection::{BackpressureReport, StreamStats};
#[cfg(feature = "hotplug-recovery")]
use crate::core::recovery::{RecoveryEvent, RecoverySnapshot};
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The user audio callback type. Boxed `FnMut` invoked once per captured buffer.
pub(super) type AudioCallback = Box<dyn FnMut(&AudioBuffer) + Send + 'static>;

/// A registered-but-not-yet-running callback, stored in [`AudioCapture`] until
/// [`start()`](AudioCapture::start) moves it into a pump thread. Held behind a
/// `Mutex<Option<...>>` only so `&self`-style set/clear can mutate it before the
/// pump owns it — the pump thread does **not** lock this while invoking the
/// callback (it takes ownership), so a callback can freely re-enter the handle.
type PendingCallback = Mutex<Option<AudioCallback>>;

/// Handle to a running callback pump thread.
///
/// The pump *owns* the callback (it was moved out of [`PendingCallback`] at
/// [`start()`](AudioCapture::start)), so no lock is held while the user closure
/// runs — a callback may call back into `AudioCapture` without deadlocking. The
/// pump exits when `stop_flag` is set or the stream errors; the [`JoinHandle`]
/// lets `stop()`/`Drop` join it deterministically rather than leaking the thread.
pub(super) struct CallbackPump {
    stop_flag: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl CallbackPump {
    /// Raises the pump's stop flag without joining. Idempotent and lock-free.
    ///
    /// `stop()`/`Drop` call this **before** stopping the supervised stream and
    /// joining, so the pump can no longer dispatch a buffer to the user
    /// callback once teardown has begun, even if it is mid-read inside a
    /// reopening recovery stream.
    pub(super) fn signal_stop(&self) {
        self.stop_flag.store(true, Ordering::SeqCst);
    }

    /// Joins the pump thread. Idempotent. Callers must [`signal_stop`](Self::signal_stop)
    /// first (and stop the stream) so the join cannot wait on a pump that is
    /// still being fed.
    ///
    /// If called from the pump's own thread (i.e. the user closure re-entered
    /// `AudioCapture` and triggered teardown), the join is skipped — a thread
    /// cannot join itself — and the handle is kept so a later `stop()`/`Drop`
    /// from another thread can still join it. This makes "clear the callback
    /// from within the callback" safe rather than a self-join deadlock.
    pub(super) fn join(&mut self) {
        if let Some(handle) = self.handle.take() {
            if handle.thread().id() == std::thread::current().id() {
                // Re-entrant teardown from the pump thread: don't join self.
                // The stop flag is set; the loop will break on its next pass.
                self.handle = Some(handle);
            } else {
                let _ = handle.join();
            }
        }
    }

    /// Signals the pump to stop and joins it. Idempotent; see
    /// [`join`](Self::join) for the re-entrancy rule.
    pub(super) fn shutdown(&mut self) {
        self.signal_stop();
        self.join();
    }
}

/// The lifecycle handle for a configured capture session, returned by
/// [`AudioCaptureBuilder::build`](crate::api::AudioCaptureBuilder::build).
///
/// `AudioCapture` owns the platform stream and exposes the full capture
/// lifecycle: [`start()`](AudioCapture::start), [`stop()`](AudioCapture::stop),
/// pull-based reads ([`read_buffer()`](AudioCapture::read_buffer)), push-based
/// delivery ([`subscribe()`](AudioCapture::subscribe),
/// [`set_callback()`](AudioCapture::set_callback)), and runtime introspection
/// ([`stream_stats()`](AudioCapture::stream_stats),
/// [`overrun_count()`](AudioCapture::overrun_count),
/// [`uptime()`](AudioCapture::uptime)).
///
/// The handle is `Send + Sync` (asserted at compile time below); see the
/// [module docs](crate::api) for the threading model and the multiple-captures
/// guarantee.
pub struct AudioCapture {
    pub(super) config: AudioCaptureConfig,
    pub(super) device: Option<Box<dyn crate::core::interface::AudioDevice>>,
    pub(super) stream: Option<Arc<dyn crate::core::interface::CapturingStream + 'static>>,
    /// Callback registered via [`set_callback`](AudioCapture::set_callback)
    /// before the capture starts. Moved into the pump thread on `start()`.
    pub(super) callback: PendingCallback,
    /// Active callback pump, if a callback was set when `start()` ran. `None`
    /// means no pump is running (so `start()` will never double-spawn).
    pub(super) callback_pump: Option<CallbackPump>,
    /// Monotonic timestamp captured the first time `start()` actually creates
    /// the OS stream. `Some` while a stream is live; reset to `None` by
    /// `stop()`/`Drop` when the stream is torn down. Set exactly once on the
    /// non-RT control path (a single `Instant` store), never on an idempotent
    /// restart of an already-running stream. Drives [`uptime`](AudioCapture::uptime).
    pub(super) start_instant: Option<Instant>,
    /// Total buffers dropped by this handle's subscribe pumps because a
    /// subscriber's bounded channel was full (rsac-d6a8). Shared (aggregated)
    /// across every subscription created from this handle and monotone for
    /// the handle's lifetime — it survives `stop()`/restart. Surfaced via
    /// [`subscriber_dropped_count`](AudioCapture::subscriber_dropped_count).
    pub(super) subscriber_dropped: Arc<AtomicU64>,
    /// Auto-recovery hub (policy, event channel, snapshot) when the builder
    /// enabled [`with_auto_recovery`](crate::api::AudioCaptureBuilder::with_auto_recovery).
    /// Outlives individual streams, so a subscription made before `start()`
    /// keeps receiving events across restarts.
    #[cfg(feature = "hotplug-recovery")]
    pub(super) recovery: Option<Arc<RecoveryHub>>,
}

// ── Send + Sync assertion (AEG-5, rsac-6f1f) ──────────────────────────────
//
// `AudioCapture` holds only `Send + Sync` parts: an owned `AudioCaptureConfig`;
// `Option<Box<dyn AudioDevice>>` and `Option<Arc<dyn CapturingStream>>` whose
// trait objects are bounded `Send + Sync` (see `crate::core::interface`); a
// `Mutex<Option<Box<dyn FnMut(&AudioBuffer) + Send>>>` (a `Mutex` of a `Send`
// payload is `Send + Sync`); an `Option<CallbackPump>` (an `Arc<AtomicBool>`
// plus a `JoinHandle<()>`, both `Send + Sync`); and an `Option<Instant>`. The
// compiler therefore derives both auto-traits.
//
// We deliberately do NOT hand-write `unsafe impl Send/Sync`: that would suppress
// the compiler's safety net if a non-`Send`/`Sync` field were ever added (e.g. a
// raw pointer or `Rc`), silently making the handle unsound to share. The
// compile-time assertion below pins the type-level guarantee the module doc
// promises (and that downstreams previously had to guess at — some assumed
// `!Sync`) without `unsafe`. Mirrors `buffer.rs`'s `_assert_send_sync`.
//
// All public read paths (`read_buffer`, `read_buffer_blocking`,
// `read_chunk_nonblocking`, `read_chunk_blocking`, `subscribe`,
// `subscribe_with_errors`, `request_stop`, stats/format) take `&self` and reach
// the underlying stream through its own `&self` methods, so an `Arc<AudioCapture>`
// shared across threads needs no external lock for reads. The only `&mut self`
// entry points are lifecycle mutators (`start`/`stop`/`set_callback`/
// `clear_callback`) and `buffers_iter`, whose iterator holds a `&mut AudioCapture`
// borrow; none of these contradict the `Send + Sync` guarantee.
const _: () = {
    const fn _assert_send_sync<T: Send + Sync>() {}
    _assert_send_sync::<AudioCapture>();
};

impl AudioCapture {
    /// Drains captured audio into an [`AudioSink`](crate::sink::AudioSink) on a
    /// dedicated `rsac-drain` background thread, returning a [`DrainHandle`].
    ///
    /// This is **the** built-in sink driver for a plain started `AudioCapture`:
    /// you do not hand-roll a `read_buffer()` loop to feed a `WavFileSink`/
    /// `ChannelSink`/`NullSink`. It is the same driver exposed on
    /// [`RunningCapture::drain_to`](crate::api::RunningCapture::drain_to) and
    /// (behind `compose`) `Composition::drain_to`
    /// — the sink runs on this thread, never the OS RT callback thread (ADR-0001),
    /// with the shared recoverable-vs-fatal read/write policy and `flush()`+
    /// `close()` finalization documented on
    /// [`RunningCapture::drain_to`](crate::api::RunningCapture::drain_to).
    ///
    /// # Competes with other readers
    ///
    /// The drain thread competes with [`read_buffer`](Self::read_buffer),
    /// [`subscribe`](Self::subscribe), and the callback pump for buffers from the
    /// same ring (one logical consumer per buffer). **Do not** mix `drain_to` with
    /// manual reads on the same capture.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::StreamReadError`] if the capture has no stream (never
    /// started, or stopped), or [`AudioError::InternalError`] if the drain thread
    /// cannot be spawned.
    pub fn drain_to<S>(&self, sink: S) -> AudioResult<DrainHandle>
    where
        S: crate::sink::AudioSink + 'static,
    {
        let stream_ref = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: REASON_NO_ACTIVE_STREAM.to_string(),
            })?;
        spawn_drain_thread(Arc::clone(stream_ref), sink)
    }

    /// Starts the audio capture stream.
    ///
    /// Creates the underlying OS stream (if not already created) and marks
    /// the capture as running. In the new `CapturingStream` contract, the
    /// stream starts producing data upon creation.
    ///
    /// # Restart-by-recreation (rsac-7aa2)
    ///
    /// A handle whose stream was released by [`stop`](Self::stop) **can** be
    /// started again: `start()` creates a *fresh* stream from the same
    /// resolved device/config, so `stop() → start() → read` works. Notes:
    ///
    /// - the uptime anchor re-anchors at the restart —
    ///   [`uptime`](Self::uptime) reports time since the *current* stream was
    ///   created, not cumulative capture time;
    /// - per-stream counters ([`overrun_count`](Self::overrun_count),
    ///   [`stream_stats`](Self::stream_stats)) reset with the new stream,
    ///   while [`subscriber_dropped_count`](Self::subscriber_dropped_count)
    ///   is handle-lifetime and survives.
    ///
    /// With auto-recovery (`AudioCaptureBuilder::with_auto_recovery`, feature
    /// `hotplug-recovery`), a `start()` that creates a stream opens the system
    /// default device current at that moment rather than the device `build()`
    /// resolved, and begins a new recovery session.
    ///
    /// Calling `start()` while a stream already exists is state-dependent:
    /// a **running** stream makes it an idempotent no-op (`Ok`); a stream
    /// that is present but **no longer running** (a naturally-ended stream
    /// that was never `stop()`ped) returns
    /// [`AudioError::StreamStartFailed`] — an individual *stream* can never be
    /// restarted; call `stop()` first to release it, then `start()` to
    /// restart with a fresh one.
    pub fn start(&mut self) -> AudioResult<()> {
        // If a stream already exists, decide based on its state:
        // - running  → no-op (idempotent restart of an active capture).
        // - stopped  → error. An individual stream cannot be restarted (the OS
        //   capture thread has exited); the caller must first release it via
        //   stop(), after which start() creates a fresh stream
        //   (restart-by-recreation — see the rustdoc above). Previously this
        //   fell through and spawned a callback pump on a dead stream, then
        //   read_buffer() would fail confusingly (audit L8).
        if let Some(stream) = self.stream.as_ref() {
            if stream.is_running() {
                return Ok(());
            }
            return Err(AudioError::StreamStartFailed {
                reason: "Stream already created and is no longer running; an individual \
                         stream cannot be restarted — call stop() to release it, then \
                         start() again to restart with a fresh stream."
                    .to_string(),
            });
        }

        if self.stream.is_none() {
            let device_ref =
                self.device
                    .as_ref()
                    .ok_or_else(|| AudioError::StreamCreationFailed {
                        reason: "Audio device not available to create stream (was None)."
                            .to_string(),
                        context: None,
                    })?;
            // hotplug-recovery: a supervised capture opens generation zero the
            // way it opens every replacement, on the default device current at
            // this start() (not the one build() resolved), and only after the
            // device-change watcher is armed: a default change before the open
            // is reflected in it, and one after is handled by the first poll.
            #[cfg(feature = "hotplug-recovery")]
            let capturing_stream_obj = match self.recovery.as_ref() {
                Some(hub) => super::recovery::start_supervised(&self.config.stream_config, hub)?,
                None => device_ref.create_stream(&self.config.stream_config)?,
            };
            #[cfg(not(feature = "hotplug-recovery"))]
            let capturing_stream_obj = device_ref.create_stream(&self.config.stream_config)?;
            self.stream = Some(Arc::from(capturing_stream_obj));
            // Record the start time exactly once, the first time a real stream
            // is created. The idempotent-restart paths above (running → Ok,
            // stopped → Err) return before reaching here, so a second start()
            // on a live stream never resets this. Single non-RT Instant store.
            self.start_instant = Some(Instant::now());
        }

        // Verify stream is available
        let stream_ref = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamCreationFailed {
                reason: "Stream not initialized before starting.".to_string(),
                context: None,
            })?;

        // If a callback was registered via set_callback() AND no pump is already
        // running, spawn a pump thread that delivers captured buffers to it.
        // Without this the stored closure is never invoked (the callback
        // delivery mode would silently do nothing). See
        // docs/designs/0002-callback-delivery.md.
        //
        // Guarding on `self.callback_pump.is_none()` makes a second start() a
        // no-op for the pump — two pumps must never race for the same ring. The
        // callback is *moved* into the pump (taken out of the pending slot), so
        // the pump never holds a lock while running the user closure.
        if self.callback_pump.is_none() {
            // A poisoned callback mutex must NOT silently drop a registered
            // callback (which would leave start() returning Ok with no
            // delivery); surface it like set_callback/clear_callback do.
            let taken = match self.callback.lock() {
                Ok(mut guard) => guard.take(),
                Err(poisoned) => {
                    return Err(AudioError::InternalError {
                        message: format!("Failed to lock callback mutex: {}", poisoned),
                        source: None,
                    });
                }
            };
            if let Some(callback) = taken {
                let pump = Self::spawn_callback_pump(Arc::clone(stream_ref), callback)?;
                self.callback_pump = Some(pump);
            }
        }

        Ok(())
    }

    /// Spawns the callback pump thread and returns a [`CallbackPump`] handle.
    ///
    /// The pump **owns** `callback` (moved in), reads buffers from `stream`, and
    /// invokes the closure on this dedicated thread — **not** the OS real-time
    /// audio thread — so a slow callback only delays delivery, it never stalls
    /// the audio callback, and the closure may freely call back into
    /// `AudioCapture` (no lock is held during invocation). The thread exits when:
    /// - `stop_flag` is set (via [`stop`](Self::stop)/[`clear_callback`](Self::clear_callback)/`Drop`), or
    /// - the stream reaches a terminal state (a **fatal** read error such as
    ///   [`AudioError::StreamEnded`]). Transient/recoverable read errors are
    ///   logged and retried — they must not permanently stop delivery.
    ///
    /// The pump competes with [`read_buffer`](Self::read_buffer) and
    /// [`subscribe`](Self::subscribe) for buffers from the same ring; avoid
    /// mixing a callback with manual reads.
    pub(super) fn spawn_callback_pump(
        stream: Arc<dyn crate::core::interface::CapturingStream + 'static>,
        mut callback: AudioCallback,
    ) -> AudioResult<CallbackPump> {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_thread = Arc::clone(&stop_flag);
        let handle = std::thread::Builder::new()
            .name("rsac-callback".into())
            .spawn(move || loop {
                if stop_flag_thread.load(Ordering::SeqCst) {
                    break;
                }
                match stream.try_read_chunk() {
                    // No lock held: the pump owns `callback`, so the user closure
                    // can re-enter AudioCapture (e.g. clear_callback) without
                    // deadlocking, and a panic here cannot poison a shared mutex.
                    Ok(Some(buffer)) => {
                        // Re-check AFTER the read: a stop that landed while the
                        // read was in flight (e.g. inside a recovery reopen) must
                        // not reach the user callback.
                        if stop_flag_thread.load(Ordering::SeqCst) {
                            break;
                        }
                        callback(&buffer)
                    }
                    Ok(None) => {
                        // No data right now — avoid busy-spinning.
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    // Only a FATAL error (e.g. StreamEnded — terminal) stops the
                    // pump. A transient/recoverable read error (StreamReadError,
                    // BufferOverrun/Underrun) must NOT kill delivery — mirror the
                    // iterator/read-loop contract and retry after a brief pause.
                    Err(e) if e.is_fatal() => break,
                    Err(e) => {
                        log::warn!("Callback pump read error (retrying): {:?}", e);
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            })
            .map_err(|e| AudioError::InternalError {
                message: format!("Failed to spawn callback pump thread: {}", e),
                source: None,
            })?;
        Ok(CallbackPump {
            stop_flag,
            handle: Some(handle),
        })
    }

    /// Stops the audio capture stream.
    ///
    /// Stops the underlying OS stream and **releases** it: the handle's stream
    /// slot becomes empty and the uptime anchor is cleared. The handle itself
    /// remains usable — a subsequent [`start`](Self::start) creates a fresh
    /// stream from the same resolved device/config (*restart-by-recreation*;
    /// see [`start`](Self::start) for what carries over). An individual
    /// *stream* can never be restarted; the handle restarts by creating a new
    /// one.
    ///
    /// Any active subscriber threads will terminate once they detect the stream
    /// has stopped. The underlying stream is released when all references
    /// (including subscriber threads) are dropped.
    pub fn stop(&mut self) -> AudioResult<()> {
        // Make stop sticky BEFORE any join: signal the callback pump and stop
        // the (possibly supervised) stream first, so a pump that is mid-read
        // inside a recovery reopen can neither install a new generation nor
        // dispatch a buffer, and the join below cannot wait on live recovery.
        if let Some(pump) = self.callback_pump.as_ref() {
            pump.signal_stop();
        }
        if let Some(stream) = self.stream.as_ref() {
            if let Err(e) = stream.stop() {
                log::warn!("Error stopping stream: {:?}", e);
            }
        }

        // Join the quiesced pump so stop() is authoritative for the thread and
        // its stream clone is released before we drop ours.
        if let Some(mut pump) = self.callback_pump.take() {
            pump.join();
        }

        // Nothing more to release if there is no stream (idempotent).
        if self.stream.is_none() {
            return Ok(());
        }

        // Drop our Arc reference. The stream will be fully deallocated once all
        // subscriber threads also drop their clones.
        self.stream.take();
        // The stream is gone, so there is no longer an uptime to report.
        self.start_instant = None;

        Ok(())
    }

    /// Returns `true` if the stream is currently capturing.
    ///
    /// Delegates to the underlying stream's state machine — the single source
    /// of truth for running status. Returns `false` if no stream has been
    /// created yet.
    pub fn is_running(&self) -> bool {
        self.stream
            .as_ref()
            .map(|s| s.is_running())
            .unwrap_or(false)
    }

    /// Returns a reference to the capture configuration.
    pub fn config(&self) -> &AudioCaptureConfig {
        &self.config
    }

    /// Reads a buffer of audio data synchronously (non-blocking).
    ///
    /// Uses `CapturingStream::try_read_chunk` for non-blocking reads.
    /// Returns `Ok(None)` if no data is currently available.
    ///
    /// # Terminal semantics (important)
    ///
    /// This is the **simple pull API**: it short-circuits with a **recoverable**
    /// [`AudioError::StreamReadError`] the moment the stream leaves `Running`, so
    /// it **never surfaces the fatal [`AudioError::StreamEnded`]** and does **not**
    /// drain the buffered tail after `stop()`. A loop that branches on
    /// [`is_fatal()`](AudioError::is_fatal) to detect end-of-stream will therefore
    /// **never terminate** on this method (the downgraded error is recoverable);
    /// it is intended for `while let Ok(Some(buf)) = capture.read_buffer()` style
    /// loops that stop when the caller decides to.
    ///
    /// Prefer [`read_chunk_nonblocking`](Self::read_chunk_nonblocking) (its
    /// terminal-observable sibling) when you need to observe the clean
    /// end-of-stream signal or drain the tail after `stop()` — that is the path
    /// the language bindings and the [`AudioBufferIterator`] use. `read_buffer` is
    /// retained unchanged for backward compatibility; new terminal-aware code
    /// should migrate to `read_chunk_nonblocking`.
    ///
    /// Takes `&self` (not `&mut self`): this path only reads `self.stream` and
    /// calls the stream's own `&self` methods, mutating no field of the handle.
    /// Sharing it behind `&self` lets a concurrent [`request_stop`](Self::request_stop)
    /// unblock a parked reader without forming a `&mut` alias to the same handle
    /// (the use-after-free precondition the C/Go bindings previously relied on).
    pub fn read_buffer(&self) -> AudioResult<Option<AudioBuffer>> {
        // Get the stream first — if there's no stream, we're not running.
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: REASON_NOT_INITIALIZED.to_string(),
            })?;

        // Check running state via the stream itself — single source of truth.
        // This eliminates the TOCTOU window that existed when a separate
        // AtomicBool was consulted before touching the stream.
        if !stream.is_running() {
            return Err(AudioError::StreamReadError {
                reason: REASON_NOT_RUNNING.to_string(),
            });
        }

        stream.try_read_chunk()
    }

    /// Reads a buffer of audio data non-blocking, **without** the
    /// [`read_buffer`](Self::read_buffer) running-state short-circuit.
    ///
    /// This is the read path the binding pumps (Node/napi, Go via the FFI) and
    /// the in-process consumers use when they must observe the stream's
    /// *terminal* state. Unlike [`read_buffer`](Self::read_buffer) — which
    /// returns a **recoverable** [`AudioError::StreamReadError`] as soon as the
    /// stream leaves `Running`, and therefore can never surface the fatal
    /// [`AudioError::StreamEnded`] — this method delegates straight to the
    /// stream's `try_read_chunk`. That preserves the bridge's drain-on-stop
    /// semantics:
    ///
    /// - `Ok(Some(buf))` while data remains (including the buffered tail that is
    ///   still drainable while the stream is `Stopping`),
    /// - `Ok(None)` when the ring is momentarily empty but the stream is not yet
    ///   terminal,
    /// - `Err(`[`AudioError::StreamEnded`]`)` — **fatal/terminal** — once the
    ///   ring is empty *and* the stream has reached a terminal state.
    ///
    /// Consumers branch on [`AudioError::is_fatal`]/[`AudioError::recoverability`]:
    /// a recoverable error must be retried (it must never end consumption), and
    /// only a fatal terminal ends it cleanly. [`read_buffer`](Self::read_buffer)
    /// keeps its `is_running()` guard for the simple pull API; this is the
    /// terminal-observable sibling. The [`AudioBufferIterator`] and the callback
    /// pump read via `try_read_chunk` for exactly this reason.
    ///
    /// Like [`read_buffer`](Self::read_buffer), this takes `&self` (it only reads
    /// `self.stream` and calls the stream's own `&self` method), so it never
    /// forms a `&mut` alias to the handle.
    pub fn read_chunk_nonblocking(&self) -> AudioResult<Option<AudioBuffer>> {
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: REASON_NOT_INITIALIZED.to_string(),
            })?;
        stream.try_read_chunk()
    }

    /// Blocking read **without** the [`read_buffer_blocking`](Self::read_buffer_blocking)
    /// running-state short-circuit — the *terminal-observable* blocking sibling.
    ///
    /// [`read_buffer_blocking`](Self::read_buffer_blocking) returns a
    /// **recoverable** [`AudioError::StreamReadError`] the moment the stream
    /// leaves `Running`, so it can never surface the fatal
    /// [`AudioError::StreamEnded`]. This method delegates straight to the
    /// stream's `read_chunk`, which blocks until either a buffer is available
    /// (including the drainable tail while `Stopping`) or the stream reaches a
    /// terminal state, in which case it returns `Err(`[`AudioError::StreamEnded`]`)`
    /// **promptly** (a concurrent [`request_stop`](Self::request_stop) unblocks
    /// it). This is the blocking read the C FFI (`rsac_capture_read`) and the Go
    /// binding use so their pumps observe the terminal signal and end cleanly
    /// instead of spinning on a downgraded recoverable error.
    ///
    /// Takes `&self` like the other read paths, so it never forms a `&mut` alias
    /// (the #28 use-after-free precondition).
    pub fn read_chunk_blocking(&self) -> AudioResult<AudioBuffer> {
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: REASON_NOT_INITIALIZED.to_string(),
            })?;
        stream.read_chunk()
    }

    /// Reads a buffer of audio data, blocking until data is available.
    ///
    /// Uses `CapturingStream::read_chunk` which blocks until data arrives.
    ///
    /// # Terminal semantics (important)
    ///
    /// Like [`read_buffer`](Self::read_buffer), this is the **simple pull API**:
    /// it returns a **recoverable** [`AudioError::StreamReadError`] the instant the
    /// stream leaves `Running`, so it **never surfaces the fatal
    /// [`AudioError::StreamEnded`]**. Use
    /// [`read_chunk_blocking`](Self::read_chunk_blocking) — the terminal-observable
    /// sibling — when you need the clean end-of-stream signal (it delegates
    /// straight to `read_chunk` and returns `StreamEnded` promptly on terminal).
    /// `read_buffer_blocking` is retained unchanged for backward compatibility.
    ///
    /// Takes `&self` (not `&mut self`) for the same reason as
    /// [`read_buffer`](Self::read_buffer): no field of the handle is mutated, so
    /// a parked `read_buffer_blocking` can be unblocked by a concurrent
    /// [`request_stop`](Self::request_stop) without ever aliasing the handle
    /// mutably. When the stream reaches a terminal state the underlying
    /// `read_chunk` returns [`AudioError::StreamEnded`] promptly instead of
    /// blocking forever — but the `is_running()` guard below means that terminal
    /// is only observed when the transition happens *during* an already-parked
    /// read; a call made after the stream is already terminal returns the
    /// recoverable "not running" error instead (use `read_chunk_blocking` to
    /// always see `StreamEnded`).
    pub fn read_buffer_blocking(&self) -> AudioResult<AudioBuffer> {
        // Get the stream first — if there's no stream, we're not running.
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: REASON_NOT_INITIALIZED.to_string(),
            })?;

        // Check running state via the stream itself — single source of truth.
        if !stream.is_running() {
            return Err(AudioError::StreamReadError {
                reason: REASON_NOT_RUNNING.to_string(),
            });
        }

        stream.read_chunk()
    }

    /// Best-effort request to stop capture, used as the *unblock primitive* for a
    /// parked [`read_buffer_blocking`](Self::read_buffer_blocking).
    ///
    /// Transitions the underlying stream toward its terminal state (via the
    /// stream's own idempotent `stop()`), which flips the bridge to a terminal
    /// state so a blocked `read_buffer_blocking` returns
    /// [`AudioError::StreamEnded`] within roughly a millisecond instead of waiting
    /// out the blocking-read timeout. It is **idempotent** and a no-op when no
    /// stream has been created (or the stream is already stopped).
    ///
    /// Unlike [`stop`](Self::stop), this takes `&self`: it does not tear down the
    /// callback pump, drop the stream `Arc`, or clear the uptime anchor — it only
    /// signals the stream. That makes it safe to call **concurrently with an
    /// in-flight read** (it forms no `&mut` alias). It is **not** safe to call
    /// concurrently with dropping/freeing the handle: callers (e.g. the C/Go
    /// bindings) must order `request_stop` + drain of in-flight reads **before**
    /// freeing the handle.
    pub fn request_stop(&self) {
        if let Some(stream) = self.stream.as_ref() {
            // CapturingStream::stop is &self and idempotent (already-terminal
            // streams no-op). Ignore the result: this is a best-effort unblock,
            // and a stop error on an already-dying stream is not actionable here.
            let _ = stream.stop();
        }
    }

    /// Returns an iterator over synchronously captured audio buffers.
    pub fn buffers_iter(&mut self) -> AudioBufferIterator<'_> {
        AudioBufferIterator { capture: self }
    }

    /// Returns an asynchronous stream of audio data buffers.
    ///
    /// The returned [`AsyncAudioStream`](crate::bridge::AsyncAudioStream) implements
    /// [`futures_core::Stream`] and yields [`AudioBuffer`]s as they become available
    /// from the audio capture backend.
    ///
    /// The capture must be started (via [`start()`](Self::start)) before calling this method.
    ///
    /// # Single async consumer (waker contract; rsac-7aa2)
    ///
    /// The bridge holds exactly **one** waker slot (an
    /// `atomic_waker::AtomicWaker`). Creating two `AsyncAudioStream`s over the
    /// same capture and polling them from two tasks concurrently is
    /// **unsupported**: each poll's waker registration displaces the other
    /// task's waker (concurrent `AtomicWaker::register` is the documented
    /// unsupported case — the displaced waker is dropped *without being
    /// woken*), so the displaced task can park forever on a wake that was
    /// promised but stolen. Use **at most one** async consumer per capture.
    /// Two consumers would in any case compete for buffers from the same ring
    /// (a single logical consumer per buffer), so there is nothing to gain
    /// from a second stream.
    ///
    /// # Feature Flag
    ///
    /// This method is only available when the `async-stream` feature is enabled.
    ///
    /// # Errors
    ///
    /// Returns an error if the capture has not been started.
    #[cfg(feature = "async-stream")]
    pub fn audio_data_stream(&self) -> AudioResult<crate::bridge::AsyncAudioStream<'_>> {
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: REASON_CAPTURE_NOT_STARTED.to_string(),
            })?;

        Ok(crate::bridge::AsyncAudioStream::new(stream.as_ref()))
    }

    /// Returns an asynchronous stream of audio data buffers.
    ///
    /// **Note:** The `async-stream` feature is not enabled. Enable it in `Cargo.toml`
    /// to use async audio streaming.
    #[cfg(not(feature = "async-stream"))]
    pub fn audio_data_stream(
        &mut self,
    ) -> AudioResult<impl futures_core::Stream<Item = AudioResult<AudioBuffer>> + Send + Sync + '_>
    {
        Err::<
            std::pin::Pin<
                Box<dyn futures_core::Stream<Item = AudioResult<AudioBuffer>> + Send + Sync>,
            >,
            AudioError,
        >(AudioError::PlatformNotSupported {
            feature: "async audio streaming".to_string(),
            platform: "enable the 'async-stream' feature".to_string(),
        })
    }

    /// Sets a callback function for captured audio data.
    ///
    /// The callback will be invoked with each captured audio buffer.
    /// Callbacks cannot be set while capture is running.
    pub fn set_callback<F>(&mut self, callback: F) -> AudioResult<()>
    where
        F: FnMut(&AudioBuffer) + Send + 'static,
    {
        if self.is_running() {
            return Err(AudioError::ConfigurationError {
                message: "Cannot set callback after capture has started.".into(),
            });
        }
        match self.callback.lock() {
            Ok(mut guard) => {
                *guard = Some(Box::new(callback));
                Ok(())
            }
            Err(poisoned) => Err(AudioError::InternalError {
                message: format!("Failed to lock callback mutex: {}", poisoned),
                source: None,
            }),
        }
    }

    /// Clears the registered audio callback.
    ///
    /// If a capture is running with an active callback pump, this signals the
    /// pump to stop and joins it (so delivery ceases promptly), in addition to
    /// clearing any pending (not-yet-started) callback. It is safe to call from
    /// outside the callback. Calling it *from within* the callback signals the
    /// pump but does not self-join (the pump only joins on `stop()`/`Drop`),
    /// avoiding a self-join deadlock.
    pub fn clear_callback(&mut self) -> AudioResult<()> {
        // Tear down a running pump (the callback now lives inside it).
        if let Some(mut pump) = self.callback_pump.take() {
            pump.shutdown();
            // If shutdown() ran re-entrantly (called from inside the pump's own
            // callback), it could not self-join and left the JoinHandle in place
            // for a later join. Re-store the pump so stop()/Drop can still join it
            // deterministically instead of detaching the thread (ADR-0002).
            if pump.handle.is_some() {
                self.callback_pump = Some(pump);
            }
        }
        // Also clear any pending callback registered before start().
        match self.callback.lock() {
            Ok(mut guard) => {
                *guard = None;
                Ok(())
            }
            Err(poisoned) => Err(AudioError::InternalError {
                message: format!("Failed to lock callback mutex for clearing: {}", poisoned),
                source: None,
            }),
        }
    }

    /// Creates a subscription channel that delivers audio buffers as they are captured.
    ///
    /// Spawns a background thread that reads from the capture stream and sends
    /// buffers over a **bounded** [`mpsc`] channel (128 buffers — the same
    /// order as the bridge ring itself; ~1.3 s of audio at the typical ~10 ms
    /// buffer cadence). Returns the receiving end of the channel.
    ///
    /// # Backpressure: drop, don't block, and count it (rsac-d6a8)
    ///
    /// If the receiver stalls long enough for the channel to fill, further
    /// buffers are **dropped** — the pump never blocks and never queues
    /// unbounded memory — and each drop is counted in
    /// [`subscriber_dropped_count`](Self::subscriber_dropped_count). This loss
    /// happens *downstream* of the capture ring, so
    /// [`overrun_count`](Self::overrun_count) and
    /// [`backpressure_report`](Self::backpressure_report) do **not** reflect
    /// it; a slow subscriber must watch `subscriber_dropped_count()` as the
    /// complement.
    ///
    /// **Important:** The background thread competes with [`read_buffer()`](Self::read_buffer)
    /// and [`read_buffer_blocking()`](Self::read_buffer_blocking) for audio data
    /// from the same ring buffer. Avoid mixing `subscribe()` with manual buffer reads.
    ///
    /// The background thread exits automatically when:
    /// - The stream reaches a **fatal terminal** state (e.g.
    ///   [`AudioError::StreamEnded`]) — the channel then disconnects, which the
    ///   receiver observes as a [`RecvError`](std::sync::mpsc::RecvError). A
    ///   **recoverable** read error (e.g. a transient
    ///   [`AudioError::StreamReadError`]) is logged and retried — it does **not**
    ///   end the subscription. (Use [`subscribe_with_errors`](Self::subscribe_with_errors)
    ///   if you need to observe the terminal `AudioError` itself rather than a
    ///   bare disconnect.)
    /// - The returned [`Receiver`](mpsc::Receiver) is dropped
    ///
    /// Multiple subscriptions are allowed but each subscriber competes for buffers.
    ///
    /// # Accepted stream states (rsac-7aa2)
    ///
    /// The subscription is accepted whenever a stream exists — while `Running`
    /// **or** in the drainable `Stopping` window: a gracefully-ended stream
    /// still holding a buffered tail can be subscribed to and drained (the old
    /// `is_running()` gate stranded that tail). Subscribing to a stream that
    /// already reached its fatal terminal also succeeds and simply yields a
    /// channel that ends immediately — racing a natural end is inherently
    /// indistinguishable from subscribing just before it. Only a capture with
    /// **no** stream (never started, or [`stop`](Self::stop)ped — `stop()`
    /// releases the stream) is rejected.
    ///
    /// # Latency floor (audit L5)
    ///
    /// The background thread polls with a 1 ms sleep when the ring buffer is
    /// momentarily empty (rather than parking on the async waker), so delivery
    /// of the *first* buffer after an idle period can be delayed by up to ~1 ms.
    /// For most capture workloads (10–20 ms callback periods) this is negligible,
    /// but latency-critical consumers should read via
    /// [`read_buffer_blocking()`](Self::read_buffer_blocking) or the async stream
    /// API (`feature = "async-stream"`), which is waker-driven and has no fixed
    /// poll-interval floor.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::StreamReadError`] if the capture has no stream
    /// (never started, or stopped).
    pub fn subscribe(&self) -> AudioResult<mpsc::Receiver<AudioBuffer>> {
        // Presence gate only (rsac-7aa2): Running and the drainable Stopping
        // window are both accepted; a stream already at its fatal terminal
        // yields an immediately-ending channel. See the rustdoc above.
        let stream_ref = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: REASON_NO_ACTIVE_STREAM.to_string(),
            })?;

        spawn_subscribe_thread(Arc::clone(stream_ref), Arc::clone(&self.subscriber_dropped))
    }

    /// Like [`subscribe`](Self::subscribe), but delivers each item as an
    /// [`AudioResult<AudioBuffer>`] so the **terminal** [`AudioError`] reaches
    /// the consumer instead of only a bare channel disconnect.
    ///
    /// This is the error-carrying counterpart to [`subscribe`](Self::subscribe)
    /// (the same `Stream` / `StreamWithErrors` split the Go binding exposes). The
    /// background reader:
    ///
    /// - sends `Ok(buffer)` for each captured buffer,
    /// - on a momentarily-empty ring, sleeps ~1 ms and retries,
    /// - on a **recoverable** read error (transient
    ///   [`AudioError::StreamReadError`], [`AudioError::BufferOverrun`],
    ///   [`AudioError::BufferUnderrun`], …), logs and retries — a recoverable
    ///   error never ends the subscription, and
    /// - on a **fatal terminal** error (e.g. [`AudioError::StreamEnded`]) sends
    ///   one final `Err(e)` **then** exits, so the consumer receives the terminal
    ///   `AudioError` as the last item *before* the channel disconnects.
    ///
    /// The `Item` type matches the async stream
    /// ([`AsyncAudioStream`](crate::bridge::AsyncAudioStream)) and the
    /// [`AudioBufferIterator`] so a consumer can reuse
    /// [`AudioError::is_fatal`]/[`AudioError::recoverability`] uniformly.
    ///
    /// Like [`subscribe`](Self::subscribe), the reader competes with
    /// [`read_buffer`](Self::read_buffer) and the callback pump for buffers from
    /// the same ring — do not mix it with manual reads.
    ///
    /// # Bounded channel, drops, and coalescing (rsac-d6a8)
    ///
    /// The channel is **bounded** (128 buffers, like [`subscribe`](Self::subscribe)):
    /// a stalled receiver causes further buffers to be dropped and counted in
    /// [`subscriber_dropped_count`](Self::subscriber_dropped_count). Repeated
    /// **recoverable** errors of the same variant are *coalesced* — forwarded
    /// at most once per second (a persistently-recoverable stream errors once
    /// per ~1 ms poll and would otherwise flood the channel with identical
    /// advisory items); an error of a different variant is always forwarded
    /// immediately. The **fatal terminal** is exempt from all of this: it is
    /// sent with a *blocking* send as the guaranteed final item, so it is never
    /// lost even when the channel is full of unread buffers (the pump exits
    /// right after; delivery fails only if the receiver is already gone).
    ///
    /// # Accepted stream states (rsac-7aa2)
    ///
    /// Same policy as [`subscribe`](Self::subscribe): accepted whenever a
    /// stream exists (`Running` or the drainable `Stopping` window — the
    /// buffered tail plus the terminal error are then delivered); a stream
    /// already at its fatal terminal yields a channel whose only item is the
    /// final `Err`. Only a capture with no stream is rejected.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::StreamReadError`] if the capture has no stream
    /// (never started, or stopped). Once a subscription exists, the terminal
    /// stream error is delivered as the final channel *item*, not as the
    /// return value of this method.
    ///
    /// # Backend caveat (FH-1)
    ///
    /// On Linux/macOS the producer-side terminal signal is only fully wired on
    /// some backends; the final `Err(StreamEnded)` is end-to-end observable
    /// wherever the producer drives the bridge to a terminal state (Windows, and
    /// Linux/macOS once the producer-terminal-signal work lands). On a backend
    /// that never reaches terminal, the subscription simply keeps delivering
    /// until the receiver is dropped — the recoverable-vs-fatal branching here is
    /// correct regardless.
    pub fn subscribe_with_errors(&self) -> AudioResult<mpsc::Receiver<AudioResult<AudioBuffer>>> {
        // Presence gate only (rsac-7aa2) — see subscribe() for the rationale.
        let stream_ref = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: REASON_NO_ACTIVE_STREAM.to_string(),
            })?;

        spawn_subscribe_with_errors_thread(
            Arc::clone(stream_ref),
            Arc::clone(&self.subscriber_dropped),
        )
    }

    /// Returns the total number of buffers **dropped by subscribe pumps** on
    /// this handle because a subscriber's bounded channel was full (rsac-d6a8).
    ///
    /// [`subscribe`](Self::subscribe) and
    /// [`subscribe_with_errors`](Self::subscribe_with_errors) deliver over a
    /// bounded channel (128 buffers) and, per the crate-wide backpressure
    /// policy, **drop rather than block** when a subscriber stalls. Those
    /// drops happen *downstream* of the capture ring, so they are invisible to
    /// [`overrun_count`](Self::overrun_count) and
    /// [`backpressure_report`](Self::backpressure_report) — this counter is
    /// the subscriber-side complement. It aggregates across every subscription
    /// created from this handle (mirroring how `overrun_count` aggregates the
    /// ring) and is monotone for the handle's lifetime: unlike the per-stream
    /// counters it survives [`stop`](Self::stop)/restart.
    ///
    /// `0` means no subscriber has ever fallen behind.
    pub fn subscriber_dropped_count(&self) -> u64 {
        self.subscriber_dropped.load(Ordering::Relaxed)
    }

    /// Returns the number of audio buffers dropped due to ring buffer overflow (overruns).
    ///
    /// This counter reflects how many times the OS audio callback had to discard
    /// a buffer because the consumer was not reading fast enough. A non-zero value
    /// indicates the consumer is too slow or the ring buffer capacity is too small.
    ///
    /// Returns `0` if the stream has not been created yet.
    pub fn overrun_count(&self) -> u64 {
        self.stream.as_ref().map(|s| s.overrun_count()).unwrap_or(0)
    }

    /// Returns true if the stream is experiencing sustained backpressure —
    /// the ring buffer has dropped enough consecutive frames to indicate the
    /// consumer cannot keep up with the producer. Consumers should slow down
    /// processing, warn the user, or switch to a lower-cost provider.
    ///
    /// Returns `false` if the stream has not been created yet.
    pub fn is_under_backpressure(&self) -> bool {
        self.stream
            .as_ref()
            .map(|s| s.is_under_backpressure())
            .unwrap_or(false)
    }

    /// Returns how long the capture has been running, or `None` if it has not
    /// been started (or has been stopped).
    ///
    /// The clock is anchored the first time [`start()`](Self::start) actually
    /// creates the OS stream and is cleared by [`stop()`](Self::stop)/`Drop`.
    /// A second `start()` on an already-running stream does not reset it, so a
    /// long-lived capture reports a continuously increasing uptime. Backed by a
    /// monotonic [`Instant`], so the value never goes backwards even if the
    /// wall clock is adjusted.
    pub fn uptime(&self) -> Option<Duration> {
        self.start_instant.map(|t| t.elapsed())
    }

    /// Returns the negotiated *delivery* `AudioFormat` the backend actually
    /// produces, or `None` before [`start()`](Self::start) creates a stream.
    ///
    /// This is the authoritative format atomically published by the bridge: each
    /// backend records the rate/channels it actually delivers at its negotiation
    /// point — PipeWire from its `param_changed` callback, WASAPI at mix-format
    /// open, CoreAudio when the AUHAL stream format is set (PU-1/PERF-07,
    /// `rsac-2c56`; see [`crate::bridge`]). It can therefore differ from the
    /// requested config in [`config()`](Self::config) when the device forced a
    /// negotiation, and the reported `sample_format` is always `F32` because the
    /// bridge payload is always interleaved f32 regardless of the endpoint's
    /// native sample type. Reading it is a single `Acquire` load behind the
    /// underlying stream's `format()` — no allocation and no lock on the data
    /// plane.
    pub fn format(&self) -> Option<AudioFormat> {
        self.stream.as_ref().map(|s| s.format())
    }

    /// Returns a point-in-time [`StreamStats`] snapshot of this capture.
    ///
    /// Bundles the bridge's diagnostic counters
    /// ([`buffers_captured`](crate::core::interface::CapturingStream::buffers_captured) /
    /// [`buffers_dropped`](crate::core::interface::CapturingStream::buffers_dropped) /
    /// [`buffers_pushed`](crate::core::interface::CapturingStream::buffers_pushed) /
    /// [`overrun_count`](crate::core::interface::CapturingStream::overrun_count))
    /// with [`is_running()`](Self::is_running), [`uptime()`](Self::uptime), and a
    /// human-readable description of the negotiated [`format()`](Self::format).
    ///
    /// All counters are read with cheap `Relaxed` loads on this (non-real-time)
    /// query path — no allocation on or contention with the OS audio callback.
    /// The format description string is built lazily here and is never stored
    /// per-buffer.
    ///
    /// When no stream has been created (before [`start()`](Self::start), or after
    /// [`stop()`](Self::stop)), this returns [`StreamStats::default()`]:
    /// `is_running == false`, `uptime == Duration::ZERO`, zeroed counters, and an
    /// empty `format_description`.
    pub fn stream_stats(&self) -> StreamStats {
        let stream = match self.stream.as_ref() {
            Some(s) => s,
            // No stream → default snapshot (is_running == false, ZERO uptime).
            None => return StreamStats::default(),
        };

        let buffers_dropped = stream.buffers_dropped();
        let format_description = self
            .format()
            .as_ref()
            .map(format_description_string)
            .unwrap_or_default();

        StreamStats {
            // `overruns` is the original field; keep it equal to buffers_dropped
            // (its documented alias) so both read consistently.
            overruns: buffers_dropped,
            buffers_captured: stream.buffers_captured(),
            buffers_dropped,
            buffers_pushed: stream.buffers_pushed(),
            uptime: self.uptime().unwrap_or(Duration::ZERO),
            is_running: self.is_running(),
            format_description,
        }
    }

    /// Returns a windowed [`BackpressureReport`] for this capture.
    ///
    /// Unlike [`is_under_backpressure()`](Self::is_under_backpressure) — an
    /// all-or-nothing flag that trips only on *consecutive* drops and resets on
    /// any successful push — this report exposes a [`drop_rate`](BackpressureReport::drop_rate)
    /// over recent push activity, so sustained partial loss (e.g. a steady 1-in-3
    /// drop pattern) is visible. The legacy bool is carried unchanged inside the
    /// report.
    ///
    /// Returns [`BackpressureReport::default()`] (all-zero, `drop_rate == 0.0`,
    /// `is_under_backpressure == false`) when no stream has been created.
    ///
    /// # Windowing
    ///
    /// The producer maintains a fixed-size, alloc-free ring of per-window
    /// `(pushed, dropped)` snapshots in the bridge (`bridge/ring_buffer.rs`); this
    /// method reads that **windowed** view via
    /// `CapturingStream::drop_window_snapshot`, so the report reflects a
    /// *recent* loss pattern — including a sustained 1-in-N drop that the
    /// consecutive-drop bool resets away — rather than lifetime totals.
    ///
    /// The reported `window` is an honest estimate of the wall-clock the windowed
    /// tallies cover: `(pushed + dropped)` buffers × the per-buffer duration
    /// (`buffer_size` frames at the negotiated `sample_rate`). When the buffer
    /// size or rate is unknown (no stream / not yet negotiated, or a zero rate),
    /// `window` falls back to [`Duration::ZERO`] — the tallies are still valid,
    /// only their span is unattributed. The legacy `is_under_backpressure` bool is
    /// carried unchanged inside the report.
    pub fn backpressure_report(&self) -> BackpressureReport {
        let stream = match self.stream.as_ref() {
            Some(s) => s,
            None => return BackpressureReport::default(),
        };
        let (pushed, dropped) = stream.drop_window_snapshot();
        BackpressureReport::from_counts(
            self.estimate_window_span(pushed + dropped),
            pushed,
            dropped,
            stream.is_under_backpressure(),
        )
    }

    /// Estimate the wall-clock span covered by `buffers` push attempts, from the
    /// configured buffer size and the negotiated (or requested) sample rate.
    ///
    /// Returns [`Duration::ZERO`] when the span cannot be attributed — an unknown
    /// buffer size or a zero/unknown sample rate — so a caller never reads a
    /// fabricated span. Each push delivers one `buffer_size`-frame buffer, so the
    /// window is `buffers × buffer_size / sample_rate` seconds.
    pub(super) fn estimate_window_span(&self, buffers: u64) -> Duration {
        let frames_per_buffer = match self.config.stream_config.buffer_size {
            Some(n) if n > 0 => n as u64,
            _ => return Duration::ZERO,
        };
        let sample_rate = self
            .format()
            .map(|f| f.sample_rate)
            .filter(|&r| r > 0)
            .unwrap_or(self.config.stream_config.sample_rate);
        if sample_rate == 0 {
            return Duration::ZERO;
        }
        let total_frames = buffers.saturating_mul(frames_per_buffer);
        Duration::from_secs_f64(total_frames as f64 / sample_rate as f64)
    }

    /// Returns a point-in-time [`RecoverySnapshot`] of the auto-recovery
    /// supervisor *(feature `hotplug-recovery`)*, or `None` when the capture was
    /// built without
    /// [`with_auto_recovery`](crate::api::AudioCaptureBuilder::with_auto_recovery).
    ///
    /// Before [`start`](Self::start) the state is
    /// [`Idle`](crate::core::recovery::RecoveryState::Idle); after
    /// [`stop`](Self::stop) it is
    /// [`Stopped`](crate::core::recovery::RecoveryState::Stopped). `session`
    /// takes a new id with each newly opened capture session — a `start()`
    /// that opens a stream (`0` before the first one; a `start()` on a running
    /// handle or a failed open starts none) — and `generation`, `attempt`, and
    /// `successful_recoveries` restart with it;
    /// `dropped_events` counts over the handle's lifetime, like the event
    /// subscription itself. Reading it takes one brief lock that recovery never
    /// holds across device work, so it does not block on a reopen in progress.
    #[cfg(feature = "hotplug-recovery")]
    pub fn recovery_snapshot(&self) -> Option<RecoverySnapshot> {
        self.recovery.as_ref().map(|hub| hub.snapshot())
    }

    /// Subscribes to the auto-recovery supervisor's [`RecoveryEvent`]s
    /// *(feature `hotplug-recovery`)*.
    ///
    /// Events arrive over a bounded channel of 32 events. A subscriber that
    /// falls behind loses the **newest** events rather than stalling recovery,
    /// and each loss is counted in [`RecoverySnapshot::dropped_events`]. The
    /// subscription may be created before [`start`](Self::start) and spans the
    /// handle's lifetime, including stop/start restarts. Every event carries
    /// the `session` that emitted it: compare it with
    /// [`RecoverySnapshot::session`] to tell events a previous session left
    /// queued before a restart from the current session's.
    ///
    /// # Errors
    ///
    /// [`AudioError::ConfigurationError`] when auto-recovery was not enabled on
    /// the builder, or when the events already have a subscriber (only one is
    /// supported per handle).
    #[cfg(feature = "hotplug-recovery")]
    pub fn subscribe_recovery_events(&self) -> AudioResult<mpsc::Receiver<RecoveryEvent>> {
        match self.recovery.as_ref() {
            Some(hub) => hub.subscribe(),
            None => Err(AudioError::ConfigurationError {
                message: "auto-recovery is not enabled for this capture; call \
                          AudioCaptureBuilder::with_auto_recovery() before build()"
                    .to_string(),
            }),
        }
    }
}

/// Formats an `AudioFormat` as a compact
/// human-readable string, e.g. `"2ch 48000Hz F32"`.
///
/// Computed lazily on the query path (e.g. by a future `stream_stats()`),
/// never stored per-buffer, so it allocates only when actually called.
#[allow(dead_code)] // Consumed by stream_stats() / diagnostics on the query path.
pub(super) fn format_description_string(fmt: &AudioFormat) -> String {
    let sample_fmt = match fmt.sample_format {
        SampleFormat::I16 => "I16",
        SampleFormat::I24 => "I24",
        SampleFormat::I32 => "I32",
        SampleFormat::F32 => "F32",
    };
    format!("{}ch {}Hz {}", fmt.channels, fmt.sample_rate, sample_fmt)
}

// AudioDataStreamWrapper has been removed — async streaming will be
// re-introduced via the BridgeStream layer in a later phase.

// ── Iterator ─────────────────────────────────────────────────────────────

/// An iterator that yields audio buffers by synchronously reading from an [`AudioCapture`].
pub struct AudioBufferIterator<'a> {
    capture: &'a mut AudioCapture,
}

impl<'a> Iterator for AudioBufferIterator<'a> {
    type Item = AudioResult<AudioBuffer>;

    fn next(&mut self) -> Option<Self::Item> {
        // Read directly from the stream (not via read_buffer(), which refuses to
        // read once the stream leaves the Running state). The stream remains
        // *readable* while Stopping, so reading directly lets us DRAIN the buffered
        // tail after stop() rather than discarding it (audit R2-2).
        //
        // Semantics of try_read_chunk():
        //   Ok(Some(buf)) → yield it.
        //   Ok(None)      → no data right now (Running or Stopping with empty ring)
        //                   → wait briefly and retry.
        //   Err(StreamEnded) → terminal (Stopped/Closed/Error) AND nothing more to
        //                   read → end the iterator (return None). Other Err →
        //                   surface to the caller.
        let stream = self.capture.stream.as_ref()?; // no stream → iteration done
        loop {
            match stream.try_read_chunk() {
                Ok(Some(buffer)) => return Some(Ok(buffer)),
                Ok(None) => {
                    // No data yet. If the stream is no longer readable (terminal)
                    // the next try_read_chunk will return StreamEnded; otherwise
                    // we're Running/Stopping with a momentarily-empty ring — wait
                    // and retry so we don't busy-spin.
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                // Terminal end-of-stream: drained and done.
                Err(AudioError::StreamEnded { .. }) => return None,
                // Any other error is surfaced once.
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

// ── Drop ─────────────────────────────────────────────────────────────────

impl Drop for AudioCapture {
    fn drop(&mut self) {
        // Make stop sticky BEFORE joining the callback pump (same order as
        // `stop()`): signal the pump, then stop the stream, so a pump that is
        // mid-read inside a recovery reopen can neither install a new generation
        // nor dispatch a buffer, and the join cannot wait on live recovery.
        if let Some(pump) = self.callback_pump.as_ref() {
            pump.signal_stop();
        }
        // Best-effort stop of whatever stream we still hold. The stream's own
        // state machine decides whether this is a no-op (already stopped) or
        // a real stop; stop() is idempotent on the stream side.
        if let Some(stream) = self.stream.as_ref() {
            if stream.is_running() {
                if let Err(e) = stream.stop() {
                    log::warn!("Error stopping audio stream during drop: {:?}", e);
                }
            }
        }
        // Join the quiesced pump so its thread stops touching the stream before
        // we drop it, and is never leaked.
        if let Some(mut pump) = self.callback_pump.take() {
            pump.join();
        }
        // Drop the Arc reference (stream fully deallocated when last clone is dropped).
        self.stream.take();
        // Clear the uptime anchor alongside the stream teardown.
        self.start_instant = None;
    }
}

// ── Debug ────────────────────────────────────────────────────────────────

impl fmt::Debug for AudioCapture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let device_name = self
            .device
            .as_ref()
            .map(|d| d.name())
            .unwrap_or_else(|| "None".to_string());

        f.debug_struct("AudioCapture")
            .field("config", &self.config)
            .field("device_name", &device_name)
            .field("stream_is_some", &self.stream.is_some())
            .field("is_running", &self.is_running())
            // Never panic inside Debug: a poisoned callback mutex must not take
            // down an infallible formatter. Fall back to reporting the poison.
            .field(
                "callback_is_some",
                &match self.callback.try_lock() {
                    Ok(guard) => guard.is_some(),
                    Err(_) => false,
                },
            )
            .finish()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{CaptureTarget, StreamConfig};
    use crate::core::interface::CapturingStream;
    use std::sync::atomic::AtomicUsize;

    /// A stream whose read wedges — as a read inside a recovery reopen would —
    /// until the stream is stopped, then returns a buffer (a "late result").
    struct WedgedRead {
        stopped: AtomicBool,
        entered: Mutex<mpsc::Sender<()>>,
    }

    impl CapturingStream for WedgedRead {
        fn read_chunk(&self) -> AudioResult<AudioBuffer> {
            Err(AudioError::StreamEnded {
                reason: "the wedged mock only serves try_read_chunk".to_string(),
            })
        }

        fn try_read_chunk(&self) -> AudioResult<Option<AudioBuffer>> {
            let _ = self.entered.lock().unwrap().send(());
            let give_up = Instant::now() + Duration::from_secs(10);
            while !self.stopped.load(Ordering::SeqCst) && Instant::now() < give_up {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(Some(AudioBuffer::new(vec![0.0; 960], 2, 48_000)))
        }

        fn stop(&self) -> AudioResult<()> {
            self.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn format(&self) -> AudioFormat {
            AudioFormat::default()
        }

        fn is_running(&self) -> bool {
            !self.stopped.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn stop_signals_the_pump_and_stops_the_stream_before_joining_a_pump_wedged_in_a_read() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let stream = Arc::new(WedgedRead {
            stopped: AtomicBool::new(false),
            entered: Mutex::new(entered_tx),
        });
        let dyn_stream: Arc<dyn CapturingStream> = stream.clone();
        let delivered = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&delivered);
        let pump = AudioCapture::spawn_callback_pump(
            Arc::clone(&dyn_stream),
            Box::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .expect("pump spawns");
        let mut capture = AudioCapture {
            config: AudioCaptureConfig {
                target: CaptureTarget::SystemDefault,
                stream_config: StreamConfig::default(),
            },
            device: None,
            stream: Some(dyn_stream),
            callback: Mutex::new(None),
            callback_pump: Some(pump),
            start_instant: None,
            subscriber_dropped: Arc::new(AtomicU64::new(0)),
            #[cfg(feature = "hotplug-recovery")]
            recovery: None,
        };
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the pump is inside the wedged read");

        // The pump cannot return from its read until the stream is stopped, so
        // joining it first (the old order) would hang here.
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = capture.stop();
            let _ = done_tx.send(result.is_ok());
        });
        let stopped_ok = done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("stop() must stop the stream before it joins the pump");

        assert!(stopped_ok);
        assert!(stream.stopped.load(Ordering::SeqCst));
        assert_eq!(
            delivered.load(Ordering::SeqCst),
            0,
            "a buffer returned after stop was signalled never reaches the callback"
        );
    }
}
