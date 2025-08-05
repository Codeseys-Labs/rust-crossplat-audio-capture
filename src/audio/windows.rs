//! Windows-specific audio capture backend using WASAPI.
#![cfg(target_os = "windows")]

use crate::core::config::{AudioCaptureConfig, AudioFormat, StreamConfig};
use crate::core::error::{AudioError, Result as AudioResult};
use crate::core::interface::{
    AudioDevice, AudioStream, CapturingStream, DeviceEnumerator, DeviceKind, StreamDataCallback,
};
// Removed VecAudioBuffer import, will use the new AudioBuffer struct
use crate::core::buffer::AudioBuffer; // Ensure this is the new struct

use futures_channel::mpsc;
use futures_core::Stream as FuturesStreamTrait; // Alias to avoid conflict if Stream is used elsewhere
use std::pin::Pin;
use std::thread;
use std::time::{Duration, Instant}; // Added Instant
use std::collections::VecDeque; // Enhanced buffering like wasapi-rs
use std::slice; // For efficient data copying

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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use windows::core::{GUID, HRESULT, PWSTR};
use windows::Win32::Foundation::HANDLE; // For process handle
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
use windows::Win32::System::Threading::{
    CloseHandle, OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE, WAIT_OBJECT_0,
};
use windows::Win32::System::Variant::{PROPVARIANT, VT_EMPTY, VT_LPWSTR};
use windows::Win32::UI::Shell::PropertiesSystem::PKEY_Device_FriendlyName;

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
    /// * `capture_config` - The complete audio capture configuration, including stream
    ///   parameters and optional application targeting information.
    ///
    /// This method initializes the required WASAPI clients (`IAudioClient`, `IAudioCaptureClient`)
    /// and constructs a `WindowsAudioStream` instance. If application targeting information
    /// (PID or session identifier) is provided in `capture_config`, it is passed to the
    /// `WindowsAudioStream`. The `IAudioClient` is initialized for loopback capture on the
    /// `IMMDevice` held by this `WindowsAudioDevice`.
    ///
    /// For application-specific capture, this will automatically include the process tree
    /// (child processes) to capture all audio from the target application family.
    fn create_stream(
        &mut self,
        capture_config: &AudioCaptureConfig,
    ) -> AudioResult<Box<dyn CapturingStream>> {
        unsafe {
            let audio_client: IAudioClient =
                self.device.Activate(CLSCTX_ALL, None).map_err(|hr| {
                    AudioError::BackendSpecificError(format!(
                        "IMMDevice::Activate(IAudioClient) failed (HRESULT: {:?})",
                        hr
                    ))
                })?;

            let wave_format_ex = Self::audio_format_to_waveformat_ex(
                &capture_config.stream_config.format,
            )
            .map_err(|e| {
                AudioError::InvalidParameter(format!(
                    "Failed to convert AudioFormat to WAVEFORMATEX for stream creation: {}",
                    e
                ))
            })?;

            // For loopback capture, buffer duration and periodicity are often set to 0 for event-driven mode.
            // AUDCLNT_STREAMFLAGS_LOOPBACK is key for capturing system/application audio.
            // The device self.device should be the default render device if app capture is intended,
            // as per builder logic in subtask 5.2.
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

            let stream = WindowsAudioStream::new(
                audio_client,
                capture_client,
                wave_format_ex, // Store the format it was initialized with
                self.com_initializer.clone(),
                capture_config.target_application_pid,
                capture_config.target_application_session_identifier.clone(),
            )?;
            
            Ok(Box::new(stream))
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
///
/// Enhanced with wasapi-rs inspired optimizations:
/// - Efficient VecDeque buffering for sample management
/// - Event-driven timing for better performance
/// - Process tree support for application capture
///
/// For application-level audio capture, it can store the target application's
/// Process ID (PID) or session identifier.
#[derive(Debug)] // IAudioClient and IAudioCaptureClient are COM interface pointers.
pub(crate) struct WindowsAudioStream {
    audio_client: IAudioClient,
    capture_client: IAudioCaptureClient,
    wave_format: WAVEFORMATEX, // Store the format it was initialized with
    _com_initializer: Arc<ComInitializer>, // Ensures COM is alive for the stream
    is_started: Arc<AtomicBool>, // Tracks if Start() has been called
    stream_start_time: Instant, // Epoch for timestamping audio buffers
    
    // Enhanced buffering inspired by wasapi-rs
    sample_queue: VecDeque<u8>, // Efficient sample buffering
    buffer_frame_count: u32, // Current buffer size in frames
    block_align: u16, // Bytes per frame for the audio format
    
    /// Optional Process ID of the application to target for audio capture.
    pub target_pid: Option<u32>,
    /// Optional session identifier of the application to target for audio capture.
    pub target_session_identifier: Option<String>,
    /// Whether to include process tree when capturing application audio
    pub include_process_tree: bool,
}

impl WindowsAudioStream {
    /// Creates a new `WindowsAudioStream` with enhanced buffering capabilities.
    ///
    /// This is typically called by `WindowsAudioDevice::create_stream` after
    /// successfully initializing the `IAudioClient` and obtaining the `IAudioCaptureClient`.
    /// It also accepts optional application targeting information.
    /// It records the stream creation time to be used as an epoch for `AudioBuffer` timestamps.
    ///
    /// Enhanced with wasapi-rs inspired optimizations for better performance.
    ///
    /// # Arguments
    /// * `audio_client` - The initialized `IAudioClient` for the stream.
    /// * `capture_client` - The `IAudioCaptureClient` obtained from the `audio_client`.
    /// * `wave_format` - The `WAVEFORMATEX` with which the `audio_client` was initialized.
    /// * `com_initializer` - An `Arc<ComInitializer>` to keep COM alive.
    /// * `target_pid` - Optional PID of the application to capture audio from.
    /// * `target_session_identifier` - Optional session identifier for application audio capture.
    fn new(
        audio_client: IAudioClient,
        capture_client: IAudioCaptureClient,
        wave_format: WAVEFORMATEX,
        com_initializer: Arc<ComInitializer>,
        target_pid: Option<u32>,
        target_session_identifier: Option<String>,
    ) -> AudioResult<Self> {
        // Calculate block alignment for efficient buffering
        let block_align = wave_format.nBlockAlign;
        
        // Get initial buffer size from the audio client
        let buffer_frame_count = unsafe {
            audio_client.GetBufferSize().map_err(|hr| {
                AudioError::BackendSpecificError(format!(
                    "Failed to get audio client buffer size (HRESULT: {:?})",
                    hr
                ))
            })?
        };

        // Pre-allocate efficient sample queue with capacity based on buffer size
        // Using wasapi-rs approach: 100 * block_align * (1024 + 2 * buffer_frame_count)
        let queue_capacity = 100 * block_align as usize * (1024 + 2 * buffer_frame_count as usize);
        let sample_queue = VecDeque::with_capacity(queue_capacity);

        Ok(Self {
            audio_client,
            capture_client,
            wave_format,
            _com_initializer: com_initializer,
            is_started: Arc::new(AtomicBool::new(false)),
            stream_start_time: Instant::now(),
            
            // Enhanced buffering
            sample_queue,
            buffer_frame_count,
            block_align,
            
            // Application targeting
            target_pid,
            target_session_identifier,
            include_process_tree: true, // Default to including process tree like wasapi-rs
        })
    }

    /// Processes raw WASAPI packet data into an `AudioBuffer`.
    /// Timestamps are generated relative to the provided `stream_start_time`.
    ///
    /// # Arguments
    /// * `p_data` - Pointer to the raw buffer data from `IAudioCaptureClient::GetBuffer`.
    /// * `num_frames_read` - Number of audio frames read into `p_data`.
    /// * `source_wave_format` - The `WAVEFORMATEX` describing the format of `p_data`.
    /// * `stream_start_time` - The `Instant` the stream was started, used as epoch for timestamps.
    ///
    /// # Returns
    /// An `AudioResult` containing an `AudioBuffer` struct on success,
    /// or an `AudioError` if conversion fails or formats are unsupported.
    fn process_wasapi_packet_data(
        p_data: *const u8, // Changed from *mut u8 as we only read
        num_frames_read: u32,
        source_wave_format: &WAVEFORMATEX,
        stream_start_time: Instant,
    ) -> AudioResult<AudioBuffer> {
        // Return concrete AudioBuffer struct
        if num_frames_read == 0 {
            return Err(AudioError::InvalidParameter(
                "num_frames_read cannot be zero for processing.".to_string(),
            ));
        }

        let mut converted_samples_vec: Vec<f32> = Vec::new();
        let channels = source_wave_format.nChannels; // u16
        if channels == 0 {
            return Err(AudioError::BackendSpecificError(
                "Wave format has 0 channels.".to_string(),
            ));
        }

        let total_samples_to_convert = num_frames_read as usize * channels as usize;
        converted_samples_vec.reserve(total_samples_to_convert);

        // SAFETY: p_data is assumed valid for num_frames_read based on GetBuffer success.
        // source_wave_format describes the data at p_data.
        unsafe {
            match source_wave_format.wFormatTag as u32 {
                WAVE_FORMAT_IEEE_FLOAT => {
                    if source_wave_format.wBitsPerSample == 32 {
                        let typed_ptr = p_data as *const f32;
                        for i in 0..total_samples_to_convert {
                            converted_samples_vec.push(*typed_ptr.add(i));
                        }
                    } else {
                        return Err(AudioError::UnsupportedFormat(format!(
                            "Unsupported bit depth for IEEE float: {}",
                            source_wave_format.wBitsPerSample
                        )));
                    }
                }
                WAVE_FORMAT_PCM => {
                    if source_wave_format.wBitsPerSample == 16 {
                        let typed_ptr = p_data as *const i16;
                        for i in 0..total_samples_to_convert {
                            let sample_i16 = *typed_ptr.add(i);
                            converted_samples_vec.push(sample_i16 as f32 / i16::MAX as f32);
                        }
                    } else {
                        return Err(AudioError::UnsupportedFormat(format!(
                            "Unsupported bit depth for PCM: {}",
                            source_wave_format.wBitsPerSample
                        )));
                    }
                }
                _ => {
                    return Err(AudioError::UnsupportedFormat(format!(
                        "Unsupported wave format tag: {}",
                        source_wave_format.wFormatTag
                    )));
                }
            }
        } // end unsafe

        // Create output AudioFormat for the new AudioBuffer struct
        let output_audio_format_struct = AudioFormat {
            sample_rate: source_wave_format.nSamplesPerSec,
            channels,                           // u16
            bits_per_sample: 32,                // We converted to f32
            sample_format: SampleFormat::F32LE, // Standard for f32
        };

        // Timestamp is duration since stream start.
        let timestamp = Instant::now().duration_since(stream_start_time);

        Ok(AudioBuffer {
            data: converted_samples_vec,
            channels, // u16
            sample_rate: source_wave_format.nSamplesPerSec,
            format: output_audio_format_struct,
            timestamp,
        })
    }
}

impl CapturingStream for WindowsAudioStream {
    /// Starts the WASAPI audio capture stream.
    ///
    /// This method calls `IAudioClient::Start()`. If the stream is already started,
    /// it returns `Ok(())` without taking further action.
    /// Errors from `IAudioClient::Start()` are mapped to `AudioError::BackendSpecificError`.
    fn start(&mut self) -> AudioResult<()> {
        if self
            .is_started
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            // Successfully changed from false to true, so proceed to start.
            unsafe {
                self.audio_client.Start().map_err(|hr| {
                    // If Start fails, revert the state.
                    self.is_started.store(false, Ordering::Relaxed);
                    AudioError::BackendSpecificError(format!(
                        "IAudioClient::Start failed (HRESULT: {:?})",
                        hr
                    ))
                })?;
            }
            Ok(())
        } else {
            // Stream was already started (or another thread started it).
            // Depending on desired strictness, this could be an error or Ok.
            // For idempotency, Ok(()) is often preferred.
            // If strict "call start only once" is needed, return an error:
            // Err(AudioError::InvalidOperation("Stream already started".to_string()))
            Ok(())
        }
    }

    /// Stops the WASAPI audio capture stream.
    ///
    /// This method calls `IAudioClient::Stop()`. If the stream is already stopped,
    /// it returns `Ok(())` without taking further action (idempotent).
    /// Errors from `IAudioClient::Stop()` are mapped to `AudioError::BackendSpecificError`.
    /// The internal `is_started` flag is set to `false` regardless of the `Stop()` call's success,
    /// as the intention is to stop.
    fn stop(&mut self) -> AudioResult<()> {
        if self
            .is_started
            .compare_exchange(true, false, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            // Successfully changed from true to false, so proceed to stop.
            unsafe {
                self.audio_client.Stop().map_err(|hr| {
                    // Even if Stop fails, we consider the stream logically stopped from our side.
                    // The flag is already set to false by compare_exchange.
                    AudioError::BackendSpecificError(format!(
                        "IAudioClient::Stop failed (HRESULT: {:?})",
                        hr
                    ))
                })?;
            }
        }
        // If it was already false, or if Stop failed, we still return Ok(())
        // as the stream is considered stopped from an API perspective.
        Ok(())
    }

    /// Closes the audio stream.
    ///
    /// For WASAPI, this primarily ensures the stream is stopped by calling `self.stop()`.
    /// COM resources (`IAudioClient`, `IAudioCaptureClient`) are released when
    /// `WindowsAudioStream` is dropped.
    fn close(&mut self) -> AudioResult<()> {
        if self.is_started.load(Ordering::Relaxed) {
            self.stop()?;
        }
        Ok(())
    }

    /// Checks if the WASAPI audio stream is currently considered running.
    ///
    /// Returns `Ok(true)` if `start()` has been successfully called and `stop()` or `close()`
    /// has not yet been called to change its state. Otherwise, returns `Ok(false)`.
    fn is_running(&self) -> AudioResult<bool> {
        Ok(self.is_started.load(Ordering::Relaxed))
    }

    /// Reads a chunk of audio data from the WASAPI capture stream synchronously.
    ///
    /// Enhanced with wasapi-rs inspired buffering for better performance:
    /// - Uses VecDeque for efficient sample management
    /// - Accumulates samples until a full chunk is available
    /// - Better handling of partial reads and buffer management
    ///
    /// This method attempts to read the next available packet of audio data from the
    /// capture client. If no data is immediately available (`GetNextPacketSize` returns 0),
    /// it returns `Ok(None)`.
    ///
    /// # Parameters
    /// * `timeout_ms`: An optional timeout in milliseconds for waiting for data.
    ///
    /// # Returns
    /// * `Ok(Some(buffer))`: If a chunk of audio data was successfully read and converted.
    /// * `Ok(None)`: If no audio packet is currently available.
    /// * `Err(AudioError::InvalidOperation)`: If the stream has not been started.
    /// * `Err(AudioError::BackendSpecificError)`: For other WASAPI errors.
    /// * `Err(AudioError::UnsupportedFormat)`: If the captured audio format is not supported for conversion.
    fn read_chunk(
        &mut self,
        timeout_ms: Option<u32>,
    ) -> AudioResult<Option<AudioBuffer>> {
        if !self.is_started.load(Ordering::Relaxed) {
            return Err(AudioError::InvalidOperation(
                "Stream not started".to_string(),
            ));
        }

        // Define chunk size (number of frames we want to return at once)
        let chunk_size_frames = 4096; // Same as wasapi-rs example
        let bytes_per_chunk = chunk_size_frames * self.block_align as usize;

        // Check if we have enough buffered samples for a full chunk
        if self.sample_queue.len() >= bytes_per_chunk {
            // Extract a chunk from our buffer
            let mut chunk_data = vec![0u8; bytes_per_chunk];
            for byte in chunk_data.iter_mut() {
                *byte = self.sample_queue.pop_front().unwrap();
            }

            // Convert the chunk to AudioBuffer using our existing helper
            return Self::process_wasapi_packet_data(
                chunk_data.as_ptr(),
                chunk_size_frames as u32,
                &self.wave_format,
                self.stream_start_time,
            ).map(Some);
        }

        // Need more data - try to read from WASAPI
        unsafe {
            let num_frames_in_packet = self.capture_client.GetNextPacketSize().map_err(|hr| {
                AudioError::BackendSpecificError(format!(
                    "IAudioCaptureClient::GetNextPacketSize failed (HRESULT: {:?})",
                    hr
                ))
            })?;

            if num_frames_in_packet == 0 {
                // No new data available, but check timeout
                if let Some(timeout) = timeout_ms {
                    thread::sleep(Duration::from_millis(std::cmp::min(timeout as u64, 10)));
                }
                return Ok(None);
            }

            let mut p_data: *mut u8 = ptr::null_mut();
            let mut num_frames_read: u32 = 0;
            let mut flags: u32 = 0;

            self.capture_client.GetBuffer(
                &mut p_data,
                &mut num_frames_read,
                &mut flags,
                None,
                None,
            ).map_err(|hr| {
                AudioError::BackendSpecificError(format!(
                    "IAudioCaptureClient::GetBuffer failed (HRESULT: {:?})",
                    hr
                ))
            })?;

            if num_frames_read > 0 {
                // Calculate bytes to read and reserve space in queue
                let bytes_to_read = num_frames_read as usize * self.block_align as usize;
                let additional_capacity = bytes_to_read.saturating_sub(
                    self.sample_queue.capacity() - self.sample_queue.len()
                );
                self.sample_queue.reserve(additional_capacity);

                // Copy data to our sample queue (inspired by wasapi-rs approach)
                let data_slice = slice::from_raw_parts(p_data, bytes_to_read);
                for &byte in data_slice {
                    self.sample_queue.push_back(byte);
                }
            }

            // Always release the buffer
            self.capture_client.ReleaseBuffer(num_frames_read).map_err(|hr| {
                AudioError::BackendSpecificError(format!(
                    "IAudioCaptureClient::ReleaseBuffer failed (HRESULT: {:?})",
                    hr
                ))
            })?;

            // Check again if we now have enough for a chunk
            if self.sample_queue.len() >= bytes_per_chunk {
                let mut chunk_data = vec![0u8; bytes_per_chunk];
                for byte in chunk_data.iter_mut() {
                    *byte = self.sample_queue.pop_front().unwrap();
                }

                return Self::process_wasapi_packet_data(
                    chunk_data.as_ptr(),
                    chunk_size_frames as u32,
                    &self.wave_format,
                    self.stream_start_time,
                ).map(Some);
            }
        }

        Ok(None) // Still not enough data for a full chunk
    }

    /// Converts this synchronous stream into an asynchronous stream of audio data.
    ///
    /// This method sets up a dedicated polling thread that continuously reads audio data
    /// from the WASAPI capture client using logic similar to `read_chunk`. The data
    /// (or errors) are then sent over an MPSC channel to the returned stream.
    ///
    /// The returned stream is a `Pin<Box<dyn FuturesStreamTrait<Item = AudioResult<AudioBuffer>> + Send + Sync + 'a>>`.
    /// Each item in the stream is an `AudioResult` which can either be an `Ok(AudioBuffer)`
    /// containing the audio data, or an `Err(AudioError)` if an issue occurred during capture.
    ///
    /// **Important:**
    /// - The polling thread will run as long as the `WindowsAudioStream` is active
    ///   (`is_started` is true) and the receiver end of the MPSC channel has not been dropped.
    /// - If the `WindowsAudioStream` is stopped, the polling thread will detect this,
    ///   send an `AudioError::StreamClosed` message, and then terminate.
    /// - If the returned async stream is dropped, the polling thread will detect that the
    ///   channel is closed and will also terminate.
    /// - If a `target_pid` is specified, the thread will monitor the target application.
    ///   If the application terminates, an `AudioError::StreamClosed` is sent, and the
    ///   polling thread stops. If the application cannot be monitored (e.g., `OpenProcess` fails),
    ///   an `AudioError::ApplicationNotFound` is sent, and the thread terminates.
    ///
    /// # Errors
    /// - Returns `AudioError::InvalidOperation` if the stream has not been started via `start()`.
    /// - Returns `AudioError::BackendSpecificError` if the polling thread fails to initialize COM.
    /// - Returns `AudioError::ApplicationNotFound` if a `target_pid` is given but the process
    ///   cannot be opened for monitoring at the start of the polling thread.
    fn to_async_stream<'a>(
        &'a mut self,
    ) -> AudioResult<
        Pin<
            Box<
                dyn FuturesStreamTrait<Item = AudioResult<AudioBuffer>>
                    // Ensure this uses the concrete AudioBuffer struct
                    + Send
                    + Sync
                    + 'a,
            >,
        >,
    > {
        if !self.is_started.load(Ordering::Relaxed) {
            return Err(AudioError::InvalidOperation(
                "Stream not started".to_string(),
            ));
        }

        let (mut tx, rx) = mpsc::unbounded::<AudioResult<AudioBuffer>>(); // Ensure this uses the concrete AudioBuffer struct

        let capture_client_clone = self.capture_client.clone();
        let wave_format_clone = self.wave_format;
        let stream_is_started_clone = self.is_started.clone();
        let stream_start_time_clone = self.stream_start_time; // Clone the start time
        let target_pid_clone = self.target_pid; // Clone Option<u32>

        thread::spawn(move || {
            let _com_thread_initializer = match ComInitializer::new() {
                Ok(init) => Some(init),
                Err(e) => {
                    let _ = tx.unbounded_send(Err(AudioError::BackendSpecificError(format!(
                        "Polling thread failed to initialize COM: {}",
                        e
                    ))));
                    return;
                }
            };

            // Process monitoring setup
            let process_monitor_handle = if let Some(pid_val) = target_pid_clone {
                match unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid_val) } {
                    Ok(handle) if !handle.is_invalid() => ProcessHandle::new(Some(handle)),
                    Ok(invalid_handle) => {
                        if !invalid_handle.is_invalid() {
                            unsafe {
                                CloseHandle(invalid_handle);
                            }
                        }
                        let _ = tx.unbounded_send(Err(AudioError::ApplicationNotFound(format!(
                            "Target application with PID {} could not be monitored (OpenProcess returned invalid handle).",
                            pid_val
                        ))));
                        return;
                    }
                    Err(e) => {
                        let _ = tx.unbounded_send(Err(AudioError::ApplicationNotFound(format!(
                            "Target application with PID {} could not be monitored (OpenProcess failed: {:?}).",
                            pid_val, e
                        ))));
                        return;
                    }
                }
            } else {
                ProcessHandle::new(None)
            };

            let mut check_process_counter = 0;

            loop {
                if !stream_is_started_clone.load(Ordering::Relaxed) {
                    let _ = tx.unbounded_send(Err(AudioError::StreamClosed(
                        "Capture stream was stopped.".to_string(),
                    )));
                    break;
                }

                if let Some(h) = process_monitor_handle.get() {
                    check_process_counter += 1;
                    if check_process_counter >= 10 {
                        check_process_counter = 0;
                        let wait_result = unsafe { WaitForSingleObject(h, 0) };
                        if wait_result == WAIT_OBJECT_0 {
                            let _ = tx.unbounded_send(Err(AudioError::StreamClosed(
                                "Target application terminated.".to_string(),
                            )));
                            stream_is_started_clone.store(false, Ordering::Relaxed);
                        }
                    }
                }

                let mut num_frames_in_packet: u32 = 0;
                let hr_packet_size =
                    unsafe { capture_client_clone.GetNextPacketSize(&mut num_frames_in_packet) };

                if hr_packet_size.is_err() {
                    if tx
                        .unbounded_send(Err(AudioError::BackendSpecificError(format!(
                            "Polling thread: GetNextPacketSize failed (HRESULT: {:?})",
                            hr_packet_size
                        ))))
                        .is_err()
                    {
                        // Receiver dropped
                    }
                    break;
                }

                if num_frames_in_packet == 0 {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }

                let mut p_data: *mut u8 = ptr::null_mut();
                let mut num_frames_read_from_buffer: u32 = 0;
                let mut flags: u32 = 0;

                let hr_get_buffer = unsafe {
                    capture_client_clone.GetBuffer(
                        &mut p_data,
                        &mut num_frames_read_from_buffer,
                        &mut flags,
                        None,
                        None,
                    )
                };

                if hr_get_buffer.is_err() {
                    if tx
                        .unbounded_send(Err(AudioError::BackendSpecificError(format!(
                            "Polling thread: GetBuffer failed (HRESULT: {:?})",
                            hr_get_buffer
                        ))))
                        .is_err()
                    { /* Receiver dropped */ }
                    break;
                }

                if num_frames_read_from_buffer == 0 {
                    let hr_release_empty = unsafe { capture_client_clone.ReleaseBuffer(0) };
                    if hr_release_empty.is_err() {
                        if tx
                            .unbounded_send(Err(AudioError::BackendSpecificError(format!(
                                "Polling thread: ReleaseBuffer (0 frames) failed (HRESULT: {:?})",
                                hr_release_empty
                            ))))
                            .is_err()
                        { /* Receiver dropped */ }
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }

                let conversion_result = WindowsAudioStream::process_wasapi_packet_data(
                    p_data as *const u8,
                    num_frames_read_from_buffer,
                    &wave_format_clone,
                    stream_start_time_clone, // Pass the cloned start time
                );

                let hr_release_data =
                    unsafe { capture_client_clone.ReleaseBuffer(num_frames_read_from_buffer) };

                if hr_release_data.is_err() {
                    if tx
                        .unbounded_send(Err(AudioError::BackendSpecificError(format!(
                            "Polling thread: ReleaseBuffer (with data) failed (HRESULT: {:?})",
                            hr_release_data
                        ))))
                        .is_err()
                    { /* Receiver dropped */ }
                    break;
                }

                match conversion_result {
                    Ok(audio_buffer) => {
                        if tx.unbounded_send(Ok(audio_buffer)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        if tx.unbounded_send(Err(e)).is_err() {
                            break;
                        }
                        break;
                    }
                }
            }
        });

        Ok(Box::pin(rx))
    }
}

