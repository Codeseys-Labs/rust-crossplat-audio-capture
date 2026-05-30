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
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::Duration;

use sysinfo::{ProcessesToUpdate, System};
use wasapi::{self, initialize_mta};

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
const RPC_E_CHANGED_MODE: windows::core::HRESULT = windows::core::HRESULT(-2147417850i32); // 0x80010106
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
    // Use wasapi-rs AudioClient for simpler implementation
    audio_client: Option<wasapi::AudioClient>,
    // Shared flag to signal capture loop to stop
    should_stop: Arc<AtomicBool>,
}

impl WindowsApplicationCapture {
    /// Create a new application capture instance for the specified process
    ///
    /// # Arguments
    /// * `process_id` - PID of the target process
    /// * `include_tree` - Whether to include child processes in capture
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
            should_stop: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Initialize the process loopback client using wasapi-rs
    ///
    /// This uses wasapi-rs AudioClient::new_application_loopback_client for simplicity
    pub fn initialize(&mut self) -> std::result::Result<(), Box<dyn std::error::Error>> {
        // Initialize COM using wasapi-rs (0.22.0: returns HRESULT, not Result).
        // S_OK (0) and S_FALSE (1) are both success. Only fail on actual errors.
        let hr = initialize_mta();
        if hr.is_err() && hr != windows::core::HRESULT(1) {
            return Err(format!("COM initialization failed: HRESULT {:?}", hr).into());
        }

        // Create wasapi-rs AudioClient for application loopback
        let mut audio_client = wasapi::AudioClient::new_application_loopback_client(
            self.process_id,
            self.include_tree,
        )?;

        // Initialize the audio client with a standard format.
        let desired_format =
            wasapi::WaveFormat::new(32, 32, &wasapi::SampleType::Float, 48000, 2, None);
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

        audio_client.initialize_client(&desired_format, &wasapi::Direction::Capture, &mode)?;

        // Store the initialized client
        self.audio_client = Some(audio_client);

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

// Unsafe Send implementations for Windows COM types
// These are safe because we ensure COM is properly initialized and the types
// are only used within the same thread context
unsafe impl Send for WindowsApplicationCapture {}

// Note: Cannot implement Send for external COM types due to orphan rules
// The thread spawn issue will need to be handled differently

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

/// Ensures COM is initialized for the current thread and uninitializes it when dropped.
///
/// This RAII guard should be held by any type that makes COM calls, such as
/// device enumerators or audio streams that interact directly with WASAPI.
#[derive(Debug)]
pub struct ComInitializer;

impl ComInitializer {
    /// Initializes COM for the current thread using `COINIT_MULTITHREADED`.
    ///
    /// Returns `Ok(Self)` on success, or an `AudioError::BackendError`
    /// if COM initialization fails.
    ///
    /// Both `S_OK` and `S_FALSE` are treated as success:
    /// - `S_OK` means COM was freshly initialized.
    /// - `S_FALSE` means COM was already initialized with MTA on this thread.
    ///
    /// In both cases, `CoUninitialize` must be called (handled by `Drop`).
    pub fn new() -> AudioResult<Self> {
        // SAFETY: CoInitializeEx is safe to call. We check the HRESULT.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

        // S_OK (0) = freshly initialized, S_FALSE (1) = already initialized in MTA.
        // Both are success — CoUninitialize must be called for each successful call.
        if hr.is_ok() || hr == windows::core::HRESULT(1) {
            Ok(ComInitializer)
        } else if hr == RPC_E_CHANGED_MODE {
            // COM was already initialized with a different concurrency model (STA).
            Err(AudioError::BackendError {
                backend: "wasapi".to_string(),
                operation: "com_init".to_string(),
                message: format!(
                    "Already initialized with a different concurrency model (HRESULT: {:?})",
                    hr
                ),
                context: Some(wasapi_backend_context(hr)),
            })
        } else {
            Err(AudioError::BackendError {
                backend: "wasapi".to_string(),
                operation: "com_init".to_string(),
                message: format!("COM initialization failed (HRESULT: {:?})", hr),
                context: Some(wasapi_backend_context(hr)),
            })
        }
    }
}

impl Drop for ComInitializer {
    fn drop(&mut self) {
        // SAFETY: CoUninitialize is safe to call if CoInitializeEx was successful.
        // This is ensured by the RAII pattern.
        unsafe { CoUninitialize() };
    }
}

/// Represents a Windows audio device using WASAPI.
///
/// This struct holds an `IMMDevice` instance, which is the core representation
/// of an audio endpoint in WASAPI.
#[derive(Debug)]
pub struct WindowsAudioDevice {
    device: IMMDevice,
    #[allow(dead_code)] // RAII guard: held to keep COM initialized for this device's lifetime
    com_initializer: Arc<ComInitializer>,
}

impl WindowsAudioDevice {
    /// Creates a new `WindowsAudioDevice` from an `IMMDevice` and a `ComInitializer`.
    fn new(device: IMMDevice, com_initializer: Arc<ComInitializer>) -> Self {
        Self {
            device,
            com_initializer,
        }
    }
}

// SAFETY: WindowsAudioDevice wraps COM interfaces (IMMDevice) that are created
// in a Multi-Threaded Apartment (MTA) context via CoInitializeEx(COINIT_MULTITHREADED).
// In MTA, COM objects are free-threaded and can be safely used from any thread.
// The Arc<ComInitializer> ensures COM remains initialized while any device exists.
unsafe impl Send for WindowsAudioDevice {}
unsafe impl Sync for WindowsAudioDevice {}

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
        // Use the IMMDevice::GetId() COM method
        unsafe {
            if let Ok(id_pwstr) = self.device.GetId() {
                if let Ok(id_str) = Self::pwstr_to_string(id_pwstr) {
                    CoTaskMemFree(Some(id_pwstr.as_ptr().cast()));
                    return DeviceId(id_str);
                }
                CoTaskMemFree(Some(id_pwstr.as_ptr().cast()));
            }
            DeviceId("unknown-device".to_string())
        }
    }

