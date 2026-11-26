//! Windows-specific audio capture backend using WASAPI.
//! Testing Windows compilation in GitHub Actions.
#![cfg(target_os = "windows")]

use crate::core::config::{AudioFormat, StreamConfig};
use crate::core::error::{AudioError, BackendContext, Result as AudioResult};
use crate::core::interface::{
    AudioDevice, CapturingStream, DeviceEnumerator, DeviceEvent, DeviceEventHandler, DeviceKind,
    DeviceWatcher,
};

use std::ffi::OsStr;
use std::marker::PhantomData;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::Duration;

use sysinfo::{ProcessesToUpdate, System};

// --- New BridgeStream Architecture Imports ---
use super::thread::{WindowsCaptureConfig, WindowsCaptureThread, WindowsPlatformStream};
use crate::bridge::{calculate_capacity, create_bridge, BridgeStream, StreamState};
use crate::core::config::DeviceId;

// --- Application-Specific Capture (Process Loopback) ---

use windows::{core::*, Win32::Foundation::*, Win32::Media::Audio::*, Win32::System::Com::*};

// Specific imports not covered by the glob imports above
use windows::Win32::Devices::Properties::DEVPKEY_Device_FriendlyName as PKEY_Device_FriendlyName;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

// Constants
const E_NOTFOUND: windows::core::HRESULT = windows::core::HRESULT(-2147024894i32); // 0x80070002
const VT_LPWSTR: u16 = 31;

/// Build a [`BackendContext`] carrying the WASAPI `HRESULT` as the structured
/// `os_error_code` (M6).
///
/// The `HRESULT.0` is an `i32`; we widen to `i64` for `os_error_code`. The raw
/// (signed) value is preserved so the high error bit is not lost — callers
/// reading the code back can reinterpret as `u32` / `0x{:08X}` if they want the
/// conventional hex `HRESULT` rendering. The human-readable message from the
/// `windows` crate is attached when available.
fn wasapi_backend_context(hr: windows::core::HRESULT) -> BackendContext {
    BackendContext {
        backend_name: "WASAPI".to_string(),
        os_error_code: Some(hr.0 as i64),
        // `HRESULT::message()` already returns an owned `String`.
        os_error_message: Some(hr.message()),
    }
}

/// Build a [`BackendContext`] from a [`windows::core::Error`] (M6).
///
/// COM wrapper calls (e.g. `IMMDevice::GetId`) return `windows::core::Result`,
/// whose error carries an `HRESULT` via [`windows::core::Error::code`]. This
/// extracts that code into the structured `os_error_code` alongside the error's
/// message, so failures from the `windows` crate's typed wrappers get the same
/// structured context as the raw-`HRESULT` sites.
fn wasapi_backend_context_err(err: &windows::core::Error) -> BackendContext {
    BackendContext {
        backend_name: "WASAPI".to_string(),
        os_error_code: Some(err.code().0 as i64),
        // `windows::core::Error::message()` already returns an owned `String`.
        os_error_message: Some(err.message()),
    }
}

/// Windows-specific application capture using wasapi-rs library
/// Based on wasapi-rs examples/record_application.rs for simplicity and reliability
///
/// # Legacy / RT-safety warning (audit L13)
///
/// This type predates the canonical capture pipeline and duplicates the WASAPI
/// loopback logic that now lives in
/// `WindowsCaptureThread` (`super::thread`; `pub(crate)`, so referenced by name
/// to keep the public-docs build clean). More
/// importantly, its [`start_capture`](Self::start_capture) /
/// [`start_capture_with_stop_flag`](Self::start_capture_with_stop_flag) methods
/// invoke a **user-supplied callback directly on the WASAPI capture thread**.
/// Any allocation, lock, or blocking call inside that callback runs on the
/// real-time audio thread and can cause glitches/xruns — exactly the footgun
/// the `BridgeProducer`/`BridgeStream` data plane exists to prevent.
///
/// Prefer the canonical path: build an `AudioCapture` targeting an application
/// (`CaptureTarget::Application` / `ProcessTree`) and read via the
/// [`CapturingStream`] API, which delivers buffers on a non-real-time consumer
/// thread. The static helpers ([`find_process_by_name`](Self::find_process_by_name),
/// [`list_audio_processes`](Self::list_audio_processes)) remain useful for
/// process discovery and are not deprecated.
pub struct WindowsApplicationCapture {
    process_id: u32,
    include_tree: bool,
    // Use wasapi-rs AudioClient for simpler implementation (declared BEFORE
    // `com_initializer` so Rust drops the COM client before decrementing MTA usage).
    audio_client: Option<wasapi::AudioClient>,
    // Process-wide MTA guard acquired on `initialize()` and released on Drop.
    #[allow(dead_code)]
    com_initializer: Option<ComInitializer>,
    // Shared flag to signal capture loop to stop
    should_stop: Arc<AtomicBool>,
}

impl WindowsApplicationCapture {
    /// Create a new application capture instance for the specified process
    ///
    /// # Arguments
    /// * `process_id` - PID of the target process
    /// * `include_tree` - Selects the WASAPI process-loopback mode. `true` uses
    ///   `PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE` (capture the target
    ///   PID **and** its descendants). `false` uses
    ///   `PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE`, which captures
    ///   everything **except** that tree — *not* "the target only, no children".
    ///   The OS mode is binary; there is no strict single-process variant. To
    ///   capture a single application, pass `true` (INCLUDE-tree of a leaf
    ///   process is exactly single-process capture). See
    ///   [ADR-0017](../../../docs/designs/0017-windows-application-capture-include-tree.md).
    ///
    /// # Example
    /// ```rust,no_run
    /// use rsac::audio::windows::WindowsApplicationCapture;
    ///
    /// let capture = WindowsApplicationCapture::new(1234, true);
    /// ```
    pub fn new(
        process_id: u32,
        include_tree: bool,
    ) -> std::result::Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            process_id,
            include_tree,
            audio_client: None,
            com_initializer: None,
            should_stop: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Initialize the process loopback client using wasapi-rs
    ///
    /// This uses wasapi-rs AudioClient::new_application_loopback_client for simplicity
    pub fn initialize(&mut self) -> std::result::Result<(), Box<dyn std::error::Error>> {
        // Refuse an incompatible caller apartment before activating a client.
        // The guard then keeps the MTA alive until the client is released.
        let com_initializer = ComInitializer::new()?;
        // Create wasapi-rs AudioClient for application loopback
        let mut audio_client = wasapi::AudioClient::new_application_loopback_client(
            self.process_id,
            self.include_tree,
        )?;

        // Initialize the audio client with a standard format.
        let desired_format =
            wasapi::WaveFormat::new(32, 32, wasapi::SampleType::Float, 48000, 2, None);
        // C1 fix: this is a process-loopback client (created via
        // new_application_loopback_client → ActivateAudioInterfaceAsync with
        // AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK). That activation path
        // does NOT support AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM — combining it
        // with the loopback flags makes IAudioClient::Initialize fail. We must
        // initialize with the exact desired format and no autoconvert. The
        // loopback client can't report its mix format (get_mixformat returns
        // "Not implemented"), so the explicit f32 format above is what we hand
        // it directly.
        let mode = wasapi::StreamMode::EventsShared {
            autoconvert: false,
            buffer_duration_hns: 0,
        };

        audio_client.initialize_client(&desired_format, wasapi::Direction::Capture, mode, None)?;

        // Store the initialized client and its MTA guard (field order guarantees
        // `audio_client` drops before `com_initializer`).
        self.audio_client = Some(audio_client);
        self.com_initializer = Some(com_initializer);
        Ok(())
    }

    /// Start capturing audio from the target process using wasapi-rs
    ///
    /// # Deprecated — RT-unsafe (audit L13)
    ///
    /// `callback` is invoked **on the WASAPI capture (real-time) thread**. Doing
    /// any allocation/lock/blocking work there risks audio glitches. Use the
    /// canonical `AudioCapture` + [`CapturingStream`] path instead, which
    /// delivers audio on a non-real-time consumer thread via the bridge ring
    /// buffer. See the type-level docs on [`WindowsApplicationCapture`].
    ///
    /// # Implementation Notes
    /// - Uses wasapi-rs for simplified audio capture
    /// - Based on wasapi-rs examples for reliability
    #[deprecated(
        note = "RT-unsafe: invokes the callback on the capture thread. Use AudioCapture + CapturingStream (BridgeStream) instead. See L13."
    )]
    pub fn start_capture<F>(
        &mut self,
        callback: F,
    ) -> std::result::Result<(), Box<dyn std::error::Error>>
    where
        F: Fn(&[f32]) + Send + 'static,
    {
        // Internal delegation to the (also deprecated) stop-flag variant.
        #[allow(deprecated)]
        self.start_capture_with_stop_flag(callback, None)
    }

    /// Start capturing audio with an external stop flag.
    ///
    /// # Deprecated — RT-unsafe (audit L13)
    ///
    /// As with [`start_capture`](Self::start_capture), `callback` runs on the
    /// real-time WASAPI capture thread. Prefer the canonical `AudioCapture` +
    /// [`CapturingStream`] pipeline. See the type-level docs.
    #[deprecated(
        note = "RT-unsafe: invokes the callback on the capture thread. Use AudioCapture + CapturingStream (BridgeStream) instead. See L13."
    )]
    pub fn start_capture_with_stop_flag<F>(
        &mut self,
        callback: F,
        external_stop_flag: Option<Arc<AtomicBool>>,
    ) -> std::result::Result<(), Box<dyn std::error::Error>>
    where
        F: Fn(&[f32]) + Send + 'static,
    {
        let audio_client = self
            .audio_client
            .as_mut()
            .ok_or("Audio client not initialized")?;

        // Get event handle and capture client from wasapi-rs
        let h_event = audio_client.set_get_eventhandle()?;
        let capture_client = audio_client.get_audiocaptureclient()?;

        // Start the audio stream using wasapi-rs
        audio_client.start_stream()?;

        // Reset stop flag at start of capture
        self.should_stop.store(false, Ordering::SeqCst);

        // Pre-allocate reusable buffers outside the loop to avoid per-iteration allocations.
        let mut sample_queue = std::collections::VecDeque::with_capacity(48000 * 4 * 2 / 10);
        let mut samples: Vec<f32> = Vec::with_capacity(48000 * 2 / 10);

        // Capture loop based on wasapi-rs examples
        loop {
            // Check if we should stop capture (internal or external flag)
            if self.should_stop.load(Ordering::SeqCst) {
                break;
            }
            if let Some(ref external_flag) = external_stop_flag {
                if external_flag.load(Ordering::SeqCst) {
                    break;
                }
            }

            // Wait for audio data (shorter timeout to check stop flag more frequently)
            if h_event.wait_for_event(100).is_err() {
                // Timeout — check stop flags and continue
                if self.should_stop.load(Ordering::SeqCst) {
                    break;
                }
                if let Some(ref external_flag) = external_stop_flag {
                    if external_flag.load(Ordering::SeqCst) {
                        break;
                    }
                }
                continue;
            }

            // Get available packet size
            let packet_length = capture_client.get_next_packet_size()?.unwrap_or(0);

            if packet_length > 0 {
                // Reuse sample_queue across iterations
                sample_queue.clear();
                capture_client.read_from_device_to_deque(&mut sample_queue)?;

                // Convert bytes to f32 samples using efficient slice access
                if !sample_queue.is_empty() {
                    samples.clear();
                    let (front, back) = sample_queue.as_slices();
                    for chunk in front.chunks_exact(4) {
                        samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                    }
                    for chunk in back.chunks_exact(4) {
                        samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                    }

                    if !samples.is_empty() {
                        callback(&samples);
                    }
                }
            }
        }

        Ok(())
    }

    /// Stop capturing audio
    pub fn stop_capture(&mut self) -> std::result::Result<(), Box<dyn std::error::Error>> {
        // Signal the capture loop to stop
        self.should_stop.store(true, Ordering::SeqCst);

        if let Some(audio_client) = &mut self.audio_client {
            audio_client.stop_stream()?;
        }

        // Clear the audio client
        self.audio_client = None;

        Ok(())
    }

    /// Check if currently capturing (simplified implementation)
    pub fn is_capturing(&self) -> bool {
        self.audio_client.is_some()
    }

    /// Stop capturing audio (alias for [`stop_capture`](Self::stop_capture)).
    pub fn stop(&mut self) -> std::result::Result<(), Box<dyn std::error::Error>> {
        self.stop_capture()
    }

    /// Find process ID by name (convenience helper)
    ///
    /// # Arguments
    /// * `process_name` - Name of the process to find (e.g., "firefox.exe")
    /// * `prefer_parent` - If true, return parent PID for process tree capture
    ///
    /// # Returns
    /// Process ID if found, None otherwise
    pub fn find_process_by_name(process_name: &str, prefer_parent: bool) -> Option<u32> {
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::All, true);

        let processes: Vec<_> = system.processes_by_name(OsStr::new(process_name)).collect();

        if let Some(process) = processes.first() {
            if prefer_parent {
                // Return parent PID if available, otherwise the process PID
                Some(
                    process
                        .parent()
                        .map(|p| p.as_u32())
                        .unwrap_or_else(|| process.pid().as_u32()),
                )
            } else {
                Some(process.pid().as_u32())
            }
        } else {
            None
        }
    }

    /// List all processes that could potentially be captured
    pub fn list_audio_processes() -> Vec<(u32, String)> {
        let mut system = System::new();
        system.refresh_processes(ProcessesToUpdate::All, true);

        system
            .processes()
            .iter()
            .map(|(pid, process)| (pid.as_u32(), process.name().to_string_lossy().to_string()))
            .collect()
    }
}