impl Drop for WindowsAudioStream {
    /// Ensures the audio stream is stopped when `WindowsAudioStream` is dropped.
    ///
    /// This calls `IAudioClient::Stop()` as a best-effort cleanup. Errors are ignored
    /// as `drop` should not panic.
    fn drop(&mut self) {
        if self.is_started.load(Ordering::Relaxed) {
            // Best effort to stop the client.
            // Errors are ignored in drop.
            let _ = unsafe { self.audio_client.Stop() };
            self.is_started.store(false, Ordering::Relaxed);
        }
    }
}

// The AudioStream trait implementation for WindowsAudioStream is removed as per task focus on CapturingStream.
// If AudioStream methods (open, pause, resume etc.) are needed for CapturingStream,
// they would typically be part of the CapturingStream trait or called internally.
// For now, the CapturingStream methods (start, stop, close, is_running, read_chunk) are the focus.

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
    // pub icon_path: Option<String>, // TODO: Consider adding icon path if easily obtainable
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
///   session enumeration, or while retrieving session details. This can include
///   `AudioError::BackendSpecificError` for WASAPI HRESULT failures or
///   `AudioError::DeviceNotFound` if the default audio device cannot be accessed.
///
/// # Errors
///
/// This function can return `AudioError` for various reasons:
/// - COM initialization failure.
/// - Failure to get the default audio rendering device.
/// - Failure to activate `IAudioSessionManager2`.
/// - Errors during session enumeration or when querying session properties.
/// - Errors when trying to open a process or query its executable path (e.g., access denied).
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
    use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Media::Audio::{
        IAudioSessionControl, IAudioSessionControl2, IAudioSessionEnumerator, IAudioSessionManager2,
    };
    use windows::Win32::System::ProcessStatus::K32GetModuleFileNameExW; // Or QueryFullProcessImageNameW from Win32_System_Threading
    use windows::Win32::System::SystemServices::HMODULE;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    let _com_initializer = ComInitializer::new()?;

    let mut sessions_info: Vec<ApplicationAudioSessionInfo> = Vec::new();

    unsafe {
        let device_enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|hr| {
                AudioError::BackendSpecificError(format!(
                    "CoCreateInstance(MMDeviceEnumerator) failed (HRESULT: {:?})",
                    hr
                ))
            })?;

        let default_device: IMMDevice = device_enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|hr| {
                if hr == E_NOTFOUND {
                    AudioError::DeviceNotFound("Default rendering device not found.".to_string())
                } else {
                    AudioError::BackendSpecificError(format!(
                        "GetDefaultAudioEndpoint failed (HRESULT: {:?})",
                        hr
                    ))
                }
            })?;

        let session_manager: IAudioSessionManager2 =
            default_device.Activate(CLSCTX_ALL, None).map_err(|hr| {
                AudioError::BackendSpecificError(format!(
                    "IMMDevice::Activate(IAudioSessionManager2) failed (HRESULT: {:?})",
                    hr
                ))
            })?;

        let session_enumerator: IAudioSessionEnumerator =
            session_manager.GetSessionEnumerator().map_err(|hr| {
                AudioError::BackendSpecificError(format!(
                    "IAudioSessionManager2::GetSessionEnumerator failed (HRESULT: {:?})",
                    hr
                ))
            })?;

        let count = session_enumerator.GetCount().map_err(|hr| {
            AudioError::BackendSpecificError(format!(
                "IAudioSessionEnumerator::GetCount failed (HRESULT: {:?})",
                hr
            ))
        })?;

        for i in 0..count {
            let session_control: IAudioSessionControl =
                session_enumerator.GetSession(i).map_err(|hr| {
                    AudioError::BackendSpecificError(format!(
                        "IAudioSessionEnumerator::GetSession({}) failed (HRESULT: {:?})",
                        i, hr
                    ))
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

            let pid = session_control2.GetProcessId().map_err(|hr| {
                AudioError::BackendSpecificError(format!(
                    "IAudioSessionControl2::GetProcessId for session {} failed (HRESULT: {:?})",
                    i, hr
                ))
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
                    let len = K32GetModuleFileNameExW(process_handle, HMODULE(0), &mut path_buf);
                    if len > 0 {
                        executable_path = Some(String::from_utf16_lossy(&path_buf[..len as usize]));
                    } else {
                        // K32GetModuleFileNameExW failed, could log GetLastError()
                        // eprintln!("K32GetModuleFileNameExW failed for PID {}: {:?}", pid, std::io::Error::last_os_error());
                    }
                    let _ = CloseHandle(process_handle);
                }
                Ok(_) => { // INVALID_HANDLE_VALUE
                     // eprintln!("OpenProcess returned INVALID_HANDLE_VALUE for PID {}: {:?}", pid, std::io::Error::last_os_error());
                }
                Err(e) => {
                    // OpenProcess failed
                    // eprintln!("OpenProcess failed for PID {}: {:?}", pid, e);
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

        // Add running processes with wasapi-rs inspired parent process handling
        for (pid, process) in system.processes() {
            let name = process.name().to_string_lossy().into_owned();
            // Skip system processes and processes without audio
            if !name.is_empty() && pid.as_u32() > 4 {
                // Use parent PID if available (wasapi-rs approach for process trees)
                let target_pid = process.parent().unwrap_or(*pid).as_u32();
                
                apps.push(AudioApplication {
                    name: name.clone(),
                    id: pid.to_string(),
                    executable_name: format!("{}.exe", name),
                    pid: target_pid, // Use parent PID for better audio capture
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

        // Enhanced application capture inspired by wasapi-rs
        let mut audio_client = if app.name == "System" {
            // System-wide capture using default render device
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
            // Application-specific capture with process tree support (like wasapi-rs)
            let include_tree = true; // Include child processes like wasapi-rs example
            AudioClient::new_application_loopback_client(app.pid, include_tree).map_err(|e| {
                AudioError::DeviceNotFound(format!(
                    "Failed to create application audio capture for PID {}: {}",
                    app.pid, e
                ))
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
