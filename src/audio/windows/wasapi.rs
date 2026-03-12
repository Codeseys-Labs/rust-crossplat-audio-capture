//! Windows-specific audio capture backend using WASAPI.
//! Testing Windows compilation in GitHub Actions.
#![cfg(target_os = "windows")]

use crate::core::buffer::AudioBuffer;
use crate::core::config::{AudioFormat, StreamConfig};
use crate::core::error::{AudioError, Result as AudioResult};
use crate::core::interface::{AudioDevice, CapturingStream, DeviceEnumerator, DeviceKind};

use std::ffi::OsStr;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use sysinfo::{ProcessesToUpdate, System};
use wasapi::{self, initialize_mta};

// --- New BridgeStream Architecture Imports ---
use super::thread::{WindowsCaptureConfig, WindowsCaptureThread, WindowsPlatformStream};
use crate::bridge::{calculate_capacity, create_bridge, BridgeStream, PlatformStream, StreamState};
use crate::core::config::{CaptureTarget, DeviceId};

// --- Application-Specific Capture (Process Loopback) ---

use windows::{
    core::*, Win32::Foundation::*, Win32::Media::Audio::*, Win32::System::Com::*,
    Win32::System::Threading::*, Win32::System::Variant::*,
};

// Specific imports not covered by the glob imports above
use windows::Win32::Devices::Properties::DEVPKEY_Device_FriendlyName as PKEY_Device_FriendlyName;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

// Constants
const E_NOTFOUND: windows::core::HRESULT = windows::core::HRESULT(-2147024894i32); // 0x80070002
const RPC_E_CHANGED_MODE: windows::core::HRESULT = windows::core::HRESULT(-2147417850i32); // 0x80010106
const VT_LPWSTR: u16 = 31;

