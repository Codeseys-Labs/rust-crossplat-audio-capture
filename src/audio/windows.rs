//! Windows-specific audio capture backend using WASAPI.
#![cfg(target_os = "windows")]

use crate::core::config::{AudioFormat, StreamConfig};
use crate::core::error::{AudioError, Result as AudioResult};
use crate::core::interface::{
    AudioBuffer, AudioDevice, AudioStream, CapturingStream, DeviceEnumerator, DeviceKind,
    StreamDataCallback,
};

// TODO: Remove these once the actual WASAPI logic is integrated with the new traits.
// These are placeholders from the old structure.
use super::core::{AudioApplication, AudioCaptureBackend, AudioCaptureStream}; // Keep for old backend
use std::collections::VecDeque; // Keep for old backend
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System}; // Keep for old backend
use wasapi::{
    self,
    AudioCaptureClient as WasapiAudioCaptureClient,
    AudioClient as WasapiAudioClient, // Renamed to avoid conflict
    Direction as WasapiDirection,
    SampleType as WasapiSampleType,
    ShareMode as WasapiShareMode,
    WaveFormat as WasapiWaveFormat,
}; // Keep for old backend, note: wasapi::get_default_device is different from IMMDeviceEnumerator::GetDefaultAudioEndpoint

// --- New Skeleton Implementations ---

use crate::core::interface::DeviceId;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows::core::{HRESULT, PWSTR};
use windows::Win32::Foundation::E_NOTFOUND;
use windows::Win32::Media::Audio::{
    eAll, eCapture, eConsole, eRender, IMMDevice, IMMDeviceCollection, IMMDeviceEnumerator,
    MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
    RPC_E_CHANGED_MODE,
}; // Assuming DeviceId is String or similar

/// Ensures COM is initialized for the current thread and uninitializes it when dropped.
///
/// This RAII guard should be held by any type that makes COM calls, such as
/// device enumerators or audio streams that interact directly with WASAPI.
#[derive(Debug)]
struct ComInitializer;

