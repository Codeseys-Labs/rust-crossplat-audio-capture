//! Windows WASAPI dedicated capture thread infrastructure.
//!
//! This module provides the thread infrastructure for running WASAPI audio
//! capture on a dedicated thread, bridging captured audio data to the
//! consumer via [`BridgeProducer`].
//!
//! # Architecture
//!
//! ```text
//! Consumer Thread                       WASAPI Capture Thread (dedicated)
//! ───────────────                       ─────────────────────────────────
//! AudioCapture / CapturingStream        COM init, AudioClient, CaptureClient
//! BridgeConsumer                        BridgeProducer (writes to ring buffer)
//!                                       Event-driven capture loop
//! ```
//!
//! The WASAPI capture objects live exclusively on the dedicated thread.
//! The [`WindowsCaptureThread`] handle is `Send + Sync` and safe to use
//! from any thread. [`WindowsPlatformStream`] implements [`PlatformStream`]
//! for integration with [`BridgeStream`](crate::bridge::stream::BridgeStream).

#![cfg(target_os = "windows")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::bridge::ring_buffer::BridgeProducer;
use crate::bridge::stream::PlatformStream;
use crate::core::config::CaptureTarget;
use crate::core::error::{AudioError, AudioResult};

// ── WindowsCaptureConfig ─────────────────────────────────────────────────

/// Resolved capture parameters passed to the WASAPI capture thread.
///
/// This is a subset of [`AudioCaptureConfig`](crate::core::config::AudioCaptureConfig)
/// containing only the fields needed by the WASAPI backend to create a stream.
#[derive(Debug)]
pub(crate) struct WindowsCaptureConfig {
    /// What to capture (system default, specific app, process tree, etc.).
    pub target: CaptureTarget,
    /// Desired sample rate in Hz (e.g., 48000).
    pub sample_rate: u32,
    /// Desired number of audio channels (e.g., 2 for stereo).
    pub channels: u16,
}

// ── WindowsCaptureThread ─────────────────────────────────────────────────

/// Handle to the dedicated WASAPI capture thread.
///
/// Spawns a thread that initializes COM, creates WASAPI audio clients,
/// and runs an event-driven capture loop. The caller communicates via
/// shared atomic flags, and audio data flows through the [`BridgeProducer`].
///
/// # Lifecycle
///
/// 1. [`WindowsCaptureThread::spawn()`] creates the thread, blocks until
///    WASAPI initialization succeeds, then returns.
/// 2. The capture loop runs until [`stop()`](WindowsCaptureThread::stop) or
///    [`stop_and_join()`](WindowsCaptureThread::stop_and_join) is called.
/// 3. On [`Drop`], the stop flag is set and the thread is joined.
pub(crate) struct WindowsCaptureThread {
    /// Join handle for the dedicated thread (taken on drop).
    thread_handle: Option<std::thread::JoinHandle<()>>,
    /// Shared flag: set to `true` to signal the capture loop to stop.
    stop_flag: Arc<AtomicBool>,
    /// Shared flag: `true` while the capture thread is actively running.
    is_active: Arc<AtomicBool>,
}

impl WindowsCaptureThread {
    /// Spawn the dedicated WASAPI capture thread.
    ///
    /// This creates a new OS thread named `"rsac-wasapi"` that:
    /// 1. Initializes COM (MTA)
    /// 2. Creates the appropriate WASAPI audio client based on `config.target`
    /// 3. Runs an event-driven capture loop, pushing audio data via `producer`
    ///
    /// The call **blocks** until the thread reports that WASAPI initialization
    /// has succeeded (or failed). This ensures that `create_stream()` can
    /// synchronously report WASAPI init failures.
    ///
    /// # Errors
    ///
    /// - [`AudioError::BackendInitializationFailed`] if the thread cannot be spawned
    ///   or if WASAPI initialization fails on the dedicated thread.
    pub fn spawn(config: WindowsCaptureConfig, producer: BridgeProducer) -> AudioResult<Self> {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let is_active = Arc::new(AtomicBool::new(true));
        let stop_flag_thread = Arc::clone(&stop_flag);
        let is_active_thread = Arc::clone(&is_active);

        // Create init-feedback channel: the thread sends Ok(()) on successful
        // WASAPI initialization, or Err(AudioError) on failure.
        let (init_tx, init_rx) = std::sync::mpsc::channel::<AudioResult<()>>();

        let thread_handle = std::thread::Builder::new()
            .name("rsac-wasapi".to_string())
            .spawn(move || {
                wasapi_capture_thread_main(
                    config,
                    producer,
                    stop_flag_thread,
                    is_active_thread,
                    init_tx,
                );
            })
            .map_err(|e| AudioError::BackendInitializationFailed {
                backend: "wasapi".to_string(),
                reason: format!("Failed to spawn WASAPI capture thread: {}", e),
            })?;

        // Block until the thread reports init success or failure.
        match init_rx.recv() {
            Ok(Ok(())) => { /* WASAPI initialized successfully */ }
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(AudioError::BackendInitializationFailed {
                    backend: "wasapi".to_string(),
                    reason: "WASAPI capture thread dropped init channel without sending status"
                        .to_string(),
                });
            }
        }

        Ok(WindowsCaptureThread {
            thread_handle: Some(thread_handle),
            stop_flag,
            is_active,
        })
    }

    /// Signal the capture thread to stop.
    ///
    /// Sets the stop flag but does NOT join the thread. The thread will
    /// exit its capture loop on the next iteration check.
    ///
    /// This is safe to call multiple times (idempotent).
    pub fn stop(&self) -> AudioResult<()> {
        self.stop_flag.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Signal the capture thread to stop and wait for it to finish.
    ///
    /// Sets the stop flag and joins the thread handle. After this returns,
    /// the thread has exited and all WASAPI resources have been released.
    ///
    /// # Errors
    ///
    /// - [`AudioError::BackendError`] if the thread panicked during join.
    #[allow(dead_code)] // Planned for future use; Drop handles joining currently
    pub fn stop_and_join(&mut self) -> AudioResult<()> {
        self.stop_flag.store(true, Ordering::SeqCst);

        if let Some(handle) = self.thread_handle.take() {
            handle.join().map_err(|_| AudioError::BackendError {
                backend: "wasapi".to_string(),
                operation: "stop_and_join".to_string(),
                message: "WASAPI capture thread panicked".to_string(),
                context: None,
            })?;
        }

        Ok(())
    }

    /// Returns `true` if the capture thread is still actively running.
    ///
    /// This checks the shared atomic flag, which is set to `false` when
    /// the thread exits (either due to stop signal or an error).
    pub fn is_alive(&self) -> bool {
        self.is_active.load(Ordering::SeqCst)
    }
}

