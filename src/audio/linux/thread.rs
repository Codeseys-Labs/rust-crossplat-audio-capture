//! PipeWire dedicated thread infrastructure.
//!
//! This module provides the thread + channel infrastructure for running PipeWire
//! objects (`Rc`/`!Send`) on a dedicated thread, communicating with the caller
//! via `std::sync::mpsc` channels.
//!
//! # Architecture
//!
//! ```text
//! User Thread                          PipeWire Thread (dedicated)
//! ────────────                         ──────────────────────────
//! AudioCapture / CapturingStream       MainLoop, Context, Core, Registry
//! BridgeConsumer                       Stream, StreamListener
//! command_tx ─────mpsc::channel────►  command_rx
//!                                      BridgeProducer (writes to ring buffer)
//! ◄──────mpsc::Sender──────────────   response_tx
//! ```
//!
//! All PipeWire `Rc`-based objects live exclusively on the dedicated thread.
//! The `PipeWireThread` handle is `Send + Sync` and safe to use from any thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::bridge::ring_buffer::BridgeProducer;
use crate::bridge::stream::PlatformStream;
use crate::core::buffer::AudioBuffer;
use crate::core::config::CaptureTarget;
use crate::core::error::{AudioError, AudioResult};

// ── CaptureConfig ────────────────────────────────────────────────────────

/// Resolved capture parameters passed to the PipeWire thread.
///
/// This is a subset of [`AudioCaptureConfig`](crate::core::config::AudioCaptureConfig)
/// containing only the fields needed by the PipeWire backend to create a stream.
#[derive(Debug)]
pub(crate) struct CaptureConfig {
    /// What to capture (system default, specific app, process tree, etc.).
    pub target: CaptureTarget,
    /// Desired sample rate in Hz (e.g., 48000).
    pub sample_rate: u32,
    /// Desired number of audio channels (e.g., 2 for stereo).
    pub channels: u16,
}

// ── PipeWireCommand ──────────────────────────────────────────────────────

/// Commands sent from the caller thread to the dedicated PipeWire thread.
///
/// Each command that expects a response includes a `response_tx` oneshot sender
/// so the PipeWire thread can reply with the result.
pub(crate) enum PipeWireCommand {
    /// Begin capturing audio with the given configuration.
    ///
    /// The [`BridgeProducer`] is moved to the PipeWire thread — it is `Send`
    /// and will be used by the PipeWire `process` callback to push audio data
    /// into the ring buffer.
    StartCapture {
        config: CaptureConfig,
        producer: BridgeProducer,
        response_tx: std_mpsc::Sender<AudioResult<()>>,
    },

    /// Stop the current capture session and clean up PipeWire stream objects.
    StopCapture {
        response_tx: std_mpsc::Sender<AudioResult<()>>,
    },

    /// Shut down the PipeWire thread entirely. No response needed — the thread exits.
    Shutdown,
}

// ── CaptureStreamData ────────────────────────────────────────────────────

/// User data stored inside the PipeWire stream listener.
///
/// Passed to `Stream::add_local_listener_with_user_data()` and accessible
/// from the `param_changed` and `process` callbacks as `&mut CaptureStreamData`.
///
/// # Real-time safety
///
/// The `producer` field uses `rtrb` lock-free push — safe for the PipeWire
/// process callback thread. The `Vec<f32>` allocation in the process callback
/// is acceptable for the initial implementation but should be optimized with
/// a pre-allocated scratch buffer in future iterations.
struct CaptureStreamData {
    /// Negotiated audio format — updated by the `param_changed` callback
    /// when PipeWire negotiates the actual stream format.
    format: libspa::param::audio::AudioInfoRaw,
    /// Ring buffer producer — pushes `AudioBuffer`s to the consumer thread.
    producer: BridgeProducer,
    /// Number of audio channels (updated from negotiated format, falls back to requested).
    channels: u16,
    /// Sample rate in Hz (updated from negotiated format, falls back to requested).
    sample_rate: u32,
}

// ── PipeWireThread ───────────────────────────────────────────────────────