impl ComInitializer {
    /// Initializes COM for the current thread using `COINIT_MULTITHREADED`.
    ///
    /// Returns `Ok(Self)` on success, or an `AudioError::BackendSpecificError`
    /// if COM initialization fails.
    pub fn new() -> AudioResult<Self> {
        // SAFETY: CoInitializeEx is safe to call. We check the HRESULT.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            Ok(ComInitializer)
        } else if hr == RPC_E_CHANGED_MODE {
            // COM was already initialized with a different concurrency model.
            // This is generally okay for our purposes if it's already initialized.
            // However, for strictness, one might treat this as an error or log it.
            // For now, we'll consider it a success if it's already initialized,
            // as long as it's not a clear failure.
            // If CoInitializeEx returns S_FALSE, it means COM was already initialized.
            // If it returns RPC_E_CHANGED_MODE, it means it was initialized with a different model.
            // We are aiming for MTA, if it's already STA and we try MTA, it's an issue.
            // Let's treat RPC_E_CHANGED_MODE as an error for now to be safe.
            Err(AudioError::BackendSpecificError(format!(
                "Failed to initialize COM: Already initialized with a different concurrency model (HRESULT: {:?})",
                hr
            )))
        } else {
            Err(AudioError::BackendSpecificError(format!(
                "Failed to initialize COM (HRESULT: {:?})",
                hr
            )))
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

// Removed WindowsDeviceId struct as DeviceId will be String

/// Represents a Windows audio device using WASAPI.
///
/// This struct holds an `IMMDevice` instance, which is the core representation
/// of an audio endpoint in WASAPI.
#[derive(Debug)] // IMMDevice itself is a COM interface pointer, Debug should be fine.
pub(crate) struct WindowsAudioDevice {
    device: IMMDevice,
    // _com_initializer: Arc<ComInitializer>, // Potentially needed if IMMDevice methods require COM to be alive
    // and this struct outlives the enumerator. For now, assume not.
}

impl WindowsAudioDevice {
    /// Creates a new `WindowsAudioDevice` from an `IMMDevice`.
    fn new(device: IMMDevice) -> Self {
        Self { device }
    }
}

impl AudioDevice for WindowsAudioDevice {
    type DeviceId = DeviceId; // This is String as per crate::core::interface::DeviceId

    fn get_id(&self) -> Self::DeviceId {
        // TODO: Implement in subtask 4.3: Get device ID string from self.device
        // For example, using IPropertyStore to get PKEY_Device_FriendlyName or PKEY_Device_InstanceId
        // For now, as per task 4.2, this is todo.
        todo!("WindowsAudioDevice::get_id()")
    }

    fn get_name(&self) -> String {
        // TODO: Implement in subtask 4.3: Get device friendly name from self.device
        todo!("WindowsAudioDevice::get_name()")
    }

    fn get_supported_formats(&self) -> AudioResult<Vec<AudioFormat>> {
        // TODO: Implement in subtask 4.3
        todo!("WindowsAudioDevice::get_supported_formats()")
    }

    fn get_default_format(&self) -> AudioResult<AudioFormat> {
        // TODO: Implement in subtask 4.3
        todo!("WindowsAudioDevice::get_default_format()")
    }

    fn is_input(&self) -> bool {
        // TODO: Implement in subtask 4.3: Determine if it's an input device
        todo!("WindowsAudioDevice::is_input()")
    }

    fn is_output(&self) -> bool {
        // TODO: Implement in subtask 4.3: Determine if it's an output device
        todo!("WindowsAudioDevice::is_output()")
    }

    fn is_active(&self) -> bool {
        // TODO: Implement in subtask 4.3: Check device state
        todo!("WindowsAudioDevice::is_active()")
    }

    fn is_format_supported(&self, _format: &AudioFormat) -> AudioResult<bool> {
        // TODO: Implement in subtask 4.3
        todo!("WindowsAudioDevice::is_format_supported()")
    }
}

/// Enumerates audio devices available on a Windows system using WASAPI.
#[derive(Debug)]
pub struct WindowsDeviceEnumerator {
    _com_initializer: ComInitializer,
    enumerator: IMMDeviceEnumerator,
}

impl WindowsDeviceEnumerator {
    /// Creates a new Windows device enumerator.
    ///
    /// This will initialize COM for the lifetime of the enumerator and
    /// create an `IMMDeviceEnumerator` instance.
    pub fn new() -> AudioResult<Self> {
        let com_initializer = ComInitializer::new()?;
        // SAFETY: CoCreateInstance is called to create a COM object.
        // The HRESULT is checked for errors.
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.map_err(
                |hr: HRESULT| {
                    AudioError::BackendSpecificError(format!(
                        "Failed to create IMMDeviceEnumerator (HRESULT: {:?})",
                        hr
                    ))
                },
            )?;

        Ok(Self {
            _com_initializer: com_initializer,
            enumerator,
        })
    }
}

impl DeviceEnumerator for WindowsDeviceEnumerator {
    // Note: The task specifies Box<dyn AudioDevice>, so Self::Device is WindowsAudioDevice,
    // but methods return Box<dyn AudioDevice>.

    /// Enumerates all active audio endpoint devices.
    ///
    /// This method retrieves a collection of all active audio rendering and capture
    /// devices on the system.
    fn enumerate_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice>>> {
        // SAFETY: Calling EnumAudioEndpoints on a valid IMMDeviceEnumerator. HRESULT is checked.
        let collection: IMMDeviceCollection = unsafe {
            self.enumerator
                .EnumAudioEndpoints(eAll, DEVICE_STATE_ACTIVE)
        }
        .map_err(|hr: HRESULT| {
            AudioError::BackendSpecificError(format!(
                "Failed to enumerate audio endpoints (HRESULT: {:?})",
                hr
            ))
        })?;

        // SAFETY: Calling GetCount on a valid IMMDeviceCollection. HRESULT is checked.
        let count = unsafe { collection.GetCount() }.map_err(|hr: HRESULT| {
            AudioError::BackendSpecificError(format!(
                "Failed to get device count from collection (HRESULT: {:?})",
                hr
            ))
        })?;

        let mut devices: Vec<Box<dyn AudioDevice>> = Vec::with_capacity(count as usize);
        for i in 0..count {
            // SAFETY: Calling Item on a valid IMMDeviceCollection with a valid index. HRESULT is checked.
            let imm_device: IMMDevice = unsafe { collection.Item(i) }.map_err(|hr: HRESULT| {
                AudioError::BackendSpecificError(format!(
                    "Failed to get device item {} from collection (HRESULT: {:?})",
                    i, hr
                ))
            })?;
            devices.push(Box::new(WindowsAudioDevice::new(imm_device)));
        }
        Ok(devices)
    }

    /// Gets the default audio endpoint device for the specified kind (input/output).
    ///
    /// # Arguments
    /// * `kind` - The [`DeviceKind`] (input/capture or output/render) for which
    ///            to get the default device.
    ///
    /// Returns `Ok(Some(device))` if a default device is found, `Ok(None)` if no
    /// default device is available for the specified kind (e.g., `E_NOTFOUND`),
    /// or an `AudioError` on other failures.
    fn get_default_device(&self, kind: DeviceKind) -> AudioResult<Option<Box<dyn AudioDevice>>> {
        let data_flow = match kind {
            DeviceKind::Input => eCapture,
            DeviceKind::Output => eRender,
        };

        // SAFETY: Calling GetDefaultAudioEndpoint on a valid IMMDeviceEnumerator. HRESULT is checked.
        match unsafe { self.enumerator.GetDefaultAudioEndpoint(data_flow, eConsole) } {
            Ok(imm_device) => Ok(Some(Box::new(WindowsAudioDevice::new(imm_device)))),
            Err(hr) if hr == E_NOTFOUND => Ok(None), // Device not found is not an error, but absence.
            Err(hr) => Err(AudioError::BackendSpecificError(format!(
                "Failed to get default audio endpoint (HRESULT: {:?})",
                hr
            ))),
        }
    }

    /// Gets an audio endpoint device by its ID string.
    ///
    /// # Arguments
    /// * `id` - The string ID of the device to retrieve. This ID is typically obtained
    ///          from a previous enumeration or from `AudioDevice::get_id()`.
    /// * `_kind` - Currently unused, but reserved for future use if device kind needs
    ///             to be validated against the ID.
    ///
    /// Returns `Ok(Some(device))` if a device with the given ID is found, `Ok(None)`
    /// if no such device exists (e.g., `E_NOTFOUND`), or an `AudioError` on other failures.
    fn get_device_by_id(
        &self,
        id: &DeviceId,
        _kind: Option<DeviceKind>,
    ) -> AudioResult<Option<Box<dyn AudioDevice>>> {
        let wide_id: Vec<u16> = OsStr::new(id).encode_wide().chain(Some(0)).collect();
        let pwstr_id = PWSTR(wide_id.as_ptr() as *mut _); // Cast to *mut _ as PWSTR is *mut u16

        // SAFETY: Calling GetDevice on a valid IMMDeviceEnumerator with a null-terminated PWSTR. HRESULT is checked.
        match unsafe { self.enumerator.GetDevice(pwstr_id) } {
            Ok(imm_device) => Ok(Some(Box::new(WindowsAudioDevice::new(imm_device)))),
            Err(hr) if hr == E_NOTFOUND => Ok(None),
            Err(hr) => Err(AudioError::DeviceNotFound(format!(
                "Failed to get device by ID '{}' (HRESULT: {:?})",
                id, hr
            ))),
        }
    }
}

pub struct WindowsAudioStream {
    // TODO: Add fields specific to a Windows audio stream (e.g., WASAPI client, buffer, config)
    config: Option<StreamConfig>, // Store the config
}

impl AudioStream for WindowsAudioStream {
    type Config = StreamConfig;
    type Device = WindowsAudioDevice;

    fn open(&mut self, device: &Self::Device, config: Self::Config) -> AudioResult<()> {
        println!(
            "TODO: WindowsAudioStream::open(device_id: {:?}, config: {:?})",
            device.get_id(),
            config
        );
        self.config = Some(config);
        todo!()
    }

    fn start(&mut self) -> AudioResult<()> {
        println!("TODO: WindowsAudioStream::start()");
        todo!()
    }

    fn pause(&mut self) -> AudioResult<()> {
        println!("TODO: WindowsAudioStream::pause()");
        todo!()
    }

    fn resume(&mut self) -> AudioResult<()> {
        println!("TODO: WindowsAudioStream::resume()");
        todo!()
    }

    fn stop(&mut self) -> AudioResult<()> {
        println!("TODO: WindowsAudioStream::stop()");
        todo!()
    }

    fn close(&mut self) -> AudioResult<()> {
        println!("TODO: WindowsAudioStream::close()");
        self.config = None;
        todo!()
    }

    fn set_format(&mut self, format: &AudioFormat) -> AudioResult<()> {
        println!("TODO: WindowsAudioStream::set_format({:?})", format);
        todo!()
    }

    fn set_callback(&mut self, _callback: StreamDataCallback) -> AudioResult<()> {
        println!("TODO: WindowsAudioStream::set_callback()");
        todo!()
    }

    fn is_running(&self) -> bool {
        println!("TODO: WindowsAudioStream::is_running()");
        false
    }

    fn get_latency_frames(&self) -> AudioResult<u64> {
        println!("TODO: WindowsAudioStream::get_latency_frames()");
        todo!()
    }

    fn get_current_format(&self) -> AudioResult<AudioFormat> {
        println!("TODO: WindowsAudioStream::get_current_format()");
        todo!()
    }
}

impl CapturingStream for WindowsAudioStream {
    fn start(&mut self) -> AudioResult<()> {
        println!("TODO: WindowsAudioStream (CapturingStream)::start()");
        // This would typically call self.start() from the AudioStream impl,
        // but for a skeleton, todo!() is fine.
        todo!()
    }

    fn stop(&mut self) -> AudioResult<()> {
        println!("TODO: WindowsAudioStream (CapturingStream)::stop()");
        todo!()
    }

    fn close(&mut self) -> AudioResult<()> {
        println!("TODO: WindowsAudioStream (CapturingStream)::close()");
        todo!()
    }

    fn is_running(&self) -> bool {
        println!("TODO: WindowsAudioStream (CapturingStream)::is_running()");
        false
    }

    fn read_chunk(&mut self, timeout_ms: Option<u32>) -> AudioResult<Option<Box<dyn AudioBuffer>>> {
        println!(
            "TODO: WindowsAudioStream (CapturingStream)::read_chunk(timeout_ms: {:?})",
            timeout_ms
        );
        todo!()
    }

    fn to_async_stream<'a>(
        &'a mut self,
    ) -> AudioResult<
        std::pin::Pin<
            Box<
                dyn futures_core::Stream<Item = AudioResult<Box<dyn AudioBuffer<Sample = f32>>>>
                    + Send
                    + Sync
                    + 'a,
            >,
        >,
    > {
        println!("TODO: WindowsAudioStream (CapturingStream)::to_async_stream()");
        todo!()
    }
}

// --- Old WASAPI Backend (To be refactored/removed) ---
// This section contains the previous implementation and will be gradually
// replaced or integrated into the new trait-based structure.

pub struct WasapiBackend {
    _system: System, // Keep system alive but mark as intentionally unused
}

impl WasapiBackend {
    pub fn new() -> Result<Self, AudioError> {
        // Initialize COM for WASAPI
        let _ = wasapi::initialize_mta();

        let system = System::new_with_specifics(
            RefreshKind::everything().with_processes(ProcessRefreshKind::everything()),
        );
        Ok(Self { _system: system })
    }
}

impl Drop for WasapiBackend {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}

impl AudioCaptureBackend for WasapiBackend {
    fn name(&self) -> &'static str {
        "WASAPI"
    }