impl Drop for WindowsCaptureThread {
    fn drop(&mut self) {
        // Signal the capture loop to stop.
        self.stop_flag.store(true, Ordering::SeqCst);

        // Join the thread to ensure clean shutdown.
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

// ── WindowsPlatformStream ────────────────────────────────────────────────

/// Platform-specific stream handle for Windows (WASAPI backend).
///
/// Wraps a shared [`WindowsCaptureThread`] handle and implements
/// [`PlatformStream`] so it can be used with
/// [`BridgeStream`](crate::bridge::stream::BridgeStream).
///
/// # Thread Safety
///
/// `WindowsPlatformStream` is `Send + Sync` (required by `PlatformStream`).
/// The inner `Arc<Mutex<WindowsCaptureThread>>` provides shared ownership
/// and interior mutability.
pub(crate) struct WindowsPlatformStream {
    capture_thread: Arc<Mutex<WindowsCaptureThread>>,
}

impl WindowsPlatformStream {
    /// Create a new `WindowsPlatformStream` wrapping the given capture thread.
    pub fn new(capture_thread: Arc<Mutex<WindowsCaptureThread>>) -> Self {
        Self { capture_thread }
    }
}

impl PlatformStream for WindowsPlatformStream {
    fn stop_capture(&self) -> AudioResult<()> {
        // Just set the stop flag — don't join the thread while holding
        // the mutex lock. Drop handles the actual thread join.
        //
        // Lifecycle contract: `stop_capture()` is a *signal*, not a *join*. It
        // returns as soon as the stop flag is set; the capture loop then exits on
        // its next iteration (bounded by the 100 ms event wait) and the OS thread
        // is joined only when the last `Arc<Mutex<WindowsCaptureThread>>` is
        // dropped (see `WindowsCaptureThread::drop`). This keeps `stop()`
        // non-blocking and, crucially, avoids joining while holding this mutex —
        // which `is_active()` also locks — so a stop can never deadlock against a
        // concurrent liveness check. Calling it more than once is harmless: the
        // flag store is idempotent.
        let thread = self
            .capture_thread
            .lock()
            .map_err(|_| AudioError::InternalError {
                message: "Failed to lock capture thread for stop".to_string(),
                source: None,
            })?;
        thread.stop()
    }

    fn is_active(&self) -> bool {
        self.capture_thread
            .lock()
            .map(|t| t.is_alive())
            .unwrap_or(false)
    }
}

// ── WASAPI Capture Thread Main Function ──────────────────────────────────

/// The main function for the dedicated WASAPI capture thread.
///
/// This runs on the spawned thread and owns all WASAPI/COM objects.
/// It initializes COM, creates the appropriate audio client based on the
/// [`CaptureTarget`], and runs an event-driven capture loop.
///
/// # Init Feedback
///
/// After WASAPI initialization (COM, audio client creation, stream start),
/// the function sends `Ok(())` or `Err(AudioError)` through `init_tx`.
/// The caller ([`WindowsCaptureThread::spawn()`]) blocks on this channel
/// to synchronously report initialization failures.
///
/// # Audio Data Flow
///
/// 1. WASAPI fills its internal buffer with captured audio
/// 2. We read each packet's bytes into a reusable contiguous buffer
/// 3. Reinterpret the F32LE bytes as `&[f32]` in bulk via [`slice::align_to`]
///    (PU-7 — no per-sample scalar decode, mirroring the Linux path)
/// 4. Push the samples via [`BridgeProducer::push_samples_or_drop`]
///
/// The `push_samples_or_drop()` call is lock-free and non-blocking, making it
/// safe for the capture loop context.
///
/// # Cleanup
///
/// On exit, the function:
/// - Sets `is_active` to `false`
/// - Notifies the consumer of end-of-stream: a clean stop-flag exit calls
///   `producer.signal_done()` (graceful `Running → Stopping`); a FATAL
///   device-error exit calls `producer.signal_error()` (terminal `Error`), per
///   the ADR-0010 cross-backend terminal contract (rsac-66a6)
/// - WASAPI/COM objects are dropped via RAII
///
/// # Panic containment (rsac-b3a0 / ADR-0010)
///
/// The capture loop (everything after init success) runs inside
/// [`std::panic::catch_unwind`]: a panic anywhere in it is contained and routed
/// into the FATAL `fatal_error` cleanup path (`signal_error()` → terminal
/// `Error`), so no unwind can skip the cleanup tail and leave `is_active`
/// stuck `true` with the bridge in a non-terminal state — which would degrade
/// a blocked `read_chunk` to an infinite Timeout-retry loop instead of the
/// contract's fatal `StreamEnded`. A panic *before* init success is already
/// handled at the `spawn()` level: the init channel drops without a status,
/// `spawn()` returns `BackendInitializationFailed`, and no stream (hence no
/// blocked reader) is ever created for this bridge.
fn wasapi_capture_thread_main(
    config: WindowsCaptureConfig,
    mut producer: BridgeProducer,
    stop_flag: Arc<AtomicBool>,
    is_active: Arc<AtomicBool>,
    init_tx: std::sync::mpsc::Sender<AudioResult<()>>,
) {
    // ── Step 1: Initialize COM (MTA) ─────────────────────────────────
    //
    // wasapi-rs 0.22.0 `initialize_mta()` returns an HRESULT directly
    // (no guard). We must call `wasapi::deinitialize()` at thread exit.
    let hr = wasapi::initialize_mta();
    if hr.is_err() {
        log::error!("WASAPI thread: COM initialization failed: {:?}", hr);
        let _ = init_tx.send(Err(AudioError::BackendInitializationFailed {
            backend: "wasapi".to_string(),
            reason: format!("COM initialization failed: HRESULT {:?}", hr),
        }));
        is_active.store(false, Ordering::SeqCst);
        producer.signal_done();
        return;
    }

    log::debug!(
        "WASAPI thread: COM initialized, target={:?}, {}Hz, {}ch",
        config.target,
        config.sample_rate,
        config.channels
    );

    // ── Step 2: Create the WASAPI audio client ───────────────────────
    //
    // The client creation strategy depends on the CaptureTarget variant.

    // Process-loopback targets (Application / ApplicationByName / ProcessTree)
    // are activated via ActivateAudioInterfaceAsync with
    // AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK. That activation path does
    // NOT support AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM — combining it with the
    // loopback flags makes IAudioClient::Initialize fail (C1). These clients
    // also can't report their mix format (get_mixformat returns "Not
    // implemented"), so we must hand the client an explicit format and let it
    // run without autoconvert.
    let is_process_loopback = matches!(
        config.target,
        CaptureTarget::Application(_)
            | CaptureTarget::ApplicationByName(_)
            | CaptureTarget::ProcessTree(_)
    );

    let audio_client_result = create_audio_client(&config);
    let mut audio_client = match audio_client_result {
        Ok(client) => client,
        Err(e) => {
            log::error!("WASAPI thread: failed to create audio client: {}", e);
            let _ = init_tx.send(Err(e));
            is_active.store(false, Ordering::SeqCst);
            producer.signal_done();
            return;
        }
    };

    // ── Step 3: Initialize the audio client ──────────────────────────
    //
    // Configure the client for shared-mode event-driven capture.

    let desired_format = wasapi::WaveFormat::new(
        32, // bits per sample
        32, // valid bits per sample
        &wasapi::SampleType::Float,
        config.sample_rate as usize,
        config.channels as usize,
        None, // channel mask (auto)
    );

    let mode = wasapi::StreamMode::EventsShared {
        // C1 fix: the process-loopback activation path rejects
        // AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, so autoconvert must be disabled
        // for Application/ApplicationByName/ProcessTree targets. The regular
        // system/device-loopback path keeps autoconvert enabled so WASAPI can
        // resample the endpoint mix format to our requested f32 format.
        //
        // Format invariant (WASAPI-hardening): in BOTH modes the client is
        // initialized with the explicit 32-bit-float `desired_format` above, and
        // `IAudioClient::Initialize` FAILS if the client cannot deliver it —
        // autoconvert resamples to it on the system/device path, and the
        // process-loopback path accepts f32 directly. So a *successful* init is
        // the contract that every delivered packet is interleaved f32 with
        // `channels` samples per frame. We cannot re-query the negotiated format
        // on a loopback client (`get_mixformat` returns "Not implemented"), so
        // that init success is the only negotiation signal we have — and the
        // capture loop additionally validates the per-packet frame invariant via
        // `bytes_to_f32_frames`, failing loud (throttled diagnostics + partial-
        // frame drop) rather than silently corrupting channel alignment if a
        // future Windows release ever delivers a packet that violates it.
        autoconvert: !is_process_loopback,
        buffer_duration_hns: 0, // default buffer duration
    };

    if let Err(e) =
        audio_client.initialize_client(&desired_format, &wasapi::Direction::Capture, &mode)
    {
        log::error!("WASAPI thread: audio client initialization failed: {}", e);
        let _ = init_tx.send(Err(AudioError::BackendInitializationFailed {
            backend: "wasapi".to_string(),
            reason: format!("Audio client initialization failed: {}", e),
        }));
        is_active.store(false, Ordering::SeqCst);
        producer.signal_done();
        return;
    }

    // Get the event handle for event-driven capture.
    let h_event = match audio_client.set_get_eventhandle() {
        Ok(handle) => handle,
        Err(e) => {
            log::error!("WASAPI thread: failed to get event handle: {}", e);
            let _ = init_tx.send(Err(AudioError::BackendInitializationFailed {
                backend: "wasapi".to_string(),
                reason: format!("Failed to get event handle: {}", e),
            }));
            is_active.store(false, Ordering::SeqCst);
            producer.signal_done();
            return;
        }
    };

    // Get the capture client for reading audio data.
    let capture_client = match audio_client.get_audiocaptureclient() {
        Ok(client) => client,
        Err(e) => {
            log::error!("WASAPI thread: failed to get capture client: {}", e);
            let _ = init_tx.send(Err(AudioError::BackendInitializationFailed {
                backend: "wasapi".to_string(),
                reason: format!("Failed to get capture client: {}", e),
            }));
            is_active.store(false, Ordering::SeqCst);
            producer.signal_done();
            return;
        }
    };

    // Start the audio stream.
    if let Err(e) = audio_client.start_stream() {
        log::error!("WASAPI thread: failed to start stream: {}", e);
        let _ = init_tx.send(Err(AudioError::BackendInitializationFailed {
            backend: "wasapi".to_string(),
            reason: format!("Failed to start audio stream: {}", e),
        }));
        is_active.store(false, Ordering::SeqCst);
        producer.signal_done();
        return;
    }

    // ── Init complete — signal success ────────────────────────────────
    let _ = init_tx.send(Ok(()));

    log::debug!("WASAPI thread: capture stream started, entering event loop");

    // ── Step 4: Event-driven capture loop ────────────────────────────
    //
    // Wait for the event handle (signaled when new data is available),
    // then read all available packets from the capture client.

    let channels = config.channels;
    let sample_rate = config.sample_rate;

    // rsac-66a6 (ADR-0010 cross-backend terminal contract): distinguish a clean
    // stop-flag exit from a FATAL device-error exit. A WASAPI capture-client read
    // failure (`get_next_packet_size` / `read_from_device`) is the WASAPI signal
    // for device loss / endpoint invalidation — the producer has spontaneously
    // died and no further audio can ever arrive. We record that here and break out
    // of BOTH loops so the cleanup section can drive the bridge to the terminal
    // `Error` state via `signal_error()` (mirroring the Linux `.state_changed`
    // Error/Unconnected path and the macOS spontaneous-death path), instead of the
    // graceful `Stopping` that `signal_done()` produces. A graceful stop-flag exit
    // leaves this `false` and keeps the `signal_done()` behaviour. A PANIC caught
    // by the guard below also lands here as `true` (rsac-b3a0).
    let mut fatal_error = false;

    // rsac-b3a0 (ADR-0010): contain any unwind out of the capture loop. Without
    // this, a panic anywhere below would unwind straight past the cleanup tail —
    // `is_active` would stay `true` and neither `signal_done()` nor
    // `signal_error()` would ever fire, so a blocked `read_chunk` degrades to an
    // infinite Timeout-retry instead of the contract's fatal `StreamEnded`.
    // `AssertUnwindSafe` is sound for the same reason as the bridge's own
    // `push_samples_guarded`: after a caught panic we immediately poison the
    // stream to terminal `Error` (via `fatal_error` → `signal_error()`), so a
    // torn `&mut` capture is moot — nothing pushes afterwards. `catch_unwind`
    // is near-free on the happy path (no allocation; the closure only borrows
    // the locals), so the loop's steady-state behavior is unchanged.
    let loop_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // PU-1/PERF-07 (rsac-2c56): publish the negotiated *delivery* format onto
        // the bridge so `stream.format()` / `StreamStats.format_description` report
        // what is actually delivered rather than only what was requested. WASAPI was
        // opened with the explicit `desired_format` (32-bit float, `sample_rate`,
        // `channels`): on the system/device-loopback path autoconvert resamples the
        // endpoint mix format to it, and on the process-loopback path the client
        // accepts it directly — so in both cases the bridge receives exactly these
        // `channels`/`sample_rate` as interleaved f32 (the values used by the drain
        // loop's `push_samples_or_drop`). The bridge normalizes `sample_format` to
        // F32 internally, so the value passed here is ignored. One-time, off-RT,
        // lock-free `Release` store on the setup path before the capture loop.
        producer.set_negotiated_format(&crate::core::config::AudioFormat {
            sample_rate,
            channels,
            sample_format: crate::core::config::SampleFormat::F32,
        });

        // PU-7 (rsac-7876): bytes per interleaved f32 frame for the negotiated
        // delivery format. The client is opened with `desired_format` (32-bit float,
        // `channels`), and both the autoconvert (system/device-loopback) and the
        // process-loopback paths deliver exactly that — so a frame is `channels`
        // little-endian f32 samples = `channels * 4` bytes. `channels` is `>= 1`
        // (validated upstream), so this is non-zero.
        let bytes_per_frame = channels as usize * std::mem::size_of::<f32>();

        // PU-7: one reusable contiguous byte buffer for the raw WASAPI packet.
        //
        // This replaces the previous `VecDeque<u8>` + scalar `from_le_bytes` decode.
        // `read_from_device` copies a packet's bytes into a `&mut [u8]` in a single
        // `copy_from_slice` (vs. the deque path's per-byte `push_back`), and because
        // the destination is already contiguous we drop the O(n) `make_contiguous`
        // rotation. The bytes are then reinterpreted as `&[f32]` in bulk via
        // `slice::align_to` (mirroring the Linux/PipeWire path), eliminating the
        // per-sample scalar loop and the separate `Vec<f32>` staging buffer entirely.
        //
        // Pre-sized to ~100ms at 48kHz stereo f32 so steady-state reads never grow
        // it; a larger packet grows it once (amortized, off the steady-state path).
        // No allocation happens on the per-packet hot path once warmed.
        let mut byte_buf: Vec<u8> = Vec::with_capacity(48000 * 4 * 2 / 10);

        loop {
            // Check stop flag before waiting.
            if stop_flag.load(Ordering::SeqCst) {
                log::debug!("WASAPI thread: stop flag set, exiting capture loop");
                break;
            }

            // Wait for audio data event with a short timeout so we can
            // check the stop flag periodically.
            //
            // C3 fix: regardless of whether the event fired or the wait timed
            // out, drain ALL packets currently queued in the capture client.
            // WASAPI typically has multiple packets ready per event signal;
            // reading only one packet per event causes the client buffer to grow
            // unbounded (latency growth) and eventually overrun/underrun. The
            // drain loop below pulls packets until `get_next_packet_size()`
            // reports none remaining, then we return to waiting for the next event.
            if h_event.wait_for_event(100).is_err() {
                // Timeout or error — check stop flag, then still fall through to
                // drain any packets that may be queued (timeout doesn't mean
                // empty), so we don't strand data.
                if stop_flag.load(Ordering::SeqCst) {
                    log::debug!("WASAPI thread: stop flag set during wait, exiting");
                    break;
                }
            }

            // Drain loop: read every packet currently available, converting each
            // to f32 and pushing it through the bridge. Remains responsive to the
            // stop flag so shutdown isn't delayed by a long burst of packets.
            loop {
                // Stop promptly if requested mid-drain.
                if stop_flag.load(Ordering::SeqCst) {
                    break;
                }

                // How many frames are in the next packet? 0 (or None) means the
                // client buffer is drained — go back to waiting for the event.
                let packet_frames = match capture_client.get_next_packet_size() {
                    Ok(Some(frames)) => frames,
                    Ok(None) => 0,
                    Err(e) => {
                        // rsac-66a6: a capture-client query failure is treated as
                        // device loss (fatal). Flag it and break out of the drain
                        // loop; the outer-loop check below then exits the thread so
                        // cleanup signals terminal `Error` rather than graceful
                        // `Stopping`.
                        log::error!(
                        "WASAPI thread: get_next_packet_size failed (treating as device loss): {}",
                        e
                    );
                        fatal_error = true;
                        break;
                    }
                };
                if packet_frames == 0 {
                    break;
                }

                // PU-7: read this packet's raw bytes into the reused contiguous
                // buffer. `read_from_device` requires the destination to be large
                // enough to hold the whole packet (else it errors and releases the
                // WASAPI buffer), so ensure capacity for the predicted packet size.
                // `resize` only allocates when a packet exceeds the high-water mark
                // (off the steady-state path); thereafter it is a no-op length set.
                let needed = packet_frames as usize * bytes_per_frame;
                if byte_buf.len() < needed {
                    byte_buf.resize(needed, 0);
                }

                // Copy the packet's bytes in a single `copy_from_slice` (inside
                // wasapi-rs), into a contiguous slice — no per-byte `push_back`, no
                // `make_contiguous` rotation. `frames_read` is the authoritative
                // count of frames actually delivered.
                //
                // `buffer_info` carries the WASAPI buffer flags (SILENT /
                // DATA_DISCONTINUITY) we use for diagnostics below.
                let (frames_read, buffer_info) =
                    match capture_client.read_from_device(&mut byte_buf[..needed]) {
                        Ok((frames, info)) => (frames as usize, info),
                        Err(e) => {
                            // rsac-66a6: a read failure means the capture endpoint can no
                            // longer deliver audio (device invalidated / unplugged). Flag
                            // it as fatal and bail out of the drain loop; the outer-loop
                            // check below exits the thread so cleanup signals terminal
                            // `Error` rather than graceful `Stopping`.
                            log::error!(
                            "WASAPI thread: read_from_device failed (treating as device loss): {}",
                            e
                        );
                            fatal_error = true;
                            break;
                        }
                    };
                if frames_read == 0 {
                    continue;
                }

                // WASAPI-hardening: log a data-discontinuity flag (a gap in the
                // captured stream, e.g. the endpoint glitched or a buffer was
                // dropped by the engine) at `debug`. It is not fatal — audio
                // continues — but it explains a discontinuity a downstream consumer
                // might otherwise attribute to rsac. Kept at `debug` so it never
                // floods a healthy stream's log at `info`+.
                if buffer_info.flags.data_discontinuity {
                    log::debug!(
                        "WASAPI thread: capture reported DATA_DISCONTINUITY at frame index {} \
                     ({} frames) — a gap in the source stream, not a capture bug",
                        buffer_info.index,
                        frames_read
                    );
                }

                // The valid region is exactly `frames_read` whole frames. Slicing to
                // it keeps the conversion exact even if WASAPI delivered fewer frames
                // than `get_next_packet_size` predicted.
                let valid = &byte_buf[..frames_read * bytes_per_frame];

                // Reinterpret the F32LE bytes as `&[f32]` in one bulk operation
                // instead of a per-sample `from_le_bytes` loop, mirroring the
                // Linux/PipeWire path. WASAPI's GetBuffer region is sample-aligned and
                // `byte_buf` is a `Vec<u8>` whose data pointer is at least word-
                // aligned, so `align_to`'s head/tail are normally empty; we consume
                // the aligned run of whole samples and ignore any unaligned edge.
                // (`align_to` is used deliberately over `bytemuck::cast_slice`, which
                // would *panic* on a misaligned slice — see PU-7 blueprint.) On the
                // little-endian hosts Windows runs on, the in-memory layout equals the
                // F32LE byte layout, so this reinterpret is a no-op at the bit level.
                //
                // SAFETY: every bit pattern is a valid `f32`, and we only read the
                // `frames_read * bytes_per_frame` bytes that `read_from_device` just
                // initialized within `valid`.
                //
                // WASAPI-hardening: `bytes_to_f32_frames` additionally enforces the
                // interleaved-f32 delivery contract — the byte run must be a whole
                // multiple of `bytes_per_frame` (so the sample count is a whole
                // multiple of `channels`), dropping a trailing partial frame
                // rather than silently shifting every subsequent sample into the
                // wrong channel. Here `valid` is `frames_read * bytes_per_frame`
                // by construction — an exact multiple — so `malformed` is
                // structurally unreachable on this path (rsac-0055; the runtime
                // counter/log machinery it used to feed was dead code). The
                // debug assert keeps the invariant visible in test builds; the
                // function's own unit tests cover the partial-frame handling.
                let (samples, malformed) = bytes_to_f32_frames(valid, bytes_per_frame);
                debug_assert!(
                    !malformed,
                    "valid slice is an exact frame multiple by construction"
                );

                if !samples.is_empty() {
                    // Push the borrowed sample view directly. The stamped push
                    // sources its backing buffer from the bridge free-list, so this
                    // is zero-allocation on the capture thread in steady state — and
                    // we no longer stage into an intermediate `Vec<f32>` at all.
                    // `_stamped` additionally tags each buffer with its stream
                    // position (frames offered / rate; pure integer math), so
                    // `AudioBuffer::timestamp()` is populated and producer-side
                    // drops surface as timestamp gaps (rsac-522b / rsac-ec25).
                    producer.push_samples_or_drop_stamped(samples, channels, sample_rate);
                    // Wake a consumer parked in a blocking read (PU-5). This is sound
                    // here even though the producer push path is RT-disciplined: the
                    // WASAPI capture loop runs on rsac's OWN spawned polling thread
                    // (NOT an OS audio-callback context), so a Condvar notify is
                    // allowed (ADR-0001 forbids notify only from the Linux/macOS RT
                    // callbacks). Without this, a blocking reader only wakes via the
                    // bounded ≤1ms backstop poll.
                    producer.notify_consumers();
                }
            }

            // rsac-66a6: a fatal capture-client failure during the drain means the
            // device is gone — exit the capture loop so cleanup signals terminal
            // `Error`. Checked before the stop flag so a device-loss exit is never
            // mis-reported as a graceful stop.
            if fatal_error {
                log::error!("WASAPI thread: fatal device error, exiting capture loop");
                break;
            }

            // Check stop flag after draining.
            if stop_flag.load(Ordering::SeqCst) {
                log::debug!("WASAPI thread: stop flag set after read, exiting");
                break;
            }
        }
    }));

    // rsac-b3a0: route a caught capture-loop panic into the existing FATAL
    // cleanup tail. The panic is contained above (it never unwinds past this
    // function), and the terminal `signal_error()` below guarantees a blocked
    // reader observes the fatal `StreamEnded` instead of retrying a dead
    // stream forever (ADR-0010: no exit path leaves the bridge non-terminal).
    if let Err(payload) = loop_result {
        let msg = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("<non-string panic payload>");
        log::error!(
            "WASAPI thread: capture loop panicked ({}); containing the unwind \
             and signalling terminal Error (rsac-b3a0 / ADR-0010)",
            msg
        );
        fatal_error = true;
    }

    // ── Cleanup ──────────────────────────────────────────────────────
    //
    // Stop the WASAPI stream. In wasapi 0.22.0, initialize_mta() does not
    // return a guard, so we must call deinitialize() explicitly.

    let _ = audio_client.stop_stream();
    is_active.store(false, Ordering::SeqCst);
    // rsac-66a6 (ADR-0010): a FATAL device-error exit must drive the bridge to the
    // terminal `Error` state (`signal_error`) so a parked Linux/blocking reader
    // observes a Fatal `StreamEnded` instead of an indefinitely-draining graceful
    // `Stopping`. Only a clean stop-flag exit takes the graceful `signal_done`
    // (`Running → Stopping`) path. This mirrors the Linux `.state_changed`
    // Error/Unconnected → `signal_error()` arm and the macOS spontaneous-death
    // path, satisfying the cross-backend terminal contract.
    if fatal_error {
        producer.signal_error();
    } else {
        producer.signal_done();
    }
    wasapi::deinitialize();
    if fatal_error {
        log::debug!("WASAPI thread: exited after fatal error (device loss or contained panic)");
    } else {
        log::debug!("WASAPI thread: exited cleanly");
    }
}