/// Handle to the dedicated PipeWire thread.
///
/// All PipeWire `Rc`-based objects (MainLoop, Context, Core, Registry, Stream)
/// live on the spawned thread. The caller communicates via [`PipeWireCommand`]s
/// sent through the command channel, and receives responses via per-command
/// response senders.
///
/// # Lifecycle
///
/// 1. [`PipeWireThread::spawn()`] creates the thread and waits for PipeWire init.
/// 2. [`start_capture()`](PipeWireThread::start_capture) / [`stop_capture()`](PipeWireThread::stop_capture)
///    send commands and block for the response.
/// 3. On [`Drop`], a `Shutdown` command is sent and the thread is joined.
pub(crate) struct PipeWireThread {
    /// Channel to send commands to the PipeWire thread.
    command_tx: std_mpsc::Sender<PipeWireCommand>,
    /// Join handle for the dedicated thread (taken on drop).
    thread_handle: Option<std::thread::JoinHandle<()>>,
    /// Shared flag: `true` while the PipeWire thread's event loop is running.
    is_running: Arc<AtomicBool>,
}

impl PipeWireThread {
    /// Spawn the dedicated PipeWire thread.
    ///
    /// This creates a new OS thread named `"rsac-pipewire"` that:
    /// 1. Initializes PipeWire (`pipewire::init()`)
    /// 2. Creates `MainLoop`, `Context`, `Core`, and `Registry`
    /// 3. Enters the event loop, pumping PipeWire events and processing commands
    ///
    /// The call blocks until PipeWire initialization completes on the new thread.
    /// Returns an error if any PipeWire initialization step fails.
    ///
    /// # Errors
    ///
    /// - [`AudioError::BackendInitializationFailed`] if the thread cannot be spawned
    ///   or if PipeWire initialization fails (MainLoop, Context, Core, or Registry).
    pub fn spawn() -> AudioResult<Self> {
        let (command_tx, command_rx) = std_mpsc::channel();
        let (init_tx, init_rx) = std_mpsc::channel();
        let is_running = Arc::new(AtomicBool::new(true));
        let is_running_thread = Arc::clone(&is_running);

        let thread_handle = std::thread::Builder::new()
            .name("rsac-pipewire".to_string())
            .spawn(move || {
                pw_thread_main(command_rx, init_tx, is_running_thread);
            })
            .map_err(|e| AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: format!("Failed to spawn PipeWire thread: {}", e),
            })?;

        // Block until the PipeWire thread reports init success or failure.
        let init_result = init_rx
            .recv()
            .map_err(|_| AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: "PipeWire thread exited before reporting init status".to_string(),
            })?;

        // If PipeWire init failed, the thread has already exited. Propagate the error.
        init_result?;

        Ok(PipeWireThread {
            command_tx,
            thread_handle: Some(thread_handle),
            is_running,
        })
    }

    /// Send a `StartCapture` command to the PipeWire thread and wait for the response.
    ///
    /// The `BridgeProducer` is moved to the PipeWire thread where it will be used
    /// by the PipeWire `process` callback to push captured audio into the ring buffer.
    ///
    /// This creates a PipeWire stream, registers listener callbacks (param_changed
    /// for format negotiation, process for audio data), and connects the stream.
    ///
    /// # Errors
    ///
    /// - [`AudioError::BackendError`] if the PipeWire thread is not running or
    ///   does not respond, or if stream creation/connection fails.
    pub fn start_capture(
        &self,
        config: CaptureConfig,
        producer: BridgeProducer,
    ) -> AudioResult<()> {
        let (response_tx, response_rx) = std_mpsc::channel();

        self.command_tx
            .send(PipeWireCommand::StartCapture {
                config,
                producer,
                response_tx,
            })
            .map_err(|_| AudioError::BackendError {
                backend: "PipeWire".to_string(),
                operation: "start_capture".to_string(),
                message: "PipeWire thread is not running (command channel closed)".to_string(),
                context: None,
            })?;

        response_rx.recv().map_err(|_| AudioError::BackendError {
            backend: "PipeWire".to_string(),
            operation: "start_capture".to_string(),
            message: "PipeWire thread did not respond to StartCapture".to_string(),
            context: None,
        })?
    }

    /// Send a `StopCapture` command to the PipeWire thread and wait for the response.
    ///
    /// Tells the PipeWire thread to tear down the current capture stream and
    /// release the `BridgeProducer`.
    ///
    /// # Errors
    ///
    /// - [`AudioError::BackendError`] if the PipeWire thread is not running or
    ///   does not respond.
    pub fn stop_capture(&self) -> AudioResult<()> {
        let (response_tx, response_rx) = std_mpsc::channel();

        self.command_tx
            .send(PipeWireCommand::StopCapture { response_tx })
            .map_err(|_| AudioError::BackendError {
                backend: "PipeWire".to_string(),
                operation: "stop_capture".to_string(),
                message: "PipeWire thread is not running (command channel closed)".to_string(),
                context: None,
            })?;

        response_rx.recv().map_err(|_| AudioError::BackendError {
            backend: "PipeWire".to_string(),
            operation: "stop_capture".to_string(),
            message: "PipeWire thread did not respond to StopCapture".to_string(),
            context: None,
        })?
    }

    /// Returns `true` if the PipeWire thread is still alive.
    ///
    /// This checks the shared atomic flag, which is set to `false` when the
    /// thread's event loop exits (either due to `Shutdown` or an error).
    pub fn is_alive(&self) -> bool {
        self.is_running.load(Ordering::SeqCst)
    }
}