    fn list_applications(&self) -> Result<Vec<AudioApplication>, AudioError> {
        let mut apps = Vec::new();

        // Add system-wide audio capture option
        apps.push(AudioApplication {
            name: "System".to_string(),
            id: "system".to_string(),
            executable_name: "system".to_string(),
            pid: 0,
        });

        // Create a new system instance for process listing
        let mut system = System::new_with_specifics(
            RefreshKind::everything().with_processes(ProcessRefreshKind::everything()),
        );
        system.refresh_processes(ProcessesToUpdate::All, true);

        // Add running processes
        for (pid, process) in system.processes() {
            let name = process.name().to_string_lossy().into_owned();
            // Skip system processes and processes without audio
            if !name.is_empty() && pid.as_u32() > 4 {
                // Skip system processes (PIDs 0-4)
                apps.push(AudioApplication {
                    name: name.clone(),
                    id: pid.to_string(),
                    executable_name: format!("{}.exe", name),
                    pid: pid.as_u32(),
                });
            }
        }

        // Sort applications: System first, then by name
        apps.sort_by(|a, b| {
            if a.name == "System" {
                std::cmp::Ordering::Less
            } else if b.name == "System" {
                std::cmp::Ordering::Greater
            } else {
                a.name.cmp(&b.name)
            }
        });

        Ok(apps)
    }