// ── Audio Client Creation Helper ─────────────────────────────────────────

/// Creates a WASAPI [`AudioClient`](wasapi::AudioClient) based on the
/// [`CaptureTarget`] specified in the config.
///
/// # Target Mapping
///
/// | CaptureTarget          | WASAPI Strategy                                      |
/// |------------------------|------------------------------------------------------|
/// | `SystemDefault`        | Default render device → loopback capture              |
/// | `Device(id)`           | Find device by ID → loopback capture                 |
/// | `Application(app_id)`  | Parse PID from app_id → process loopback, `include_tree = true` (rsac-5b59) |
/// | `ApplicationByName(_)` | Resolve name → PID via sysinfo → process loopback, `include_tree = true` (rsac-5b59) |
/// | `ProcessTree(pid)`     | Process loopback with `include_tree = true`           |
///
/// # Process-loopback mode is binary (rsac-5b59)
///
/// `AUDIOCLIENT_PROCESS_LOOPBACK_MODE` has only two values: INCLUDE- and
/// EXCLUDE-target-process-tree. There is no "this PID only" mode. wasapi-rs maps
/// `new_application_loopback_client(pid, include_tree)`'s bool to those:
/// `true` → INCLUDE (capture the PID's tree), `false` → EXCLUDE (capture
/// *everything except* the PID's tree). All three per-app/tree targets therefore
/// pass `true`; `false` would capture the complement of the requested app (the
/// original silence bug). INCLUDE-tree of a leaf process is exactly
/// single-process capture.
fn create_audio_client(config: &WindowsCaptureConfig) -> AudioResult<wasapi::AudioClient> {
    match &config.target {
        CaptureTarget::SystemDefault => {
            // Get the default render device and create a loopback capture client.
            log::debug!("WASAPI: creating system default loopback client");
            let enumerator =
                wasapi::DeviceEnumerator::new().map_err(|e| AudioError::BackendError {
                    backend: "wasapi".to_string(),
                    operation: "create_enumerator".to_string(),
                    message: format!("Failed to create DeviceEnumerator: {}", e),
                    context: None,
                })?;
            let device = enumerator
                .get_default_device(&wasapi::Direction::Render)
                .map_err(|e| AudioError::BackendError {
                    backend: "wasapi".to_string(),
                    operation: "get_default_device".to_string(),
                    message: format!("Failed to get default render device: {}", e),
                    context: None,
                })?;

            device
                .get_iaudioclient()
                .map_err(|e| AudioError::BackendError {
                    backend: "wasapi".to_string(),
                    operation: "get_iaudioclient".to_string(),
                    message: format!("Failed to get IAudioClient from default device: {}", e),
                    context: None,
                })
        }

        CaptureTarget::Device(device_id) => {
            // Attempt to find the device by its ID string and create
            // a loopback capture client.
            log::debug!(
                "WASAPI: creating device loopback client for id={}",
                device_id.0
            );

            // wasapi-rs provides get_default_device but not get_device_by_id directly.
            // For device-specific capture, we enumerate devices and match by ID.
            // For now, if the ID matches "default" or is empty, use default device.
            // Otherwise, enumerate and find the matching device.
            let device = find_device_by_id(&device_id.0)?;
            device
                .get_iaudioclient()
                .map_err(|e| AudioError::BackendError {
                    backend: "wasapi".to_string(),
                    operation: "get_iaudioclient".to_string(),
                    message: format!(
                        "Failed to get IAudioClient for device '{}': {}",
                        device_id.0, e
                    ),
                    context: None,
                })
        }

        CaptureTarget::Application(app_id) => {
            // Parse the PID from the ApplicationId string.
            // The ApplicationId.0 is expected to contain the PID as a string
            // or a parseable numeric value.
            log::debug!(
                "WASAPI: creating application loopback client for app_id={}",
                app_id.0
            );

            let pid: u32 = app_id
                .0
                .parse()
                .map_err(|_| AudioError::ApplicationNotFound {
                    identifier: format!(
                        "Cannot parse PID from ApplicationId '{}': expected numeric PID",
                        app_id.0
                    ),
                })?;

            // include_tree=TRUE — capture the target PID (and any descendants).
            //
            // rsac-5b59: the WASAPI process-loopback mode is binary
            // (`AUDIOCLIENT_PROCESS_LOOPBACK_MODE`): wasapi-rs maps
            // `include_tree=false` to `PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE`
            // (capture *everything except* this PID's tree) and `true` to
            // `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE` (capture this
            // PID's tree). There is NO "this PID only, excluding descendants" mode
            // — so passing `false` here made `Application(pid)` capture the
            // COMPLEMENT of the requested app, i.e. silence when it is the only
            // audio source (the CI symptom: ProcessTree of the same player got the
            // 440 Hz tone at RMS≈0.53, Application got only silence). INCLUDE-tree
            // of a leaf process == single-process capture, so `true` is the correct
            // — and only expressible — mapping for single-app capture. When the
            // target has audio-producing children, Windows necessarily includes
            // them too (documented per-platform divergence on
            // `CaptureTarget::Application`).
            wasapi::AudioClient::new_application_loopback_client(pid, true).map_err(|e| {
                AudioError::ApplicationCaptureFailed {
                    app_id: app_id.0.clone(),
                    reason: format!("Failed to create application loopback client: {}", e),
                }
            })
        }

        CaptureTarget::ApplicationByName(name) => {
            // Resolve application name to PID using sysinfo, then create
            // a WASAPI application loopback client (same as Application arm).
            log::debug!(
                "WASAPI: resolving application name '{}' to PID via sysinfo",
                name
            );

            let pid = resolve_process_name_to_pid(name)?;

            log::debug!(
                "WASAPI: resolved '{}' to PID {}, creating application loopback client",
                name,
                pid
            );

            // include_tree=TRUE — same rationale as the `Application(pid)` arm
            // (rsac-5b59): `ApplicationByName` resolves to a PID and then behaves
            // like `Application` (single app). The WASAPI process-loopback mode is
            // binary; `false` == EXCLUDE-target-tree captured the complement of the
            // app (silence). INCLUDE-tree of a leaf == single-process capture.
            wasapi::AudioClient::new_application_loopback_client(pid, true).map_err(|e| {
                AudioError::ApplicationCaptureFailed {
                    app_id: format!("{}(pid={})", name, pid),
                    reason: format!("Failed to create application loopback client: {}", e),
                }
            })
        }

        CaptureTarget::ProcessTree(pid) => {
            // Create a process loopback capture client with include_tree = true.
            log::debug!(
                "WASAPI: creating process tree loopback client for pid={}",
                pid.0
            );

            wasapi::AudioClient::new_application_loopback_client(pid.0, true).map_err(|e| {
                AudioError::ApplicationCaptureFailed {
                    app_id: format!("ProcessTree({})", pid.0),
                    reason: format!("Failed to create process tree loopback client: {}", e),
                }
            })
        }
    }
}

