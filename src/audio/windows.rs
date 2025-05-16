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

use crate::core::config::SampleFormat;
use crate::core::interface::DeviceId;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::ptr;
use std::sync::Arc;
use windows::core::{GUID, HRESULT, PWSTR};
use windows::Win32::Foundation::{E_NOTFOUND, S_FALSE, S_OK};
use windows::Win32::Media::Audio::{
    eAll, eCapture, eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDevice,
    IMMDeviceCollection, IMMDeviceEnumerator, IMMEndpoint, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, CLSCTX_ALL, DEVICE_STATE_ACTIVE,
    WAVEFORMATEX, WAVE_FORMAT_IEEE_FLOAT, WAVE_FORMAT_PCM,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IPropertyStore, CLSCTX_ALL,
    COINIT_MULTITHREADED, RPC_E_CHANGED_MODE, STGM_READ,
};
use windows::Win32::System::Propsystem::PropVariantClear;
use windows::Win32::System::Variant::{PROPVARIANT, VT_EMPTY, VT_LPWSTR};
use windows::Win32::UI::Shell::PropertiesSystem::PKEY_Device_FriendlyName;

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
    com_initializer: Arc<ComInitializer>,
}

impl WindowsAudioDevice {
    /// Creates a new `WindowsAudioDevice` from an `IMMDevice`.
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
            return Err(AudioError::BackendSpecificError(
                "PWSTR pointer was null".into(),
            ));
        }
        pwstr.to_string().map_err(|e| {
            AudioError::BackendSpecificError(format!("Failed to convert PWSTR to string: {:?}", e))
        })
    }

    /// Helper function to convert an `AudioFormat` to a `WAVEFORMATEX`.
    fn audio_format_to_waveformat_ex(format: &AudioFormat) -> AudioResult<WAVEFORMATEX> {
        let w_format_tag = match format.sample_format {
            SampleFormat::S16LE | SampleFormat::S32LE | SampleFormat::S8 | SampleFormat::U8 => {
                WAVE_FORMAT_PCM
            }
            SampleFormat::F32LE => WAVE_FORMAT_IEEE_FLOAT,
            // TODO: Add other sample format mappings if necessary
            _ => {
                return Err(AudioError::UnsupportedFormat(format!(
                    "Unsupported sample format for WAVEFORMATEX conversion: {:?}",
                    format.sample_format
                )))
            }
        };

        if format.bits_per_sample == 0 || format.channels == 0 {
            return Err(AudioError::InvalidParameter(
                "Bits per sample and channels must be non-zero".to_string(),
            ));
        }
        let block_align = format.channels * (format.bits_per_sample / 8);
        if block_align == 0 {
            return Err(AudioError::InvalidParameter(
                "Calculated block_align is zero, check bits_per_sample and channels".to_string(),
            ));
        }

        Ok(WAVEFORMATEX {
            wFormatTag: w_format_tag.0 as u16, // WAVE_FORMAT_PCM or WAVE_FORMAT_IEEE_FLOAT
            nChannels: format.channels,
            nSamplesPerSec: format.sample_rate,
            nAvgBytesPerSec: format.sample_rate * block_align as u32,
            nBlockAlign: block_align,
            wBitsPerSample: format.bits_per_sample,
            cbSize: 0, // Typically 0 for PCM and IEEE_FLOAT
        })
    }

    /// Helper function to convert a `*mut WAVEFORMATEX` to an `AudioFormat`.
    /// The caller is responsible for freeing the `WAVEFORMATEX` memory.
    unsafe fn waveformat_ex_to_audio_format(
        wave_format_ptr: *const WAVEFORMATEX,
    ) -> AudioResult<AudioFormat> {
        if wave_format_ptr.is_null() {
            return Err(AudioError::BackendSpecificError(
                "WAVEFORMATEX pointer was null".into(),
            ));
        }
        let wf = &*wave_format_ptr;

        let sample_format = match wf.wFormatTag as u32 {
            WAVE_FORMAT_PCM => match wf.wBitsPerSample {
                8 => SampleFormat::U8, // Or S8, common for PCM 8-bit to be U8
                16 => SampleFormat::S16LE,
                32 => SampleFormat::S32LE,
                _ => {
                    return Err(AudioError::UnsupportedFormat(format!(
                        "Unsupported bits per sample for PCM: {}",
                        wf.wBitsPerSample
                    )))
                }
            },
            WAVE_FORMAT_IEEE_FLOAT => match wf.wBitsPerSample {
                32 => SampleFormat::F32LE,
                _ => {
                    return Err(AudioError::UnsupportedFormat(format!(
                        "Unsupported bits per sample for IEEE FLOAT: {}",
                        wf.wBitsPerSample
                    )))
                }
            },
            _ => {
                return Err(AudioError::UnsupportedFormat(format!(
                    "Unsupported wFormatTag: {}",
                    wf.wFormatTag
                )))
            }
        };

        Ok(AudioFormat {
            sample_rate: wf.nSamplesPerSec,
            channels: wf.nChannels,
            bits_per_sample: wf.wBitsPerSample,
            sample_format,
        })
    }
}