// SAFETY: `initialize` acquires the IAudioClient only after checking that its
// caller is in the MTA. This is not a full Send guarantee: #105 tracks later
// STA use/release and the required same-thread release of the IAudioCaptureClient
// and its parent IAudioClient.
unsafe impl Send for WindowsApplicationCapture {}

/// RAII wrapper for a Windows HANDLE to ensure it's closed on drop.
#[derive(Debug)]
struct ProcessHandle(Option<HANDLE>);

impl ProcessHandle {
    /// Wraps a HANDLE value. Pass `None` for no handle.
    fn new(handle: Option<HANDLE>) -> Self {
        Self(handle)
    }

    /// Returns the wrapped HANDLE, if any.
    #[allow(dead_code)] // Available for future callers who need the raw HANDLE
    fn get(&self) -> Option<HANDLE> {
        self.0
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        if let Some(h) = self.0.take() {
            if !h.is_invalid() {
                // Ensure we don't try to close an invalid handle like INVALID_HANDLE_VALUE
                unsafe {
                    let _ = CloseHandle(h);
                }
            }
        }
    }
}

/// Keeps the process-wide COM multithreaded apartment (MTA) alive while held.
///
/// Holds only the cookie from `CoIncrementMTAUsage` and returns it with
/// `CoDecrementMTAUsage` on drop. It makes no apartment check and leaves the
/// calling thread's apartment as it is: while a pin is alive, a thread that
/// never initialised COM runs in the implicit MTA, and a thread in an STA or in
/// the MTA stays where it is. The decrement is valid on any thread, so a pin may
/// outlive the thread that created it.
#[derive(Debug)]
pub(crate) struct MtaUsagePin {
    cookie: CO_MTA_USAGE_COOKIE,
}

// SAFETY: The only field is the cookie, and the only operation on it is the
// single `CoDecrementMTAUsage` in `Drop`. Microsoft documents that call as
// valid on a thread other than the one that called `CoIncrementMTAUsage`:
// https://learn.microsoft.com/windows/win32/api/combaseapi/nf-combaseapi-codecrementmtausage
unsafe impl Send for MtaUsagePin {}
// SAFETY: A shared reference exposes no operation on the cookie; only `Drop`,
// with exclusive access, passes it to `CoDecrementMTAUsage`, under the
// any-thread contract cited on the `Send` impl.
unsafe impl Sync for MtaUsagePin {}

impl MtaUsagePin {
    /// Increments the process-wide MTA usage count, creating the MTA if needed.
    pub(crate) fn new() -> AudioResult<Self> {
        // SAFETY: No arguments. On success the cookie is owned by the returned
        // value and released exactly once, by `Drop`.
        let cookie = unsafe { CoIncrementMTAUsage() }.map_err(|err| AudioError::BackendError {
            backend: "wasapi".to_string(),
            operation: "com_init".to_string(),
            message: format!("CoIncrementMTAUsage failed (HRESULT: {:?})", err),
            context: Some(wasapi_backend_context_err(&err)),
        })?;
        Ok(Self { cookie })
    }
}

impl Drop for MtaUsagePin {
    fn drop(&mut self) {
        // SAFETY: `self.cookie` was returned by the successful
        // `CoIncrementMTAUsage` in `new` and is released exactly once, here, on
        // whichever thread drops the pin (valid per the `Send` impl's citation).
        let _ = unsafe { CoDecrementMTAUsage(self.cookie) };
    }
}

/// Pins the process-wide COM multithreaded apartment (MTA) and requires the
/// calling thread to be in it.
///
/// The strict form of the MTA usage pin, for code whose WASAPI objects must live
/// in the MTA: the capture worker thread creates one before any audio client,
/// and [`WindowsApplicationCapture::initialize`] does too. A thread that never
/// initialised COM joins the implicit MTA and passes. A thread already in a
/// single-threaded apartment (STA) keeps its apartment and gets a `com_init`
/// backend error carrying `RPC_E_CHANGED_MODE`.
///
/// [`WindowsDeviceEnumerator`], [`WindowsAudioDevice`] and
/// [`enumerate_application_audio_sessions`] do not make this check; they work
/// on STA threads too.
///
/// Dropping it releases the MTA pin, which is valid on any thread. It never
/// uninitialises COM on the dropping thread.
#[derive(Debug)]
pub struct ComInitializer {
    #[allow(dead_code)] // RAII guard: held for its `Drop`, which releases the MTA pin.
    pin: MtaUsagePin,
}

impl ComInitializer {
    /// Pins the process-wide MTA and checks the calling thread belongs to it.
    ///
    /// An uninitialized caller joins the implicit MTA. A caller already in an
    /// STA retains its apartment and receives a `com_init` backend error.
    pub fn new() -> AudioResult<Self> {
        // Own the pin before the query so every error path releases it.
        let guard = Self {
            pin: MtaUsagePin::new()?,
        };
        let mut apartment = APTTYPE_CURRENT;
        let mut qualifier = APTTYPEQUALIFIER_NONE;
        // SAFETY: Both out-pointers refer to live, writable stack variables.
        unsafe { CoGetApartmentType(&mut apartment, &mut qualifier) }.map_err(|err| {
            AudioError::BackendError {
                backend: "wasapi".to_string(),
                operation: "com_init".to_string(),
                message: format!("CoGetApartmentType failed: {err}"),
                context: Some(wasapi_backend_context_err(&err)),
            }
        })?;
        if apartment == APTTYPE_MTA {
            return Ok(guard);
        }

        let description = if matches!(apartment, APTTYPE_STA | APTTYPE_MAINSTA) {
            "a single-threaded apartment"
        } else {
            "a non-MTA apartment"
        };
        Err(AudioError::BackendError {
            backend: "wasapi".to_string(),
            operation: "com_init".to_string(),
            message: format!(
                "Already initialized with a different concurrency model: the calling thread is in \
                 {description} (HRESULT: 0x{:08X})",
                RPC_E_CHANGED_MODE.0
            ),
            context: Some(wasapi_backend_context(RPC_E_CHANGED_MODE)),
        })
    }
}

/// A Windows audio endpoint, as read when it was enumerated.
///
/// Holds plain data only: the endpoint id, its friendly name and data-flow
/// kind, and whether it was the default render endpoint, all read at
/// enumeration, plus an MTA usage pin shared with its enumerator. It holds no
/// COM interface. [`supported_formats`](AudioDevice::supported_formats) looks the
/// endpoint up by id and creates, uses and releases its COM objects within the
/// call, on the calling thread. A device may therefore be used and dropped on
/// any thread, in any apartment, including after the thread that enumerated it
/// has exited; it is `Send + Sync` through the auto traits.
#[derive(Debug)]
pub struct WindowsAudioDevice {
    id: DeviceId,
    name: String,
    kind: std::result::Result<DeviceKind, KindReadFailure>,
    is_default: bool,
    /// Keeps the MTA alive, so a calling thread that never initialised COM runs
    /// in the implicit MTA during `supported_formats`.
    #[allow(dead_code)] // RAII guard: held for its `Drop`.
    pin: Arc<MtaUsagePin>,
}

/// Why an endpoint's data-flow direction could not be read at enumeration.
/// [`AudioDevice::kind`] rebuilds the `get_device_kind` backend error from it.
#[derive(Debug, Clone)]
struct KindReadFailure {
    message: String,
    context: Option<BackendContext>,
}

impl WindowsAudioDevice {
    /// Reads `device`'s id, friendly name and kind on the calling thread.
    ///
    /// `default_id` is the id of the default render endpoint, read in the same
    /// call; the device is the default when its id equals it. A field that
    /// cannot be read takes the value its getter reports for that failure.
    fn from_endpoint(device: &IMMDevice, default_id: Option<&str>, pin: Arc<MtaUsagePin>) -> Self {
        let id = read_endpoint_id(device);
        let is_default = id.is_some() && id.as_deref() == default_id;
        Self {
            id: DeviceId(id.unwrap_or_else(|| "unknown-device".to_string())),
            name: read_friendly_name(device)
                .unwrap_or_else(|_| "Unknown Windows Device".to_string()),
            kind: read_device_kind(device),
            is_default,
            pin,
        }
    }
}

impl WindowsAudioDevice {
    /// Helper function to convert a PWSTR to a String.
    /// Assumes the PWSTR is null-terminated.
    unsafe fn pwstr_to_string(pwstr: PWSTR) -> AudioResult<String> {
        if pwstr.is_null() {
            return Err(AudioError::BackendError {
                backend: "wasapi".to_string(),
                operation: "pwstr_to_string".to_string(),
                message: "PWSTR pointer was null".to_string(),
                context: None,
            });
        }
        pwstr.to_string().map_err(|e| AudioError::BackendError {
            backend: "wasapi".to_string(),
            operation: "pwstr_to_string".to_string(),
            message: format!("Failed to convert PWSTR to string: {:?}", e),
            context: None,
        })
    }
}