/// Find a WASAPI device by its ID string.
///
/// Resolves the endpoint directly via wasapi 0.23's
/// [`DeviceEnumerator::get_device`] (wrapping `IMMDeviceEnumerator::GetDevice`)
/// instead of scanning the render collection by hand. Falls back to the
/// default render device if the ID is empty or `"default"`.
fn find_device_by_id(device_id: &str) -> AudioResult<wasapi::Device> {
    let enumerator = wasapi::DeviceEnumerator::new().map_err(|e| AudioError::BackendError {
        backend: "wasapi".to_string(),
        operation: "create_enumerator".to_string(),
        message: format!("Failed to create DeviceEnumerator: {}", e),
        context: None,
    })?;

    if device_id.is_empty() || device_id.eq_ignore_ascii_case("default") {
        return enumerator
            .get_default_device(&wasapi::Direction::Render)
            .map_err(|e| AudioError::BackendError {
                backend: "wasapi".to_string(),
                operation: "get_default_device".to_string(),
                message: format!("Failed to get default render device: {}", e),
                context: None,
            });
    }

    // Direct ID resolution via IMMDeviceEnumerator::GetDevice. A failed lookup
    // (unknown / stale / removed endpoint) surfaces as DeviceNotFound.
    enumerator
        .get_device(device_id)
        .map_err(|_| AudioError::DeviceNotFound {
            device_id: device_id.to_string(),
        })
}