/// Windows-specific application capture using wasapi-rs library
/// Based on wasapi-rs examples/record_application.rs for simplicity and reliability
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
    /// use rust_crossplat_audio_capture::audio::windows::WindowsApplicationCapture;
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
        // Initialize COM using wasapi-rs
        initialize_mta().ok().unwrap();

        // Create wasapi-rs AudioClient for application loopback
        let mut audio_client = wasapi::AudioClient::new_application_loopback_client(
            self.process_id,
            self.include_tree,
        )?;

        // Initialize the audio client with a standard format
        let desired_format =
            wasapi::WaveFormat::new(32, 32, &wasapi::SampleType::Float, 48000, 2, None);
        let mode = wasapi::StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: 0,
        };

        audio_client.initialize_client(&desired_format, &wasapi::Direction::Capture, &mode)?;

        // Store the initialized client
        self.audio_client = Some(audio_client);

        Ok(())
    }

    /// Start capturing audio from the target process using wasapi-rs
    ///
    /// # Implementation Notes
    /// - Uses wasapi-rs for simplified audio capture
    /// - Based on wasapi-rs examples for reliability
    pub fn start_capture<F>(
        &mut self,
        callback: F,
    ) -> std::result::Result<(), Box<dyn std::error::Error>>
    where
        F: Fn(&[f32]) + Send + 'static,
    {
        self.start_capture_with_stop_flag(callback, None)
    }

    /// Start capturing audio with an external stop flag
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

        // Simple capture loop based on wasapi-rs examples
        loop {
            // Check if we should stop capture (internal flag)
            if self.should_stop.load(Ordering::SeqCst) {
                break; // Stop requested
            }

            // Check external stop flag if provided
            if let Some(ref external_flag) = external_stop_flag {
                if external_flag.load(Ordering::SeqCst) {
                    break; // External stop requested
                }
            }

            // Wait for audio data (shorter timeout to check stop flag more frequently)
            if h_event.wait_for_event(100).is_err() {
                // Check stop flags on timeout too
                if self.should_stop.load(Ordering::SeqCst) {
                    break;
                }
                if let Some(ref external_flag) = external_stop_flag {
                    if external_flag.load(Ordering::SeqCst) {
                        break;
                    }
                }
                continue; // Continue on timeout, don't break immediately
            }

            // Get available packet size
            let packet_length = capture_client.get_next_packet_size()?.unwrap_or(0);

            if packet_length > 0 {
                // Use wasapi-rs to read audio data into a VecDeque
                let mut sample_queue = std::collections::VecDeque::new();
                capture_client.read_from_device_to_deque(&mut sample_queue)?;

                // Convert bytes to f32 samples and call callback
                if !sample_queue.is_empty() {
                    // Convert VecDeque<u8> to Vec<f32>
                    // Assuming 32-bit float format (4 bytes per sample)
                    let mut samples = Vec::new();
                    while sample_queue.len() >= 4 {
                        let bytes: [u8; 4] = [
                            sample_queue.pop_front().unwrap(),
                            sample_queue.pop_front().unwrap(),
                            sample_queue.pop_front().unwrap(),
                            sample_queue.pop_front().unwrap(),
                        ];
                        samples.push(f32::from_le_bytes(bytes));
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

    /// Stop capturing audio
    pub fn stop(&mut self) -> std::result::Result<(), Box<dyn std::error::Error>> {
        // Clear the audio client
        self.audio_client = None;
        Ok(())
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
    fn new(handle: Option<HANDLE>) -> Self {
        Self(handle)
    }

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
    pub fn new() -> AudioResult<Self> {
        // SAFETY: CoInitializeEx is safe to call. We check the HRESULT.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            Ok(ComInitializer)
        } else if hr == RPC_E_CHANGED_MODE {
            // COM was already initialized with a different concurrency model.
            Err(AudioError::BackendError {
                backend: "wasapi".to_string(),
                operation: "com_init".to_string(),
                message: format!(
                    "Already initialized with a different concurrency model (HRESULT: {:?})",
                    hr
                ),
                context: None,
            })
        } else {
            Err(AudioError::BackendError {
                backend: "wasapi".to_string(),
                operation: "com_init".to_string(),
                message: format!("COM initialization failed (HRESULT: {:?})", hr),
                context: None,
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
        // Return common default format — detailed format negotiation can be added later
        vec![AudioFormat::default()] // 48kHz, 2ch, F32
    }

    fn create_stream(&self, config: &StreamConfig) -> AudioResult<Box<dyn CapturingStream>> {
        // === 9-step BridgeStream wiring (following Linux pattern) ===

        // 1. Build AudioFormat from StreamConfig
        let format = config.to_audio_format();

        // 2. Determine capture target from device ID
        let device_id_str = self.id().0;
        let target = if device_id_str == "default" || device_id_str == "unknown-device" {
            CaptureTarget::SystemDefault
        } else {
            CaptureTarget::Device(DeviceId(device_id_str))
        };

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
    /// Helper method that returns Result for get_name implementation
    fn get_name_internal(&self) -> AudioResult<String> {
        unsafe {
            let property_store: IPropertyStore =
                self.device.OpenPropertyStore(STGM_READ).map_err(|hr| {
                    AudioError::BackendError {
                        backend: "wasapi".to_string(),
                        operation: "get_device_name".to_string(),
                        message: format!("IMMDevice::OpenPropertyStore failed (HRESULT: {:?})", hr),
                        context: None,
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
                    context: None,
                })?;

            let name = if prop_variant.vt() == windows::Win32::System::Variant::VARENUM(VT_LPWSTR) {
                let name_pwstr = unsafe { prop_variant.Anonymous.Anonymous.Anonymous.pwszVal };
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
    /// Determines the kind of device (Input or Output).
    /// This is a helper method, not part of the AudioDevice trait.
    pub fn kind(&self) -> AudioResult<DeviceKind> {
        // QueryInterface for IMMEndpoint
        let endpoint: IMMEndpoint = self.device.cast().map_err(|hr| AudioError::BackendError {
            backend: "wasapi".to_string(),
            operation: "get_device_kind".to_string(),
            message: format!(
                "Failed to cast IMMDevice to IMMEndpoint (HRESULT: {:?})",
                hr
            ),
            context: None,
        })?;

        unsafe {
            let data_flow_val = endpoint
                .GetDataFlow()
                .map_err(|hr| AudioError::BackendError {
                    backend: "wasapi".to_string(),
                    operation: "get_device_kind".to_string(),
                    message: format!("IMMEndpoint::GetDataFlow failed (HRESULT: {:?})", hr),
                    context: None,
                })?;

            match data_flow_val {
                eRender => Ok(DeviceKind::Output),
                eCapture => Ok(DeviceKind::Input),
                _ => Err(AudioError::BackendError {
                    backend: "wasapi".to_string(),
                    operation: "get_device_kind".to_string(),
                    message: format!("Unknown data flow value: {:?}", data_flow_val),
                    context: None,
                }),
            }
        }
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
            context: None,
        })?;

        Ok(Self {
            com_initializer,
            enumerator,
        })
    }
}

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
            context: None,
        })?;

        // SAFETY: Calling GetCount on a valid IMMDeviceCollection.
        let count =
            unsafe { collection.GetCount() }.map_err(|hr| AudioError::DeviceEnumerationError {
                reason: format!(
                    "Failed to get device count from collection (HRESULT: {:?})",
                    hr
                ),
                context: None,
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
                    context: None,
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
                context: None,
            }),
        }
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
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Media::Audio::{
        IAudioSessionControl, IAudioSessionControl2, IAudioSessionEnumerator, IAudioSessionManager2,
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
                    context: None,
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
                        context: None,
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
                context: None,
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
                context: None,
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
                context: None,
            })?;

        for i in 0..count {
            let session_control: IAudioSessionControl =
                session_enumerator
                    .GetSession(i)
                    .map_err(|hr| AudioError::BackendError {
                        backend: "wasapi".to_string(),
                        operation: "enumerate_audio_sessions".to_string(),
                        message: format!(
                            "IAudioSessionEnumerator::GetSession({}) failed (HRESULT: {:?})",
                            i, hr
                        ),
                        context: None,
                    })?;

            let session_control2: IAudioSessionControl2 = match session_control.cast() {
                Ok(sc2) => sc2,
                Err(hr) => {
                    // Log or skip if IAudioSessionControl2 is not available for this session
                    eprintln!(
                        "Warning: Could not cast IAudioSessionControl to IAudioSessionControl2 for session {}: {:?}",
                        i, hr
                    );
                    continue;
                }
            };

            let pid = session_control2
                .GetProcessId()
                .map_err(|hr| AudioError::BackendError {
                    backend: "wasapi".to_string(),
                    operation: "enumerate_audio_sessions".to_string(),
                    message: format!(
                        "IAudioSessionControl2::GetProcessId for session {} failed (HRESULT: {:?})",
                        i, hr
                    ),
                    context: None,
                })?;

            if pid == 0 {
                // Skip system sounds or non-application sessions
                continue;
            }

            let display_name_pwstr = session_control2.GetDisplayName().unwrap_or(PWSTR::null());
            let mut display_name = if !display_name_pwstr.is_null() {
                let dn = WindowsAudioDevice::pwstr_to_string(display_name_pwstr)
                    .unwrap_or_else(|_| String::new());
                CoTaskMemFree(Some(display_name_pwstr.as_ptr().cast()));
                dn
            } else {
                String::new()
            };

            let session_identifier_pwstr = session_control2
                .GetSessionIdentifier()
                .unwrap_or(PWSTR::null());
            let session_identifier = if !session_identifier_pwstr.is_null() {
                let si = WindowsAudioDevice::pwstr_to_string(session_identifier_pwstr)
                    .unwrap_or_else(|_| String::new());
                CoTaskMemFree(Some(session_identifier_pwstr.as_ptr().cast()));
                si
            } else {
                String::new()
            };

            if display_name.is_empty() && !session_identifier.is_empty() {
                // Fallback to session identifier if display name is empty
                let parts: Vec<&str> = session_identifier.split('|').collect();
                if let Some(name_part) = parts.get(0) {
                    if let Some(exe_name) = name_part.split('\\').last() {
                        display_name = exe_name.trim_end_matches(".exe").to_string();
                    }
                }
                if display_name.is_empty() {
                    // if still empty after trying to parse session_identifier
                    display_name = format!("PID: {}", pid); // Fallback further
                }
            } else if display_name.is_empty() {
                display_name = format!("Unknown App (PID: {})", pid); // Ultimate fallback
            }

            let mut executable_path: Option<String> = None;
            let process_handle_result = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid);

            match process_handle_result {
                Ok(process_handle) if process_handle != INVALID_HANDLE_VALUE => {
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
                    let _ = CloseHandle(process_handle);
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
                session_identifier,
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
    use crate::core::config::{ApplicationId, CaptureTarget, DeviceId, ProcessId, StreamConfig};
    use crate::core::interface::{AudioDevice, DeviceEnumerator};

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

    /// Test AudioDevice::supported_formats() returns at least the default format.
    #[test]
    fn test_audio_device_supported_formats() {
        let enumerator = WindowsDeviceEnumerator::new().expect("new() failed");
        let device = enumerator.default_device().expect("no default device");
        let formats = device.supported_formats();
        assert!(
            !formats.is_empty(),
            "Device should have at least one supported format"
        );
        // First format should be the default (48kHz, 2ch, F32).
        let first = &formats[0];
        assert_eq!(first.sample_rate, 48000);
        assert_eq!(first.channels, 2);
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

    /// Test that enumerate_application_audio_sessions doesn't panic.
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

    // ── BridgeStream Integration (full wiring) ──────────────────────

    /// Test that create_stream() produces a CapturingStream that can read audio.
    /// This is a deeper integration test of the full WASAPI → BridgeStream pipeline.
    #[test]
    fn test_create_stream_can_read() {
        use crate::core::interface::CapturingStream;

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
}
