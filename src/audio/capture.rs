use std::fmt;

#[cfg(target_os = "windows")]
use std::collections::VecDeque;
#[cfg(target_os = "windows")]
use std::ffi::OsString;

#[cfg(target_os = "windows")]
use wasapi::*;

#[cfg(target_os = "windows")]
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

/// Error types for audio capture operations
#[derive(Debug)]
#[allow(dead_code)] // Used on Windows, fallback stubs on other platforms
pub enum AudioCaptureError {
    ProcessNotFound(String),
    WasapiError(String),
    InitializationError(String),
}

impl std::error::Error for AudioCaptureError {}

impl fmt::Display for AudioCaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioCaptureError::ProcessNotFound(msg) => write!(f, "Process not found: {}", msg),
            AudioCaptureError::WasapiError(msg) => write!(f, "WASAPI error: {}", msg),
            AudioCaptureError::InitializationError(msg) => {
                write!(f, "Initialization error: {}", msg)
            }
        }
    }
}

impl From<Box<dyn std::error::Error>> for AudioCaptureError {
    fn from(error: Box<dyn std::error::Error>) -> Self {
        AudioCaptureError::WasapiError(error.to_string())
    }
}

#[cfg(target_os = "windows")]
pub struct ProcessAudioCapture {
    audio_client: Option<AudioClient>,
    capture_client: Option<AudioCaptureClient>,
    format: WaveFormat,
    target_pid: u32,
    verbose: bool,
}

#[cfg(target_os = "windows")]
impl ProcessAudioCapture {
    pub fn new() -> Result<Self, AudioCaptureError> {
        let _ = initialize_mta();

        Ok(Self {
            audio_client: None,
            capture_client: None,
            format: WaveFormat::new(32, 32, &SampleType::Float, 48000, 2, None),
            target_pid: 0,
            verbose: false,
        })
    }

    pub fn set_verbose(&mut self, verbose: bool) {
        self.verbose = verbose;
    }

    pub fn list_processes() -> Result<Vec<String>, AudioCaptureError> {
        let mut system = System::new_with_specifics(
            RefreshKind::everything().with_processes(ProcessRefreshKind::everything()),
        );
        system.refresh_processes(ProcessesToUpdate::All, true);
        let mut processes = Vec::new();

        for (_, process) in system.processes() {
            processes.push(process.name().to_owned());
        }

        processes.sort();
        Ok(processes
            .into_iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect())
    }

    pub fn init_for_process(&mut self, process_name: &str) -> Result<(), AudioCaptureError> {
        let mut system = System::new_with_specifics(
            RefreshKind::everything().with_processes(ProcessRefreshKind::everything()),
        );
        system.refresh_processes(ProcessesToUpdate::All, true);
        let mut target_pid = 0;

        let process_name = OsString::from(process_name);
        for process in system.processes_by_name(&process_name) {
            // When capturing audio windows allows you to capture an app's entire process tree,
            // however you must ensure you use the parent as the target process ID
            target_pid = process.parent().unwrap_or(process.pid()).as_u32();
            break;
        }

        if target_pid == 0 {
            return Err(AudioCaptureError::ProcessNotFound(
                process_name.to_string_lossy().into_owned(),
            ));
        }

        if self.verbose {
            println!(
                "Found process {} with PID: {}",
                process_name.to_string_lossy(),
                target_pid
            );
        }
        self.target_pid = target_pid;
        self.initialize()
    }

    fn initialize(&mut self) -> Result<(), AudioCaptureError> {
        let include_tree = true;
        let autoconvert = true;

        // Create audio client for process-specific capture
        let mut audio_client =
            AudioClient::new_application_loopback_client(self.target_pid, include_tree)
                .map_err(|e| AudioCaptureError::InitializationError(e.to_string()))?;

        // Initialize audio client with correct wasapi-rs signature
        let stream_mode = wasapi::StreamMode::EventsShared {
            autoconvert,
            buffer_duration_hns: 0,
        };
        audio_client
            .initialize_client(&self.format, &Direction::Capture, &stream_mode)
            .map_err(|e| AudioCaptureError::InitializationError(e.to_string()))?;

        // Get capture client
        let capture_client = audio_client
            .get_audiocaptureclient()
            .map_err(|e| AudioCaptureError::InitializationError(e.to_string()))?;

        self.audio_client = Some(audio_client);
        self.capture_client = Some(capture_client);

        if self.verbose {
            println!("Audio capture initialized successfully");
        }
        Ok(())
    }