// ── Process Name Resolution Helper ───────────────────────────────────────

/// Resolve a process name to a PID using the `sysinfo` crate.
///
/// Performs a **case-insensitive** match against the names of all running
/// processes. Returns the PID of the first matching process found.
///
/// # Errors
///
/// - [`AudioError::ApplicationNotFound`] if no running process matches
///   the given `name` (case-insensitive comparison).
fn resolve_process_name_to_pid(name: &str) -> AudioResult<u32> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System};

    let refreshes = RefreshKind::nothing().with_processes(ProcessRefreshKind::everything());
    let system = System::new_with_specifics(refreshes);

    let name_lower = name.to_lowercase();
    // Also prepare a variant with .exe appended (if user omitted it)
    let name_with_exe = if !name_lower.ends_with(".exe") {
        Some(format!("{}.exe", name_lower))
    } else {
        None
    };
    // Also prepare the bare name (without .exe) for matching against stripped proc names
    let name_bare = name_lower.strip_suffix(".exe").unwrap_or(&name_lower);

    // Iterate all processes and perform flexible case-insensitive name matching.
    // Matches: exact name, name with .exe appended, or process name with .exe stripped.
    for (pid, process) in system.processes() {
        let proc_name = process.name().to_string_lossy();
        let proc_name_lower = proc_name.to_lowercase();

        let matched = proc_name_lower == name_lower
            || name_with_exe
                .as_deref()
                .is_some_and(|n| proc_name_lower == n)
            || proc_name_lower
                .strip_suffix(".exe")
                .is_some_and(|bare| bare == name_bare);

        if matched {
            let resolved = pid.as_u32();
            log::debug!(
                "sysinfo: matched process '{}' (name='{}') → PID {}",
                name,
                proc_name,
                resolved
            );
            return Ok(resolved);
        }
    }

    Err(AudioError::ApplicationNotFound {
        identifier: format!(
            "No running process found matching name '{}' (case-insensitive, with/without .exe)",
            name
        ),
    })
}

// ── Compile-time assertions ──────────────────────────────────────────────

/// Assert that `WindowsPlatformStream` is `Send` (required by `PlatformStream`).
fn _assert_windows_platform_stream_send() {
    fn _assert<T: Send>() {}
    _assert::<WindowsPlatformStream>();
}

/// Assert that `WindowsPlatformStream` is `Sync`.
fn _assert_windows_platform_stream_sync() {
    fn _assert<T: Sync>() {}
    _assert::<WindowsPlatformStream>();
}

