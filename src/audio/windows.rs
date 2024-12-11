use std::sync::Arc;
use wasapi::{
    AudioClient, AudioClientProperties, AudioClientShareMode, AudioCaptureClient, Device,
    DeviceCollection, Direction, Role, WaveFormat,
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

        // Set low-latency mode
        client
            .set_client_properties(&AudioClientProperties {
                cbsize: std::mem::size_of::<AudioClientProperties>() as u32,
                bIsOffload: false,
                eCategory: Role::Console,
                Options: 0,
            })
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
            .initialize_client(
                &wave_format,
                100_000, // 100ms buffer
                AudioClientShareMode::Shared,
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