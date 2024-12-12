use std::sync::Arc;
use wasapi::{
    self,
    AudioClient,
    Device,
    DeviceCollection,
    Direction,
    Role,
    WaveFormat,
};
use super::core::{
    AudioApplication, AudioCaptureBackend, AudioCaptureStream, AudioConfig,
    AudioError, AudioFormat,
};

pub struct WasapiBackend {
    device_enumerator: wasapi::Enumerator,
}

impl WasapiBackend {
    pub fn new() -> Result<Self, AudioError> {
        let device_enumerator = wasapi::Enumerator::new()
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;
        
        Ok(Self { device_enumerator })
    }

    fn get_process_device(&self, app: &AudioApplication) -> Result<Device, AudioError> {
        let devices = self.device_enumerator
            .get_device_collection::<DeviceCollection>()
            .map_err(|e| AudioError::DeviceNotFound(e.to_string()))?;

        for device in devices {
            if let Ok(device) = device {
                // TODO: Implement proper device-to-process matching
                // Current WASAPI implementation needs improvement here
                if let Ok(name) = device.get_friendlyname() {
                    if name.contains(&app.name) {
                        return Ok(device);
                    }
                }
            }
        }

        Err(AudioError::DeviceNotFound(format!(
            "No audio device found for application: {}", app.name
        )))
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
                    // TODO: Improve process detection
                    // Currently just creating dummy entries for testing
                    apps.push(AudioApplication {
                        name: name.clone(),
                        id: name,
                        executable_name: "unknown.exe".to_string(),
                        pid: 0,
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
        let device = self.get_process_device(app)?;
        
        let stream = WasapiCaptureStream::new(device, config)?;
        Ok(Box::new(stream))
    }
}

pub struct WasapiCaptureStream {
    client: AudioClient,
    capture_client: AudioCaptureClient,
    config: AudioConfig,
}

impl WasapiCaptureStream {
    fn new(device: Device, config: AudioConfig) -> Result<Self, AudioError> {
        let client = device
            .get_iaudioclient()
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        let wave_format = WaveFormat::new(
            config.channels as u16,
            config.sample_rate as u32,
            match config.format {
                AudioFormat::F32LE => 32,
                AudioFormat::S16LE => 16,
                AudioFormat::S32LE => 32,
            },
        );

        client
            .initialize(
                &wave_format,
                100_000, // 100ms buffer
                wasapi::Direction::Render,
                wasapi::ShareMode::Shared,
                true,
            )
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        let capture_client = client
            .get_audiocaptureclient()
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        Ok(Self {
            client,
            capture_client,
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
        let next_packet = self.capture_client
            .get_buffer()
            .map_err(|e| AudioError::CaptureError(e.to_string()))?;

        if next_packet.is_empty() {
            return Ok(0);
        }

        let bytes_to_copy = std::cmp::min(buffer.len(), next_packet.len());
        buffer[..bytes_to_copy].copy_from_slice(&next_packet[..bytes_to_copy]);

        Ok(bytes_to_copy)
    }

    fn config(&self) -> &AudioConfig {
        &self.config
    }
}