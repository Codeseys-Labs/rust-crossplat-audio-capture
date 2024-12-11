use std::{
    ffi::{c_void, CStr},
    mem,
    os::raw::c_char,
    ptr,
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
        kAudioDevicePropertyDeviceNameCFString, kAudioDevicePropertyDeviceUID,
        kAudioDevicePropertyStreamConfiguration, kAudioObjectPropertyElement_Output,
        kAudioObjectPropertyScope_Global, kAudioObjectPropertyScope_Output,
        kAudioUnitProperty_StreamFormat, kAudioUnitScope_Input, kAudioUnitScope_Output,
        kAudioUnitType_Output, AudioBuffer, AudioBufferList, AudioObjectAddPropertyListener,
        AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
        AudioObjectPropertyAddress, AudioObjectRemovePropertyListener, AURenderCallbackStruct,
        OSStatus,
    },
};

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectSetPropertyData(
        inObjectID: AudioObjectID,
        inPropertyAddress: *const AudioObjectPropertyAddress,
        inQualifierDataSize: u32,
        inQualifierData: *const c_void,
        inDataSize: u32,
        inData: *const c_void,
    ) -> OSStatus;
}

use core_foundation::{
    base::TCFType,
    string::{CFString, CFStringRef},
};

use super::core::{
    AudioApplication, AudioCaptureBackend, AudioCaptureStream, AudioConfig, AudioError, AudioFormat,
};

const kAudioHardwarePropertyProcessIsMain: u32 = b'main' as u32;
const kAudioHardwarePropertyProcessIsMaster: u32 = b'mast' as u32;
const kProcessAudioProperty: u32 = b'paud' as u32;

#[repr(C)]
struct ProcessAudioInfo {
    pid: u32,
    is_input_master: bool,
    is_output_master: bool,
    volume: f32,
    muted: bool,
    app_name: [c_char; 256],
    bundle_id: [c_char; 256],
}

pub struct CoreAudioBackend {
    device_list: Arc<Mutex<Vec<AudioApplication>>>,
    system_device_id: AudioObjectID,
}

impl CoreAudioBackend {
    pub fn new() -> Result<Self, AudioError> {
        // Initialize device list
        let device_list = Arc::new(Mutex::new(Vec::new()));
        
        // Get the system output device ID
        let system_device_id = unsafe { Self::get_default_output_device()? };
        
        // Create backend instance
        let backend = Self { 
            device_list,
            system_device_id,
        };
        
        // Initial device scan
        backend.refresh_device_list()?;
        
        Ok(backend)
    }

    unsafe fn get_default_output_device() -> Result<AudioObjectID, AudioError> {
        let address = AudioObjectPropertyAddress {
            mSelector: coreaudio::sys::kAudioHardwarePropertyDefaultOutputDevice,
            mScope: kAudioObjectPropertyScope_Global,
            mElement: kAudioObjectPropertyElement_Output,
        };

        let mut device_id: AudioObjectID = 0;
        let mut size = mem::size_of::<AudioObjectID>() as u32;

        let status = AudioObjectGetPropertyData(
            coreaudio::sys::kAudioObjectSystemObject,
            &address as *const _,
            0,
            ptr::null(),
            &mut size as *mut _,
            &mut device_id as *mut _ as *mut c_void,
        );

        if status != 0 {
            return Err(AudioError::DeviceNotFound(
                "Failed to get default output device".into(),
            ));
        }

        Ok(device_id)
    }

    fn get_device_name(device_id: AudioObjectID) -> Result<String, AudioError> {
        unsafe {
            let address = AudioObjectPropertyAddress {
                mSelector: kAudioDevicePropertyDeviceNameCFString,
                mScope: kAudioObjectPropertyScope_Global,
                mElement: kAudioObjectPropertyElement_Output,
            };

            let mut name_ref: CFStringRef = ptr::null();
            let mut size = mem::size_of::<CFStringRef>() as u32;

            let status = AudioObjectGetPropertyData(
                device_id,
                &address as *const _,
                0,
                ptr::null(),
                &mut size as *mut _,
                &mut name_ref as *mut _ as *mut c_void,
            );

            if status != 0 {
                return Err(AudioError::DeviceNotFound("Failed to get device name".into()));
            }

            let cf_string = CFString::wrap_under_create_rule(name_ref);
            Ok(cf_string.to_string())
        }
    }