impl AudioDevice for WindowsAudioDevice {
    /// Gets the unique identifier for this audio device.
    /// This ID is typically a string provided by the underlying OS audio backend.
    fn get_id(&self) -> AudioResult<DeviceId> {
        unsafe {
            let mut id_pwstr: PWSTR = PWSTR::null();
            self.device.GetId(&mut id_pwstr).map_err(|hr| {
                AudioError::BackendSpecificError(format!(
                    "IMMDevice::GetId failed (HRESULT: {:?})",
                    hr
                ))
            })?;
            let id_str = Self::pwstr_to_string(id_pwstr)?;
            CoTaskMemFree(Some(id_pwstr.as_ptr().cast()));
            Ok(id_str)
        }
    }

    /// Gets the human-readable friendly name of this audio device.
    fn get_name(&self) -> AudioResult<String> {
        unsafe {
            let property_store: IPropertyStore =
                self.device.OpenPropertyStore(STGM_READ).map_err(|hr| {
                    AudioError::BackendSpecificError(format!(
                        "IMMDevice::OpenPropertyStore failed (HRESULT: {:?})",
                        hr
                    ))
                })?;

            let mut prop_variant = PROPVARIANT::default();
            property_store
                .GetValue(&PKEY_Device_FriendlyName, &mut prop_variant)
                .map_err(|hr| AudioError::BackendSpecificError(format!("IPropertyStore::GetValue for PKEY_Device_FriendlyName failed (HRESULT: {:?})", hr)))?;

            let name = if prop_variant.vt == VT_LPWSTR {
                let name_pwstr = prop_variant.data.pwszVal;
                Self::pwstr_to_string(name_pwstr)
            } else {
                Err(AudioError::BackendSpecificError(format!(
                    "PKEY_Device_FriendlyName was not a string (VT: {:?})",
                    prop_variant.vt
                )))
            };
            PropVariantClear(&mut prop_variant).map_err(|hr| {
                AudioError::BackendSpecificError(format!(
                    "PropVariantClear failed (HRESULT: {:?})",
                    hr
                ))
            })?;
            name
        }
    }

    /// Determines the kind of device (Input or Output).
    fn kind(&self) -> AudioResult<DeviceKind> {
        // QueryInterface for IMMEndpoint
        let endpoint: IMMEndpoint = self.device.cast().map_err(|hr| {
            AudioError::BackendSpecificError(format!(
                "Failed to cast IMMDevice to IMMEndpoint (HRESULT: {:?})",
                hr
            ))
        })?;

        unsafe {
            let mut data_flow_val = Default::default();
            endpoint.GetDataFlow(&mut data_flow_val).map_err(|hr| {
                AudioError::BackendSpecificError(format!(
                    "IMMEndpoint::GetDataFlow failed (HRESULT: {:?})",
                    hr
                ))
            })?;

            match data_flow_val {
                eRender => Ok(DeviceKind::Output),
                eCapture => Ok(DeviceKind::Input),
                _ => Err(AudioError::BackendSpecificError(format!(
                    "Unknown data flow value: {:?}",
                    data_flow_val
                ))),
            }
        }
    }