/// New canonical `AudioDevice` trait implementation for `WindowsAudioDevice`.
///
/// This follows the BridgeStream architecture pattern established by the Linux
/// backend. The `create_stream()` method wires through:
///   `WindowsCaptureThread` → `BridgeProducer` → ring buffer → `BridgeConsumer` → `BridgeStream`
impl AudioDevice for WindowsAudioDevice {
    fn id(&self) -> DeviceId {
        self.id.clone()
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    /// `true` when this endpoint was the default render (console) endpoint at
    /// enumeration: the endpoint
    /// [`default_device`](DeviceEnumerator::default_device) returns.
    fn is_default(&self) -> bool {
        self.is_default
    }

    /// Looks the endpoint up by its id on the calling thread and queries its
    /// formats. An endpoint that no longer exists, or whose query fails, yields
    /// the single [`AudioFormat::default`] fallback, never another endpoint's
    /// formats.
    fn supported_formats(&self) -> Vec<AudioFormat> {
        self.query_supported_formats_internal()
            .unwrap_or_else(|_| vec![AudioFormat::default()])
    }

    /// The endpoint direction read via `IMMEndpoint::GetDataFlow` at
    /// enumeration: `eRender` → [`DeviceKind::Output`], `eCapture` →
    /// [`DeviceKind::Input`]. Stays fallible rather than collapsing to a
    /// default: when the direction could not be read, this returns a backend
    /// error with operation `get_device_kind` and the context of the failed call.
    fn kind(&self) -> AudioResult<DeviceKind> {
        self.kind
            .clone()
            .map_err(|failure| AudioError::BackendError {
                backend: "wasapi".to_string(),
                operation: "get_device_kind".to_string(),
                message: failure.message,
                context: failure.context,
            })
    }

    fn create_stream(&self, config: &StreamConfig) -> AudioResult<Box<dyn CapturingStream>> {
        // === 9-step BridgeStream wiring (following Linux pattern) ===

        // 1. Build AudioFormat from StreamConfig
        let format = config.to_audio_format();

        // 2. Use the capture target from StreamConfig (propagated from builder)
        let target = config.capture_target.clone();

        // 3. Create the ring buffer bridge
        let capacity = calculate_capacity(config.buffer_size, 4);
        let (producer, consumer) = create_bridge(capacity, format.clone());

        // 4. Transition bridge state Created → Running
        consumer
            .shared()
            .state
            .transition(StreamState::Created, StreamState::Running)
            .map_err(|actual| AudioError::InternalError {
                message: format!(
                    "Bridge state transition failed: expected Created, got {:?}",
                    actual
                ),
                source: None,
            })?;

        // 5. Build WindowsCaptureConfig
        let capture_config = WindowsCaptureConfig {
            target,
            sample_rate: config.sample_rate,
            channels: config.channels,
        };

        // 6. Spawn the WASAPI capture thread (sends producer to thread)
        let capture_thread = WindowsCaptureThread::spawn(capture_config, producer)?;

        // 7. Wrap in Arc<Mutex> → WindowsPlatformStream
        let capture_thread_arc = std::sync::Arc::new(std::sync::Mutex::new(capture_thread));
        let platform_stream = WindowsPlatformStream::new(capture_thread_arc);

        // 8. Create BridgeStream with 1-second default timeout
        let bridge_stream = BridgeStream::new(
            consumer,
            platform_stream,
            format,
            std::time::Duration::from_secs(1),
        );

        // 9. Return as boxed CapturingStream
        Ok(Box::new(bridge_stream))
    }
}

impl WindowsAudioDevice {
    /// Query the endpoint's supported audio formats via WASAPI format negotiation.
    ///
    /// Resolves the endpoint by its id through a call-local enumerator on the
    /// calling thread, with no fallback to another endpoint, then queries the
    /// device's mix format (its preferred shared-mode format) and probes common
    /// sample rates. The mix format is always first in the returned list. Every
    /// COM object is released before this returns; `self.pin` outlives them.
    ///
    /// Uses `IAudioClient::IsFormatSupported` in shared mode, matching the
    /// pattern from camilladsp's `get_supported_wave_format()`.
    fn query_supported_formats_internal(&self) -> AudioResult<Vec<AudioFormat>> {
        let mut formats = Vec::new();

        // Declared in creation order, so they release client, device, enumerator.
        let enumerator = wasapi::DeviceEnumerator::new().map_err(|e| AudioError::BackendError {
            backend: "wasapi".to_string(),
            operation: "query_supported_formats".to_string(),
            message: format!("Failed to create DeviceEnumerator: {}", e),
            context: None,
        })?;
        let wasapi_device =
            enumerator
                .get_device(&self.id.0)
                .map_err(|e| AudioError::BackendError {
                    backend: "wasapi".to_string(),
                    operation: "query_supported_formats".to_string(),
                    message: format!("Failed to resolve endpoint '{}': {}", self.id.0, e),
                    context: None,
                })?;
        let audio_client =
            wasapi_device
                .get_iaudioclient()
                .map_err(|e| AudioError::BackendError {
                    backend: "wasapi".to_string(),
                    operation: "query_supported_formats".to_string(),
                    message: format!("Failed to get IAudioClient: {}", e),
                    context: None,
                })?;

        // Query the device's mix format (its preferred shared-mode format).
        // This is always supported and goes first in the list.
        if let Ok(mix_fmt) = audio_client.get_mixformat() {
            let mix_sr = mix_fmt.get_samplespersec();
            let mix_ch = mix_fmt.get_nchannels();
            let mix_bits = mix_fmt.get_bitspersample();
            let mix_valid_bits = mix_fmt.get_validbitspersample();

            let sample_format = wasapi_bits_to_sample_format(mix_bits, mix_valid_bits);
            formats.push(AudioFormat {
                sample_rate: mix_sr,
                channels: mix_ch,
                sample_format,
            });
        }

        // Probe common sample rates with the device's channel count.
        let probe_channels = formats.first().map(|f| f.channels).unwrap_or(2) as usize;
        let common_rates: &[usize] = &[44100, 48000, 88200, 96000, 176400, 192000];
        let probe_formats: &[(usize, usize, wasapi::SampleType)] = &[
            (32, 32, wasapi::SampleType::Float), // F32
            (16, 16, wasapi::SampleType::Int),   // I16
            (32, 24, wasapi::SampleType::Int),   // I24 in 32-bit container
            (32, 32, wasapi::SampleType::Int),   // I32
        ];

        for &rate in common_rates {
            for &(bits, valid_bits, sample_type) in probe_formats {
                let wave_fmt = wasapi::WaveFormat::new(
                    bits,
                    valid_bits,
                    sample_type,
                    rate,
                    probe_channels,
                    None,
                );

                // is_supported returns Ok(None) if directly supported,
                // Ok(Some(closest)) if a close match exists, Err if unsupported.
                match audio_client.is_supported(&wave_fmt, wasapi::ShareMode::Shared) {
                    Ok(None) => {
                        // Format is directly supported
                        let sf = wasapi_bits_to_sample_format(bits as u16, valid_bits as u16);
                        let fmt = AudioFormat {
                            sample_rate: rate as u32,
                            channels: probe_channels as u16,
                            sample_format: sf,
                        };
                        if !formats.contains(&fmt) {
                            formats.push(fmt);
                        }
                    }
                    Ok(Some(_closest)) => {
                        // A close match exists — the exact requested format is not supported,
                        // but we don't add the closest since it may be redundant with mix format.
                    }
                    Err(_) => {
                        // Format not supported at all
                    }
                }
            }
        }

        // Ensure we always return at least the default format as a fallback.
        if formats.is_empty() {
            formats.push(AudioFormat::default());
        }

        Ok(formats)
    }
}

/// Reads `device`'s id on the calling thread; `None` when `GetId` fails.
fn read_endpoint_id(device: &IMMDevice) -> Option<String> {
    // SAFETY: `device` is a live IMMDevice. `GetId` returns a string allocated
    // with `CoTaskMemAlloc`; it is copied, then freed exactly once.
    unsafe {
        let id_pwstr = device.GetId().ok()?;
        let id = WindowsAudioDevice::pwstr_to_string(id_pwstr).ok();
        CoTaskMemFree(Some(id_pwstr.as_ptr().cast()));
        id
    }
}

/// Reads `PKEY_Device_FriendlyName` from `device`'s property store on the
/// calling thread. A value that is not a wide string reads as `"Unknown Device"`.
fn read_friendly_name(device: &IMMDevice) -> AudioResult<String> {
    // SAFETY: `device` is a live IMMDevice. The property store and the
    // PROPVARIANT are owned values released when they drop, and `pwszVal` is
    // read only when `vt` says the variant holds a wide string.
    unsafe {
        let property_store: IPropertyStore =
            device
                .OpenPropertyStore(STGM_READ)
                .map_err(|hr| AudioError::BackendError {
                    backend: "wasapi".to_string(),
                    operation: "get_device_name".to_string(),
                    message: format!("IMMDevice::OpenPropertyStore failed (HRESULT: {:?})", hr),
                    context: Some(wasapi_backend_context_err(&hr)),
                })?;

        let prop_variant = property_store
            .GetValue(&PKEY_Device_FriendlyName as *const _ as *const _)
            .map_err(|hr| AudioError::BackendError {
                backend: "wasapi".to_string(),
                operation: "get_device_name".to_string(),
                message: format!(
                    "IPropertyStore::GetValue for PKEY_Device_FriendlyName failed (HRESULT: {:?})",
                    hr
                ),
                context: Some(wasapi_backend_context_err(&hr)),
            })?;

        let name = if prop_variant.vt() == windows::Win32::System::Variant::VARENUM(VT_LPWSTR) {
            let name_pwstr = prop_variant.Anonymous.Anonymous.Anonymous.pwszVal;
            WindowsAudioDevice::pwstr_to_string(name_pwstr)
                .unwrap_or_else(|_| "Unknown Device".to_string())
        } else {
            "Unknown Device".to_string()
        };
        Ok(name)
    }
}

/// Reads `device`'s direction via `IMMEndpoint::GetDataFlow` on the calling
/// thread: `eRender` → [`DeviceKind::Output`], `eCapture` → [`DeviceKind::Input`].
/// Any other flow, or a failed call, is a [`KindReadFailure`].
fn read_device_kind(device: &IMMDevice) -> std::result::Result<DeviceKind, KindReadFailure> {
    let endpoint: IMMEndpoint = device.cast().map_err(|hr| KindReadFailure {
        message: format!(
            "Failed to cast IMMDevice to IMMEndpoint (HRESULT: {:?})",
            hr
        ),
        context: Some(wasapi_backend_context_err(&hr)),
    })?;
    // SAFETY: `endpoint` is the live IMMEndpoint obtained above.
    let flow = unsafe { endpoint.GetDataFlow() }.map_err(|hr| KindReadFailure {
        message: format!("IMMEndpoint::GetDataFlow failed (HRESULT: {:?})", hr),
        context: Some(wasapi_backend_context_err(&hr)),
    })?;
    data_flow_to_kind(flow).ok_or_else(|| KindReadFailure {
        message: format!("Unknown data flow value: {:?}", flow),
        context: None,
    })
}

// ── Device-change notifications (M10 watch() — rsac-e360) ─────────────────
//
// WASAPI delivers hot-plug / default-change notifications through an
// `IMMNotificationClient` COM object registered via
// `IMMDeviceEnumerator::RegisterEndpointNotificationCallback`. The MMDevice API
// invokes those callbacks on an internal system thread that holds COM locks; the
// docs are explicit that the callback must return quickly and must not block,
// wait on another thread, or call back into the enumerator in a way that can
// re-enter the audio engine. Invoking an arbitrary user `FnMut` directly there
// risks deadlock and COM re-entrancy.
//
// Hand-off pattern (the canonical Windows/macOS design in the M10 surface): the
// COM callback only *translates* the event and pushes it into a bounded channel
// — alloc/lock on this thread is permitted (it is NOT the real-time audio
// callback thread), but it never runs user code, makes no COM call and holds no
// COM pointer, only the sender.
//
// Ownership: one helper thread per watcher (`rsac-wasapi-device-watch`) creates,
// uses and releases every COM object of that watcher. It initialises COM as MTA,
// creates its own enumerator and the notification client, registers, reports the
// outcome to `watch()`, pops events (resolving a `DeviceAdded` name and kind with
// its own enumerator) and invokes the user handler. On shutdown it unregisters
// the same client pointer and releases both interfaces before uninitialising
// COM, all on that thread. The `DeviceWatcher` teardown only sets a flag and
// joins the helper — no leak, no hang.

/// Bound on the channel between the COM notification thread and the helper
/// thread that runs the user handler.
///
/// Device-change events are rare (hot-plug, default switch), so a small bound is
/// ample. If the user handler stalls and the buffer fills, the COM callback drops
/// the oldest-unqueued event rather than block the system notification thread —
/// dropping a redundant notification is far better than deadlocking COM.
const DEVICE_EVENT_CHANNEL_BOUND: usize = 64;

/// How long the helper thread blocks waiting for the next event before
/// re-checking the shutdown flag. Keeps teardown latency bounded without busy
/// spinning.
const NOTIFY_THREAD_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long `watch()` waits for the helper thread's registration report.
/// Registration is a few in-process COM calls; the bound only keeps `watch()`
/// from hanging on a stuck one.
const WATCH_REGISTRATION_TIMEOUT: Duration = Duration::from_secs(5);

/// Map a WASAPI [`EDataFlow`] to the rsac [`DeviceKind`].
///
/// `eRender` → [`DeviceKind::Output`], `eCapture` → [`DeviceKind::Input`].
/// `eAll` (or any unexpected value) has no single direction; callers treat
/// `None` as "kind unknown" and skip kind-specific events.
fn data_flow_to_kind(flow: EDataFlow) -> Option<DeviceKind> {
    if flow == eRender {
        Some(DeviceKind::Output)
    } else if flow == eCapture {
        Some(DeviceKind::Input)
    } else {
        None
    }
}

/// Resolve a newly added device's name and kind on the device-watch helper
/// thread, with that thread's enumerator. The endpoint may not be ready yet, so
/// resolution is best-effort.
fn resolve_added_device(enumerator: &IMMDeviceEnumerator, device_id: &str) -> (String, DeviceKind) {
    // SAFETY: `enumerator` is a live IMMDeviceEnumerator; GetDevice takes a
    // null-terminated wide string. We build one from `device_id`.
    let wide: Vec<u16> = device_id.encode_utf16().chain(std::iter::once(0)).collect();
    let device: Option<IMMDevice> = unsafe { enumerator.GetDevice(PCWSTR(wide.as_ptr())) }.ok();

    let Some(device) = device else {
        // Endpoint not resolvable (already gone / not ready). Default to Input so
        // the event is still self-consistent; name unknown.
        return (String::new(), DeviceKind::Input);
    };

    // Kind via IMMEndpoint::GetDataFlow.
    let kind = device
        .cast::<IMMEndpoint>()
        .ok()
        .and_then(|endpoint| unsafe { endpoint.GetDataFlow() }.ok())
        .and_then(data_flow_to_kind)
        .unwrap_or(DeviceKind::Input);

    // Friendly name via the property store (mirrors `read_friendly_name`).
    let name = unsafe { friendly_name_of(&device) }.unwrap_or_default();

    (name, kind)
}

/// Read `PKEY_Device_FriendlyName` from an `IMMDevice`'s property store.
///
/// # Safety
/// `device` must be a valid `IMMDevice`. Caller must hold a COM apartment.
unsafe fn friendly_name_of(device: &IMMDevice) -> Option<String> {
    let store: IPropertyStore = device.OpenPropertyStore(STGM_READ).ok()?;
    let prop = store
        .GetValue(&PKEY_Device_FriendlyName as *const _ as *const _)
        .ok()?;
    if prop.vt() == windows::Win32::System::Variant::VARENUM(VT_LPWSTR) {
        let pwstr = prop.Anonymous.Anonymous.Anonymous.pwszVal;
        WindowsAudioDevice::pwstr_to_string(pwstr).ok()
    } else {
        None
    }
}

/// What the notification client hands the device-watch helper thread.
#[derive(Debug)]
enum WatchNotice {
    /// A translated event, delivered as is.
    Event(DeviceEvent),
    /// A device was added; the helper resolves its name and kind before delivery.
    Added(DeviceId),
}

/// COM notification client that forwards WASAPI endpoint changes onto a channel.
///
/// Created and registered on the device-watch helper thread with
/// `IMMDeviceEnumerator::RegisterEndpointNotificationCallback`. Its methods run
/// on the MMDevice system notification thread; they only translate and enqueue
/// (never run user code, never call COM) — see the module comment above for the
/// deadlock rationale. It holds no COM pointer, only the sender.
#[implement(IMMNotificationClient)]
struct WatcherNotificationClient {
    /// Bounded sender to the helper thread. `try_send` so a stalled consumer can
    /// never block the COM notification thread.
    tx: mpsc::SyncSender<WatchNotice>,
}

impl WatcherNotificationClient {
    /// Enqueue a notice without ever blocking the COM notification thread.
    /// A full buffer means the consumer is behind; we drop the notice (a device
    /// change is idempotent — the consumer can re-enumerate) rather than stall
    /// COM. A disconnected receiver means the watcher is being torn down.
    fn emit(&self, notice: WatchNotice) {
        match self.tx.try_send(notice) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(notice)) => {
                log::warn!(
                    "wasapi: device-event channel full, dropping notification: {:?}",
                    notice
                );
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                // Helper thread tearing down — nothing to do.
            }
        }
    }
}

