use std::{
    ffi::c_void,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use coreaudio::{
    audio_unit::{
        audio_unit_start, audio_unit_stop, render_callback, AudioUnit, Element, SampleFormat,
        StreamFormat,
    },
    sys::{
        kAudioUnitProperty_StreamFormat, kAudioUnitScope_Input, kAudioUnitScope_Output,
        kAudioUnitType_Output, AURenderCallbackStruct, OSStatus,
    },
};

use super::core::{
    AudioApplication, AudioCaptureBackend, AudioCaptureStream, AudioConfig, AudioError, AudioFormat,
};

pub struct CoreAudioBackend {
    device_list: Arc<Mutex<Vec<AudioApplication>>>,
}

impl CoreAudioBackend {
    pub fn new() -> Result<Self, AudioError> {
        // Initialize device list
        let device_list = Arc::new(Mutex::new(Vec::new()));
        
        // Create backend instance
        let backend = Self { device_list };
        
        // Initial device scan
        backend.refresh_device_list()?;
        
        Ok(backend)
    }

    fn refresh_device_list(&self) -> Result<(), AudioError> {
        let mut devices = Vec::new();

        // On macOS, we need to use Audio HAL to get the list of applications
        // This is a simplified version that creates virtual "applications"
        // In a production environment, you'd want to use the Audio HAL API
        // to get the actual list of applications playing audio
        devices.push(AudioApplication {
            name: "System Audio".to_string(),
            id: "system".to_string(),
            executable_name: "system".to_string(),
            pid: 0,
        });

        *self.device_list.lock().unwrap() = devices;
        Ok(())
    }
}

impl AudioCaptureBackend for CoreAudioBackend {
    fn name(&self) -> &'static str {
        "CoreAudio"
    }

    fn list_applications(&self) -> Result<Vec<AudioApplication>, AudioError> {
        self.refresh_device_list()?;
        Ok(self.device_list.lock().unwrap().clone())
    }

    fn capture_application(
        &self,
        app: &AudioApplication,
        config: AudioConfig,
    ) -> Result<Box<dyn AudioCaptureStream>, AudioError> {
        let stream = CoreAudioStream::new(app, config)?;
        Ok(Box::new(stream))
    }
}

struct CoreAudioStream {
    audio_unit: AudioUnit,
    config: AudioConfig,
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl CoreAudioStream {
    fn new(app: &AudioApplication, config: AudioConfig) -> Result<Self, AudioError> {
        // Create an AudioUnit for system output capture
        let mut audio_unit = AudioUnit::new(kAudioUnitType_Output)
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        // Configure the audio format
        let sample_format = match config.format {
            AudioFormat::F32LE => SampleFormat::F32,
            AudioFormat::S16LE => SampleFormat::I16,
            AudioFormat::S32LE => SampleFormat::I32,
        };

        let stream_format = StreamFormat {
            sample_rate: config.sample_rate as f64,
            sample_format,
            flags: coreaudio::audio_unit::Flags::IS_FLOAT
                | coreaudio::audio_unit::Flags::IS_NONINTERLEAVED,
            channels: config.channels,
        };

        // Set the stream format for input and output
        audio_unit
            .set_property(
                kAudioUnitProperty_StreamFormat,
                kAudioUnitScope_Output,
                0,
                Some(&stream_format),
            )
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        audio_unit
            .set_property(
                kAudioUnitProperty_StreamFormat,
                kAudioUnitScope_Input,
                1,
                Some(&stream_format),
            )
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        // Create a buffer for audio data
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let buffer_clone = Arc::clone(&buffer);

        // Set up the render callback
        let callback = move |_action_flags: *mut u32,
                           _time_stamp: *const c_void,
                           _bus_number: u32,
                           number_frames: u32,
                           io_data: *mut c_void|
              -> OSStatus {
            let data = unsafe {
                std::slice::from_raw_parts(
                    io_data as *const u8,
                    (number_frames as usize) * std::mem::size_of::<f32>(),
                )
            };
            
            buffer_clone.lock().unwrap().extend_from_slice(data);
            0
        };

        let callback = render_callback(callback);
        let callback = AURenderCallbackStruct {
            inputProc: Some(callback),
            inputProcRefCon: std::ptr::null_mut(),
        };

        audio_unit
            .set_property(
                coreaudio::sys::kAudioOutputUnitProperty_SetInputCallback,
                kAudioUnitScope_Input,
                0,
                Some(&callback),
            )
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        // Initialize the AudioUnit
        audio_unit
            .initialize()
            .map_err(|e| AudioError::InitializationFailed(e.to_string()))?;

        Ok(Self {
            audio_unit,
            config,
            buffer,
        })
    }
}

impl AudioCaptureStream for CoreAudioStream {
    fn start(&mut self) -> Result<(), AudioError> {
        audio_unit_start(&self.audio_unit)
            .map_err(|e| AudioError::CaptureError(e.to_string()))
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        audio_unit_stop(&self.audio_unit)
            .map_err(|e| AudioError::CaptureError(e.to_string()))
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, AudioError> {
        let mut internal_buffer = self.buffer.lock().unwrap();
        let bytes_to_copy = std::cmp::min(buffer.len(), internal_buffer.len());
        
        if bytes_to_copy > 0 {
            buffer[..bytes_to_copy].copy_from_slice(&internal_buffer[..bytes_to_copy]);
            internal_buffer.drain(..bytes_to_copy);
        }
        
        Ok(bytes_to_copy)
    }

    fn config(&self) -> &AudioConfig {
        &self.config
    }
}