    /// Gets the default audio format for this device in shared mode.
    /// Returns `Ok(None)` if the device does not have a default mix format.
    fn get_default_format(&self) -> AudioResult<Option<AudioFormat>> {
        unsafe {
            let audio_client: IAudioClient =
                self.device.Activate(CLSCTX_ALL, None).map_err(|hr| {
                    AudioError::BackendSpecificError(format!(
                        "IMMDevice::Activate(IAudioClient) failed (HRESULT: {:?})",
                        hr
                    ))
                })?;

            let wave_format_ptr = audio_client.GetMixFormat().map_err(|hr| {
                AudioError::BackendSpecificError(format!(
                    "IAudioClient::GetMixFormat failed (HRESULT: {:?})",
                    hr
                ))
            })?;

            if wave_format_ptr.is_null() {
                // This case might indicate no mix format is available or an error,
                // but GetMixFormat returning S_OK with null is unlikely.
                // More likely an error HRESULT would be returned.
                // However, to be safe, handle null.
                return Ok(None);
            }

            let audio_format = Self::waveformat_ex_to_audio_format(wave_format_ptr)?;
            CoTaskMemFree(Some(wave_format_ptr.cast()));
            Ok(Some(audio_format))
        }
    }

    /// Gets a list of supported audio formats for this device.
    /// For WASAPI, this can be complex. This implementation currently returns
    /// only the default format if available.
    fn get_supported_formats(&self) -> AudioResult<Vec<AudioFormat>> {
        // TODO: Implement more thorough format enumeration by trying various formats
        // with IAudioClient::IsFormatSupported in shared and exclusive modes.
        match self.get_default_format()? {
            Some(format) => Ok(vec![format]),
            None => Ok(Vec::new()),
        }
    }

    /// Checks if a specific audio format is supported by this device in shared mode.
    fn is_format_supported(&self, format_to_check: &AudioFormat) -> AudioResult<bool> {
        unsafe {
            let audio_client: IAudioClient =
                self.device.Activate(CLSCTX_ALL, None).map_err(|hr| {
                    AudioError::BackendSpecificError(format!(
                        "IMMDevice::Activate(IAudioClient) failed (HRESULT: {:?})",
                        hr
                    ))
                })?;

            let native_format_to_check = Self::audio_format_to_waveformat_ex(format_to_check)?;
            let mut closest_match_ptr: *mut WAVEFORMATEX = ptr::null_mut();

            let hr = audio_client.IsFormatSupported(
                AUDCLNT_SHAREMODE_SHARED,
                &native_format_to_check,
                Some(&mut closest_match_ptr),
            );

            if !closest_match_ptr.is_null() {
                CoTaskMemFree(Some(closest_match_ptr.cast()));
            }

            if hr == S_OK {
                Ok(true)
            } else if hr == S_FALSE {
                Ok(false) // Format not supported, closest_match_ptr might point to a suggestion
            } else if hr == E_NOTFOUND {
                // AUDCLNT_E_UNSUPPORTED_FORMAT
                Ok(false)
            } else {
                Err(AudioError::BackendSpecificError(format!(
                    "IAudioClient::IsFormatSupported failed (HRESULT: {:?})",
                    hr
                )))
            }
        }
    }