    fn capture_application(
        &self,
        app: &AudioApplication,
        config: crate::core::config::AudioConfig,
    ) -> Result<Box<dyn AudioCaptureStream>, AudioError> {
        let wave_format = WaveFormat::new(
            32, // bits per sample
            32, // valid bits per sample
            &SampleType::Float,
            config.sample_rate.try_into().unwrap(),
            config.channels.into(),
            None,
        );

        let mut audio_client = if app.name == "System" {
            get_default_device(&Direction::Render)
                .map_err(|e| {
                    AudioError::DeviceNotFound(format!("Failed to get default device: {}", e))
                })?
                .get_iaudioclient()
                .map_err(|e| {
                    AudioError::DeviceNotFound(format!(
                        "Failed to create system audio capture: {}",
                        e
                    ))
                })?
        } else {
            AudioClient::new_application_loopback_client(app.pid, true).map_err(|e| {
                AudioError::DeviceNotFound(format!("Failed to create process audio capture: {}", e))
            })?
        };

        audio_client
            .initialize_client(
                &wave_format,
                0,
                &Direction::Capture,
                &ShareMode::Shared,
                true,
            )
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        let stream = WasapiCaptureStream::new(audio_client, config, wave_format)?;
        Ok(Box::new(stream))
    }
}

pub struct WasapiCaptureStream {
    client: AudioClient,
    capture_client: AudioCaptureClient,
    buffer: VecDeque<u8>,
    config: crate::core::config::AudioConfig,
    event_handle: Option<wasapi::Handle>,
    format: WaveFormat,
}