// The MMDevice notification callbacks. Each maps a WASAPI endpoint change to a
// `DeviceEvent` and enqueues it. They must return `Ok(())` quickly.
#[allow(non_snake_case)]
impl IMMNotificationClient_Impl for WatcherNotificationClient_Impl {
    fn OnDeviceStateChanged(
        &self,
        pwstrdeviceid: &PCWSTR,
        dwnewstate: DEVICE_STATE,
    ) -> windows::core::Result<()> {
        let id = unsafe { pwstrdeviceid.to_string() }.unwrap_or_default();
        // `available` == is the endpoint usable for capture right now.
        let available = dwnewstate == DEVICE_STATE_ACTIVE;
        self.emit(WatchNotice::Event(DeviceEvent::StateChanged {
            id: DeviceId(id),
            available,
        }));
        Ok(())
    }

    fn OnDeviceAdded(&self, pwstrdeviceid: &PCWSTR) -> windows::core::Result<()> {
        // SAFETY: COM passes a valid null-terminated id for the duration of the call.
        let id = unsafe { pwstrdeviceid.to_string() }.unwrap_or_default();
        // The helper thread resolves the name and kind; this callback makes no
        // COM call.
        self.emit(WatchNotice::Added(DeviceId(id)));
        Ok(())
    }

    fn OnDeviceRemoved(&self, pwstrdeviceid: &PCWSTR) -> windows::core::Result<()> {
        let id = unsafe { pwstrdeviceid.to_string() }.unwrap_or_default();
        self.emit(WatchNotice::Event(DeviceEvent::DeviceRemoved {
            id: DeviceId(id),
        }));
        Ok(())
    }

    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        pwstrdefaultdeviceid: &PCWSTR,
    ) -> windows::core::Result<()> {
        // Collapse the three roles (console/multimedia/communications) onto a
        // single per-direction default. We key off the Console role to avoid
        // emitting three near-identical events for one user-visible change.
        if role != eConsole {
            return Ok(());
        }
        let Some(kind) = data_flow_to_kind(flow) else {
            return Ok(());
        };
        // A null id means "no default device for this role" (e.g. last endpoint
        // unplugged). `to_string` on a null PCWSTR yields an empty string.
        let id = if pwstrdefaultdeviceid.is_null() {
            String::new()
        } else {
            unsafe { pwstrdefaultdeviceid.to_string() }.unwrap_or_default()
        };
        self.emit(WatchNotice::Event(DeviceEvent::DefaultChanged {
            id: DeviceId(id),
            kind,
        }));
        Ok(())
    }

    fn OnPropertyValueChanged(
        &self,
        _pwstrdeviceid: &PCWSTR,
        _key: &PROPERTYKEY,
    ) -> windows::core::Result<()> {
        // Property churn (volume, format tweaks) is far too noisy to surface as a
        // DeviceEvent and has no corresponding variant. Intentionally ignored.
        Ok(())
    }
}

/// Enumerates audio devices available on a Windows system using WASAPI.
///
/// Holds no COM interface, only an MTA usage pin shared with the devices it
/// returns. Each call creates, uses and releases its COM objects within the
/// call, on the calling thread and in that thread's apartment: the
/// `MMDeviceEnumerator` class is registered `ThreadingModel=Both`, so COM creates
/// it directly in an STA or in the MTA, and a thread that never initialised COM
/// runs in the implicit MTA that the pin keeps alive. No call changes the calling
/// thread's apartment. The enumerator is `Send + Sync` through the auto traits
/// and may be shared, used and dropped on any thread, including after the thread
/// that created it has exited. [`watch`](DeviceEnumerator::watch) keeps its COM
/// objects on a helper thread of its own.
#[derive(Debug)]
pub struct WindowsDeviceEnumerator {
    /// Cloned into every device this enumerator returns.
    pin: Arc<MtaUsagePin>,
}

impl WindowsDeviceEnumerator {
    /// Creates a new Windows device enumerator on a thread in any COM apartment
    /// (STA, MTA, or COM never initialised).
    ///
    /// Pins the process-wide MTA, then creates and releases an
    /// `IMMDeviceEnumerator` on the calling thread, so an unavailable MMDevice
    /// API is reported here rather than on first use.
    ///
    /// # Errors
    ///
    /// [`AudioError::BackendError`] with operation `com_init` when the MTA cannot
    /// be pinned, or `create_device_enumerator` when `CoCreateInstance` fails.
    pub fn new() -> AudioResult<Self> {
        let pin = Arc::new(MtaUsagePin::new()?);
        drop(create_imm_device_enumerator()?);
        Ok(Self { pin })
    }
}

/// Creates an `IMMDeviceEnumerator` in the calling thread's apartment. Every
/// caller releases it on the same thread, before the call that created it
/// returns.
fn create_imm_device_enumerator() -> AudioResult<IMMDeviceEnumerator> {
    // SAFETY: `CoCreateInstance` gets a valid CLSID reference and no outer
    // object; the interface is used only after the HRESULT is checked.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(|hr| {
        AudioError::BackendError {
            backend: "wasapi".to_string(),
            operation: "create_device_enumerator".to_string(),
            message: format!("Failed to create IMMDeviceEnumerator (HRESULT: {:?})", hr),
            context: Some(wasapi_backend_context_err(&hr)),
        }
    })
}

/// New canonical `DeviceEnumerator` trait implementation for `WindowsDeviceEnumerator`.
///
/// Returns `Box<dyn AudioDevice>` instead of concrete types, matching the
/// platform-agnostic trait contract in `core::interface`.
impl DeviceEnumerator for WindowsDeviceEnumerator {
    fn enumerate_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice>>> {
        // Every interface below is created, used and released in this call, on
        // the calling thread; `self.pin` outlives all of them.
        let enumerator = create_imm_device_enumerator()?;

        // The default render endpoint's id marks `is_default`. Best-effort: a
        // system without a render endpoint still enumerates.
        // SAFETY: Calling GetDefaultAudioEndpoint on a valid IMMDeviceEnumerator.
        let default_id = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }
            .ok()
            .and_then(|endpoint| read_endpoint_id(&endpoint));

        // SAFETY: Calling EnumAudioEndpoints on a valid IMMDeviceEnumerator.
        let collection: IMMDeviceCollection = unsafe {
            enumerator.EnumAudioEndpoints(eAll, DEVICE_STATE_ACTIVE)
        }
        .map_err(|hr| AudioError::DeviceEnumerationError {
            reason: format!("Failed to enumerate audio endpoints (HRESULT: {:?})", hr),
            context: Some(wasapi_backend_context_err(&hr)),
        })?;

        // SAFETY: Calling GetCount on a valid IMMDeviceCollection.
        let count =
            unsafe { collection.GetCount() }.map_err(|hr| AudioError::DeviceEnumerationError {
                reason: format!(
                    "Failed to get device count from collection (HRESULT: {:?})",
                    hr
                ),
                context: Some(wasapi_backend_context_err(&hr)),
            })?;

