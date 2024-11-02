use std::collections::VecDeque;
use std::ffi::OsString;
use sysinfo::{ProcessRefreshKind, RefreshKind, System};
use wasapi::*;

pub struct ProcessAudioCapture {
    audio_client: Option<AudioClient>,
    capture_client: Option<AudioCaptureClient>,
    format: WaveFormat,
    target_pid: u32,
}

impl ProcessAudioCapture {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        initialize_mta().ok().unwrap();

        Ok(Self {
            audio_client: None,
            capture_client: None,
            format: WaveFormat::new(32, 32, &SampleType::Float, 48000, 2, None),
            target_pid: 0,
        })
    }

    pub fn list_processes() -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let refreshes = RefreshKind::new().with_processes(ProcessRefreshKind::everything());
        let system = System::new_with_specifics(refreshes);
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

    pub fn init_for_process(
        &mut self,
        process_name: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let refreshes = RefreshKind::new().with_processes(ProcessRefreshKind::everything());
        let system = System::new_with_specifics(refreshes);
        let mut target_pid = 0;

        let process_name = OsString::from(process_name);
        for process in system.processes_by_name(&process_name) {
            // When capturing audio windows allows you to capture an app's entire process tree,
            // however you must ensure you use the parent as the target process ID
            target_pid = process.parent().unwrap_or(process.pid()).as_u32();
            break;
        }

        if target_pid == 0 {
            return Err(format!("Process {} not found", process_name.to_string_lossy()).into());
        }

        println!(
            "Found process {} with PID: {}",
            process_name.to_string_lossy(),
            target_pid
        );
        self.target_pid = target_pid;
        self.initialize()
    }

    fn initialize(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let include_tree = true;
        let autoconvert = true;

        // Create audio client for process-specific capture
        let mut audio_client =
            AudioClient::new_application_loopback_client(self.target_pid, include_tree)?;

        // Initialize audio client
        audio_client.initialize_client(
            &self.format,
            0,
            &Direction::Capture,
            &ShareMode::Shared,
            autoconvert,
        )?;

        // Get capture client
        let capture_client = audio_client.get_audiocaptureclient()?;

        self.audio_client = Some(audio_client);
        self.capture_client = Some(capture_client);

        println!("Audio capture initialized successfully");
        Ok(())
    }

    pub fn start(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(client) = &self.audio_client {
            client.start_stream()?;
        }
        Ok(())
    }

    pub fn stop(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(client) = &self.audio_client {
            client.stop_stream()?;
        }
        Ok(())
    }

    pub fn get_data(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        if let Some(capture_client) = &self.capture_client {
            let mut sample_queue: VecDeque<u8> = VecDeque::new();
            let new_frames = capture_client.get_next_nbr_frames()?.unwrap_or(0);

            if new_frames > 0 {
                let block_align = self.format.get_blockalign() as usize;
                let additional = (new_frames as usize * block_align)
                    .saturating_sub(sample_queue.capacity() - sample_queue.len());
                sample_queue.reserve(additional);

                capture_client.read_from_device_to_deque(&mut sample_queue)?;

                let data: Vec<u8> = sample_queue.into_iter().collect();
                if !data.is_empty() {
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