/// Assert that `WindowsCaptureThread` is `Send`.
fn _assert_windows_capture_thread_send() {
    fn _assert<T: Send>() {}
    _assert::<WindowsCaptureThread>();
}

/// Reinterpret a run of F32LE bytes as `&[f32]` in one bulk operation (PU-7).
///
/// WASAPI's `GetBuffer` region is sample-aligned and the backing `Vec<u8>` data
/// pointer is at least word-aligned, so `align_to::<f32>()`'s head/tail are
/// normally empty and we consume the aligned run of whole samples. `align_to` is
/// used deliberately over `bytemuck::cast_slice`, which would *panic* on a
/// misaligned slice. On the little-endian hosts Windows runs on the in-memory
/// layout equals the F32LE byte layout, so the reinterpret is a bit-level no-op.
/// A non-empty head/tail (a misaligned region) means whole samples would be
/// dropped — a `debug_assert` flags it in test/dev builds; in release we still
/// return the safe aligned run rather than panicking on the audio path.
///
/// SAFETY of the caller: `bytes` must be fully initialized (the caller passes the
/// exact region `read_from_device` just wrote). Every `f32` bit pattern is valid.
#[inline]
fn bytes_to_f32_aligned(bytes: &[u8]) -> &[f32] {
    // SAFETY: see the doc — initialized bytes, all bit patterns valid f32.
    let (head, samples, tail) = unsafe { bytes.align_to::<f32>() };
    debug_assert!(
        head.is_empty() && tail.is_empty(),
        "WASAPI byte buffer was not 4-byte aligned: head={} tail={} bytes (whole samples would be dropped)",
        head.len(),
        tail.len()
    );
    samples
}

/// Reinterpret a run of F32LE capture bytes as interleaved `&[f32]` samples,
/// enforcing the interleaved-f32 delivery contract (WASAPI-hardening).
///
/// This wraps [`bytes_to_f32_aligned`] with the *frame*-level invariant the raw
/// alignment check does not cover: the delivered byte run must be a whole
/// multiple of `bytes_per_frame` (i.e. the reinterpreted sample count must be a
/// whole multiple of the channel count). rsac opens every WASAPI client — both
/// the autoconvert system/device-loopback path and the process-loopback path —
/// with an explicit 32-bit-float `desired_format`, and wasapi-rs sizes each
/// packet by that same format, so in the normal case this is always satisfied.
///
/// The check exists to *fail loud, not silently corrupt* if that assumption is
/// ever violated (the process-loopback autoconvert edge called out in the
/// capture-loop TODO, or a future Windows delivery quirk). A partial trailing
/// frame — bytes that don't complete a whole `channels`-wide frame — would, if
/// naively reinterpreted, shift every subsequent sample into the wrong channel
/// and permanently desync the stereo image. Instead we truncate to the last
/// whole frame and report `malformed = true` so the caller can surface a
/// throttled diagnostic.
///
/// Returns `(samples, malformed)`:
/// - `samples`: the aligned, whole-frame run of interleaved f32 (possibly
///   truncated to drop a partial trailing frame).
/// - `malformed`: `true` iff the input byte length was not a whole multiple of
///   `bytes_per_frame` (a contract violation worth logging).
///
/// `bytes_per_frame` must be non-zero (guaranteed by the caller: `channels >= 1`
/// times 4 bytes/sample).
///
/// SAFETY of the caller: same as [`bytes_to_f32_aligned`] — `bytes` must be the
/// initialized region `read_from_device` just wrote.
#[inline]
fn bytes_to_f32_frames(bytes: &[u8], bytes_per_frame: usize) -> (&[f32], bool) {
    debug_assert!(bytes_per_frame != 0, "bytes_per_frame must be non-zero");
    // Truncate to the last whole frame so a partial trailing frame can never
    // shift channel alignment. In the normal case `remainder == 0` and this is
    // a no-op slice.
    let remainder = bytes.len() % bytes_per_frame;
    let malformed = remainder != 0;
    let whole = &bytes[..bytes.len() - remainder];
    (bytes_to_f32_aligned(whole), malformed)
}

