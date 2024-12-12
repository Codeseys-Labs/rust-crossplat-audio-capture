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
        let devices = self.device_enumerator
            .get_device_collection::<DeviceCollection>()
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        let mut apps = Vec::new();
        for device in devices {
            if let Ok(device) = device {
                if let Ok(name) = device.get_friendlyname() {
                    apps.push(AudioApplication {
                        name: name.clone(),
                        id: name.clone(),
                        executable_name: format!("{}.exe", name),
                        pid: 0,  // We'll get this when capturing
                    });
                }
            }
        }

        Ok(apps)
    }

    fn capture_application(
        &self,
        app: &AudioApplication,
        config: AudioConfig,
    ) -> Result<Box<dyn AudioCaptureStream>, AudioError> {
        let devices = self.device_enumerator
            .get_device_collection::<DeviceCollection>()
            .map_err(|e| AudioError::DeviceNotFound(e.to_string()))?;

        for device in devices {
            if let Ok(device) = device {
                if let Ok(name) = device.get_friendlyname() {
                    if name.contains(&app.name) {
                        let stream = WasapiCaptureStream::new(device, config)?;
                        return Ok(Box::new(stream));
                    }
                }
            }
        }

        Err(AudioError::DeviceNotFound(format!(
            "No audio device found for application: {}", app.name
        )))
    }
}

pub struct WasapiCaptureStream {
    client: AudioClient,
    capture_client: AudioCaptureClient,
    buffer: VecDeque<u8>,
    config: AudioConfig,
}

impl WasapiCaptureStream {
    fn new(device: wasapi::Device, config: AudioConfig) -> Result<Self, AudioError> {
        let client = device
            .get_iaudioclient()
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        let wave_format = WaveFormat::new(
            32,  // bits per sample
            32,  // valid bits per sample
            &SampleType::Float,
            config.sample_rate,
            config.channels,
            None,
        );

        client
            .initialize_client(
                &wave_format,
                100_000, // 100ms buffer
                &Direction::Capture,
                &ShareMode::Shared,
                true,  // Allow format conversion
            )
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        let capture_client = client
            .get_audiocaptureclient()
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        Ok(Self {
            client,
            capture_client,
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

        // Get new frames
        let next_packet = self.capture_client
            .get_next_packet_size()
            .map_err(|e| AudioError::CaptureError(e.to_string()))?;

        if next_packet == 0 {
            return Ok(0);
        }

        let data = self.capture_client
            .get_buffer(next_packet)
            .map_err(|e| AudioError::CaptureError(e.to_string()))?;

        let bytes_to_copy = std::cmp::min(buffer.len(), data.len());
        buffer[..bytes_to_copy].copy_from_slice(&data[..bytes_to_copy]);

        self.capture_client
            .release_buffer(next_packet)
            .map_err(|e| AudioError::CaptureError(e.to_string()))?;

        Ok(bytes_to_copy)
    }

    fn config(&self) -> &AudioConfig {
        &self.config
    }
}