        let mut devices: Vec<Box<dyn AudioDevice>> = Vec::with_capacity(count as usize);
        for i in 0..count {
            // SAFETY: Calling Item on a valid IMMDeviceCollection with a valid index.
            let endpoint: IMMDevice =
                unsafe { collection.Item(i) }.map_err(|hr| AudioError::DeviceEnumerationError {
                    reason: format!(
                        "Failed to get device item {} from collection (HRESULT: {:?})",
                        i, hr
                    ),
                    context: Some(wasapi_backend_context_err(&hr)),
                })?;
            devices.push(Box::new(WindowsAudioDevice::from_endpoint(
                &endpoint,
                default_id.as_deref(),
                Arc::clone(&self.pin),
            )));
        }
        Ok(devices)
    }

    fn default_device(&self) -> AudioResult<Box<dyn AudioDevice>> {
        // Created, used and released in this call, on the calling thread.
        let enumerator = create_imm_device_enumerator()?;

        // For audio capture, the default render device is most relevant (loopback).
        // SAFETY: Calling GetDefaultAudioEndpoint on a valid IMMDeviceEnumerator.
        match unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) } {
            Ok(endpoint) => {
                let mut device =
                    WindowsAudioDevice::from_endpoint(&endpoint, None, Arc::clone(&self.pin));
                device.is_default = true;
                Ok(Box::new(device))
            }
            Err(hr) if hr.code() == E_NOTFOUND => Err(AudioError::DeviceNotFound {
                device_id: "default_render".to_string(),
            }),
            Err(hr) => Err(AudioError::DeviceEnumerationError {
                reason: format!("Failed to get default audio endpoint (HRESULT: {:?})", hr),
                context: Some(wasapi_backend_context_err(&hr)),
            }),
        }
    }

    /// Subscribe to WASAPI device hot-plug / default-change notifications
    /// (M10 `watch()` — rsac-e360).
    ///
    /// Callable on any thread, whatever its COM apartment: the caller makes no
    /// COM call. A dedicated helper thread (`rsac-wasapi-device-watch`) owns
    /// every COM object of the subscription: it initialises COM as MTA, creates
    /// its own `IMMDeviceEnumerator` and an [`IMMNotificationClient`], registers
    /// the client via `RegisterEndpointNotificationCallback`, and reports the
    /// outcome, which `watch` waits for (at most five seconds). The COM callbacks
    /// (run on the MMDevice system notification thread) only translate each
    /// change and push it onto a bounded channel; the helper pops it, resolves a
    /// `DeviceAdded` name and kind with its own enumerator, and invokes
    /// `on_event`. The user handler therefore **never** runs on the COM
    /// notification thread, avoiding COM re-entrancy / deadlock (see the
    /// module-level notes by `WatcherNotificationClient`, the crate-private COM
    /// callback object).
    ///
    /// Dropping the [`DeviceWatcher`], on any thread, signals the helper and
    /// joins it; the helper stops delivery, unregisters the client it
    /// registered, and releases its COM objects before it exits. After `drop`
    /// returns, `on_event` is guaranteed not to be called again. Dropping the
    /// watcher from inside `on_event` returns without joining (the helper cannot
    /// join itself): `on_event` is not called again, and the helper finishes the
    /// same teardown once the handler returns.
    ///
    /// # Errors
    ///
    /// - [`AudioError::BackendError`] when the helper thread cannot be spawned
    ///   (operation `spawn_device_watch_thread`), COM cannot be initialised on it
    ///   (`com_init`), its enumerator cannot be created
    ///   (`create_device_enumerator`), or registration fails
    ///   (`register_endpoint_notification_callback`).
    /// - [`AudioError::Timeout`] when the helper does not report within five
    ///   seconds. The helper then exits on its own once it can, unregistering
    ///   if it registered, without calling `on_event`.
    fn watch(&self, on_event: DeviceEventHandler) -> AudioResult<DeviceWatcher> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = Arc::clone(&shutdown);
        // Zero capacity: the helper's `send` succeeds only once this thread has
        // taken the report, so a report that misses the bound below fails on the
        // helper, which then releases everything without delivering.
        let (ready_tx, ready_rx) = mpsc::sync_channel::<AudioResult<()>>(0);

        let helper = std::thread::Builder::new()
            .name("rsac-wasapi-device-watch".to_string())
            .spawn(move || device_watch_thread_main(ready_tx, thread_shutdown, on_event))
            .map_err(|e| AudioError::BackendError {
                backend: "wasapi".to_string(),
                operation: "spawn_device_watch_thread".to_string(),
                message: format!("Failed to spawn device-watch helper thread: {}", e),
                context: None,
            })?;

        match ready_rx.recv_timeout(WATCH_REGISTRATION_TIMEOUT) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                // The helper has released its COM objects and only returns now.
                let _ = helper.join();
                return Err(e);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // COM calls are not cancellable; do not join a stuck one. Dropping
                // `ready_rx` on return fails the helper's report, so it unregisters
                // (if it registered) and exits without calling the handler.
                return Err(AudioError::Timeout {
                    operation: "WASAPI device-watch registration".to_string(),
                    duration: WATCH_REGISTRATION_TIMEOUT,
                });
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = helper.join();
                return Err(AudioError::BackendError {
                    backend: "wasapi".to_string(),
                    operation: "spawn_device_watch_thread".to_string(),
                    message: "device-watch helper thread exited without reporting registration"
                        .to_string(),
                    context: None,
                });
            }
        }

        // Captures only the flag and the join handle; it holds no COM object.
        let teardown = Box::new(move || {
            shutdown.store(true, Ordering::Release);
            if helper.thread().id() == std::thread::current().id() {
                // Dropped from inside the handler, on the helper itself, which
                // cannot join itself. The helper checks the flag before every
                // handler call and runs its teardown once the handler returns.
                return;
            }
            // Bounded by one poll interval after the flag is set, plus a handler
            // call in progress and the helper's unregister and release. A
            // panicked helper surfaces only as a join error; never panic in Drop.
            let _ = helper.join();
        });

        Ok(DeviceWatcher::from_teardown(teardown))
    }
}

/// COM initialised as MTA on the current thread for as long as the value lives.
///
/// `Drop` calls `CoUninitialize`; the type is `!Send`, so that runs on the
/// thread that initialised COM.
struct MtaApartment {
    _not_send: PhantomData<*const ()>,
}

impl MtaApartment {
    fn enter() -> AudioResult<Self> {
        // SAFETY: No pointer arguments. A successful return (S_OK or S_FALSE) is
        // balanced by exactly one `CoUninitialize`, in `Drop`, on this thread.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(|err| AudioError::BackendError {
                backend: "wasapi".to_string(),
                operation: "com_init".to_string(),
                message: format!(
                    "CoInitializeEx(COINIT_MULTITHREADED) failed (HRESULT: {:?})",
                    err
                ),
                context: Some(wasapi_backend_context_err(&err)),
            })?;
        Ok(Self {
            _not_send: PhantomData,
        })
    }
}

impl Drop for MtaApartment {
    fn drop(&mut self) {
        // SAFETY: Balances the successful `CoInitializeEx` in `enter`, on the
        // thread that made it (`MtaApartment` is `!Send`).
        unsafe { CoUninitialize() };
    }
}

/// A notification client registered with an enumerator, both created on the
/// device-watch helper thread.
///
/// Its COM fields make it `!Send`, so it never leaves that thread. Dropping it,
/// including during an unwind from a panicking handler, unregisters the same
/// client pointer and then releases both interfaces, on that thread.
struct EndpointRegistration {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
}

impl EndpointRegistration {
    fn register(tx: mpsc::SyncSender<WatchNotice>) -> AudioResult<Self> {
        let enumerator = create_imm_device_enumerator()?;
        let client: IMMNotificationClient = WatcherNotificationClient { tx }.into();
        // SAFETY: `enumerator` and `client` are live interfaces created above on
        // this thread. From here on COM may invoke `client` on its notification
        // thread.
        unsafe { enumerator.RegisterEndpointNotificationCallback(&client) }.map_err(|hr| {
            AudioError::BackendError {
                backend: "wasapi".to_string(),
                operation: "register_endpoint_notification_callback".to_string(),
                message: format!(
                    "RegisterEndpointNotificationCallback failed (HRESULT: {:?})",
                    hr
                ),
                context: Some(wasapi_backend_context_err(&hr)),
            }
        })?;
        Ok(Self { enumerator, client })
    }
}

impl Drop for EndpointRegistration {
    fn drop(&mut self) {
        // SAFETY: `self.client` is the pointer registered with `self.enumerator`
        // in `register`; both are live and owned by this value. Best-effort: a
        // failed unregister must not panic in `Drop`.
        let _ = unsafe {
            self.enumerator
                .UnregisterEndpointNotificationCallback(&self.client)
        };
    }
}

