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
use super::core::{AudioApplication, AudioCaptureBackend, AudioCaptureStream};
use std::collections::VecDeque;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
use wasapi::{
    self, get_default_device, AudioCaptureClient, AudioClient, Direction, SampleType, ShareMode,
    WaveFormat,
};

// --- New Skeleton Implementations ---

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WindowsDeviceId(String); // Example: Use a String for now

pub struct WindowsAudioDevice {
    id: WindowsDeviceId,
    name: String,
    kind: DeviceKind, // To determine if it's input or output
                      // TODO: Add other necessary fields, e.g., WASAPI device reference
}

impl AudioDevice for WindowsAudioDevice {
    type DeviceId = WindowsDeviceId;

    fn get_id(&self) -> Self::DeviceId {
        println!("TODO: WindowsAudioDevice::get_id()");
        self.id.clone()
    }

    fn get_name(&self) -> String {
        println!("TODO: WindowsAudioDevice::get_name()");
        self.name.clone()
    }

    fn get_supported_formats(&self) -> AudioResult<Vec<AudioFormat>> {
        println!("TODO: WindowsAudioDevice::get_supported_formats()");
        todo!()
    }

    fn get_default_format(&self) -> AudioResult<AudioFormat> {
        println!("TODO: WindowsAudioDevice::get_default_format()");
        todo!()
    }

    fn is_input(&self) -> bool {
        println!("TODO: WindowsAudioDevice::is_input()");
        self.kind == DeviceKind::Input
    }

    fn is_output(&self) -> bool {
        println!("TODO: WindowsAudioDevice::is_output()");
        self.kind == DeviceKind::Output
    }

    fn is_active(&self) -> bool {
        println!("TODO: WindowsAudioDevice::is_active()");
        // TODO: Implement actual status check
        false
    }

    fn is_format_supported(&self, format: &AudioFormat) -> AudioResult<bool> {
        println!(
            "TODO: WindowsAudioDevice::is_format_supported({:?})",
            format
        );
        // For now, assume all formats are supported or let the actual stream creation fail.
        // Later tasks will implement actual format checking.
        Ok(true)
    }
}

pub struct WindowsDeviceEnumerator;

impl DeviceEnumerator for WindowsDeviceEnumerator {
    type Device = WindowsAudioDevice;

    fn enumerate_devices(&self) -> AudioResult<Vec<Self::Device>> {
        println!("TODO: WindowsDeviceEnumerator::enumerate_devices()");
        todo!()
    }

    fn get_default_device(&self, kind: DeviceKind) -> AudioResult<Self::Device> {
        println!(
            "TODO: WindowsDeviceEnumerator::get_default_device({:?})",
            kind
        );
        todo!()
    }

    fn get_input_devices(&self) -> AudioResult<Vec<Self::Device>> {
        println!("TODO: WindowsDeviceEnumerator::get_input_devices()");
        todo!()
    }

    fn get_output_devices(&self) -> AudioResult<Vec<Self::Device>> {
        println!("TODO: WindowsDeviceEnumerator::get_output_devices()");
        todo!()
    }

    fn get_device_by_id(
        &self,
        id: &<Self::Device as AudioDevice>::DeviceId,
    ) -> AudioResult<Self::Device> {
        println!("TODO: WindowsDeviceEnumerator::get_device_by_id({:?})", id);
        todo!()
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