    /// Creates a new capturing audio stream for this device.
    ///
    /// # Arguments
    /// * `config` - The desired stream configuration (sample rate, channels, etc.).
    ///
    /// For this subtask (4.3), this method initializes the required WASAPI clients
    /// (`IAudioClient`, `IAudioCaptureClient`) and constructs a `WindowsAudioStream`
    /// instance. The actual streaming methods (`start`, `read_chunk`, etc.) on
    /// `WindowsAudioStream` will be implemented in a subsequent subtask (4.4).
    fn create_stream(&mut self, config: StreamConfig) -> AudioResult<Box<dyn CapturingStream>> {
        unsafe {
            let audio_client: IAudioClient =
                self.device.Activate(CLSCTX_ALL, None).map_err(|hr| {
                    AudioError::BackendSpecificError(format!(
                        "IMMDevice::Activate(IAudioClient) failed (HRESULT: {:?})",
                        hr
                    ))
                })?;

            let wave_format_ex =
                Self::audio_format_to_waveformat_ex(&config.format).map_err(|e| {
                    AudioError::InvalidParameter(format!(
                        "Failed to convert AudioFormat to WAVEFORMATEX for stream creation: {}",
                        e
                    ))
                })?;

            // For loopback capture, buffer duration and periodicity are often set to 0 for event-driven mode.
            // These might need to be configurable or calculated based on needs later.
            // AUDCLNT_STREAMFLAGS_LOOPBACK is key for capturing system audio.
            audio_client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK, // For capturing output
                    0,                            // hnsBufferDuration (0 for default/event-driven)
                    0,                            // hnsPeriodicity (0 for default)
                    &wave_format_ex,
                    None, // AudioSessionGuid (None for default)
                )
                .map_err(|hr| {
                    AudioError::BackendSpecificError(format!(
                        "IAudioClient::Initialize failed (HRESULT: {:?})",
                        hr
                    ))
                })?;

            let capture_client: IAudioCaptureClient = audio_client.GetService().map_err(|hr| {
                AudioError::BackendSpecificError(format!(
                    "IAudioClient::GetService(IAudioCaptureClient) failed (HRESULT: {:?})",
                    hr
                ))
            })?;

            // As per task 4.3, WindowsAudioStream methods are todo!(), but the struct is created.
            Ok(Box::new(WindowsAudioStream::new(
                audio_client,
                capture_client,
                wave_format_ex, // Store the format it was initialized with
                self.com_initializer.clone(),
            )))
        }
    }
}

/// Enumerates audio devices available on a Windows system using WASAPI.
#[derive(Debug)]
pub struct WindowsDeviceEnumerator {
    com_initializer: Arc<ComInitializer>, // Changed to Arc
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
            com_initializer, // Store Arc
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
            devices.push(Box::new(WindowsAudioDevice::new(
                imm_device,
                self.com_initializer.clone(),
            )));
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
            Ok(imm_device) => Ok(Some(Box::new(WindowsAudioDevice::new(
                imm_device,
                self.com_initializer.clone(),
            )))),
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
            Ok(imm_device) => Ok(Some(Box::new(WindowsAudioAudioDevice::new(
                imm_device,
                self.com_initializer.clone(),
            )))),
            Err(hr) if hr == E_NOTFOUND => Ok(None),
            Err(hr) => Err(AudioError::DeviceNotFound(format!(
                "Failed to get device by ID '{}' (HRESULT: {:?})",
                id, hr
            ))),
        }
    }
}

/// Represents an active audio capture stream on Windows using WASAPI.
///
/// This struct holds the necessary WASAPI client interfaces (`IAudioClient`, `IAudioCaptureClient`)
/// to manage and read data from an audio capture stream. It also holds an `Arc<ComInitializer>`
/// to ensure COM remains initialized for the lifetime of the stream.
#[derive(Debug)] // IAudioClient and IAudioCaptureClient are COM interface pointers.
pub(crate) struct WindowsAudioStream {
    audio_client: IAudioClient,
    capture_client: IAudioCaptureClient,
    #[allow(dead_code)] // Will be used when implementing read_chunk, etc.
    wave_format: WAVEFORMATEX, // Store the format it was initialized with
    _com_initializer: Arc<ComInitializer>, // Ensures COM is alive for the stream
}

impl WindowsAudioStream {
    /// Creates a new `WindowsAudioStream`.
    ///
    /// This is typically called by `WindowsAudioDevice::create_stream` after
    /// successfully initializing the `IAudioClient` and obtaining the `IAudioCaptureClient`.
    fn new(
        audio_client: IAudioClient,
        capture_client: IAudioCaptureClient,
        wave_format: WAVEFORMATEX,
        com_initializer: Arc<ComInitializer>,
    ) -> Self {
        Self {
            audio_client,
            capture_client,
            wave_format,
            _com_initializer: com_initializer,
        }
    }
}