impl Drop for PipeWireThread {
    fn drop(&mut self) {
        // Send Shutdown command — ignore errors (thread may already be dead).
        let _ = self.command_tx.send(PipeWireCommand::Shutdown);

        // Join the thread to ensure clean shutdown.
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

// ── LinuxPlatformStream ──────────────────────────────────────────────────

/// Platform-specific stream handle for Linux (PipeWire backend).
///
/// Wraps a shared [`PipeWireThread`] handle and implements [`PlatformStream`]
/// so it can be used with [`BridgeStream`](crate::bridge::stream::BridgeStream).
///
/// # Thread Safety
///
/// `LinuxPlatformStream` is `Send` (required by `PlatformStream`). The inner
/// `Arc<Mutex<PipeWireThread>>` provides shared ownership and interior mutability.
pub(crate) struct LinuxPlatformStream {
    pw_thread: Arc<Mutex<PipeWireThread>>,
}

impl LinuxPlatformStream {
    /// Create a new `LinuxPlatformStream` wrapping the given PipeWire thread.
    pub fn new(pw_thread: Arc<Mutex<PipeWireThread>>) -> Self {
        Self { pw_thread }
    }
}

impl PlatformStream for LinuxPlatformStream {
    fn stop_capture(&self) -> AudioResult<()> {
        self.pw_thread
            .lock()
            .map_err(|_| AudioError::InternalError {
                message: "PipeWire thread mutex poisoned".to_string(),
                source: None,
            })?
            .stop_capture()
    }

    fn is_active(&self) -> bool {
        self.pw_thread.lock().map(|t| t.is_alive()).unwrap_or(false)
    }
}

// ── PipeWire Thread Main Function ────────────────────────────────────────

/// The main function for the dedicated PipeWire thread.
///
/// This runs on the spawned thread and owns all PipeWire `Rc` objects.
/// It communicates with the caller thread via the command channel and
/// reports initialization status via `init_tx`.
///
/// # Event Loop
///
/// The loop alternates between:
/// 1. Pumping PipeWire events via `main_loop.loop_().iterate(50ms)` — this is
///    where PipeWire callbacks (including the `process` callback) fire.
/// 2. Checking for incoming commands via `command_rx.try_recv()`.
///
/// The loop exits on `Shutdown` command or if the command channel disconnects.
///
/// # Audio Data Flow
///
/// When a `StartCapture` command is received, the thread:
/// 1. Creates a PipeWire `Stream` with properties matching the [`CaptureTarget`]
/// 2. Registers a **process callback** that converts raw PipeWire audio data
///    (F32LE bytes) into [`AudioBuffer`]s and pushes them to the [`BridgeProducer`]
/// 3. Registers a **param_changed callback** for format negotiation
/// 4. Connects the stream with `AUTOCONNECT | MAP_BUFFERS` flags
///
/// The `BridgeProducer::push_or_drop()` call in the process callback is lock-free
/// and non-blocking, making it safe for the real-time PipeWire callback context.
fn pw_thread_main(
    command_rx: std_mpsc::Receiver<PipeWireCommand>,
    init_tx: std_mpsc::Sender<AudioResult<()>>,
    is_running: Arc<AtomicBool>,
) {
    use pipewire::context::ContextBox;
    use pipewire::main_loop::MainLoopBox;
    use pipewire::properties::properties;
    use pipewire::stream::{StreamBox, StreamFlags, StreamListener};

    use libspa::param::audio::{AudioFormat as SpaAudioFormat, AudioInfoRaw};
    use libspa::param::format::{MediaSubtype, MediaType};
    use libspa::param::{format_utils, ParamType};
    use libspa::pod::{Object, Pod};

    // Step 1: Initialize PipeWire library.
    pipewire::init();

    // Step 2: Create the MainLoop (non-threaded — we drive it manually via iterate()).
    let main_loop = match MainLoopBox::new(None) {
        Ok(ml) => ml,
        Err(e) => {
            let _ = init_tx.send(Err(AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: format!("Failed to create MainLoop: {}", e),
            }));
            is_running.store(false, Ordering::SeqCst);
            return;
        }
    };

    // Step 3: Create Context and connect to the PipeWire daemon.
    let context = match ContextBox::new(&main_loop.loop_(), None) {
        Ok(ctx) => ctx,
        Err(e) => {
            let _ = init_tx.send(Err(AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: format!("Failed to create PipeWire Context: {}", e),
            }));
            is_running.store(false, Ordering::SeqCst);
            return;
        }
    };