    fn name(&self) -> String {
        self.get_name_internal()
            .unwrap_or_else(|_| "Unknown Windows Device".to_string())
    }

    fn is_default(&self) -> bool {
        // Default detection requires enumerator context — return false.
        // The enumerator's default_device() handles default selection.
        false
    }

    fn supported_formats(&self) -> Vec<AudioFormat> {
        self.query_supported_formats_internal()
            .unwrap_or_else(|_| vec![AudioFormat::default()])
    }

    /// Resolves the endpoint direction via `IMMEndpoint::GetDataFlow`.
    ///
    /// Delegates to the inherent `kind_internal`
    /// helper so the fallible `IMMEndpoint` probe is shared with any
    /// crate-internal callers. `eRender` → [`DeviceKind::Output`],
    /// `eCapture` → [`DeviceKind::Input`]. Stays fallible (resolves the M1
    /// api-fit clash) rather than collapsing to a default.
    fn kind(&self) -> AudioResult<DeviceKind> {
        self.kind_internal()
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
    /// Query the device's supported audio formats via WASAPI format negotiation.
    ///
    /// This queries the device's mix format (its preferred shared-mode format),
    /// then probes common sample rates to build a list of supported formats.
    /// The mix format is always first in the returned list.
    ///
    /// Uses `IAudioClient::IsFormatSupported` in shared mode, matching the
    /// pattern from camilladsp's `get_supported_wave_format()`.
    fn query_supported_formats_internal(&self) -> AudioResult<Vec<AudioFormat>> {
        let mut formats = Vec::new();

        // Get a wasapi-rs AudioClient through the wasapi-rs Device wrapper,
        // which gives us access to get_mixformat() and is_supported().
        let audio_client: wasapi::AudioClient = unsafe {
            let enumerator =
                wasapi::DeviceEnumerator::new().map_err(|e| AudioError::BackendError {
                    backend: "wasapi".to_string(),
                    operation: "query_supported_formats".to_string(),
                    message: format!("Failed to create DeviceEnumerator: {}", e),
                    context: None,
                })?;

            // Get our device's ID to find the matching wasapi-rs Device
            let id_pwstr = self.device.GetId().map_err(|hr| AudioError::BackendError {
                backend: "wasapi".to_string(),
                operation: "query_supported_formats".to_string(),
                message: format!("Failed to get device ID (HRESULT: {:?})", hr),
                context: Some(wasapi_backend_context_err(&hr)),
            })?;
            let device_id = if !id_pwstr.is_null() {
                let id = Self::pwstr_to_string(id_pwstr).unwrap_or_default();
                CoTaskMemFree(Some(id_pwstr.as_ptr().cast()));
                id
            } else {
                String::new()
            };

            let wasapi_device = find_wasapi_device_by_id(&enumerator, &device_id)?;
            wasapi_device
                .get_iaudioclient()
                .map_err(|e| AudioError::BackendError {
                    backend: "wasapi".to_string(),
                    operation: "query_supported_formats".to_string(),
                    message: format!("Failed to get IAudioClient: {}", e),
                    context: None,
                })?
        };

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
            for &(bits, valid_bits, ref sample_type) in probe_formats {
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
                match audio_client.is_supported(&wave_fmt, &wasapi::ShareMode::Shared) {
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

    /// Helper method that returns Result for get_name implementation
    fn get_name_internal(&self) -> AudioResult<String> {
        unsafe {
            let property_store: IPropertyStore =
                self.device.OpenPropertyStore(STGM_READ).map_err(|hr| {
                    AudioError::BackendError {
                        backend: "wasapi".to_string(),
                        operation: "get_device_name".to_string(),
                        message: format!("IMMDevice::OpenPropertyStore failed (HRESULT: {:?})", hr),
                        context: Some(wasapi_backend_context_err(&hr)),
                    }
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
                Self::pwstr_to_string(name_pwstr).unwrap_or_else(|_| "Unknown Device".to_string())
            } else {
                "Unknown Device".to_string()
            };
            // Note: prop_variant is returned by value, no need to clear manually
            Ok(name)
        }
    }
}

impl WindowsAudioDevice {
    /// Determines the kind of device (Input or Output) via `IMMEndpoint`.
    ///
    /// Backing implementation for the [`AudioDevice::kind`] trait method. Kept
    /// as a `pub(crate)` inherent helper (mirroring `get_name_internal` /
    /// `query_supported_formats_internal`) so the trait method can delegate
    /// without inherent-vs-trait call ambiguity.
    pub(crate) fn kind_internal(&self) -> AudioResult<DeviceKind> {
        // QueryInterface for IMMEndpoint
        let endpoint: IMMEndpoint = self.device.cast().map_err(|hr| AudioError::BackendError {
            backend: "wasapi".to_string(),
            operation: "get_device_kind".to_string(),
            message: format!(
                "Failed to cast IMMDevice to IMMEndpoint (HRESULT: {:?})",
                hr
            ),
            context: Some(wasapi_backend_context_err(&hr)),
        })?;

        unsafe {
            let data_flow_val = endpoint
                .GetDataFlow()
                .map_err(|hr| AudioError::BackendError {
                    backend: "wasapi".to_string(),
                    operation: "get_device_kind".to_string(),
                    message: format!("IMMEndpoint::GetDataFlow failed (HRESULT: {:?})", hr),
                    context: Some(wasapi_backend_context_err(&hr)),
                })?;

            // EDataFlow is a newtype struct (e.g. EDataFlow(0)), not a Rust enum.
            // Using `match` with `eRender` as a pattern would create a variable binding,
            // not compare against the constant. We must use `if` chains instead.
            if data_flow_val == eRender {
                Ok(DeviceKind::Output)
            } else if data_flow_val == eCapture {
                Ok(DeviceKind::Input)
            } else {
                Err(AudioError::BackendError {
                    backend: "wasapi".to_string(),
                    operation: "get_device_kind".to_string(),
                    message: format!("Unknown data flow value: {:?}", data_flow_val),
                    context: None,
                })
            }
        }
    }
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
// Hand-off pattern (matches the canonical Windows/macOS design in the M10
// surface and cpal/CamillaDSP): the COM callback only *translates* the event and
// pushes a [`DeviceEvent`] into a bounded channel — alloc/lock on this thread is
// permitted (it is NOT the real-time audio callback thread), but it never runs
// user code. A dedicated helper thread owned by the returned [`DeviceWatcher`]
// pops events and invokes the user handler. Teardown unregisters the COM client,
// signals the helper thread, and joins it — no leak, no hang.

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

/// Resolve a device's friendly name and kind from its endpoint id, using the
/// enumerator that is already valid on the COM notification thread.
///
/// Best-effort: returns `("", None)`-ish fallbacks rather than failing, because a
/// `DeviceAdded` event must still be delivered even if the brand-new endpoint
/// cannot be fully probed yet (it may still be initializing). Runs on the COM
/// notification thread, which already has a valid MTA apartment — no extra COM
/// init needed.
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

    // Friendly name via the property store (mirrors get_name_internal).
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

/// COM notification client that forwards WASAPI endpoint changes onto a channel.
///
/// Registered with `IMMDeviceEnumerator::RegisterEndpointNotificationCallback`.
/// Its methods run on the MMDevice system notification thread; they only
/// translate and enqueue events (never run user code) — see the module comment
/// above for the deadlock rationale.
#[implement(IMMNotificationClient)]
struct WatcherNotificationClient {
    /// Bounded sender to the helper thread. `try_send` so a stalled consumer can
    /// never block the COM notification thread.
    tx: mpsc::SyncSender<DeviceEvent>,
    /// Enumerator clone used to resolve name/kind for `OnDeviceAdded`. Valid on
    /// the COM notification thread (free-threaded MTA object).
    enumerator: IMMDeviceEnumerator,
}

impl WatcherNotificationClient {
    /// Enqueue an event without ever blocking the COM notification thread.
    /// A full buffer means the consumer is behind; we drop the event (a device
    /// change is idempotent — the consumer can re-enumerate) rather than stall
    /// COM. A disconnected receiver means the watcher is being torn down.
    fn emit(&self, event: DeviceEvent) {
        match self.tx.try_send(event) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(ev)) => {
                log::warn!(
                    "wasapi: device-event channel full, dropping notification: {:?}",
                    ev
                );
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                // Helper thread gone (teardown in progress) — nothing to do.
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
        self.emit(DeviceEvent::StateChanged {
            id: DeviceId(id),
            available,
        });
        Ok(())
    }

    fn OnDeviceAdded(&self, pwstrdeviceid: &PCWSTR) -> windows::core::Result<()> {
        let id = unsafe { pwstrdeviceid.to_string() }.unwrap_or_default();
        let (name, kind) = resolve_added_device(&self.enumerator, &id);
        self.emit(DeviceEvent::DeviceAdded {
            id: DeviceId(id),
            name,
            kind,
        });
        Ok(())
    }

    fn OnDeviceRemoved(&self, pwstrdeviceid: &PCWSTR) -> windows::core::Result<()> {
        let id = unsafe { pwstrdeviceid.to_string() }.unwrap_or_default();
        self.emit(DeviceEvent::DeviceRemoved { id: DeviceId(id) });
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
        self.emit(DeviceEvent::DefaultChanged {
            id: DeviceId(id),
            kind,
        });
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
#[derive(Debug)]
pub struct WindowsDeviceEnumerator {
    com_initializer: Arc<ComInitializer>,
    enumerator: IMMDeviceEnumerator,
}

impl WindowsDeviceEnumerator {
    /// Creates a new Windows device enumerator.
    ///
    /// This will initialize COM for the lifetime of the enumerator and
    /// create an `IMMDeviceEnumerator` instance.
    pub fn new() -> AudioResult<Self> {
        let com_initializer = Arc::new(ComInitializer::new()?);
        // SAFETY: CoCreateInstance is called to create a COM object.
        // The HRESULT is checked for errors.
        let enumerator: IMMDeviceEnumerator = unsafe {
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
        }
        .map_err(|hr| AudioError::BackendError {
            backend: "wasapi".to_string(),
            operation: "create_device_enumerator".to_string(),
            message: format!("Failed to create IMMDeviceEnumerator (HRESULT: {:?})", hr),
            context: Some(wasapi_backend_context_err(&hr)),
        })?;

        Ok(Self {
            com_initializer,
            enumerator,
        })
    }
}

// SAFETY: WindowsDeviceEnumerator wraps IMMDeviceEnumerator, also created in MTA.
// Same MTA thread-safety reasoning as WindowsAudioDevice.
unsafe impl Send for WindowsDeviceEnumerator {}
unsafe impl Sync for WindowsDeviceEnumerator {}

/// New canonical `DeviceEnumerator` trait implementation for `WindowsDeviceEnumerator`.
///
/// Returns `Box<dyn AudioDevice>` instead of concrete types, matching the
/// platform-agnostic trait contract in `core::interface`.
impl DeviceEnumerator for WindowsDeviceEnumerator {
    fn enumerate_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice>>> {
        // SAFETY: Calling EnumAudioEndpoints on a valid IMMDeviceEnumerator.
        let collection: IMMDeviceCollection = unsafe {
            self.enumerator
                .EnumAudioEndpoints(eAll, DEVICE_STATE_ACTIVE)
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
            let imm_device: IMMDevice =
                unsafe { collection.Item(i) }.map_err(|hr| AudioError::DeviceEnumerationError {
                    reason: format!(
                        "Failed to get device item {} from collection (HRESULT: {:?})",
                        i, hr
                    ),
                    context: Some(wasapi_backend_context_err(&hr)),
                })?;
            devices.push(Box::new(WindowsAudioDevice::new(
                imm_device,
                self.com_initializer.clone(),
            )));
        }
        Ok(devices)
    }

    fn default_device(&self) -> AudioResult<Box<dyn AudioDevice>> {
        // For audio capture, the default render device is most relevant (loopback).
        let data_flow = eRender;

        // SAFETY: Calling GetDefaultAudioEndpoint on a valid IMMDeviceEnumerator.
        match unsafe { self.enumerator.GetDefaultAudioEndpoint(data_flow, eConsole) } {
            Ok(imm_device) => Ok(Box::new(WindowsAudioDevice::new(
                imm_device,
                self.com_initializer.clone(),
            ))),
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
    /// Registers an [`IMMNotificationClient`] via
    /// `IMMDeviceEnumerator::RegisterEndpointNotificationCallback`. The COM
    /// callbacks (run on the MMDevice system notification thread) translate each
    /// change into a [`DeviceEvent`] and push it onto a bounded channel; a
    /// dedicated helper thread owned by the returned [`DeviceWatcher`] pops events
    /// and invokes `on_event`. The user handler therefore **never** runs on the
    /// COM notification thread, avoiding COM re-entrancy / deadlock (the canonical
    /// thread hand-off pattern; see the module-level notes by
    /// `WatcherNotificationClient`, the crate-private COM callback object).
    ///
    /// Dropping the [`DeviceWatcher`] unregisters the COM client, signals the
    /// helper thread, and joins it — no thread leak, no hang. After `drop`
    /// returns, `on_event` is guaranteed not to be called again.
    fn watch(&self, on_event: DeviceEventHandler) -> AudioResult<DeviceWatcher> {
        // Bounded hand-off channel: COM notification thread → helper thread.
        let (tx, rx) = mpsc::sync_channel::<DeviceEvent>(DEVICE_EVENT_CHANNEL_BOUND);

        // Build the COM notification client and obtain its IMMNotificationClient
        // interface. `enumerator.clone()` is a ref-count bump on the same
        // free-threaded MTA object, used inside the callback to resolve added
        // devices' name/kind.
        let client: IMMNotificationClient = WatcherNotificationClient {
            tx,
            enumerator: self.enumerator.clone(),
        }
        .into();

        // Register the callback. From here on, COM may invoke `client` on its
        // notification thread.
        // SAFETY: `self.enumerator` is a valid IMMDeviceEnumerator; `client` is a
        // live IMMNotificationClient we own a reference to.
        unsafe {
            self.enumerator
                .RegisterEndpointNotificationCallback(&client)
        }
        .map_err(|hr| AudioError::BackendError {
            backend: "wasapi".to_string(),
            operation: "register_endpoint_notification_callback".to_string(),
            message: format!(
                "RegisterEndpointNotificationCallback failed (HRESULT: {:?})",
                hr
            ),
            context: Some(wasapi_backend_context_err(&hr)),
        })?;

        // Shutdown flag observed by the helper thread's recv loop.
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = shutdown.clone();

        // Helper thread: owns the receiver + the user handler. Pops events and
        // invokes `on_event` off the COM thread. Exits when the shutdown flag is
        // set (teardown) or the channel disconnects.
        let mut on_event = on_event;
        let notify_thread = std::thread::Builder::new()
            .name("rsac-wasapi-device-watch".to_string())
            .spawn(move || loop {
                match rx.recv_timeout(NOTIFY_THREAD_POLL_INTERVAL) {
                    Ok(event) => on_event(event),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if thread_shutdown.load(Ordering::Acquire) {
                            break;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            })
            .map_err(|e| {
                // Spawn failed — unregister so we do not leave a dangling COM
                // callback registered with no consumer.
                // SAFETY: same valid enumerator + client as the registration.
                let _ = unsafe {
                    self.enumerator
                        .UnregisterEndpointNotificationCallback(&client)
                };
                AudioError::BackendError {
                    backend: "wasapi".to_string(),
                    operation: "spawn_device_watch_thread".to_string(),
                    message: format!("Failed to spawn device-watch helper thread: {}", e),
                    context: None,
                }
            })?;

        // Move the COM objects needed for teardown into a Send holder. Both are
        // free-threaded MTA objects, so using them (incl. Unregister) from the
        // thread that drops the DeviceWatcher is sound.
        //
        // The `com_initializer` clone PINS the COM apartment for the watcher's
        // whole lifetime. `watch(&self)` is dispatched on a *borrowed* enumerator
        // (see `CrossPlatformDeviceEnumerator::watch`), so the legal
        // `get_enumerator()?.watch(handler)?` pattern drops the
        // `WindowsDeviceEnumerator` — and its `Arc<ComInitializer>` — as soon as
        // `watch` returns. Without holding our own `Arc` clone here, that drop
        // could run `CoUninitialize()` while the IMMNotificationClient is still
        // registered and before this teardown's `Unregister`, tearing down the
        // MTA out from under a live callback. Keeping the clone alive until the
        // teardown closure finishes guarantees ordering: Unregister + join the
        // helper thread first, then release the apartment. (This mirrors why
        // `WindowsAudioDevice` also holds the initializer.)
        let teardown_state = SendWatcherTeardown {
            enumerator: self.enumerator.clone(),
            client,
            com_initializer: self.com_initializer.clone(),
        };

        let teardown = Box::new(move || {
            // 1. Stop new events: unregister the COM callback first, so no further
            //    OnDevice* fires after this returns. Best-effort — a failed
            //    unregister must not panic in Drop.
            // SAFETY: free-threaded MTA enumerator + the client we registered.
            let _ = unsafe {
                teardown_state
                    .enumerator
                    .UnregisterEndpointNotificationCallback(&teardown_state.client)
            };
            // 2. Signal the helper thread to exit, then join. Setting the flag
            //    plus dropping nothing else: the sender lives inside `client`,
            //    which is dropped when `teardown_state` drops at end of closure;
            //    the recv_timeout loop notices the flag within one poll interval.
            shutdown.store(true, Ordering::Release);
            // Join: bounded by NOTIFY_THREAD_POLL_INTERVAL after the flag is set.
            // Ignore a poisoned-thread join error — never panic in Drop.
            let _ = notify_thread.join();
            // `teardown_state` (enumerator + client + com_initializer, holding the
            // SyncSender) drops here, after the consumer thread has been joined —
            // so the COM apartment is released only after Unregister + join, never
            // before.
            drop(teardown_state);
        });

        Ok(DeviceWatcher::from_teardown(teardown))
    }
}

/// `Send` holder for the COM objects a [`DeviceWatcher`] teardown closure needs.
///
/// `IMMDeviceEnumerator` / `IMMNotificationClient` are not `Send` in the
/// `windows` crate, but rsac creates them in a Multi-Threaded Apartment
/// (`COINIT_MULTITHREADED`), where COM objects are free-threaded and may be used
/// from any thread. The teardown closure must be `Send` (it is boxed as
/// `FnOnce() + Send` in [`DeviceWatcher`]), so we assert that here, scoped to
/// exactly these MTA objects.
struct SendWatcherTeardown {
    enumerator: IMMDeviceEnumerator,
    client: IMMNotificationClient,
    /// Keeps the COM apartment (MTA) initialized for the watcher's whole
    /// lifetime, independent of the `WindowsDeviceEnumerator` that created it.
    /// Dropped last (at the end of the teardown closure), after Unregister +
    /// helper-thread join, so `CoUninitialize()` can never race a live callback.
    ///
    /// Held purely for its `Drop` (RAII lifetime extension) — never read — hence
    /// `#[allow(dead_code)]`: the value's *existence* is the contract, and
    /// removing it would reintroduce the apartment-teardown race.
    #[allow(dead_code)]
    com_initializer: Arc<ComInitializer>,
}

// SAFETY: see the type doc — both wrapped interfaces are MTA free-threaded
// objects (created under COINIT_MULTITHREADED), safe to use from any thread.
// `Arc<ComInitializer>` is `Send` already; it is listed for documentation.
unsafe impl Send for SendWatcherTeardown {}

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

/// Find a wasapi-rs `Device` by its Windows device ID string.
///
/// Uses wasapi 0.23's [`DeviceEnumerator::get_device`], which wraps
/// `IMMDeviceEnumerator::GetDevice` and resolves the endpoint directly from
/// its ID — no manual render/capture collection scan needed. Falls back to the
/// default render device if the ID lookup fails (empty ID, stale ID, or an
/// endpoint that has since disappeared).
fn find_wasapi_device_by_id(
    enumerator: &wasapi::DeviceEnumerator,
    device_id: &str,
) -> AudioResult<wasapi::Device> {
    // Direct ID resolution via IMMDeviceEnumerator::GetDevice.
    if !device_id.is_empty() {
        if let Ok(device) = enumerator.get_device(device_id) {
            return Ok(device);
        }
    }

    // Fall back to default render device.
    enumerator
        .get_default_device(&wasapi::Direction::Render)
        .map_err(|e| AudioError::BackendError {
            backend: "wasapi".to_string(),
            operation: "find_wasapi_device_by_id".to_string(),
            message: format!(
                "Device '{}' not found, and default device unavailable: {}",
                device_id, e
            ),
            context: None,
        })
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

    let _com_initializer = ComInitializer::new()?;

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

    /// Test that ComInitializer can be created (COM init succeeds on Windows).
    #[test]
    fn test_com_initializer_creation() {
        let result = ComInitializer::new();
        assert!(
            result.is_ok(),
            "ComInitializer::new() failed: {:?}",
            result.err()
        );
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
    /// enumerator (and its `Arc<ComInitializer>`) drops as soon as `watch`
    /// returns. The watcher pins its own `Arc<ComInitializer>` clone, so the COM
    /// apartment stays initialized until the watcher's teardown (Unregister +
    /// join) completes. Regression guard for the apartment-lifetime fix; before
    /// it, this would `CoUninitialize()` with a live registered callback.
    #[test]
    fn test_watch_outlives_enumerator_drop() {
        let watcher = {
            let enumerator = WindowsDeviceEnumerator::new().expect("new() failed");
            let w = enumerator
                .watch(Box::new(|_event: DeviceEvent| {}))
                .expect("watch() should succeed");
            // `enumerator` drops HERE, at end of block — its Arc<ComInitializer>
            // refcount decrements but the watcher holds another clone.
            w
        };

        // The COM client is still registered and the apartment still live.
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Clean teardown after the enumerator is already gone: no panic, no hang,
        // no use-after-CoUninitialize.
        drop(watcher);
    }
}