    fn get_running_applications() -> Result<Vec<ProcessAudioInfo>, AudioError> {
        unsafe {
            let address = AudioObjectPropertyAddress {
                mSelector: kProcessAudioProperty,
                mScope: kAudioObjectPropertyScope_Global,
                mElement: kAudioObjectPropertyElement_Output,
            };

            let mut size: u32 = 0;
            let status = AudioObjectGetPropertyDataSize(
                coreaudio::sys::kAudioObjectSystemObject,
                &address as *const _,
                0,
                ptr::null(),
                &mut size as *mut _,
            );

            if status != 0 {
                return Err(AudioError::CaptureError(
                    "Failed to get process list size".into(),
                ));
            }

            let count = size as usize / mem::size_of::<ProcessAudioInfo>();
            let mut processes = Vec::with_capacity(count);
            let mut buffer = vec![ProcessAudioInfo {
                pid: 0,
                is_input_master: false,
                is_output_master: false,
                volume: 0.0,
                muted: false,
                app_name: [0; 256],
                bundle_id: [0; 256],
            }; count];

            let status = AudioObjectGetPropertyData(
                coreaudio::sys::kAudioObjectSystemObject,
                &address as *const _,
                0,
                ptr::null(),
                &mut size as *mut _,
                buffer.as_mut_ptr() as *mut c_void,
            );

            if status != 0 {
                return Err(AudioError::CaptureError(
                    "Failed to get process list".into(),
                ));
            }

            for info in buffer {
                if info.pid != 0 {
                    processes.push(info);
                }
            }

            Ok(processes)
        }
    }

    fn refresh_device_list(&self) -> Result<(), AudioError> {
        let mut devices = Vec::new();

        // Add system audio as a fallback
        devices.push(AudioApplication {
            name: "System Audio".to_string(),
            id: self.system_device_id.to_string(),
            executable_name: "system".to_string(),
            pid: 0,
        });

        // Get applications playing audio
        if let Ok(processes) = Self::get_running_applications() {
            for process in processes {
                let app_name = unsafe {
                    CStr::from_ptr(process.app_name.as_ptr())
                        .to_string_lossy()
                        .into_owned()
                };

                let bundle_id = unsafe {
                    CStr::from_ptr(process.bundle_id.as_ptr())
                        .to_string_lossy()
                        .into_owned()
                };

                devices.push(AudioApplication {
                    name: app_name.clone(),
                    id: format!("app_{}", process.pid),
                    executable_name: bundle_id,
                    pid: process.pid,
                });
            }
        }

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
    target_pid: u32,
}

impl CoreAudioStream {
    fn new(app: &AudioApplication, config: AudioConfig) -> Result<Self, AudioError> {
        // Create an AudioUnit for output capture
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

        // Get the target PID from the application ID
        let target_pid = if app.id.starts_with("app_") {
            app.id[4..].parse().unwrap_or(0)
        } else {
            0 // System audio
        };

        // Set up process-specific audio capture
        if target_pid != 0 {
            unsafe {
                let address = AudioObjectPropertyAddress {
                    mSelector: kAudioHardwarePropertyProcessIsMain,
                    mScope: kAudioObjectPropertyScope_Global,
                    mElement: kAudioObjectPropertyElement_Output,
                };

                let mut is_main: u32 = 1;
                let size = mem::size_of::<u32>() as u32;

                let status = AudioObjectSetPropertyData(
                    coreaudio::sys::kAudioObjectSystemObject,
                    &address as *const _,
                    0,
                    ptr::null(),
                    size,
                    &mut is_main as *mut _ as *mut c_void,
                );

                if status != 0 {
                    return Err(AudioError::CaptureError(
                        "Failed to set process as main audio handler".into(),
                    ));
                }
            }
        }

        // Set up the render callback with process filtering
        let target_pid = target_pid;
        let callback = move |_action_flags: *mut u32,
                           _time_stamp: *const c_void,
                           _bus_number: u32,
                           number_frames: u32,
                           io_data: *mut c_void|
              -> OSStatus {
            unsafe {
                // Get the current process info
                let address = AudioObjectPropertyAddress {
                    mSelector: kProcessAudioProperty,
                    mScope: kAudioObjectPropertyScope_Global,
                    mElement: kAudioObjectPropertyElement_Output,
                };

                let mut process_info = ProcessAudioInfo {
                    pid: 0,
                    is_input_master: false,
                    is_output_master: false,
                    volume: 0.0,
                    muted: false,
                    app_name: [0; 256],
                    bundle_id: [0; 256],
                };

                let mut size = mem::size_of::<ProcessAudioInfo>() as u32;

                let status = AudioObjectGetPropertyData(
                    coreaudio::sys::kAudioObjectSystemObject,
                    &address as *const _,
                    0,
                    ptr::null(),
                    &mut size as *mut _,
                    &mut process_info as *mut _ as *mut c_void,
                );

                // If we can't get process info or this is not our target process, skip
                if status != 0 || (target_pid != 0 && process_info.pid != target_pid) {
                    return 0;
                }

                // Copy the audio data
                let data = std::slice::from_raw_parts(
                    io_data as *const u8,
                    (number_frames as usize) * mem::size_of::<f32>(),
                );
                
                buffer_clone.lock().unwrap().extend_from_slice(data);
            }
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
            target_pid,
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