    let core = match context.connect(None) {
        Ok(c) => c,
        Err(e) => {
            let _ = init_tx.send(Err(AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: format!("Failed to connect to PipeWire daemon: {}", e),
            }));
            is_running.store(false, Ordering::SeqCst);
            return;
        }
    };

    let _registry = match core.get_registry() {
        Ok(r) => r,
        Err(e) => {
            let _ = init_tx.send(Err(AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: format!("Failed to get PipeWire registry: {}", e),
            }));
            is_running.store(false, Ordering::SeqCst);
            return;
        }
    };

    // Signal successful initialization back to the caller.
    if init_tx.send(Ok(())).is_err() {
        // Caller dropped the receiver — no point continuing.
        is_running.store(false, Ordering::SeqCst);
        return;
    }

    // ── Step 4: Enter the event loop ─────────────────────────────────

    // Thread-local state for the current capture session.
    // The stream must outlive its listener (the listener registers C callbacks
    // against the stream's raw pointer). We enforce this by dropping the
    // listener before the stream in all cleanup paths.
    let mut capture_stream: Option<StreamBox> = None;
    let mut capture_listener: Option<StreamListener<CaptureStreamData>> = None;

    loop {
        // Pump PipeWire events. The `process` callback fires during iterate()
        // on this same thread, pushing audio data via BridgeProducer::push_or_drop().
        let _ = main_loop.loop_().iterate(Duration::from_millis(50));

        // Check for incoming commands (non-blocking).
        match command_rx.try_recv() {
            Ok(PipeWireCommand::StartCapture {
                config,
                producer,
                response_tx,
            }) => {
                log::debug!(
                    "PipeWire thread: StartCapture received (target={:?}, {}Hz, {}ch)",
                    config.target,
                    config.sample_rate,
                    config.channels
                );

                // Clean up any existing capture session first.
                if capture_listener.is_some() || capture_stream.is_some() {
                    log::debug!("PipeWire thread: cleaning up previous capture session");
                    capture_listener = None;
                    capture_stream = None;
                }

                // ── Build PipeWire stream properties based on CaptureTarget ──

                let mut props = properties! {
                    *pipewire::keys::NODE_NAME => "rsac-capture",
                    *pipewire::keys::STREAM_CAPTURE_SINK => "true",
                    *pipewire::keys::STREAM_MONITOR => "true",
                };

                match &config.target {
                    CaptureTarget::SystemDefault => {
                        // No TARGET_OBJECT — captures from the default output
                        // sink monitor. STREAM_CAPTURE_SINK + STREAM_MONITOR
                        // handle the routing.
                        log::debug!("PipeWire: SystemDefault — no TARGET_OBJECT");
                    }
                    CaptureTarget::Device(device_id) => {
                        // TARGET_OBJECT = the device's PipeWire node ID or serial.
                        props.insert(*pipewire::keys::TARGET_OBJECT, device_id.0.as_str());
                        log::debug!("PipeWire: Device target — TARGET_OBJECT={}", device_id.0);
                    }
                    CaptureTarget::Application(app_id) => {
                        // TARGET_OBJECT = the application's PipeWire node ID.
                        // The ApplicationId string should contain the PW node
                        // ID or serial assigned by the caller.
                        props.insert(*pipewire::keys::TARGET_OBJECT, app_id.0.as_str());
                        log::debug!("PipeWire: Application target — TARGET_OBJECT={}", app_id.0);
                    }
                    CaptureTarget::ApplicationByName(name) => {
                        // Best-effort: use the name as TARGET_OBJECT.
                        // Full implementation would enumerate PW nodes and
                        // resolve the name to a node ID first.
                        props.insert(*pipewire::keys::TARGET_OBJECT, name.as_str());
                        log::debug!(
                            "PipeWire: ApplicationByName target — TARGET_OBJECT={}",
                            name
                        );
                    }
                    CaptureTarget::ProcessTree(pid) => {
                        // Treat as single-process capture for now.
                        // Full tree capture is a future enhancement.
                        let pid_str = pid.0.to_string();
                        props.insert(*pipewire::keys::TARGET_OBJECT, pid_str.as_str());
                        log::debug!(
                            "PipeWire: ProcessTree target (single-process) — TARGET_OBJECT={}",
                            pid.0
                        );
                    }
                }

                // ── Create the PipeWire stream ──

                let stream = match StreamBox::new(&core, "rsac-capture", props) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = response_tx.send(Err(AudioError::BackendError {
                            backend: "PipeWire".to_string(),
                            operation: "create_stream".to_string(),
                            message: format!("Failed to create PipeWire stream: {}", e),
                            context: None,
                        }));
                        continue;
                    }
                };

                // ── Build user data for stream callbacks ──

                let user_data = CaptureStreamData {
                    format: AudioInfoRaw::new(),
                    producer,
                    channels: config.channels,
                    sample_rate: config.sample_rate,
                };

                // ── Register stream listener with callbacks ──

                let listener = match stream
                    .add_local_listener_with_user_data(user_data)
                    .param_changed(|_stream, user_data, id, param| {
                        // Format negotiation callback.
                        // PipeWire calls this when the actual stream format is
                        // negotiated (may differ from what we requested).

                        let Some(param) = param else {
                            // NULL param means format cleared.
                            return;
                        };

                        if id != ParamType::Format.as_raw() {
                            // Not a format parameter — ignore.
                            return;
                        }

                        let (media_type, media_subtype) = match format_utils::parse_format(param) {
                            Ok(v) => v,
                            Err(_) => return,
                        };

                        // Only accept raw audio.
                        if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
                            return;
                        }

                        // Parse the negotiated format into our AudioInfoRaw.
                        let _ = user_data.format.parse(param);

                        // Update channels/sample_rate from the negotiated format
                        // so the process callback creates AudioBuffers with the
                        // correct metadata.
                        let negotiated_channels = user_data.format.channels();
                        let negotiated_rate = user_data.format.rate();
                        if negotiated_channels > 0 {
                            user_data.channels = negotiated_channels as u16;
                        }
                        if negotiated_rate > 0 {
                            user_data.sample_rate = negotiated_rate;
                        }

                        log::debug!(
                            "PipeWire format negotiated: {:?}, {}ch @ {}Hz",
                            user_data.format.format(),
                            negotiated_channels,
                            negotiated_rate
                        );
                    })
                    .process(|stream, user_data| {
                        // Audio data callback — runs in the PipeWire real-time
                        // context during main_loop.iterate().
                        //
                        // REAL-TIME SAFETY:
                        // - BridgeProducer::push_or_drop() is lock-free (rtrb)
                        // - Vec allocation is acceptable for initial impl
                        //   (optimize with scratch buffer later)
                        // - No locks, no blocking, no I/O

                        let Some(mut buffer) = stream.dequeue_buffer() else {
                            return;
                        };

                        let datas = buffer.datas_mut();
                        if datas.is_empty() {
                            return;
                        }

                        let data = &mut datas[0];
                        let chunk_size = data.chunk().size() as usize;
                        let n_samples = chunk_size / std::mem::size_of::<f32>();

                        if n_samples == 0 {
                            return;
                        }

                        if let Some(raw_bytes) = data.data() {
                            // Convert raw F32LE bytes to f32 samples.
                            let mut samples = Vec::with_capacity(n_samples);
                            for i in 0..n_samples {
                                let offset = i * 4;
                                if offset + 4 <= raw_bytes.len() {
                                    let sample = f32::from_le_bytes([
                                        raw_bytes[offset],
                                        raw_bytes[offset + 1],
                                        raw_bytes[offset + 2],
                                        raw_bytes[offset + 3],
                                    ]);
                                    samples.push(sample);
                                }
                            }

                            if !samples.is_empty() {
                                let audio_buffer = AudioBuffer::new(
                                    samples,
                                    user_data.channels,
                                    user_data.sample_rate,
                                );
                                // Push to ring buffer. If full, the buffer is
                                // silently dropped (back-pressure).
                                user_data.producer.push_or_drop(audio_buffer);
                            }
                        }

                        // The PipeWire buffer is automatically re-queued when
                        // `buffer` goes out of scope (RAII).
                    })
                    .register()
                {
                    Ok(l) => l,
                    Err(e) => {
                        let _ = response_tx.send(Err(AudioError::BackendError {
                            backend: "PipeWire".to_string(),
                            operation: "register_listener".to_string(),
                            message: format!("Failed to register PipeWire stream listener: {}", e),
                            context: None,
                        }));
                        continue;
                    }
                };

                // ── Build format Pod for stream.connect() ──

                let mut audio_info = AudioInfoRaw::new();
                audio_info.set_format(SpaAudioFormat::F32LE);
                audio_info.set_rate(config.sample_rate);
                audio_info.set_channels(config.channels as u32);

                let pod_object = Object {
                    type_: pipewire::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
                    id: ParamType::EnumFormat.as_raw(),
                    properties: audio_info.into(),
                };

                let values: Vec<u8> = match pipewire::spa::pod::serialize::PodSerializer::serialize(
                    std::io::Cursor::new(Vec::new()),
                    &pipewire::spa::pod::Value::Object(pod_object),
                ) {
                    Ok(result) => result.0.into_inner(),
                    Err(e) => {
                        let _ = response_tx.send(Err(AudioError::BackendError {
                            backend: "PipeWire".to_string(),
                            operation: "format_pod".to_string(),
                            message: format!("Failed to serialize format Pod: {:?}", e),
                            context: None,
                        }));
                        continue;
                    }
                };

                let Some(pod) = Pod::from_bytes(&values) else {
                    let _ = response_tx.send(Err(AudioError::BackendError {
                        backend: "PipeWire".to_string(),
                        operation: "format_pod".to_string(),
                        message: "Failed to create Pod from serialized bytes".to_string(),
                        context: None,
                    }));
                    continue;
                };
                let mut params = [pod];

                // ── Connect the stream ──

                if let Err(e) = stream.connect(
                    libspa::utils::Direction::Input,
                    None,
                    StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
                    &mut params,
                ) {
                    let _ = response_tx.send(Err(AudioError::BackendError {
                        backend: "PipeWire".to_string(),
                        operation: "stream_connect".to_string(),
                        message: format!("Failed to connect PipeWire stream: {}", e),
                        context: None,
                    }));
                    continue;
                }

                log::debug!(
                    "PipeWire thread: stream created and connected (state={:?})",
                    stream.state()
                );

                // Store the stream and listener — they must stay alive for
                // callbacks to fire. Listener is dropped before stream in all
                // cleanup paths.
                capture_stream = Some(stream);
                capture_listener = Some(listener);

                let _ = response_tx.send(Ok(()));
            }

            Ok(PipeWireCommand::StopCapture { response_tx }) => {
                log::debug!("PipeWire thread: StopCapture received");

                // Drop listener first (unregisters callbacks from the C stream),
                // then drop the stream (destroys the C stream object).
                capture_listener = None;
                capture_stream = None;

                let _ = response_tx.send(Ok(()));
            }

            Ok(PipeWireCommand::Shutdown) => {
                log::debug!("PipeWire thread: Shutdown received, exiting event loop");
                // Clean up any active capture before exiting.
                capture_listener = None;
                capture_stream = None;
                break;
            }

            Err(std_mpsc::TryRecvError::Empty) => {
                // No commands waiting — continue pumping PipeWire events.
            }

            Err(std_mpsc::TryRecvError::Disconnected) => {
                // Command channel closed — caller is gone, exit gracefully.
                log::debug!("PipeWire thread: command channel disconnected, exiting");
                capture_listener = None;
                capture_stream = None;
                break;
            }
        }
    }

    // Cleanup: PipeWire objects are dropped via RAII when this function returns.
    // The MainLoop, Context, Core, and Registry are all dropped here.
    is_running.store(false, Ordering::SeqCst);
    log::debug!("PipeWire thread: exited cleanly");
}

// ── Compile-time assertions ──────────────────────────────────────────────

/// Assert that `LinuxPlatformStream` is `Send` (required by `PlatformStream`).
fn _assert_linux_platform_stream_send() {
    fn _assert<T: Send>() {}
    _assert::<LinuxPlatformStream>();
}

/// Assert that `PipeWireThread` is `Send`.
fn _assert_pipewire_thread_send() {
    fn _assert<T: Send>() {}
    _assert::<PipeWireThread>();
}