unsafe impl Send for WasapiCaptureStream {}
unsafe impl Send for WasapiBackend {}

impl WasapiCaptureStream {
    fn new(
        client: AudioClient,
        config: crate::core::config::AudioConfig,
        format: WaveFormat,
    ) -> Result<Self, AudioError> {
        let event_handle = client
            .set_get_eventhandle()
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        let capture_client = client
            .get_audiocaptureclient()
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        Ok(Self {
            client,
            capture_client,
            buffer: VecDeque::new(),
            config,
            event_handle: Some(event_handle),
            format,
        })
    }
}

impl AudioCaptureStream for WasapiCaptureStream {
    fn start(&mut self) -> Result<(), AudioError> {
        self.client
            .start_stream()
            .map_err(|e| AudioError::CaptureError(e.to_string()))
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        self.client
            .stop_stream()
            .map_err(|e| AudioError::CaptureError(e.to_string()))
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, AudioError> {
        if !self.buffer.is_empty() {
            let bytes_to_copy = std::cmp::min(buffer.len(), self.buffer.len());
            for i in 0..bytes_to_copy {
                buffer[i] = self.buffer.pop_front().unwrap();
            }
            return Ok(bytes_to_copy);
        }

        let new_frames = self
            .capture_client
            .get_next_nbr_frames()
            .map_err(|e| AudioError::CaptureError(e.to_string()))?
            .unwrap_or(0);

        if new_frames > 0 {
            let block_align = self.format.get_blockalign() as usize;
            let additional = (new_frames as usize * block_align)
                .saturating_sub(self.buffer.capacity() - self.buffer.len());
            self.buffer.reserve(additional);

            self.capture_client
                .read_from_device_to_deque(&mut self.buffer)
                .map_err(|e| AudioError::CaptureError(e.to_string()))?;
        }

        if let Some(event_handle) = &self.event_handle {
            if event_handle.wait_for_event(3000).is_err() {
                return Err(AudioError::CaptureError(
                    "Timeout waiting for audio data".to_string(),
                ));
            }
        }

        let bytes_to_copy = std::cmp::min(buffer.len(), self.buffer.len());
        for i in 0..bytes_to_copy {
            buffer[i] = self.buffer.pop_front().unwrap();
        }
        Ok(bytes_to_copy)
    }

    fn config(&self) -> &crate::core::config::AudioConfig {
        &self.config
    }
}