/// Body of the `rsac-wasapi-device-watch` helper thread: the one thread on
/// which a watcher's COM objects are created, used and released.
///
/// Reports the registration outcome on `ready`, a zero-capacity channel whose
/// `send` returns `Ok` only once `watch()` has taken the report. When `watch()`
/// has stopped waiting, the helper releases everything without ever calling
/// `on_event`. Otherwise it delivers until `shutdown` is set, checking the flag
/// before every handler call.
fn device_watch_thread_main(
    ready: mpsc::SyncSender<AudioResult<()>>,
    shutdown: Arc<AtomicBool>,
    mut on_event: DeviceEventHandler,
) {
    // Declared first, so it drops last: every interface below is released before
    // COM is uninitialised on this thread.
    let _apartment = match MtaApartment::enter() {
        Ok(apartment) => apartment,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let (tx, rx) = mpsc::sync_channel::<WatchNotice>(DEVICE_EVENT_CHANNEL_BOUND);
    // Drops before `rx` and `_apartment`: unregister, then release.
    let registration = match EndpointRegistration::register(tx) {
        Ok(registration) => registration,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    if ready.send(Ok(())).is_err() {
        return;
    }
    drop(ready);

    while !shutdown.load(Ordering::Acquire) {
        let notice = match rx.recv_timeout(NOTIFY_THREAD_POLL_INTERVAL) {
            Ok(notice) => notice,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let event = match notice {
            WatchNotice::Event(event) => event,
            WatchNotice::Added(id) => {
                let (name, kind) = resolve_added_device(&registration.enumerator, &id.0);
                DeviceEvent::DeviceAdded { id, name, kind }
            }
        };
        // A teardown that began during the wait sees no further handler call.
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        on_event(event);
    }
}

/// Information about an active audio session associated with an application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationAudioSessionInfo {
    /// The process ID of the application owning the audio session.
    pub process_id: u32,
    /// The display name of the audio session. This is often the application name.
    pub display_name: String,
    /// A unique identifier for the audio session.
    pub session_identifier: String,
    /// The full path to the executable file of the application owning the session.
    /// This can be `None` if the path cannot be retrieved (e.g., due to permissions).
    pub executable_path: Option<String>,
}

/// Convert WASAPI bits-per-sample / valid-bits to rsac [`SampleFormat`].
fn wasapi_bits_to_sample_format(
    bits_per_sample: u16,
    valid_bits_per_sample: u16,
) -> crate::core::config::SampleFormat {
    use crate::core::config::SampleFormat;
    match (bits_per_sample, valid_bits_per_sample) {
        (32, 32) => SampleFormat::F32, // Could be Int32, but F32 is our default
        (32, 24) => SampleFormat::I24,
        (16, 16) => SampleFormat::I16,
        (32, _) => SampleFormat::F32,
        _ => SampleFormat::F32, // Fallback
    }
}

/// Resolves a process name from its PID by opening the process handle and querying
/// its module filename.
///
/// Returns the executable filename without the `.exe` extension (e.g. `"firefox"`),
/// or `None` if the process cannot be opened or the name cannot be retrieved.
fn get_process_name_by_pid(pid: u32) -> Option<String> {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::ProcessStatus::K32GetModuleFileNameExW;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        // Wrap in RAII guard so the handle is always closed, even on early return.
        let _guard = ProcessHandle::new(Some(handle));

        let mut name_buf = [0u16; 260];
        let len = K32GetModuleFileNameExW(
            Some(handle),
            Some(HMODULE(std::ptr::null_mut())),
            &mut name_buf,
        );
        if len > 0 {
            let path = String::from_utf16_lossy(&name_buf[..len as usize]);
            let filename = path.rsplit('\\').next().unwrap_or(&path);
            let name = filename.strip_suffix(".exe").unwrap_or(filename);
            Some(name.to_string())
        } else {
            None
        }
    }
}

/// Extracts a human-readable name from a WASAPI session identifier string.
///
/// Session identifiers typically look like:
/// `{executable_path}|{session_guid}|{device_id}`
///
/// This function attempts to extract the executable name from the path component.
/// Returns an empty string if no meaningful name can be extracted.
fn parse_session_identifier(session_id: &str) -> String {
    let parts: Vec<&str> = session_id.split('|').collect();
    if let Some(name_part) = parts.first() {
        if let Some(exe_name) = name_part.split('\\').next_back() {
            let name = exe_name.strip_suffix(".exe").unwrap_or(exe_name);
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }
    String::new()
}

/// Enumerates all active application audio sessions on the system.
///
/// This function queries WASAPI for audio sessions that are currently active and
/// associated with a running process. It attempts to retrieve identifying information
/// for each session, including its process ID, display name, session identifier,
/// and the executable path of the owning application.
///
/// Sessions with a process ID of 0 (often system sounds or non-application sessions)
/// are typically filtered out.
///
/// Works on any thread, whatever its COM apartment: it pins the MTA without an
/// apartment check and creates, uses and releases every COM object within the
/// call, on the calling thread.
///
/// # Returns
///
/// * `Ok(Vec<ApplicationAudioSessionInfo>)` - A vector of structs, each containing
///   information about an active application audio session.
/// * `Err(AudioError)` - If an error occurs during COM initialization, device enumeration,
///   session enumeration, or while retrieving session details.
///
/// # Example
///
/// ```no_run
/// # use rsac::audio::windows::enumerate_application_audio_sessions;
/// # use rsac::core::error::AudioResult;
/// fn main() -> AudioResult<()> {
///     match enumerate_application_audio_sessions() {
///         Ok(sessions) => {
///             if sessions.is_empty() {
///                 println!("No active application audio sessions found.");
///             } else {
///                 println!("Active application audio sessions:");
///                 for session in sessions {
///                     println!(
///                         "  PID: {}, Name: \"{}\", Path: {:?}",
///                         session.process_id,
///                         session.display_name,
///                         session.executable_path.as_deref().unwrap_or("N/A")
///                     );
///                 }
///             }
///         }
///         Err(e) => {
///             eprintln!("Error enumerating audio sessions: {}", e);
///         }
///     }
///     Ok(())
/// }
/// ```
pub fn enumerate_application_audio_sessions() -> AudioResult<Vec<ApplicationAudioSessionInfo>> {
    use windows::Win32::Foundation::{HMODULE, INVALID_HANDLE_VALUE};
    use windows::Win32::Media::Audio::{
        AudioSessionStateActive, IAudioSessionControl, IAudioSessionControl2,
        IAudioSessionEnumerator, IAudioSessionManager2,
    };
    use windows::Win32::System::ProcessStatus::K32GetModuleFileNameExW;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    // Declared before the interfaces below, so they are released first.
    let _pin = MtaUsagePin::new()?;

    let mut sessions_info: Vec<ApplicationAudioSessionInfo> = Vec::new();

    unsafe {
        let device_enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|hr| {
                AudioError::BackendError {
                    backend: "wasapi".to_string(),
                    operation: "enumerate_audio_sessions".to_string(),
                    message: format!(
                        "CoCreateInstance(MMDeviceEnumerator) failed (HRESULT: {:?})",
                        hr
                    ),
                    context: Some(wasapi_backend_context_err(&hr)),
                }
            })?;

        let default_device: IMMDevice = device_enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|hr| {
                if hr.code() == E_NOTFOUND {
                    AudioError::DeviceNotFound {
                        device_id: "default_render".to_string(),
                    }
                } else {
                    AudioError::BackendError {
                        backend: "wasapi".to_string(),
                        operation: "enumerate_audio_sessions".to_string(),
                        message: format!("GetDefaultAudioEndpoint failed (HRESULT: {:?})", hr),
                        context: Some(wasapi_backend_context_err(&hr)),
                    }
                }
            })?;

        let session_manager: IAudioSessionManager2 = default_device
            .Activate(CLSCTX_ALL, None)
            .map_err(|hr| AudioError::BackendError {
                backend: "wasapi".to_string(),
                operation: "enumerate_audio_sessions".to_string(),
                message: format!(
                    "IMMDevice::Activate(IAudioSessionManager2) failed (HRESULT: {:?})",
                    hr
                ),
                context: Some(wasapi_backend_context_err(&hr)),
            })?;

        let session_enumerator: IAudioSessionEnumerator = session_manager
            .GetSessionEnumerator()
            .map_err(|hr| AudioError::BackendError {
                backend: "wasapi".to_string(),
                operation: "enumerate_audio_sessions".to_string(),
                message: format!(
                    "IAudioSessionManager2::GetSessionEnumerator failed (HRESULT: {:?})",
                    hr
                ),
                context: Some(wasapi_backend_context_err(&hr)),
            })?;

        let count = session_enumerator
            .GetCount()
            .map_err(|hr| AudioError::BackendError {
                backend: "wasapi".to_string(),
                operation: "enumerate_audio_sessions".to_string(),
                message: format!(
                    "IAudioSessionEnumerator::GetCount failed (HRESULT: {:?})",
                    hr
                ),
                context: Some(wasapi_backend_context_err(&hr)),
            })?;

        for i in 0..count {
            // Per-session errors must NOT abort the whole enumeration — a single
            // transient COM failure on one session would otherwise drop every
            // discovered app (audit M9). Skip the offending session and continue.
            let session_control: IAudioSessionControl = match session_enumerator.GetSession(i) {
                Ok(sc) => sc,
                Err(hr) => {
                    log::warn!(
                        "wasapi: IAudioSessionEnumerator::GetSession({}) failed, skipping (HRESULT: {:?})",
                        i,
                        hr
                    );
                    continue;
                }
            };

            // Check session state — only include active sessions
            let state = match session_control.GetState() {
                Ok(s) => s,
                Err(hr) => {
                    log::warn!(
                        "wasapi: IAudioSessionControl::GetState for session {} failed, skipping (HRESULT: {:?})",
                        i,
                        hr
                    );
                    continue;
                }
            };
            if state != AudioSessionStateActive {
                continue;
            }

            let session_control2: IAudioSessionControl2 = match session_control.cast() {
                Ok(sc2) => sc2,
                Err(hr) => {
                    // Skip if IAudioSessionControl2 is not available for this session
                    log::warn!(
                        "wasapi: could not cast IAudioSessionControl to IAudioSessionControl2 for session {}, skipping: {:?}",
                        i, hr
                    );
                    continue;
                }
            };

            let pid = match session_control2.GetProcessId() {
                Ok(p) => p,
                Err(hr) => {
                    log::warn!(
                        "wasapi: IAudioSessionControl2::GetProcessId for session {} failed, skipping (HRESULT: {:?})",
                        i,
                        hr
                    );
                    continue;
                }
            };

            if pid == 0 {
                // Skip system sounds or non-application sessions
                continue;
            }

            // Retrieve the session identifier (used as fallback for display name)
            let session_identifier_pwstr = session_control2
                .GetSessionIdentifier()
                .unwrap_or(PWSTR::null());
            let session_id_str = if !session_identifier_pwstr.is_null() {
                let si = WindowsAudioDevice::pwstr_to_string(session_identifier_pwstr)
                    .unwrap_or_else(|_| String::new());
                CoTaskMemFree(Some(session_identifier_pwstr.as_ptr().cast()));
                si
            } else {
                String::new()
            };

            // Better display name resolution:
            // 1. Try GetDisplayName() first
            // 2. Try to get the process executable name via PID
            // 3. Fall back to session identifier parsing
            let display_name = {
                let raw_name = {
                    let display_name_pwstr =
                        session_control2.GetDisplayName().unwrap_or(PWSTR::null());
                    if !display_name_pwstr.is_null() {
                        let dn = WindowsAudioDevice::pwstr_to_string(display_name_pwstr)
                            .unwrap_or_else(|_| String::new());
                        CoTaskMemFree(Some(display_name_pwstr.as_ptr().cast()));
                        dn
                    } else {
                        String::new()
                    }
                };

                if !raw_name.is_empty() {
                    raw_name
                } else {
                    // Resolve executable name from PID for a cleaner display name
                    get_process_name_by_pid(pid).unwrap_or_else(|| {
                        // Last resort: parse session identifier
                        let parsed = parse_session_identifier(&session_id_str);
                        if !parsed.is_empty() {
                            parsed
                        } else {
                            format!("PID: {}", pid)
                        }
                    })
                }
            };

            // Retrieve the executable path for the process
            let mut executable_path: Option<String> = None;
            let process_handle_result = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid);

            match process_handle_result {
                Ok(process_handle) if process_handle != INVALID_HANDLE_VALUE => {
                    // RAII guard ensures handle is closed even on early exit
                    let _guard = ProcessHandle::new(Some(process_handle));
                    let mut path_buf: [u16; 1024] = [0; 1024];
                    // Using HMODULE(0) for the main executable module of the process.
                    let len = K32GetModuleFileNameExW(
                        Some(process_handle),
                        Some(HMODULE(std::ptr::null_mut())),
                        &mut path_buf,
                    );
                    if len > 0 {
                        executable_path = Some(String::from_utf16_lossy(&path_buf[..len as usize]));
                    }
                }
                Ok(_) => { // INVALID_HANDLE_VALUE
                }
                Err(_e) => {
                    // OpenProcess failed
                }
            }

            sessions_info.push(ApplicationAudioSessionInfo {
                process_id: pid,
                display_name,
                session_identifier: session_id_str,
                executable_path,
            });
        }
    }

    Ok(sessions_info)
}

