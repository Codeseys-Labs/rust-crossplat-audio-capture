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
        kAudioUnitType_Output, AURenderCallbackStruct, AudioBuffer, AudioBufferList,
        AudioObjectAddPropertyListener, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
        AudioObjectID, AudioObjectPropertyAddress, AudioObjectRemovePropertyListener, OSStatus,
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

const kAudioHardwarePropertyProcessIsMain: u32 = 0x6D61696E; // 'main'
const kAudioHardwarePropertyProcessIsMaster: u32 = 0x6D617374; // 'mast'
const kProcessAudioProperty: u32 = 0x70617564; // 'paud'

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
        let system_device_id = unsafe { Self::get_default_output_device()? };

        let backend = Self {
            device_list: Arc::new(Mutex::new(Vec::new())),
            system_device_id,
        };

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
                return Err(AudioError::DeviceNotFound(
                    "Failed to get device name".into(),
                ));
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
            let mut buffer = vec![
                ProcessAudioInfo {
                    pid: 0,
                    is_input_master: false,
                    is_output_master: false,
                    volume: 0.0,
                    muted: false,
                    app_name: [0; 256],
                    bundle_id: [0; 256],
                };
                count
            ];

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
        let mut apps = Vec::new();

        // Add system-wide audio capture option
        apps.push(AudioApplication {
            name: "System Audio".to_string(),
            id: "system".to_string(),
            executable_name: "system".to_string(),
            pid: 0,
        });

        // Get running applications with audio
        if let Ok(process_list) = Self::get_running_applications() {
            for proc in process_list {
                apps.push(AudioApplication {
                    name: proc.name.clone(),
                    id: proc.bundle_id.unwrap_or_else(|| proc.pid.to_string()),
                    executable_name: proc.executable.clone(),
                    pid: proc.pid,
                });
            }
        }

        let mut list = self.device_list.lock().unwrap();
        *list = apps;
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
        let stream = if app.pid == 0 {
            // System-wide capture
            CoreAudioStream::new_system(self.system_device_id, config)?
        } else {
            // Application-specific capture
            CoreAudioStream::new_application(app, config)?
        };

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
    fn new_system(device_id: AudioObjectID, config: AudioConfig) -> Result<Self, AudioError> {
        // Implementation for system-wide capture
        // ... existing implementation ...
        Ok(Self {
            audio_unit: AudioUnit::new(kAudioUnitType_Output)?,
            config,
            buffer: Arc::new(Mutex::new(Vec::new())),
            target_pid: 0,
        })
    }

    fn new_application(app: &AudioApplication, config: AudioConfig) -> Result<Self, AudioError> {
        // New implementation for application-specific capture using Audio HAL
        let mut audio_unit = AudioUnit::new(kAudioUnitType_Output)?;

        // Set up audio unit for application-specific capture
        unsafe {
            // Get the application's audio session
            let mut session_id: u32 = 0;
            let mut size = mem::size_of::<u32>() as u32;
            let status = AudioObjectGetPropertyData(
                app.pid as AudioObjectID,
                &AudioObjectPropertyAddress {
                    mSelector: kAudioDevicePropertyDeviceUID,
                    mScope: kAudioObjectPropertyScope_Output,
                    mElement: kAudioObjectPropertyElement_Output,
                },
                0,
                ptr::null(),
                &mut size as *mut u32,
                &mut session_id as *mut u32 as *mut c_void,
            );

            if status != 0 {
                return Err(AudioError::CaptureError(
                    "Failed to get application audio session".into(),
                ));
            }

            // Configure audio unit for the application's session
            audio_unit.set_property(
                kAudioUnitProperty_StreamFormat,
                kAudioUnitScope_Output,
                0,
                Some(&StreamFormat::from_config(&config)),
            )?;
        }

        Ok(Self {
            audio_unit,
            config,
            buffer: Arc::new(Mutex::new(Vec::new())),
            target_pid: app.pid,
        })
    }

    fn start(&mut self) -> Result<(), AudioError> {
        audio_unit_start(&self.audio_unit).map_err(|e| AudioError::CaptureError(e.to_string()))
    }

    fn stop(&mut self) -> Result<(), AudioError> {
        audio_unit_stop(&self.audio_unit).map_err(|e| AudioError::CaptureError(e.to_string()))
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