impl CapturingStream for WindowsAudioStream {
    /// Starts the audio capture stream.
    /// After this call, the system will begin buffering audio data.
    fn start(&mut self) -> AudioResult<()> {
        // TODO: Implement in subtask 4.4: Call IAudioClient::Start()
        // unsafe { self.audio_client.Start().map_err(|hr| AudioError::StreamStartFailed(format!("IAudioClient::Start failed (HRESULT: {:?})", hr)))? };
        // Ok(())
        todo!("WindowsAudioStream::start()")
    }

    /// Stops the audio capture stream.
    /// After this call, the system will stop buffering audio data.
    fn stop(&mut self) -> AudioResult<()> {
        // TODO: Implement in subtask 4.4: Call IAudioClient::Stop()
        // unsafe { self.audio_client.Stop().map_err(|hr| AudioError::StreamStopFailed(format!("IAudioClient::Stop failed (HRESULT: {:?})", hr)))? };
        // Ok(())
        todo!("WindowsAudioStream::stop()")
    }

    /// Closes the audio stream, releasing any system resources.
    /// This method should be called when the stream is no longer needed.
    /// Note: `IAudioClient` and `IAudioCaptureClient` are COM objects and will be released
    /// when `WindowsAudioStream` is dropped. Explicit cleanup might involve stopping
    /// the stream if it's running.
    fn close(&mut self) -> AudioResult<()> {
        // TODO: Implement in subtask 4.4: Ensure stream is stopped. Resources are auto-released on drop.
        // if self.is_running() { self.stop()?; }
        // Ok(())
        todo!("WindowsAudioStream::close()")
    }

    /// Checks if the audio stream is currently capturing data.
    fn is_running(&self) -> bool {
        // TODO: Implement in subtask 4.4: Check stream state (e.g., internal flag set by start/stop)
        todo!("WindowsAudioStream::is_running()")
    }

    /// Reads a chunk of audio data from the stream.
    ///
    /// # Arguments
    /// * `timeout_ms` - An optional timeout in milliseconds to wait for data.
    ///                  If `None`, the call may block indefinitely or return immediately
    ///                  depending on the backend implementation.
    ///
    /// Returns `Ok(Some(buffer))` with audio data, `Ok(None)` if no data is
    /// available (e.g., on timeout or if the stream is not producing data),
    /// or an `AudioError` on failure.
    fn read_chunk(
        &mut self,
        _timeout_ms: Option<u32>,
    ) -> AudioResult<Option<Box<dyn AudioBuffer>>> {
        // TODO: Implement in subtask 4.4:
        // 1. Get packet size: unsafe { self.capture_client.GetNextPacketSize()? }
        // 2. If packet_size > 0:
        //    - Get buffer: unsafe { self.capture_client.GetBuffer(...) }
        //    - Create AudioBuffer from the data.
        //    - Release buffer: unsafe { self.capture_client.ReleaseBuffer(...) }
        // 3. Handle flags (e.g., AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY)
        // 4. Handle timeouts if event-driven mechanism is used.
        todo!("WindowsAudioStream::read_chunk()")
    }

    /// Converts this synchronous stream into an asynchronous stream.
    ///
    /// This allows the stream to be used in `async` contexts, typically by
    /// polling `read_chunk` in a separate task or thread.
    fn to_async_stream<'a>(
        &'a mut self,
    ) -> AudioResult<
        std::pin::Pin<
            Box<
                dyn futures_core::Stream<Item = AudioResult<Box<dyn AudioBuffer<Sample = f32>>>>
                    // Assuming f32 for now
                    + Send
                    + Sync
                    + 'a,
            >,
        >,
    > {
        // TODO: Implement in subtask 4.4 or later if async support is prioritized.
        // This would involve wrapping the synchronous read_chunk logic in a way
        // that conforms to the futures::Stream trait, possibly using a helper
        // struct and a channel or a dedicated thread for polling.
        todo!("WindowsAudioStream::to_async_stream()")
    }
}

// The AudioStream trait implementation for WindowsAudioStream is removed as per task focus on CapturingStream.
// If AudioStream methods (open, pause, resume etc.) are needed for CapturingStream,
// they would typically be part of the CapturingStream trait or called internally.
// For now, the CapturingStream methods (start, stop, close, is_running, read_chunk) are the focus.

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
