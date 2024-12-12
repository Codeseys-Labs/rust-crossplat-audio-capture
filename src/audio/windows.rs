use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use wasapi::{
    self,
    AudioClient,
    AudioCaptureClient,
    Direction,
    ShareMode,
    WaveFormat,
    SampleType,
    DeviceCollection,
    Role,
};
use windows::Win32::System::Com;
use sysinfo::{ProcessRefreshKind, RefreshKind, System};
use super::core::{
    AudioApplication, AudioCaptureBackend, AudioCaptureStream, AudioConfig,
    AudioError, AudioFormat,
};

pub struct WasapiBackend {
    device_enumerator: wasapi::Enumerator,
}

impl WasapiBackend {
    pub fn new() -> Result<Self, AudioError> {
        wasapi::initialize_mta()
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;
        
        let device_enumerator = wasapi::Enumerator::new()
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;
        
        Ok(Self { device_enumerator })
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
        let refreshes = RefreshKind::new().with_processes(ProcessRefreshKind::everything());
        let system = System::new_with_specifics(refreshes);
        
        let mut apps = Vec::new();
        for process in system.processes_by_name("") {
            if let Ok(name) = process.name() {
                apps.push(AudioApplication {
                    name: name.to_string(),
                    id: process.pid().to_string(),
                    executable_name: format!("{}.exe", name),
                    pid: process.pid().as_u32(),
                });
            }
        }

        Ok(apps)
    }

    fn capture_application(
        &self,
        app: &AudioApplication,
        config: AudioConfig,
    ) -> Result<Box<dyn AudioCaptureStream>, AudioError> {
        let wave_format = WaveFormat::new(
            32,  // bits per sample
            32,  // valid bits per sample
            &SampleType::Float,
            config.sample_rate,
            config.channels,
            None,
        );

        let audio_client = AudioClient::new_application_loopback_client(
            app.pid,
            true, // include_tree - capture audio from child processes too
        ).map_err(|e| AudioError::DeviceNotFound(e.to_string()))?;

        audio_client.initialize_client(
            &wave_format,
            0, // buffer duration in 100ns units, 0 for default
            &Direction::Capture,
            &ShareMode::Shared,
            true, // allow format conversion
        ).map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        let stream = WasapiCaptureStream::new(audio_client, config)?;
        Ok(Box::new(stream))
    }
}

pub struct WasapiCaptureStream {
    client: AudioClient,
    capture_client: AudioCaptureClient,
    buffer: VecDeque<u8>,
    config: AudioConfig,
    event_handle: Option<wasapi::Handle>,
}

impl WasapiCaptureStream {
    fn new(client: AudioClient, config: AudioConfig) -> Result<Self, AudioError> {
        let event_handle = client.set_get_eventhandle()
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
        // If we have enough data in the buffer, return it
        if !self.buffer.is_empty() {
            let bytes_to_copy = std::cmp::min(buffer.len(), self.buffer.len());
            for i in 0..bytes_to_copy {
                buffer[i] = self.buffer.pop_front().unwrap();
            }
            return Ok(bytes_to_copy);
        }

        // Get new frames
        let new_frames = self.capture_client
            .get_next_nbr_frames()?
            .unwrap_or(0);

        if new_frames > 0 {
            // Calculate additional buffer space needed
            let blockalign = (self.config.channels * 4) as usize; // 4 bytes per sample (32-bit float)
            let additional = (new_frames as usize * blockalign)
                .saturating_sub(self.buffer.capacity() - self.buffer.len());
            self.buffer.reserve(additional);

            // Read data into our buffer
            self.capture_client
                .read_from_device_to_deque(&mut self.buffer)
                .map_err(|e| AudioError::CaptureError(e.to_string()))?;
        }

        // Wait for more data if needed
        if let Some(event_handle) = &self.event_handle {
            if event_handle.wait_for_event(3000).is_err() {
                return Err(AudioError::CaptureError("Timeout waiting for audio data".to_string()));
            }
        }

        // Try to fill the output buffer again
        let bytes_to_copy = std::cmp::min(buffer.len(), self.buffer.len());
        for i in 0..bytes_to_copy {
            buffer[i] = self.buffer.pop_front().unwrap();
        }
        Ok(bytes_to_copy)
    }

    fn config(&self) -> &AudioConfig {
        &self.config
    }
}