use super::core::{
    AudioApplication, AudioCaptureBackend, AudioCaptureStream, AudioConfig, AudioError,
};
use std::collections::VecDeque;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
use wasapi::{self, AudioCaptureClient, AudioClient, Direction, SampleType, ShareMode, WaveFormat};

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
            if !name.is_empty() && pid.as_u32() > 4 {  // Skip system processes (PIDs 0-4)
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
        config: AudioConfig,
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
            // System-wide audio capture using default render device in loopback mode
            AudioClient::new_default_render_device_loopback()
                .map_err(|e| AudioError::DeviceNotFound(format!("Failed to create system audio capture: {}", e)))?
        } else {
            // Process-specific audio capture
            AudioClient::new_application_loopback_client(
                app.pid,
                true, // include_tree - capture audio from child processes too
            )
            .map_err(|e| AudioError::DeviceNotFound(format!("Failed to create process audio capture: {}", e)))?
        };

        audio_client
            .initialize_client(
                &wave_format,
                0, // buffer duration in 100ns units, 0 for default
                &Direction::Capture,
                &ShareMode::Shared,
                true, // allow format conversion
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
    config: AudioConfig,
    event_handle: Option<wasapi::Handle>,
    format: WaveFormat,
}

// SAFETY: This type is Send because:
// 1. AudioClient and AudioCaptureClient contain COM objects that are thread-safe
//    by design (COM handles synchronization internally)
// 2. VecDeque<u8> is Send
// 3. AudioConfig and WaveFormat are Send
// 4. wasapi::Handle is Send
// 5. All methods properly synchronize access to shared resources
// 6. No shared mutable state exists between threads
// 7. The underlying COM objects are designed for multi-threaded use
unsafe impl Send for WasapiCaptureStream {}

impl WasapiCaptureStream {
    fn new(
        client: AudioClient,
        config: AudioConfig,
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
        // If we have enough data in the buffer, return it
        if !self.buffer.is_empty() {
            let bytes_to_copy = std::cmp::min(buffer.len(), self.buffer.len());
            for i in 0..bytes_to_copy {
                buffer[i] = self.buffer.pop_front().unwrap();
            }
            return Ok(bytes_to_copy);
        }

        // Get new frames
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

            // Read data directly into our buffer
            self.capture_client
                .read_from_device_to_deque(&mut self.buffer)
                .map_err(|e| AudioError::CaptureError(e.to_string()))?;
        }

        // Wait for more data if needed
        if let Some(event_handle) = &self.event_handle {
            if event_handle.wait_for_event(3000).is_err() {
                return Err(AudioError::CaptureError(
                    "Timeout waiting for audio data".to_string(),
                ));
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