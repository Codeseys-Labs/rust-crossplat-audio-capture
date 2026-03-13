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
use crate::core::buffer::AudioBuffer;
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
/// 2. We read packets from the capture client
/// 3. Convert raw bytes to `f32` samples
/// 4. Create [`AudioBuffer`] and push via [`BridgeProducer::push_or_drop()`]
///
/// The `push_or_drop()` call is lock-free and non-blocking, making it safe
/// for the capture loop context.
///
/// # Cleanup
///
/// On exit (stop signal or error), the function:
/// - Sets `is_active` to `false`
/// - Calls `producer.signal_done()` to notify the consumer
/// - WASAPI/COM objects are dropped via RAII
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
        autoconvert: true,
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

    loop {
        // Check stop flag before waiting.
        if stop_flag.load(Ordering::SeqCst) {
            log::debug!("WASAPI thread: stop flag set, exiting capture loop");
            break;
        }

        // Wait for audio data event with a short timeout so we can
        // check the stop flag periodically.
        if h_event.wait_for_event(100).is_err() {
            // Timeout or error — check stop flag and continue.
            if stop_flag.load(Ordering::SeqCst) {
                log::debug!("WASAPI thread: stop flag set during wait, exiting");
                break;
            }
            continue;
        }

        // Read all available packets from the capture client.
        // The wasapi-rs crate provides read_from_device_to_deque which
        // reads raw bytes into a VecDeque.
        let mut sample_queue = std::collections::VecDeque::new();

        match capture_client.read_from_device_to_deque(&mut sample_queue) {
            Ok(_) => {}
            Err(e) => {
                log::warn!("WASAPI thread: read_from_device_to_deque failed: {}", e);
                // Check if we should stop or continue on transient errors.
                if stop_flag.load(Ordering::SeqCst) {
                    break;
                }
                continue;
            }
        }

        // Convert the raw byte data to f32 samples.
        // Data from WASAPI with our requested format is IEEE float 32-bit LE.
        if sample_queue.is_empty() {
            continue;
        }

        // Make the VecDeque contiguous for efficient byte processing.
        let raw_bytes: Vec<u8> = sample_queue.into_iter().collect();

        // Convert bytes to f32 samples (4 bytes per sample, little-endian).
        let n_samples = raw_bytes.len() / 4;
        if n_samples == 0 {
            continue;
        }

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
            let audio_buffer = AudioBuffer::new(samples, channels, sample_rate);
            // Push to ring buffer. If full, the buffer is silently dropped
            // (back-pressure — real-time safety).
            producer.push_or_drop(audio_buffer);
        }

        // Check stop flag after processing.
        if stop_flag.load(Ordering::SeqCst) {
            log::debug!("WASAPI thread: stop flag set after read, exiting");
            break;
        }
    }

    // ── Cleanup ──────────────────────────────────────────────────────
    //
    // Stop the WASAPI stream. In wasapi 0.22.0, initialize_mta() does not
    // return a guard, so we must call deinitialize() explicitly.

    let _ = audio_client.stop_stream();
    is_active.store(false, Ordering::SeqCst);
    producer.signal_done();
    wasapi::deinitialize();
    log::debug!("WASAPI thread: exited cleanly");
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
/// | `Application(app_id)`  | Parse PID from app_id → process loopback              |
/// | `ApplicationByName(_)` | Resolve name → PID via sysinfo → process loopback     |
/// | `ProcessTree(pid)`     | Process loopback with `include_tree = true`           |
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

            wasapi::AudioClient::new_application_loopback_client(pid, false).map_err(|e| {
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

            wasapi::AudioClient::new_application_loopback_client(pid, false).map_err(|e| {
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
/// Enumerates all audio devices and returns the one matching the given ID.
/// Falls back to the default render device if the ID is empty or "default".
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

    // Enumerate all render devices and find one matching the ID.
    let collection = enumerator
        .get_device_collection(&wasapi::Direction::Render)
        .map_err(|e| AudioError::DeviceEnumerationError {
            reason: format!("Failed to enumerate render devices: {}", e),
            context: None,
        })?;

    let device_count =
        collection
            .get_nbr_devices()
            .map_err(|e| AudioError::DeviceEnumerationError {
                reason: format!("Failed to get device count: {}", e),
                context: None,
            })?;

    for i in 0..device_count {
        if let Ok(device) = collection.get_device_at_index(i) {
            if let Ok(id) = device.get_id() {
                if id == device_id {
                    return Ok(device);
                }
            }
        }
    }

    Err(AudioError::DeviceNotFound {
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

    // Iterate all processes and perform a case-insensitive name match.
    for (pid, process) in system.processes() {
        let proc_name = process.name().to_string_lossy();
        if proc_name.to_lowercase() == name_lower {
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
            "No running process found matching name '{}' (case-insensitive)",
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

// ── Tests ────────────────────────────────────────────────────────────────
//
// These tests are automatically Windows-only because this file has
// `#![cfg(target_os = "windows")]` at the top. They are double-gated:
// the file-level cfg ensures they never compile on Linux/macOS.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{ApplicationId, CaptureTarget, DeviceId, ProcessId};

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
        use crate::core::interface::{AudioDevice, DeviceEnumerator};

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
        use crate::core::interface::{AudioDevice, DeviceEnumerator};

        let enumerator = WindowsDeviceEnumerator::new().expect("Enumerator creation failed");
        let device = enumerator.default_device().expect("No default device");
        let id = device.id();
        assert!(!id.0.is_empty(), "Device ID should not be empty");
    }

    /// Test that device supported_formats() returns at least one format.
    #[test]
    fn test_device_supported_formats() {
        use crate::audio::windows::wasapi::WindowsDeviceEnumerator;
        use crate::core::interface::{AudioDevice, DeviceEnumerator};

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