    pub fn start(&self) -> Result<(), AudioCaptureError> {
        if let Some(client) = &self.audio_client {
            client
                .start_stream()
                .map_err(|e| AudioCaptureError::WasapiError(e.to_string()))?;
        }
        Ok(())
    }

    pub fn stop(&self) -> Result<(), AudioCaptureError> {
        if let Some(client) = &self.audio_client {
            client
                .stop_stream()
                .map_err(|e| AudioCaptureError::WasapiError(e.to_string()))?;
        }
        Ok(())
    }

    pub fn get_data(&self) -> Result<Vec<u8>, AudioCaptureError> {
        if let Some(capture_client) = &self.capture_client {
            let mut sample_queue: VecDeque<u8> = VecDeque::new();
            let new_frames = capture_client
                .get_next_packet_size()
                .map_err(|e| AudioCaptureError::WasapiError(e.to_string()))?
                .unwrap_or(0);

            if new_frames > 0 {
                let block_align = self.format.get_blockalign() as usize;
                let additional = (new_frames as usize * block_align)
                    .saturating_sub(sample_queue.capacity() - sample_queue.len());
                sample_queue.reserve(additional);

                capture_client
                    .read_from_device_to_deque(&mut sample_queue)
                    .map_err(|e| AudioCaptureError::WasapiError(e.to_string()))?;

                let data: Vec<u8> = sample_queue.into_iter().collect();
                if !data.is_empty() && self.verbose {
                    println!("Got {} bytes of audio data", data.len());
                }
                return Ok(data);
            }
        }
        Ok(Vec::new())
    }

    pub fn channels(&self) -> Option<i32> {
        Some(self.format.get_nchannels() as i32)
    }

    pub fn sample_rate(&self) -> Option<i32> {
        Some(self.format.get_samplespersec() as i32)
    }

    pub fn bits_per_sample(&self) -> Option<i32> {
        Some(self.format.get_bitspersample() as i32)
    }
}

/// Fallback stub for non-Windows platforms
#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
pub struct ProcessAudioCapture {
    verbose: bool,
}

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
impl ProcessAudioCapture {
    pub fn new() -> Result<Self, AudioCaptureError> {
        Ok(Self { verbose: false })
    }

    pub fn set_verbose(&mut self, verbose: bool) {
        self.verbose = verbose;
    }

    pub fn list_processes() -> Result<Vec<String>, AudioCaptureError> {
        Ok(Vec::new())
    }

    pub fn init_for_process(&mut self, _process_name: &str) -> Result<(), AudioCaptureError> {
        Err(AudioCaptureError::InitializationError(
            "WASAPI is only available on Windows".to_string(),
        ))
    }

    pub fn start(&self) -> Result<(), AudioCaptureError> {
        Err(AudioCaptureError::InitializationError(
            "WASAPI is only available on Windows".to_string(),
        ))
    }

    pub fn stop(&self) -> Result<(), AudioCaptureError> {
        Err(AudioCaptureError::InitializationError(
            "WASAPI is only available on Windows".to_string(),
        ))
    }

    pub fn get_data(&self) -> Result<Vec<u8>, AudioCaptureError> {
        Err(AudioCaptureError::InitializationError(
            "WASAPI is only available on Windows".to_string(),
        ))
    }

    pub fn channels(&self) -> Option<i32> {
        None
    }

    pub fn sample_rate(&self) -> Option<i32> {
        None
    }

    pub fn bits_per_sample(&self) -> Option<i32> {
        None
    }
}
