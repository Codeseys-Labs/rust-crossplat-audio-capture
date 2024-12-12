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
};
use windows::Win32::System::Com;
use sysinfo::{System, SystemExt, ProcessExt};
use super::core::{
    AudioApplication, AudioCaptureBackend, AudioCaptureStream, AudioConfig,
    AudioError, AudioFormat,
};

pub struct WasapiBackend {
    initialized: bool,
}

impl WasapiBackend {
    pub fn new() -> Result<Self, AudioError> {
        unsafe {
            if let Err(e) = Com::CoInitializeEx(None, Com::COINIT_MULTITHREADED) {
                return Err(AudioError::InitializationFailed(format!("Failed to initialize COM: {:?}", e)));
            }
        }
        
        Ok(Self { initialized: true })
    }
}

impl Drop for WasapiBackend {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                Com::CoUninitialize();
            }
        }
    }
}

impl AudioCaptureBackend for WasapiBackend {
    fn name(&self) -> &'static str {
        "WASAPI"
    }

    fn list_applications(&self) -> Result<Vec<AudioApplication>, AudioError> {
        let mut system = System::new();
        system.refresh_processes();
        
        let mut apps = Vec::new();
        for (pid, process) in system.processes() {
            // Only include processes that might produce audio
            apps.push(AudioApplication {
                name: process.name().to_owned(),
                id: pid.to_string(),
                executable_name: process.name().to_owned(),
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
    client: Arc<Mutex<AudioClient>>,
    capture_client: Arc<Mutex<AudioCaptureClient>>,
    event_handle: Arc<wasapi::Handle>,
    buffer: Arc<Mutex<VecDeque<u8>>>,
    config: AudioConfig,
}

// SAFETY: The Arc<Mutex<...>> makes the stream Send-safe
unsafe impl Send for WasapiCaptureStream {}

impl WasapiCaptureStream {
    fn new(process_id: u32, config: AudioConfig) -> Result<Self, AudioError> {
        let desired_format = WaveFormat::new(
            config.channels.into(),
            config.sample_rate.try_into().unwrap(),
            &match config.format {
                AudioFormat::F32LE => SampleType::Float,
                AudioFormat::S16LE | AudioFormat::S32LE => SampleType::Int,
            },
        );

        let include_process_tree = true;
        let mut client = AudioClient::new_application_loopback_client(process_id, include_process_tree)
            .map_err(|e| AudioError::InitializationFailed(format!("Failed to create audio client: {:?}", e)))?;

        client.initialize_client(
            &desired_format,
            0,  // Use default buffer duration
            &Direction::Capture,
            &ShareMode::Shared,
            true,  // Allow format conversion
        ).map_err(|e| AudioError::InitializationFailed(format!("Failed to initialize client: {:?}", e)))?;

        let event_handle = client.set_get_eventhandle()
            .map_err(|e| AudioError::InitializationFailed(format!("Failed to get event handle: {:?}", e)))?;

        let capture_client = client.get_audiocaptureclient()
            .map_err(|e| AudioError::InitializationFailed(format!("Failed to get capture client: {:?}", e)))?;

        Ok(Self {
            client: Arc::new(Mutex::new(client)),
            capture_client: Arc::new(Mutex::new(capture_client)),
            event_handle: Arc::new(event_handle),
            buffer: Arc::new(Mutex::new(VecDeque::new())),
            config,
        })
    }
}

impl AudioCaptureStream for WasapiCaptureStream {
    fn start(&mut self) -> Result<(), AudioError> {
        let mut client = self.client.lock()
            .map_err(|e| AudioError::CaptureError(format!("Failed to lock client: {:?}", e)))?;
        client.start_stream()
            .map_err(|e| AudioError::CaptureError(format!("Failed to start stream: {:?}", e)))
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        let mut client = self.client.lock()
            .map_err(|e| AudioError::CaptureError(format!("Failed to lock client: {:?}", e)))?;
        client.stop_stream()
            .map_err(|e| AudioError::CaptureError(format!("Failed to stop stream: {:?}", e)))
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, AudioError> {
        // If we have enough data in the buffer, return it
        {
            let mut internal_buffer = self.buffer.lock()
                .map_err(|e| AudioError::CaptureError(format!("Failed to lock buffer: {:?}", e)))?;
            if !internal_buffer.is_empty() {
                let bytes_to_copy = std::cmp::min(buffer.len(), internal_buffer.len());
                for i in 0..bytes_to_copy {
                    buffer[i] = internal_buffer.pop_front().unwrap();
                }
                return Ok(bytes_to_copy);
            }
        }

        // Wait for new data
        if self.event_handle.wait_for_event(100).is_err() {
            return Ok(0);  // No data available
        }

        // Get new frames
        let mut capture_client = self.capture_client.lock()
            .map_err(|e| AudioError::CaptureError(format!("Failed to lock capture client: {:?}", e)))?;

        if let Ok(Some(frames)) = capture_client.get_next_nbr_frames() {
            if frames > 0 {
                let mut internal_buffer = self.buffer.lock()
                    .map_err(|e| AudioError::CaptureError(format!("Failed to lock buffer: {:?}", e)))?;
                
                capture_client.read_from_device_to_deque(&mut internal_buffer)
                    .map_err(|e| AudioError::CaptureError(format!("Failed to read from device: {:?}", e)))?;
                
                let bytes_to_copy = std::cmp::min(buffer.len(), internal_buffer.len());
                for i in 0..bytes_to_copy {
                    buffer[i] = internal_buffer.pop_front().unwrap();
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