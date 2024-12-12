use std::collections::VecDeque;
use wasapi::{
    self,
    AudioClient,
    AudioCaptureClient,
    Direction,
    ShareMode,
    WaveFormat,
    SampleType,
};
use sysinfo::{System, SystemExt, ProcessExt};
use super::core::{
    AudioApplication, AudioCaptureBackend, AudioCaptureStream, AudioConfig,
    AudioError,
};

pub struct WasapiBackend {
    system: System,
}

impl WasapiBackend {
    pub fn new() -> Result<Self, AudioError> {
        if let Err(e) = wasapi::initialize_mta() {
            return Err(AudioError::InitializationFailed(e.to_string()));
        }
        
        let system = System::new_all();
        Ok(Self { system })
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
        self.system.refresh_processes();
        
        let mut apps = Vec::new();
        for (pid, process) in self.system.processes() {
            let name = process.name().to_string();
            apps.push(AudioApplication {
                name: name.clone(),
                id: pid.to_string(),
                executable_name: format!("{}.exe", name),
                pid: pid.as_u32(),
            });
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

// Make WasapiCaptureStream Send-safe
unsafe impl Send for WasapiCaptureStream {}

impl WasapiCaptureStream {
    fn new(client: AudioClient, config: AudioConfig) -> Result<Self, AudioError> {
        let event_handle = if let Err(e) = client.set_get_eventhandle() {
            return Err(AudioError::InitializationFailed(e.to_string()));
        } else {
            None
        };

        let capture_client = if let Err(e) = client.get_audiocaptureclient() {
            return Err(AudioError::InitializationFailed(e.to_string()));
        } else {
            client.get_audiocaptureclient().unwrap()
        };

        Ok(Self {
            client,
            capture_client,
            buffer: VecDeque::new(),
            config,
            event_handle,
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
        let new_frames = match self.capture_client.get_next_packet_size() {
            Ok(frames) => frames,
            Err(e) => return Err(AudioError::CaptureError(e.to_string())),
        };

        if new_frames > 0 {
            // Get the data
            let data = match self.capture_client.get_buffer(new_frames) {
                Ok(data) => data,
                Err(e) => return Err(AudioError::CaptureError(e.to_string())),
            };

            // Copy data to our buffer
            self.buffer.extend(data.iter().copied());

            // Release the buffer
            if let Err(e) = self.capture_client.release_buffer(new_frames) {
                return Err(AudioError::CaptureError(e.to_string()));
            }
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