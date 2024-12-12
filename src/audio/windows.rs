use std::collections::VecDeque;
use std::sync::Arc;
use wasapi::{
    self,
    AudioClient,
    AudioCaptureClient,
    Direction,
    ShareMode,
    WaveFormat,
    SampleType,
};
use super::core::{
    AudioApplication, AudioCaptureBackend, AudioCaptureStream, AudioConfig,
    AudioError, AudioFormat,
};

pub struct WasapiBackend {
    initialized: bool,
}

impl WasapiBackend {
    pub fn new() -> Result<Self, AudioError> {
        wasapi::initialize_mta()
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;
        
        Ok(Self { initialized: true })
    }
}

impl Drop for WasapiBackend {
    fn drop(&mut self) {
        if self.initialized {
            wasapi::deinitialize();
        }
    }
}

impl AudioCaptureBackend for WasapiBackend {
    fn name(&self) -> &'static str {
        "WASAPI"
    }

    fn list_applications(&self) -> Result<Vec<AudioApplication>, AudioError> {
        // Use sysinfo to get running processes
        use sysinfo::{ProcessRefreshKind, RefreshKind, System};
        let refreshes = RefreshKind::new().with_processes(ProcessRefreshKind::everything());
        let system = System::new_with_specifics(refreshes);
        
        let mut apps = Vec::new();
        for (pid, process) in system.processes() {
            // Only include processes that might produce audio
            apps.push(AudioApplication {
                name: process.name().to_string(),
                id: pid.to_string(),
                executable_name: process.name().to_string(),
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
        let stream = WasapiCaptureStream::new(app.pid, config)?;
        Ok(Box::new(stream))
    }
}

pub struct WasapiCaptureStream {
    client: AudioClient,
    capture_client: AudioCaptureClient,
    event_handle: wasapi::Handle,
    buffer: VecDeque<u8>,
    config: AudioConfig,
}

impl WasapiCaptureStream {
    fn new(process_id: u32, config: AudioConfig) -> Result<Self, AudioError> {
        let desired_format = WaveFormat::new(
            match config.format {
                AudioFormat::F32LE => 32,
                AudioFormat::S16LE => 16,
                AudioFormat::S32LE => 32,
            },
            match config.format {
                AudioFormat::F32LE => 32,
                AudioFormat::S16LE => 16,
                AudioFormat::S32LE => 32,
            },
            &match config.format {
                AudioFormat::F32LE => SampleType::Float,
                AudioFormat::S16LE | AudioFormat::S32LE => SampleType::Int,
            },
            config.sample_rate,
            config.channels,
            None,
        );

        let include_process_tree = true;
        let mut client = AudioClient::new_application_loopback_client(process_id, include_process_tree)
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        client.initialize_client(
            &desired_format,
            0,  // Use default buffer duration
            &Direction::Capture,
            &ShareMode::Shared,
            true,  // Allow format conversion
        ).map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        let event_handle = client.set_get_eventhandle()
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        let capture_client = client.get_audiocaptureclient()
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        Ok(Self {
            client,
            capture_client,
            event_handle,
            buffer: VecDeque::new(),
            config,
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

        // Wait for new data
        if self.event_handle.wait_for_event(100).is_err() {
            return Ok(0);  // No data available
        }

        // Get new frames
        if let Ok(Some(frames)) = self.capture_client.get_next_nbr_frames() {
            if frames > 0 {
                self.capture_client.read_from_device_to_deque(&mut self.buffer)
                    .map_err(|e| AudioError::CaptureError(e.to_string()))?;
                
                let bytes_to_copy = std::cmp::min(buffer.len(), self.buffer.len());
                for i in 0..bytes_to_copy {
                    buffer[i] = self.buffer.pop_front().unwrap();
                }
                return Ok(bytes_to_copy);
            }
        }

        Ok(0)
    }

    fn config(&self) -> &AudioConfig {
        &self.config
    }
}