// ── Tests ────────────────────────────────────────────────────────────────
//
// These tests are automatically Windows-only because this file has
// `#![cfg(target_os = "windows")]` at the top. They will never compile
// on Linux or macOS.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::StreamConfig;
    use crate::core::interface::DeviceEnumerator;

    // ── M6: BackendContext os_error_code population ──────────────────

    /// `wasapi_backend_context` carries the raw HRESULT as `os_error_code`.
    #[test]
    fn test_wasapi_backend_context_carries_hresult() {
        // E_NOTFOUND = 0x80070002, which as i32 is negative.
        let ctx = wasapi_backend_context(E_NOTFOUND);
        assert_eq!(ctx.backend_name, "WASAPI");
        assert_eq!(ctx.os_error_code, Some(E_NOTFOUND.0 as i64));
        assert!(
            ctx.os_error_message.is_some(),
            "should attach an OS error message"
        );
    }

    /// `wasapi_backend_context_err` extracts the HRESULT from a
    /// `windows::core::Error` into `os_error_code`.
    #[test]
    fn test_wasapi_backend_context_err_carries_code() {
        let err = windows::core::Error::from(RPC_E_CHANGED_MODE);
        let ctx = wasapi_backend_context_err(&err);
        assert_eq!(ctx.backend_name, "WASAPI");
        assert_eq!(ctx.os_error_code, Some(RPC_E_CHANGED_MODE.0 as i64));
    }

    // ── ComInitializer Tests ─────────────────────────────────────────

    #[test]
    fn test_com_init_accepts_plain_thread() {
        std::thread::spawn(|| {
            let _guard = ComInitializer::new().expect("plain thread should join the MTA");
        })
        .join()
        .expect("plain thread panicked");
    }

    #[test]
    fn test_com_init_rejects_sta() {
        std::thread::spawn(|| {
            // SAFETY: Initialize this fresh thread as STA and balance the
            // successful call on this same thread before any assertion.
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .expect("initialize STA");
            let result = ComInitializer::new();
            // SAFETY: Balances the successful CoInitializeEx above.
            unsafe { CoUninitialize() };

            let err = result.expect_err("an STA must not pass the MTA check");
            let AudioError::BackendError {
                backend,
                operation,
                message,
                context: Some(context),
            } = err
            else {
                panic!("expected a backend error with HRESULT context, got {err:?}");
            };
            assert_eq!(backend, "wasapi");
            assert_eq!(operation, "com_init");
            assert!(message.contains("single-threaded apartment"));
            assert_eq!(context.os_error_code, Some(i64::from(RPC_E_CHANGED_MODE.0)));
        })
        .join()
        .expect("STA thread panicked");
    }

    /// Test that ComInitializer is Debug-printable.
    #[test]
    fn test_com_initializer_debug() {
        let com = ComInitializer::new().expect("COM init failed");
        let dbg = format!("{:?}", com);
        assert!(dbg.contains("ComInitializer"));
    }

    // ── WindowsDeviceEnumerator Tests ────────────────────────────────

    /// Test WindowsDeviceEnumerator creation (COM + IMMDeviceEnumerator).
    #[test]
    fn test_windows_device_enumerator_creation() {
        let result = WindowsDeviceEnumerator::new();
        assert!(
            result.is_ok(),
            "WindowsDeviceEnumerator::new() failed: {:?}",
            result.err()
        );
    }

    /// Test that WindowsDeviceEnumerator is Debug-printable.
    #[test]
    fn test_windows_device_enumerator_debug() {
        let enumerator = WindowsDeviceEnumerator::new().expect("new() failed");
        let dbg = format!("{:?}", enumerator);
        assert!(dbg.contains("WindowsDeviceEnumerator"));
    }

    /// Test enumerate_devices returns a non-empty list on Windows.
    #[test]
    fn test_enumerate_devices_returns_devices() {
        let enumerator = WindowsDeviceEnumerator::new().expect("new() failed");
        let devices = enumerator.enumerate_devices().expect("enumerate failed");
        assert!(
            !devices.is_empty(),
            "Expected at least one audio device on Windows"
        );
    }

    /// Test default_device returns a valid boxed AudioDevice.
    #[test]
    fn test_default_device_returns_device() {
        let enumerator = WindowsDeviceEnumerator::new().expect("new() failed");
        let device = enumerator.default_device().expect("default_device failed");
        // Verify it has a name.
        let name = device.name();
        assert!(!name.is_empty(), "Default device should have a name");
    }

    // ── WindowsAudioDevice Trait Implementation Tests ────────────────

    /// Test AudioDevice::id() returns a non-empty DeviceId for a real device.
    #[test]
    fn test_audio_device_id() {
        let enumerator = WindowsDeviceEnumerator::new().expect("new() failed");
        let device = enumerator.default_device().expect("no default device");
        let id = device.id();
        assert!(
            !id.0.is_empty(),
            "Device ID should not be empty, got: {:?}",
            id
        );
    }

    /// Test AudioDevice::name() returns a non-empty string.
    #[test]
    fn test_audio_device_name() {
        let enumerator = WindowsDeviceEnumerator::new().expect("new() failed");
        let device = enumerator.default_device().expect("no default device");
        let name = device.name();
        assert!(
            !name.is_empty() && name != "Unknown Windows Device",
            "Expected a real device name, got: {:?}",
            name
        );
    }

    /// Test AudioDevice::supported_formats() returns at least one format.
    /// The first format is now the device's mix format (queried from WASAPI),
    /// which may differ from the hardcoded 48kHz/2ch default.
    #[test]
    fn test_audio_device_supported_formats() {
        let enumerator = WindowsDeviceEnumerator::new().expect("new() failed");
        let device = enumerator.default_device().expect("no default device");
        let formats = device.supported_formats();
        assert!(
            !formats.is_empty(),
            "Device should have at least one supported format"
        );
        // The first format should be the device's mix format.
        let first = &formats[0];
        assert!(
            first.sample_rate > 0,
            "Mix format sample rate should be > 0"
        );
        assert!(first.channels > 0, "Mix format channels should be > 0");
        // Most devices support at least 2 formats (mix format + common probes)
        println!(
            "Device supports {} format(s), mix format: {}Hz {}ch {:?}",
            formats.len(),
            first.sample_rate,
            first.channels,
            first.sample_format
        );
    }

    /// Test AudioDevice::kind() (the trait method) on the default device.
    ///
    /// The default device returned by `default_device()` is the default
    /// **render** endpoint (used for loopback capture), so its kind must
    /// resolve to `Output` via `IMMEndpoint::GetDataFlow`.
    #[test]
    fn test_audio_device_kind_default_is_output() {
        let enumerator = WindowsDeviceEnumerator::new().expect("new() failed");
        let device = enumerator.default_device().expect("no default device");

        // Call through the trait object to exercise the AudioDevice::kind path.
        let kind = device
            .kind()
            .expect("kind() should succeed for a real device");
        assert_eq!(
            kind,
            DeviceKind::Output,
            "default loopback device should be a render (Output) endpoint, got {:?}",
            kind
        );
    }

    /// Test that every enumerated device reports a definite, Ok kind.
    ///
    /// `enumerate_devices()` uses `eAll`, so the collection mixes render
    /// (`Output`) and capture (`Input`) endpoints — both must resolve.
    #[test]
    fn test_enumerated_devices_kind_is_resolvable() {
        let enumerator = WindowsDeviceEnumerator::new().expect("new() failed");
        let devices = enumerator.enumerate_devices().expect("enumerate failed");
        for device in &devices {
            let kind = device.kind();
            assert!(
                kind.is_ok(),
                "device {:?} ({}) should report a definite kind, got {:?}",
                device.id(),
                device.name(),
                kind.err()
            );
            // The kind must be one of the two valid variants.
            assert!(matches!(
                kind.unwrap(),
                DeviceKind::Input | DeviceKind::Output
            ));
        }
    }

    /// Test AudioDevice::create_stream() creates a valid CapturingStream.
    /// This is the full BridgeStream wiring test.
    #[test]
    fn test_audio_device_create_stream() {
        let enumerator = WindowsDeviceEnumerator::new().expect("new() failed");
        let device = enumerator.default_device().expect("no default device");

        let config = StreamConfig::default(); // 48kHz, 2ch, F32
        let stream_result = device.create_stream(&config);

        assert!(
            stream_result.is_ok(),
            "create_stream() failed: {:?}",
            stream_result.err()
        );

        let stream = stream_result.unwrap();
        // Stream should be running after creation.
        assert!(stream.is_running(), "Stream should be running after create");
        // Format should match config.
        assert_eq!(stream.format().sample_rate, 48000);
        assert_eq!(stream.format().channels, 2);

        // Stop the stream cleanly.
        let stop_result = stream.stop();
        assert!(
            stop_result.is_ok(),
            "stop() failed: {:?}",
            stop_result.err()
        );
        assert!(
            !stream.is_running(),
            "Stream should not be running after stop"
        );
    }

    // ── WindowsApplicationCapture Tests ──────────────────────────────

    /// Test WindowsApplicationCapture::new() construction.
    #[test]
    fn test_application_capture_creation() {
        let result = WindowsApplicationCapture::new(1234, false);
        assert!(result.is_ok(), "new() failed: {:?}", result.err());
        let capture = result.unwrap();
        assert!(!capture.is_capturing());
    }

    /// Test WindowsApplicationCapture::new() with include_tree.
    #[test]
    fn test_application_capture_with_tree() {
        let result = WindowsApplicationCapture::new(5678, true);
        assert!(result.is_ok());
    }

    /// Test find_process_by_name returns None for non-existent process.
    #[test]
    fn test_find_process_by_name_nonexistent() {
        let result = WindowsApplicationCapture::find_process_by_name(
            "nonexistent_process_xyz_12345.exe",
            false,
        );
        assert!(result.is_none(), "Should not find a nonexistent process");
    }

    /// Test list_audio_processes returns a non-empty list on a running system.
    #[test]
    fn test_list_audio_processes() {
        let processes = WindowsApplicationCapture::list_audio_processes();
        assert!(
            !processes.is_empty(),
            "Expected at least one process on a running system"
        );
    }

    // ── ApplicationAudioSessionInfo Tests ────────────────────────────

    /// Test ApplicationAudioSessionInfo struct construction and equality.
    #[test]
    fn test_application_session_info_construction() {
        let info = ApplicationAudioSessionInfo {
            process_id: 1234,
            display_name: "TestApp".to_string(),
            session_identifier: "session-1".to_string(),
            executable_path: Some("C:\\test\\app.exe".to_string()),
        };
        assert_eq!(info.process_id, 1234);
        assert_eq!(info.display_name, "TestApp");
        assert_eq!(info.session_identifier, "session-1");
        assert_eq!(info.executable_path, Some("C:\\test\\app.exe".to_string()));
    }

    /// Test ApplicationAudioSessionInfo Clone and PartialEq.
    #[test]
    fn test_application_session_info_clone_eq() {
        let info = ApplicationAudioSessionInfo {
            process_id: 42,
            display_name: "App".to_string(),
            session_identifier: "s".to_string(),
            executable_path: None,
        };
        let cloned = info.clone();
        assert_eq!(info, cloned);
    }

    /// Test ApplicationAudioSessionInfo Debug format.
    #[test]
    fn test_application_session_info_debug() {
        let info = ApplicationAudioSessionInfo {
            process_id: 100,
            display_name: "Debug Test".to_string(),
            session_identifier: "sid".to_string(),
            executable_path: None,
        };
        let dbg = format!("{:?}", info);
        assert!(dbg.contains("100"));
        assert!(dbg.contains("Debug Test"));
    }

    // ── enumerate_application_audio_sessions Tests ───────────────────

    /// Test that enumerate_application_audio_sessions doesn't panic and returns Ok.
    #[test]
    fn test_enumerate_audio_sessions() {
        let result = enumerate_application_audio_sessions();
        assert!(
            result.is_ok(),
            "enumerate_application_audio_sessions() failed: {:?}",
            result.err()
        );
        // Result may be empty if no apps are playing audio.
    }

    /// Test that enumerate_application_audio_sessions returns a Vec (even if empty).
    /// This validates the return type and basic contract.
    #[test]
    fn test_enumerate_audio_sessions_returns_vec() {
        let sessions = enumerate_application_audio_sessions().expect("enumeration failed");
        // The Vec may be empty if no apps are actively playing audio.
        // We just verify it's a valid Vec<ApplicationAudioSessionInfo>.
        let _len: usize = sessions.len();
        for session in &sessions {
            // Every returned session should have a non-zero PID
            assert_ne!(
                session.process_id, 0,
                "Sessions with PID 0 should be filtered out"
            );
            // Display name should not be empty (3-tier fallback guarantees this)
            assert!(
                !session.display_name.is_empty(),
                "Display name should not be empty for PID {}",
                session.process_id
            );
        }
    }

    /// Test that calling enumerate_application_audio_sessions twice doesn't panic
    /// (validates COM re-initialization).
    #[test]
    fn test_enumerate_audio_sessions_twice() {
        let _result1 = enumerate_application_audio_sessions();
        let result2 = enumerate_application_audio_sessions();
        assert!(
            result2.is_ok(),
            "Second call to enumerate_application_audio_sessions() failed: {:?}",
            result2.err()
        );
    }

    // ── get_process_name_by_pid Tests ────────────────────────────────

    /// Test that get_process_name_by_pid returns a name for the current process.
    #[test]
    fn test_get_process_name_by_pid_current_process() {
        let current_pid = std::process::id();
        let name = get_process_name_by_pid(current_pid);
        assert!(
            name.is_some(),
            "Should be able to resolve the current process name (PID {})",
            current_pid
        );
        // The test binary name should contain "rsac" or the test runner name
        let name = name.unwrap();
        assert!(
            !name.is_empty(),
            "Process name should not be empty for PID {}",
            current_pid
        );
    }

    /// Test that get_process_name_by_pid returns None for PID 0.
    #[test]
    fn test_get_process_name_by_pid_zero() {
        // PID 0 is the System Idle Process — OpenProcess should fail or return unusable name
        let name = get_process_name_by_pid(0);
        // It's ok if this returns Some (System) or None — just don't panic
        let _ = name;
    }

    /// Test that get_process_name_by_pid returns None for a non-existent PID.
    #[test]
    fn test_get_process_name_by_pid_nonexistent() {
        // Use a very high PID that's unlikely to exist
        let name = get_process_name_by_pid(4_000_000_000);
        assert!(
            name.is_none(),
            "Should return None for a non-existent PID, got: {:?}",
            name
        );
    }

    /// Test that get_process_name_by_pid strips the .exe extension.
    #[test]
    fn test_get_process_name_by_pid_strips_exe() {
        let current_pid = std::process::id();
        if let Some(name) = get_process_name_by_pid(current_pid) {
            assert!(
                !name.ends_with(".exe"),
                "Process name should have .exe stripped, got: {:?}",
                name
            );
        }
    }

    // ── parse_session_identifier Tests ───────────────────────────────

    /// Test parse_session_identifier with a typical WASAPI session ID.
    #[test]
    fn test_parse_session_identifier_typical() {
        let session_id = r"C:\Program Files\Mozilla Firefox\firefox.exe|{guid}|{device}";
        let name = parse_session_identifier(session_id);
        assert_eq!(name, "firefox");
    }

    /// Test parse_session_identifier with a device path format.
    #[test]
    fn test_parse_session_identifier_device_path() {
        let session_id =
            r"\Device\HarddiskVolume8\Users\test\AppData\Local\Discord\app-1.0\Discord.exe|{guid}";
        let name = parse_session_identifier(session_id);
        assert_eq!(name, "Discord");
    }

    /// Test parse_session_identifier with empty string.
    #[test]
    fn test_parse_session_identifier_empty() {
        assert_eq!(parse_session_identifier(""), String::new());
    }

    /// Test parse_session_identifier with no path separators.
    #[test]
    fn test_parse_session_identifier_no_path() {
        let session_id = "someapp.exe|{guid}";
        let name = parse_session_identifier(session_id);
        assert_eq!(name, "someapp");
    }

    /// Test parse_session_identifier with system sounds identifier.
    #[test]
    fn test_parse_session_identifier_system_guid() {
        // System sounds sessions have GUIDs, not paths
        let session_id = "#%b{A9EF3FD9-4240-455E-A925-035F1494B5F7}";
        let name = parse_session_identifier(session_id);
        // This will likely parse to something non-empty (the GUID string)
        // but that's expected — the caller filters these via PID == 0
        let _ = name;
    }

    /// Test parse_session_identifier with pipe-only separator.
    #[test]
    fn test_parse_session_identifier_pipe_only() {
        let session_id = "|{guid}|{device}";
        let name = parse_session_identifier(session_id);
        // First part is empty, so result should be empty
        assert_eq!(name, String::new());
    }

    // ── BridgeStream Integration (full wiring) ──────────────────────

    /// Test that create_stream() produces a CapturingStream that can read audio.
    /// This is a deeper integration test of the full WASAPI → BridgeStream pipeline.
    #[test]
    fn test_create_stream_can_read() {
        let enumerator = WindowsDeviceEnumerator::new().expect("new() failed");
        let device = enumerator.default_device().expect("no default device");

        let config = StreamConfig::default();
        let stream = device.create_stream(&config).expect("create_stream failed");

        // Try a non-blocking read — may or may not have data yet.
        let try_result = stream.try_read_chunk();
        // Should not be an error (stream is Running).
        match try_result {
            Ok(None) => { /* No data yet — fine */ }
            Ok(Some(buf)) => {
                // Got data — verify it has the right format.
                assert_eq!(buf.channels(), 2);
                assert_eq!(buf.sample_rate(), 48000);
            }
            Err(e) => {
                panic!("try_read_chunk() returned unexpected error: {:?}", e);
            }
        }

        // Clean up.
        stream.stop().expect("stop failed");
    }

    // ── Device-change notifications: watch() (M10 — rsac-e360) ───────────

    /// `data_flow_to_kind` maps WASAPI render/capture flows to rsac kinds and
    /// returns `None` for anything else (e.g. `eAll`).
    #[test]
    fn test_data_flow_to_kind_mapping() {
        assert_eq!(data_flow_to_kind(eRender), Some(DeviceKind::Output));
        assert_eq!(data_flow_to_kind(eCapture), Some(DeviceKind::Input));
        assert_eq!(data_flow_to_kind(eAll), None);
    }

    /// `watch()` registers an IMMNotificationClient and returns a DeviceWatcher
    /// over a short live window, then drops it cleanly.
    ///
    /// We cannot deterministically force a real hot-plug / default-device change
    /// in CI, so this asserts the register → live-window → clean-drop lifecycle:
    /// no panic, no hang, and the helper thread is joined on drop. If any
    /// spurious device events do arrive during the window, the handler increments
    /// a counter — exercising the COM-thread → helper-thread → user-FnMut path
    /// without requiring a change to occur.
    #[test]
    fn test_watch_register_and_clean_drop() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let enumerator = WindowsDeviceEnumerator::new().expect("new() failed");

        let count = Arc::new(AtomicUsize::new(0));
        let count_cb = count.clone();

        let watcher = enumerator
            .watch(Box::new(move |_event: DeviceEvent| {
                // Runs on the helper thread, never the COM notification thread.
                count_cb.fetch_add(1, Ordering::Relaxed);
            }))
            .expect("watch() should register a notification client on Windows");

        // Hold the subscription briefly so the COM client is live.
        std::thread::sleep(std::time::Duration::from_millis(200));

        // Drop unregisters the COM callback and joins the helper thread. This
        // must return promptly (no hang) and must not panic.
        drop(watcher);

        // After drop, the helper thread is joined; the handler will not run again.
        // The observed count is environment-dependent (usually 0 in headless CI),
        // so we only assert the lifecycle completed without deadlock.
        let _observed = count.load(Ordering::Relaxed);
    }

    /// Registering and dropping multiple watchers in sequence must each tear down
    /// cleanly (no accumulated COM registrations, no leaked threads).
    #[test]
    fn test_watch_repeated_register_unregister() {
        let enumerator = WindowsDeviceEnumerator::new().expect("new() failed");
        for _ in 0..3 {
            let watcher = enumerator
                .watch(Box::new(|_event: DeviceEvent| {}))
                .expect("watch() should succeed");
            // Immediate drop exercises the unregister+join path back-to-back.
            drop(watcher);
        }
    }

    /// Two concurrent watchers from independent enumerators can coexist and both
    /// drop cleanly — validates that registration/unregistration is per-client.
    #[test]
    fn test_watch_two_concurrent_watchers() {
        let enum_a = WindowsDeviceEnumerator::new().expect("new() failed");
        let enum_b = WindowsDeviceEnumerator::new().expect("new() failed");

        let w_a = enum_a
            .watch(Box::new(|_event: DeviceEvent| {}))
            .expect("watch A failed");
        let w_b = enum_b
            .watch(Box::new(|_event: DeviceEvent| {}))
            .expect("watch B failed");

        std::thread::sleep(std::time::Duration::from_millis(50));

        // Drop in reverse order; both must tear down without hang/panic.
        drop(w_b);
        drop(w_a);
    }

    /// Dropping the `WindowsDeviceEnumerator` while its `DeviceWatcher` is still
    /// alive must remain sound. This is the `get_enumerator()?.watch(h)?`
    /// pattern: `watch(&self)` is dispatched on a borrowed enumerator, so the
    /// enumerator (and its MTA pin) drops as soon as `watch` returns. The
    /// watcher's COM objects live on its helper thread, which keeps its own MTA
    /// initialised until it has unregistered and released them, so the
    /// enumerator's lifetime does not bound the watcher's.
    #[test]
    fn test_watch_outlives_enumerator_drop() {
        let watcher = {
            let enumerator = WindowsDeviceEnumerator::new().expect("new() failed");
            let w = enumerator
                .watch(Box::new(|_event: DeviceEvent| {}))
                .expect("watch() should succeed");
            // `enumerator` drops HERE, at end of block; the watcher does not
            // depend on it.
            w
        };

        // The COM client is still registered and the apartment still live.
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Clean teardown after the enumerator is already gone: no panic, no hang,
        // no use-after-CoUninitialize.
        drop(watcher);
    }

    /// The MTA usage pin makes no apartment check: it succeeds on an STA thread
    /// and leaves that thread in its STA.
    #[test]
    fn test_mta_usage_pin_keeps_sta_apartment() {
        std::thread::spawn(|| {
            // SAFETY: Initialize this fresh thread as STA and balance the
            // successful call on this same thread before any assertion.
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .expect("initialize STA");
            let pin = MtaUsagePin::new();
            let mut apartment = APTTYPE_CURRENT;
            let mut qualifier = APTTYPEQUALIFIER_NONE;
            // SAFETY: Both out-pointers refer to live, writable stack variables.
            let query = unsafe { CoGetApartmentType(&mut apartment, &mut qualifier) };
            let pinned = pin.is_ok();
            drop(pin);
            // SAFETY: Balances the successful CoInitializeEx above.
            unsafe { CoUninitialize() };

            assert!(pinned, "the MTA usage pin must not reject an STA caller");
            query.expect("CoGetApartmentType");
            assert!(
                matches!(apartment, APTTYPE_STA | APTTYPE_MAINSTA),
                "the MTA usage pin must leave the thread in its STA, got {apartment:?}"
            );
        })
        .join()
        .expect("STA thread panicked");
    }

    // ── #105 structural guards (no audio endpoint needed) ────────────

    // `WindowsAudioDevice` and `WindowsDeviceEnumerator` are `Send + Sync`
    // through the auto traits; no `unsafe impl` names them (see the allowlist).
    const _: () = {
        const fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<WindowsAudioDevice>();
        assert_send_sync::<WindowsDeviceEnumerator>();
    };

    /// Every `.rs` file under `src/audio/windows/`, embedded at compile time.
    const BACKEND_SOURCES: &[(&str, &str)] = &[
        ("apartment_tests.rs", include_str!("apartment_tests.rs")),
        ("mod.rs", include_str!("mod.rs")),
        ("thread.rs", include_str!("thread.rs")),
        ("wasapi.rs", include_str!("wasapi.rs")),
    ];

    /// The only `unsafe impl` items allowed under `src/audio/windows/` (#105),
    /// as `(file, item line with whitespace normalised)`.
    const ALLOWED_UNSAFE_IMPLS: &[(&str, &str)] = &[
        // The MTA usage pin, the only holder of a `CO_MTA_USAGE_COOKIE`.
        ("wasapi.rs", "unsafe impl Send for MtaUsagePin {}"),
        ("wasapi.rs", "unsafe impl Sync for MtaUsagePin {}"),
        // Deprecated legacy type, left unchanged; it is removed, with this
        // entry, at the 0.5.0 cutover.
        (
            "wasapi.rs",
            "unsafe impl Send for WindowsApplicationCapture {}",
        ),
    ];

    /// Each `unsafe impl` item in `source`: its first line, whitespace
    /// normalised, and whether a `// SAFETY:` line is in the comment block
    /// directly above it.
    fn scan_unsafe_impls(source: &str) -> Vec<(String, bool)> {
        let lines: Vec<&str> = source.lines().collect();
        let mut items = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            let is_unsafe_impl = tokens.first() == Some(&"unsafe")
                && tokens
                    .get(1)
                    .is_some_and(|token| *token == "impl" || token.starts_with("impl<"));
            if !is_unsafe_impl {
                continue;
            }
            let has_safety = lines[..index]
                .iter()
                .rev()
                .map(|line| line.trim_start())
                .take_while(|line| line.starts_with("//"))
                .any(|line| line.starts_with("// SAFETY:"));
            items.push((tokens.join(" "), has_safety));
        }
        items
    }

    /// The allowlist violations in `source`, read from `file`: items not on
    /// the allowlist, and allowed items without a `// SAFETY:` comment.
    fn unsafe_impl_violations(file: &str, source: &str) -> Vec<String> {
        scan_unsafe_impls(source)
            .into_iter()
            .filter_map(|(item, has_safety)| {
                let allowed = ALLOWED_UNSAFE_IMPLS
                    .iter()
                    .any(|&(allowed_file, allowed_item)| {
                        allowed_file == file && allowed_item == item
                    });
                if !allowed {
                    Some(format!("{file}: `{item}` is not on the #105 allowlist"))
                } else if !has_safety {
                    Some(format!(
                        "{file}: `{item}` lacks a `// SAFETY:` comment directly above it"
                    ))
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn unsafe_impl_allowlist() {
        // Negative controls: the scanner flags an item outside the allowlist even
        // with a SAFETY comment, and an allowed item without one.
        let foreign = "struct Foo;\n// SAFETY: synthetic.\nunsafe impl Send for Foo {}\n";
        assert_eq!(
            unsafe_impl_violations("wasapi.rs", foreign),
            vec!["wasapi.rs: `unsafe impl Send for Foo {}` is not on the #105 allowlist"],
        );
        let uncommented = "struct Foo;\nunsafe impl Send for MtaUsagePin {}\n";
        assert_eq!(
            unsafe_impl_violations("wasapi.rs", uncommented),
            vec![
                "wasapi.rs: `unsafe impl Send for MtaUsagePin {}` lacks a `// SAFETY:` \
                 comment directly above it"
            ],
        );

        // The embedded sources are exactly the `.rs` files in the directory.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/audio/windows");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
            .map(|entry| {
                entry
                    .expect("read directory entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.ends_with(".rs"))
            .collect();
        on_disk.sort();
        let mut embedded: Vec<String> = BACKEND_SOURCES
            .iter()
            .map(|(name, _)| name.to_string())
            .collect();
        embedded.sort();
        assert_eq!(
            on_disk, embedded,
            "BACKEND_SOURCES must embed every .rs file under src/audio/windows/"
        );

        let mut violations = Vec::new();
        let mut found = Vec::new();
        for &(file, source) in BACKEND_SOURCES {
            violations.extend(unsafe_impl_violations(file, source));
            found.extend(
                scan_unsafe_impls(source)
                    .into_iter()
                    .map(|(item, _)| (file.to_string(), item)),
            );
        }
        assert!(
            violations.is_empty(),
            "unsafe impl allowlist violations: {violations:#?}"
        );
        let mut expected: Vec<(String, String)> = ALLOWED_UNSAFE_IMPLS
            .iter()
            .map(|&(file, item)| (file.to_string(), item.to_string()))
            .collect();
        found.sort();
        expected.sort();
        assert_eq!(
            found, expected,
            "every allowlisted item must appear exactly once"
        );
    }
}