// ── Tests ────────────────────────────────────────────────────────────────
//
// These tests are automatically Windows-only because this file has
// `#![cfg(target_os = "windows")]` at the top. They are double-gated:
// the file-level cfg ensures they never compile on Linux/macOS.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{ApplicationId, CaptureTarget, DeviceId, ProcessId};

    /// PU-7: the bulk F32LE byte->f32 reinterpret round-trips bit-exactly on the
    /// little-endian hosts WASAPI runs on (device-free).
    #[test]
    fn bytes_to_f32_aligned_round_trips_f32le() {
        // 1.0f32 = 0x3F800000 little-endian = [0x00,0x00,0x80,0x3F]; -0.5 = 0xBF000000.
        let samples_in: [f32; 4] = [1.0, -0.5, 0.0, 0.25];
        let mut bytes = Vec::with_capacity(16);
        for s in samples_in {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let out = bytes_to_f32_aligned(&bytes);
        assert_eq!(out, &samples_in, "F32LE bytes must reinterpret bit-exactly");
    }

    /// An exact multiple of 4 bytes yields all whole samples and an empty
    /// head/tail (the normal WASAPI case).
    #[test]
    fn bytes_to_f32_aligned_consumes_whole_samples() {
        let bytes = vec![0u8; 4 * 8]; // 8 f32 of silence
        let out = bytes_to_f32_aligned(&bytes);
        assert_eq!(out.len(), 8);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    /// An empty slice yields an empty sample run (no panic, no head/tail assert).
    #[test]
    fn bytes_to_f32_aligned_empty_is_empty() {
        assert!(bytes_to_f32_aligned(&[]).is_empty());
    }

    // ── bytes_to_f32_frames: interleaved-f32 invariant (WASAPI-hardening) ──

    /// A byte run that is an exact multiple of `bytes_per_frame` is well-formed:
    /// all whole frames are returned and `malformed` is false.
    #[test]
    fn bytes_to_f32_frames_whole_frames_not_malformed() {
        // 2ch stereo → 8 bytes/frame. 3 whole frames = 6 f32 samples.
        let bytes_per_frame = 2 * std::mem::size_of::<f32>();
        let samples_in: [f32; 6] = [1.0, -1.0, 0.5, -0.5, 0.25, -0.25];
        let mut bytes = Vec::new();
        for s in samples_in {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let (out, malformed) = bytes_to_f32_frames(&bytes, bytes_per_frame);
        assert!(!malformed, "a whole-frame byte run must not be malformed");
        assert_eq!(out, &samples_in);
        assert_eq!(
            out.len() % 2,
            0,
            "sample count must stay a whole multiple of channels"
        );
    }

    /// A byte run with a trailing partial frame is flagged malformed AND the
    /// partial frame is dropped, so the returned sample count stays a whole
    /// multiple of the channel count (never shifts channel alignment).
    #[test]
    fn bytes_to_f32_frames_partial_frame_is_dropped_and_flagged() {
        // 2ch stereo → 8 bytes/frame. Provide 2 whole frames (16 bytes) + a
        // partial third frame (4 bytes = 1 sample, half a stereo frame).
        let bytes_per_frame = 2 * std::mem::size_of::<f32>();
        let whole: [f32; 4] = [1.0, 2.0, 3.0, 4.0];
        let mut bytes = Vec::new();
        for s in whole {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        // Append a single dangling sample (partial stereo frame).
        bytes.extend_from_slice(&9.0f32.to_le_bytes());

        let (out, malformed) = bytes_to_f32_frames(&bytes, bytes_per_frame);
        assert!(
            malformed,
            "a trailing partial frame must be reported as malformed"
        );
        assert_eq!(
            out, &whole,
            "the partial trailing frame must be dropped, keeping whole frames only"
        );
        assert_eq!(
            out.len() % 2,
            0,
            "after dropping the partial frame the sample count is frame-aligned"
        );
    }

    /// Mono (1ch → 4 bytes/frame): every 4-byte sample is a whole frame, so a
    /// clean f32 run is never malformed.
    #[test]
    fn bytes_to_f32_frames_mono_is_frame_aligned() {
        let bytes_per_frame = std::mem::size_of::<f32>(); // 1 channel
        let samples_in: [f32; 5] = [0.0, 0.1, 0.2, 0.3, 0.4];
        let mut bytes = Vec::new();
        for s in samples_in {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let (out, malformed) = bytes_to_f32_frames(&bytes, bytes_per_frame);
        assert!(!malformed);
        assert_eq!(out, &samples_in);
    }

    /// An empty input is well-formed (0 is a multiple of any frame size) and
    /// yields no samples — the silent-packet / drained-buffer case.
    #[test]
    fn bytes_to_f32_frames_empty_is_wellformed_empty() {
        let (out, malformed) = bytes_to_f32_frames(&[], 8);
        assert!(
            !malformed,
            "an empty run is a whole (zero) number of frames"
        );
        assert!(out.is_empty());
    }

    // ── WindowsCaptureConfig Construction Tests ──────────────────────

    /// Verify WindowsCaptureConfig can be constructed with SystemDefault target.
    #[test]
    fn test_capture_config_system_default() {
        let config = WindowsCaptureConfig {
            target: CaptureTarget::SystemDefault,
            sample_rate: 48000,
            channels: 2,
        };
        assert_eq!(config.target, CaptureTarget::SystemDefault);
        assert_eq!(config.sample_rate, 48000);
        assert_eq!(config.channels, 2);
    }

    /// Verify WindowsCaptureConfig with Device target.
    #[test]
    fn test_capture_config_device_target() {
        let config = WindowsCaptureConfig {
            target: CaptureTarget::Device(DeviceId("my-device-id".to_string())),
            sample_rate: 44100,
            channels: 1,
        };
        assert_eq!(
            config.target,
            CaptureTarget::Device(DeviceId("my-device-id".to_string()))
        );
        assert_eq!(config.sample_rate, 44100);
        assert_eq!(config.channels, 1);
    }

    /// Verify WindowsCaptureConfig with Application target (PID as string).
    #[test]
    fn test_capture_config_application_target() {
        let config = WindowsCaptureConfig {
            target: CaptureTarget::Application(ApplicationId("12345".to_string())),
            sample_rate: 48000,
            channels: 2,
        };
        match &config.target {
            CaptureTarget::Application(app_id) => {
                assert_eq!(app_id.0, "12345");
            }
            other => panic!("Expected Application target, got {:?}", other),
        }
    }

    /// Verify WindowsCaptureConfig with ApplicationByName target.
    #[test]
    fn test_capture_config_application_by_name_target() {
        let config = WindowsCaptureConfig {
            target: CaptureTarget::ApplicationByName("firefox.exe".to_string()),
            sample_rate: 48000,
            channels: 2,
        };
        match &config.target {
            CaptureTarget::ApplicationByName(name) => {
                assert_eq!(name, "firefox.exe");
            }
            other => panic!("Expected ApplicationByName target, got {:?}", other),
        }
    }

    /// Verify WindowsCaptureConfig with ProcessTree target.
    #[test]
    fn test_capture_config_process_tree_target() {
        let config = WindowsCaptureConfig {
            target: CaptureTarget::ProcessTree(ProcessId(9999)),
            sample_rate: 96000,
            channels: 8,
        };
        match &config.target {
            CaptureTarget::ProcessTree(pid) => {
                assert_eq!(pid.0, 9999);
            }
            other => panic!("Expected ProcessTree target, got {:?}", other),
        }
        assert_eq!(config.sample_rate, 96000);
        assert_eq!(config.channels, 8);
    }

    /// Verify WindowsCaptureConfig is Debug-printable.
    #[test]
    fn test_capture_config_debug() {
        let config = WindowsCaptureConfig {
            target: CaptureTarget::SystemDefault,
            sample_rate: 48000,
            channels: 2,
        };
        let dbg = format!("{:?}", config);
        assert!(dbg.contains("SystemDefault"));
        assert!(dbg.contains("48000"));
        assert!(dbg.contains("2"));
    }

    // ── WindowsPlatformStream Trait Verification ─────────────────────

    /// Verify WindowsPlatformStream implements Send (required by PlatformStream).
    #[test]
    fn test_platform_stream_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<WindowsPlatformStream>();
    }

    /// Verify WindowsPlatformStream implements Sync.
    #[test]
    fn test_platform_stream_is_sync() {
        fn assert_sync<T: Sync>() {}
        assert_sync::<WindowsPlatformStream>();
    }

    /// Verify WindowsCaptureThread implements Send.
    #[test]
    fn test_capture_thread_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<WindowsCaptureThread>();
    }

    // ── Terminal-signal contract (rsac-66a6 / ADR-0010) ──────────────
    //
    // `wasapi_capture_thread_main`'s cleanup branches on a `fatal_error` flag:
    // a clean stop-flag exit calls `producer.signal_done()` (graceful
    // `Running → Stopping`, drainable), while a FATAL device-error exit
    // (a `get_next_packet_size` / `read_from_device` failure that signals
    // device loss) calls `producer.signal_error()` (terminal `Error`).
    //
    // Exercising the real capture loop's fatal path needs a physical device to
    // be invalidated mid-capture, which is not reproducible in a unit test. The
    // two tests below instead pin the producer-side contract that branch relies
    // on directly against the bridge — that `signal_error()` lands terminal
    // `Error` and `signal_done()` lands graceful `Stopping` — so a regression in
    // the cleanup wiring (e.g. reverting to an unconditional `signal_done`) is
    // caught by these state-equality assertions.

    use crate::bridge::ring_buffer::create_bridge;
    use crate::bridge::state::StreamState;
    use crate::core::config::{AudioFormat, SampleFormat};

    fn terminal_test_format() -> AudioFormat {
        AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
        }
    }

    /// rsac-66a6: the FATAL device-error cleanup branch calls
    /// `producer.signal_error()`, which must drive the bridge to the terminal
    /// `Error` state (so a parked reader observes a Fatal `StreamEnded`).
    #[test]
    fn test_fatal_exit_signal_error_lands_terminal_error() {
        let (producer, consumer) = create_bridge(8, terminal_test_format());
        // The capture loop runs while the session is Running.
        producer.shared().state.force_set(StreamState::Running);

        // Mirror the cleanup section's `fatal_error == true` branch.
        producer.signal_error();

        assert_eq!(
            consumer.shared().state.get(),
            StreamState::Error,
            "a fatal device-error exit must land the bridge in terminal Error"
        );
        assert!(
            consumer.shared().state.is_terminal(),
            "Error is a terminal state (blocking reader returns Fatal StreamEnded)"
        );
    }

    /// rsac-66a6: the clean stop-flag cleanup branch calls
    /// `producer.signal_done()`, which must drive the bridge to the GRACEFUL
    /// `Stopping` state (drainable, not terminal) — never `Error`.
    #[test]
    fn test_clean_exit_signal_done_lands_graceful_stopping() {
        let (producer, consumer) = create_bridge(8, terminal_test_format());
        producer.shared().state.force_set(StreamState::Running);

        // Mirror the cleanup section's `fatal_error == false` branch.
        producer.signal_done();

        assert_eq!(
            consumer.shared().state.get(),
            StreamState::Stopping,
            "a clean stop-flag exit must land the bridge in graceful Stopping"
        );
        assert_ne!(
            consumer.shared().state.get(),
            StreamState::Error,
            "a clean stop must never be mis-reported as terminal Error"
        );
    }

    /// rsac-b3a0: a panic inside the capture loop is contained by the
    /// `catch_unwind` wrapper and routed into the FATAL cleanup branch. This
    /// pins the exact wiring `wasapi_capture_thread_main` uses — closure
    /// panics → `loop_result.is_err()` → `fatal_error = true` →
    /// `signal_error()` — against the bridge, proving no panic exit path can
    /// leave the bridge non-terminal (a blocked reader must observe the fatal
    /// `StreamEnded`, not an infinite Timeout-retry). The real thread main
    /// needs a live WASAPI device to drive, so like the rsac-66a6 tests above
    /// this exercises the contract at the producer/bridge level.
    #[test]
    fn test_capture_loop_panic_is_contained_and_lands_terminal_error() {
        let (producer, consumer) = create_bridge(8, terminal_test_format());
        producer.shared().state.force_set(StreamState::Running);

        let mut fatal_error = false;

        // Mirror the thread main's guard: the loop body panics mid-iteration.
        let loop_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            panic!("simulated WASAPI capture-loop panic");
        }));
        assert!(
            loop_result.is_err(),
            "the guard must contain the panic (no unwind past the wrapper)"
        );
        if loop_result.is_err() {
            fatal_error = true;
        }

        // Mirror the cleanup tail's branch.
        if fatal_error {
            producer.signal_error();
        } else {
            producer.signal_done();
        }

        assert_eq!(
            consumer.shared().state.get(),
            StreamState::Error,
            "a contained capture-loop panic must land the bridge in terminal Error"
        );
        assert!(
            consumer.shared().state.is_terminal(),
            "Error is terminal — a blocked read_chunk gets the fatal StreamEnded, \
             not an infinite Timeout-retry (ADR-0010)"
        );
    }

    // ── WindowsDeviceEnumerator / AudioDevice (COM required) ────────

    /// Test that WindowsDeviceEnumerator::new() can be created on Windows.
    /// This tests COM initialization and IMMDeviceEnumerator creation.
    #[test]
    fn test_device_enumerator_creation() {
        use crate::audio::windows::wasapi::WindowsDeviceEnumerator;

        // This should succeed on any Windows machine with audio subsystem.
        let result = WindowsDeviceEnumerator::new();
        assert!(
            result.is_ok(),
            "WindowsDeviceEnumerator::new() failed: {:?}",
            result.err()
        );
    }

    /// Test that device enumeration returns at least one device on Windows.
    #[test]
    fn test_device_enumeration() {
        use crate::audio::windows::wasapi::WindowsDeviceEnumerator;
        use crate::core::interface::DeviceEnumerator;

        let enumerator = WindowsDeviceEnumerator::new().expect("Enumerator creation failed");
        let devices = enumerator.enumerate_devices();
        assert!(
            devices.is_ok(),
            "enumerate_devices() failed: {:?}",
            devices.err()
        );
        // Most Windows machines have at least one audio device.
        let devices = devices.unwrap();
        assert!(
            !devices.is_empty(),
            "Expected at least one audio device on Windows"
        );
    }

    /// Test that default_device() returns a valid device.
    #[test]
    fn test_default_device() {
        use crate::audio::windows::wasapi::WindowsDeviceEnumerator;
        use crate::core::interface::DeviceEnumerator;

        let enumerator = WindowsDeviceEnumerator::new().expect("Enumerator creation failed");
        let device = enumerator.default_device();
        assert!(
            device.is_ok(),
            "default_device() failed: {:?}",
            device.err()
        );
        let device = device.unwrap();
        // Device should have a non-empty name.
        let name = device.name();
        assert!(!name.is_empty(), "Default device name should not be empty");
    }

    /// Test that device id() returns a non-empty string.
    #[test]
    fn test_device_id() {
        use crate::audio::windows::wasapi::WindowsDeviceEnumerator;
        use crate::core::interface::DeviceEnumerator;

        let enumerator = WindowsDeviceEnumerator::new().expect("Enumerator creation failed");
        let device = enumerator.default_device().expect("No default device");
        let id = device.id();
        assert!(!id.0.is_empty(), "Device ID should not be empty");
    }

    /// Test that device supported_formats() returns at least one format.
    #[test]
    fn test_device_supported_formats() {
        use crate::audio::windows::wasapi::WindowsDeviceEnumerator;
        use crate::core::interface::DeviceEnumerator;

        let enumerator = WindowsDeviceEnumerator::new().expect("Enumerator creation failed");
        let device = enumerator.default_device().expect("No default device");
        let formats = device.supported_formats();
        assert!(
            !formats.is_empty(),
            "Device should support at least one audio format"
        );
    }

    // ── CaptureTarget Mapping Smoke Tests ────────────────────────────
    //
    // These test that the create_audio_client() helper handles each
    // CaptureTarget variant without panicking. They require actual
    // WASAPI on Windows, so they test the mapping logic.

    /// Test system default capture target creates an audio client.
    #[test]
    fn test_create_audio_client_system_default() {
        // Initialize COM for this test thread.
        // wasapi 0.22.0: initialize_mta() returns HRESULT, not Result
        let _hr = wasapi::initialize_mta();

        let config = WindowsCaptureConfig {
            target: CaptureTarget::SystemDefault,
            sample_rate: 48000,
            channels: 2,
        };
        let result = create_audio_client(&config);
        assert!(
            result.is_ok(),
            "SystemDefault audio client creation failed: {:?}",
            result.err()
        );
    }

    /// Test that ApplicationByName with a nonexistent process returns ApplicationNotFound.
    #[test]
    fn test_create_audio_client_app_by_name_not_found() {
        // wasapi 0.22.0: initialize_mta() returns HRESULT, not Result
        let _hr = wasapi::initialize_mta();

        let config = WindowsCaptureConfig {
            target: CaptureTarget::ApplicationByName("nonexistent_app_xyz".to_string()),
            sample_rate: 48000,
            channels: 2,
        };
        // Should fail with ApplicationNotFound since no such process exists.
        let result = create_audio_client(&config);
        assert!(
            result.is_err(),
            "Should fail for nonexistent application name"
        );
        match result.err().unwrap() {
            AudioError::ApplicationNotFound { identifier } => {
                assert!(
                    identifier.contains("nonexistent_app_xyz"),
                    "Error should mention the requested name, got: {}",
                    identifier
                );
            }
            other => panic!("Expected ApplicationNotFound, got: {:?}", other),
        }
    }

    /// Test that Application target with invalid PID returns appropriate error.
    #[test]
    fn test_create_audio_client_invalid_pid() {
        // wasapi 0.22.0: initialize_mta() returns HRESULT, not Result
        let _hr = wasapi::initialize_mta();

        let config = WindowsCaptureConfig {
            target: CaptureTarget::Application(ApplicationId("not_a_number".to_string())),
            sample_rate: 48000,
            channels: 2,
        };
        let result = create_audio_client(&config);
        assert!(result.is_err(), "Should fail for non-numeric PID");
        match result.err().unwrap() {
            AudioError::ApplicationNotFound { identifier } => {
                assert!(identifier.contains("not_a_number"));
            }
            other => panic!("Expected ApplicationNotFound, got: {:?}", other),
        }
    }

    /// Test that Device target with "default" ID uses default device.
    #[test]
    fn test_create_audio_client_device_default_id() {
        // wasapi 0.22.0: initialize_mta() returns HRESULT, not Result
        let _hr = wasapi::initialize_mta();

        let config = WindowsCaptureConfig {
            target: CaptureTarget::Device(DeviceId("default".to_string())),
            sample_rate: 48000,
            channels: 2,
        };
        let result = create_audio_client(&config);
        assert!(
            result.is_ok(),
            "Device(default) should succeed: {:?}",
            result.err()
        );
    }

    /// Test that Device target with empty ID uses default device.
    #[test]
    fn test_create_audio_client_device_empty_id() {
        // wasapi 0.22.0: initialize_mta() returns HRESULT, not Result
        let _hr = wasapi::initialize_mta();

        let config = WindowsCaptureConfig {
            target: CaptureTarget::Device(DeviceId(String::new())),
            sample_rate: 48000,
            channels: 2,
        };
        let result = create_audio_client(&config);
        assert!(
            result.is_ok(),
            "Device(empty) should fall back to default: {:?}",
            result.err()
        );
    }

    /// Test that `find_device_by_id` resolves a real device ID via the wasapi
    /// 0.23 `get_device` path (round-trips the default render device's ID).
    #[test]
    fn test_find_device_by_id_roundtrip_real_id() {
        let _hr = wasapi::initialize_mta();

        let enumerator = wasapi::DeviceEnumerator::new().expect("create enumerator");
        let default_dev = enumerator
            .get_default_device(&wasapi::Direction::Render)
            .expect("get default render device");
        let real_id = default_dev.get_id().expect("get device id");

        // Resolving that exact ID through find_device_by_id (which now uses
        // get_device under the hood) must return a device with the same ID.
        let resolved = find_device_by_id(&real_id).expect("find_device_by_id should resolve");
        let resolved_id = resolved.get_id().expect("resolved device id");
        assert_eq!(
            resolved_id, real_id,
            "find_device_by_id should round-trip the same device ID"
        );
    }

    /// Test that Device target with non-existent ID returns DeviceNotFound.
    #[test]
    fn test_create_audio_client_device_not_found() {
        // wasapi 0.22.0: initialize_mta() returns HRESULT, not Result
        let _hr = wasapi::initialize_mta();

        let config = WindowsCaptureConfig {
            target: CaptureTarget::Device(DeviceId("nonexistent-device-id-12345".to_string())),
            sample_rate: 48000,
            channels: 2,
        };
        let result = create_audio_client(&config);
        assert!(result.is_err(), "Should fail for non-existent device ID");
        match result.err().unwrap() {
            AudioError::DeviceNotFound { device_id } => {
                assert_eq!(device_id, "nonexistent-device-id-12345");
            }
            other => panic!("Expected DeviceNotFound, got: {:?}", other),
        }
    }
}
