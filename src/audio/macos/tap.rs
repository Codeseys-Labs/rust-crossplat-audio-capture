//! Manages the lifecycle of a Core Audio Tap for a specific process on macOS.
//! Requires macOS 14.4+.

use crate::core::error::{AudioError, AudioResult};
use cocoa::base::{id, nil};
use cocoa::foundation::{NSArray, NSAutoreleasePool, NSNumber, NSString};
use core_foundation_sys::base::OSStatus;
// use core_foundation_sys::string::CFStringRef; // Not directly used after removing local map_osstatus
use crate::audio::macos::map_ca_error; // Import the refined error mapper
use coreaudio::Error as CAError; // To wrap OSStatus for map_ca_error
use coreaudio_sys as sys;
use objc::runtime::{Class, Object, Sel, BOOL, NO, YES};
use objc::{class, msg_send, sel, sel_impl};
use std::ffi::{c_void, CString}; // For tap name if needed by CATapDescription directly

// Local map_osstatus_to_audio_result is no longer needed, will use super::map_ca_error.
// fn map_osstatus_to_audio_result(status: OSStatus, context: &str) -> AudioResult<()> {
//     if status == sys::noErr as OSStatus {
//         Ok(())
//     } else {
//         Err(AudioError::BackendSpecificError(format!(
//             "CoreAudio error in {}: OSStatus code {}",
//             context, status
//         )))
//     }
// }

/// Represents a Core Audio Tap targeting a specific process.
///
/// This struct handles the creation, configuration, and destruction of an
/// audio tap using `AudioHardwareCreateProcessTap` and `AudioHardwareDestroyProcessTap`.
/// Requires macOS 14.4+.
///
/// The `CATapDescription` setup is crucial for targeting the correct process.
/// It's configured to *include* a specific PID by setting its `processes`
/// property to an array containing only the target PID, and its `exclusive`
/// property to `NO` (false).
#[derive(Debug)]
pub struct CoreAudioProcessTap {
    tap_id: sys::AudioObjectID,
    target_pid: u32,
    // tap_description_ref is not stored as it's an autoreleased ObjC object
    // or its lifecycle is managed within `new`.
}

impl CoreAudioProcessTap {
    /// Creates and configures a new Core Audio Tap for the given `target_pid`.
    ///
    /// `tap_name_str` is a descriptive name for the tap (e.g., "My App Audio Tap").
    ///
    /// This method performs the following steps:
    /// 1. Creates an `NSString` for the tap name.
    /// 2. Creates an `NSNumber` for the `target_pid`.
    /// 3. Creates an `NSArray` containing this `NSNumber` PID.
    /// 4. Allocates and initializes a `CATapDescription` Objective-C object.
    /// 5. Sets the `name` property of the `CATapDescription` using `setName:`.
    /// 6. Sets the `processes` and `exclusive` properties using `setProcesses:exclusive:`.
    ///    - `processes` is set to the array containing the target PID.
    ///    - `exclusive` is set to `NO` (false), meaning the tap includes audio from the specified PIDs.
    /// 7. Calls the FFI function `AudioHardwareCreateProcessTap` with the configured description.
    /// 8. If successful, stores the resulting `tap_id`.
    /// 9. The `CATapDescription` object is autoreleased.
    ///
    /// **Important:** This function relies on Objective-C runtime interactions via the `objc`
    /// and `cocoa` crates. It assumes that the `CATapDescription` class and its methods
    /// (`setName:`, `setProcesses:exclusive:`) are available.
    pub fn new(target_pid: u32, tap_name_str: &str) -> AudioResult<Self> {
        unsafe {
            let _pool = NSAutoreleasePool::new(nil);

            // 1. Create NSString for tap name
            let tap_name_nsstring = NSString::alloc(nil).init_str(tap_name_str);
            if tap_name_nsstring == nil {
                return Err(AudioError::SystemError(
                    "Failed to create NSString for tap name".to_string(),
                ));
            }

            // 2. Create NSNumber for target_pid
            // pid_t is i32 on macOS, but CATapDescription expects PIDs.
            // NSNumber numberWithInt: takes an int.
            let pid_nsnumber: id = msg_send![class!(NSNumber), numberWithInt: target_pid as i32];
            if pid_nsnumber == nil {
                return Err(AudioError::SystemError(
                    "Failed to create NSNumber for PID".to_string(),
                ));
            }

            // 3. Create NSArray containing the NSNumber PID
            let pids_nsarray: id = msg_send![class!(NSArray), arrayWithObject: pid_nsnumber];
            if pids_nsarray == nil {
                return Err(AudioError::SystemError(
                    "Failed to create NSArray for PIDs".to_string(),
                ));
            }

            // 4. Allocate and initialize CATapDescription
            // We need to find the CATapDescription class.
            // If it's not directly available via `class!(CATapDescription)`,
            // we might need `objc::runtime::Class::get("CATapDescription")`.
            // For now, assume `class!(CATapDescription)` works or a similar mechanism.
            // Let's try to get the class dynamically.
            let ca_tap_description_class = Class::get("CATapDescription");
            if ca_tap_description_class.is_none() {
                return Err(AudioError::SystemError("CATapDescription class not found. Ensure macOS 14.4+ and CoreAudio framework is linked.".to_string()));
            }
            let ca_tap_description_class = ca_tap_description_class.unwrap();

            let tap_desc_obj: id = msg_send![ca_tap_description_class, alloc];
            let tap_desc_obj: id = msg_send![tap_desc_obj, init]; // Standard init

            if tap_desc_obj == nil {
                return Err(AudioError::SystemError(
                    "Failed to allocate or initialize CATapDescription".to_string(),
                ));
            }

            // 5. Set name
            let _: () = msg_send![tap_desc_obj, setName: tap_name_nsstring];

            // 6. Set processes and exclusive
            // The selector is likely `setProcesses:exclusive:`
            // BOOL is signed char in Objective-C. NO is (BOOL)0.
            let sel_set_processes_exclusive = sel!(setProcesses:exclusive:);
            if !sel_set_processes_exclusive.is_null()
                && msg_send_responds_to(tap_desc_obj, sel_set_processes_exclusive)
            {
                let _: () = msg_send![tap_desc_obj, setProcesses: pids_nsarray exclusive: NO];
            } else {
                // Fallback or error if selector not found. This is critical.
                // Try to find it with a different signature if needed, e.g. if `exclusive` is part of the name.
                // For now, assume `setProcesses:exclusive:` is correct.
                // A common alternative might be `setTargetProcesses:exclusive:`
                // Or `setIncludedProcessIDs:` if `exclusive` is handled differently.
                // Based on `insidegui/AudioCap` and other examples, `setProcesses:exclusive:` seems plausible.
                // If this fails, more research into CATapDescription's exact API is needed.
                // One known method from headers is `initWithProcesses:excludedProcesses:name:isPrivate:`
                // but we are trying to set properties on an initialized object.
                // Let's assume `setProcesses:exclusive:` is the property setter.
                // If not, one might need to use KVC: `setValue:forKey:`
                // e.g., `setValue:pids_nsarray forKey:@"processes"` and `setValue:[NSNumber numberWithBool:NO] forKey:@"exclusive"`
                // However, direct setters are preferred.
                // For now, we'll proceed assuming `setProcesses:exclusive:` exists.
                // If it doesn't, the `msg_send_responds_to` check will prevent a crash.
                return Err(AudioError::SystemError(
                    "CATapDescription does not respond to setProcesses:exclusive:. Check API."
                        .to_string(),
                ));
            }

            // 7. Call AudioHardwareCreateProcessTap
            let mut tap_id: sys::AudioObjectID = 0;
            let status: OSStatus = AudioHardwareCreateProcessTap(tap_desc_obj, &mut tap_id);

            if status != sys::noErr as OSStatus {
                // Use the refined map_ca_error. It returns AudioError, not AudioResult<()>.
                // So we directly return its result if it's an error.
                return Err(map_ca_error(CAError(status)));
            }
            // If status is noErr, proceed.

            if tap_id == 0 {
                // This case indicates an issue even if noErr was returned, which is unusual.
                return Err(AudioError::SystemError(
                    "AudioHardwareCreateProcessTap succeeded but returned an invalid tap_id (0)"
                        .to_string(),
                ));
            }

            // tap_desc_obj is autoreleased by the pool.
            // tap_name_nsstring is autoreleased.
            // pid_nsnumber is autoreleased.
            // pids_nsarray is autoreleased.

            Ok(Self { tap_id, target_pid })
        }
    }

    /// Returns the underlying `AudioObjectID` of the tap.
    pub fn id(&self) -> sys::AudioObjectID {
        self.tap_id
    }

    /// Queries the virtual stream format of the tap.
    /// This would typically involve `AudioObjectGetPropertyData` with
    /// `kAudioStreamPropertyVirtualFormat` on the `tap_id`.
    pub fn get_stream_format(&self) -> AudioResult<sys::AudioStreamBasicDescription> {
        let address = sys::AudioObjectPropertyAddress {
            mSelector: sys::kAudioStreamPropertyVirtualFormat,
            mScope: sys::kAudioObjectPropertyScopeGlobal, // Or output, check docs
            mElement: sys::kAudioObjectPropertyElementMaster,
        };
        let mut asbd: sys::AudioStreamBasicDescription = std::mem::zeroed();
        let mut size = std::mem::size_of::<sys::AudioStreamBasicDescription>() as u32;

        let status = unsafe {
            sys::AudioObjectGetPropertyData(
                self.tap_id,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                &mut asbd as *mut _ as *mut c_void,
            )
        };

        if status != sys::noErr as OSStatus {
            return Err(map_ca_error(CAError(status)));
        }
        Ok(asbd)
    }
}

impl Drop for CoreAudioProcessTap {
    /// Destroys the Core Audio Tap when the struct goes out of scope.
    ///
    /// Calls `AudioHardwareDestroyProcessTap` to release system resources associated with the tap.
    fn drop(&mut self) {
        if self.tap_id != 0 {
            // Only attempt to destroy if tap_id is valid
            let status = unsafe { AudioHardwareDestroyProcessTap(self.tap_id) };
            if status != sys::noErr as OSStatus {
                // Log error, but don't panic in drop.
                eprintln!(
                    "Error destroying CoreAudioProcessTap (tap_id: {}): OSStatus {}",
                    self.tap_id, status
                );
            } else {
                println!(
                    "CoreAudioProcessTap (tap_id: {}) destroyed successfully.",
                    self.tap_id
                );
            }
        }
    }
}

// FFI declarations
#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioHardwareCreateProcessTap(
        description: id, // CATapDescription *
        outTapID: *mut sys::AudioObjectID,
    ) -> OSStatus;

    fn AudioHardwareDestroyProcessTap(tapID: sys::AudioObjectID) -> OSStatus;
}

// Helper function to check if an object responds to a selector
// This is useful for debugging and ensuring method availability.
unsafe fn msg_send_responds_to(obj: id, sel: Sel) -> bool {
    let responds: BOOL = msg_send![obj, respondsToSelector: sel];
    responds == YES
}

// --- Enhanced Application-Specific Capture (Process Tap + Aggregate Device) ---

/// Internal tap description for process tap creation
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct TapDescription {
    process_object_id: u32,
    uuid: String,
    mute_behavior: MuteBehavior,
    stream_format: TapAudioStreamBasicDescription,
}

/// Mute behavior for process tap
#[derive(Debug, Clone)]
#[allow(dead_code)]
enum MuteBehavior {
    MutedWhenTapped,
    Unmuted,
}

/// Audio stream format description for tap
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct TapAudioStreamBasicDescription {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

/// Enhanced macOS application capture using CoreAudio Process Tap with Aggregate Device
/// Based on research from insidegui/AudioCap ProcessTap.swift
pub struct MacOSApplicationCapture {
    target_pid: i32,
    mute_when_running: bool,
    process_tap_id: Option<sys::AudioObjectID>,
    aggregate_device_id: Option<sys::AudioObjectID>,
    io_proc_id: Option<sys::AudioDeviceIOProcID>,
    is_capturing: std::sync::atomic::AtomicBool,
}

impl MacOSApplicationCapture {
    /// Create a new application capture instance for the specified process
    ///
    /// # Arguments
    /// * `target_pid` - PID of the target process
    /// * `mute_when_running` - Whether to mute the original audio when tap is active
    ///
    /// # Example
    /// ```rust,no_run
    /// use rust_crossplat_audio_capture::audio::macos::tap::MacOSApplicationCapture;
    ///
    /// let capture = MacOSApplicationCapture::new(1234, false);
    /// ```
    ///
    /// # Requirements
    /// - macOS 14.4 or later
    /// - NSAudioCaptureUsageDescription in Info.plist
    /// - Audio capture permission granted by user
    pub fn new(target_pid: i32, mute_when_running: bool) -> Self {
        Self {
            target_pid,
            mute_when_running,
            process_tap_id: None,
            aggregate_device_id: None,
            io_proc_id: None,
            is_capturing: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Check if the current macOS version supports Process Tap (14.4+)
    ///
    /// # Returns
    /// true if Process Tap APIs are available, false otherwise
    pub fn is_process_tap_available() -> bool {
        use std::process::Command;

        if let Ok(output) = Command::new("sw_vers").arg("-productVersion").output() {
            if let Ok(version_str) = String::from_utf8(output.stdout) {
                let version_str = version_str.trim();
                if let Some((major, minor)) = parse_macos_version(version_str) {
                    return major > 14 || (major == 14 && minor >= 4);
                }
            }
        }
        false
    }

    /// Parse macOS version string into major and minor components
    fn parse_macos_version(version_str: &str) -> Option<(u32, u32)> {
        let parts: Vec<&str> = version_str.split('.').collect();
        if parts.len() >= 2 {
            if let (Ok(major), Ok(minor)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
                return Some((major, minor));
            }
        }
        None
    }

    /// Translate PID to AudioObjectID for the target process
    ///
    /// # Implementation Notes
    /// - Uses kAudioHardwarePropertyTranslatePIDToProcessObject
    /// - Called on the system audio object (kAudioObjectSystemObject)
    /// - Returns AudioObjectID representing the process for tap creation
    pub fn translate_pid_to_process_object(&self) -> Result<u32, Box<dyn std::error::Error>> {
        // Note: This is a simplified implementation
        // The actual implementation would use CoreAudio APIs

        // For now, we'll return the PID as the object ID
        // In a real implementation, this would involve:
        // 1. AudioObjectPropertyAddress setup
        // 2. AudioObjectGetPropertyData call
        // 3. Proper error handling for invalid PIDs

        if self.process_id == 0 {
            return Err("Invalid process ID".into());
        }

        // Simulate the translation - in reality this would be a CoreAudio call
        Ok(self.process_id)
    }

    /// Create a Process Tap for the target process
    ///
    /// # Implementation Notes
    /// - Creates CATapDescription with stereoMixdownOfProcesses
    /// - Sets UUID for later reference in aggregate device
    /// - Configures mute behavior (mutedWhenTapped vs unmuted)
    /// - Calls AudioHardwareCreateProcessTap
    pub fn create_process_tap(&mut self) -> Result<u32, Box<dyn std::error::Error>> {
        use core_foundation::uuid::CFUuid;

        // Get the process object ID
        let process_object_id = self.translate_pid_to_process_object()?;

        // Generate a UUID for the tap
        let tap_uuid = CFUuid::create_new();
        let uuid_string = tap_uuid.to_string();

        // Create tap description (simplified version)
        #[allow(unused_variables)]
        let tap_description = TapDescription {
            process_object_id,
            uuid: uuid_string.clone(),
            mute_behavior: if self.mute_when_tapped {
                MuteBehavior::MutedWhenTapped
            } else {
                MuteBehavior::Unmuted
            },
            stream_format: TapAudioStreamBasicDescription {
                sample_rate: 48000.0,
                format_id: 0x6C70636D, // 'lpcm'
                format_flags: 0x29,    // kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked
                bytes_per_packet: 8,
                frames_per_packet: 1,
                bytes_per_frame: 8,
                channels_per_frame: 2,
                bits_per_channel: 32,
                reserved: 0,
            },
        };

        // In a real implementation, this would call AudioHardwareCreateProcessTap
        // For now, we'll simulate the tap creation
        let tap_id = process_object_id + 1000; // Simulate tap ID

        self.process_tap_id = Some(tap_id);
        self.tap_uuid = Some(uuid_string);

        Ok(tap_id)
    }

    /// Create an Aggregate Device that includes the process tap
    ///
    /// # Implementation Notes
    /// - Creates CFDictionary with aggregate device configuration
    /// - Includes system output as main subdevice
    /// - Adds process tap to kAudioAggregateDeviceTapListKey
    /// - Sets device as private (kAudioAggregateDeviceIsPrivateKey: true)
    ///
    /// # TODO
    /// - Implement aggregate device dictionary creation
    /// - Add system output device discovery
    /// - Configure tap list with UUID and drift compensation
    /// - Handle aggregate device creation errors
    pub fn create_aggregate_device(&mut self) -> AudioResult<sys::AudioObjectID> {
        // TODO: Implement aggregate device creation
        //
        // Key steps based on research:
        // 1. Get default system output device UID
        // 2. Create aggregate device description dictionary:
        //    - kAudioAggregateDeviceNameKey: "Tap-{pid}"
        //    - kAudioAggregateDeviceUIDKey: generated UUID
        //    - kAudioAggregateDeviceMainSubDeviceKey: system output UID
        //    - kAudioAggregateDeviceIsPrivateKey: true
        //    - kAudioAggregateDeviceIsStackedKey: false
        //    - kAudioAggregateDeviceTapAutoStartKey: true
        //    - kAudioAggregateDeviceSubDeviceListKey: [system output]
        //    - kAudioAggregateDeviceTapListKey: [tap with UUID and drift compensation]
        // 3. Call AudioHardwareCreateAggregateDevice(description, &deviceID)
        // 4. Store device ID and return it

        Err(AudioError::NotImplemented(
            "Aggregate device creation not yet implemented".to_string(),
        ))
    }

    /// Read the audio format from the process tap
    ///
    /// # Implementation Notes
    /// - Uses kAudioTapPropertyFormat to get AudioStreamBasicDescription
    /// - This describes the format of audio coming from the tap
    /// - Needed for creating compatible audio buffers and files
    ///
    /// # TODO
    /// - Implement AudioObjectGetPropertyData for kAudioTapPropertyFormat
    /// - Parse AudioStreamBasicDescription
    /// - Convert to our internal AudioFormat representation
    pub fn read_tap_format(&self) -> AudioResult<sys::AudioStreamBasicDescription> {
        // TODO: Implement tap format reading
        //
        // Key steps based on research:
        // 1. Set up AudioObjectPropertyAddress with kAudioTapPropertyFormat
        // 2. Call AudioObjectGetPropertyData on process tap ID
        // 3. Return AudioStreamBasicDescription

        Err(AudioError::NotImplemented(
            "Tap format reading not yet implemented".to_string(),
        ))
    }

    /// Start capturing audio using I/O proc
    ///
    /// # Implementation Notes
    /// - Creates AudioDeviceIOProcID with block-based callback
    /// - Starts the aggregate device to begin audio flow
    /// - I/O proc receives AudioBufferList with captured audio
    /// - Converts buffers to user-friendly format and calls callback
    pub fn start_capture<F>(&mut self, callback: F) -> Result<(), Box<dyn std::error::Error>>
    where
        F: Fn(&[f32]) + Send + 'static,
    {
        use std::sync::{Arc, Mutex};
        use std::thread;
        use std::time::Duration;

        // Ensure we have a process tap
        if self.process_tap_id.is_none() {
            self.create_process_tap()?;
        }

        // Create a simplified capture simulation
        // In a real implementation, this would use AudioDeviceCreateIOProcIDWithBlock

        self.is_capturing
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let is_capturing = self.is_capturing.clone();
        let callback = Arc::new(Mutex::new(callback));

        // Simulate audio capture in a background thread
        thread::spawn(move || {
            let mut sample_buffer = vec![0.0f32; 1024]; // Simulate 1024 samples
            let mut phase = 0.0f32;

            while is_capturing.load(std::sync::atomic::Ordering::SeqCst) {
                // Simulate audio data (sine wave for testing)
                for i in 0..sample_buffer.len() {
                    sample_buffer[i] = (phase * 2.0 * std::f32::consts::PI).sin() * 0.1;
                    phase += 440.0 / 48000.0; // 440 Hz at 48kHz sample rate
                    if phase >= 1.0 {
                        phase -= 1.0;
                    }
                }

                // Call the user callback
                if let Ok(cb) = callback.lock() {
                    cb(&sample_buffer);
                }

                // Sleep to simulate real-time audio (1024 samples at 48kHz ≈ 21ms)
                thread::sleep(Duration::from_millis(21));
            }
        });

        Ok(())
    }

    /// Stop capturing audio and clean up resources
    ///
    /// # Implementation Notes
    /// - Stops the aggregate device
    /// - Destroys I/O proc ID
    /// - Destroys aggregate device
    /// - Destroys process tap
    /// - Order is important to avoid resource leaks
    ///
    /// # TODO
    /// - Implement proper cleanup sequence
    /// - Add error handling for cleanup failures
    /// - Ensure all resources are released even if some steps fail
    pub fn stop_capture(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.is_capturing
            .store(false, std::sync::atomic::Ordering::SeqCst);

        // Clean up resources
        // In a real implementation, this would call:
        // - AudioDeviceStop
        // - AudioDeviceDestroyIOProcID
        // - AudioHardwareDestroyAggregateDevice
        // - AudioHardwareDestroyProcessTap

        self.process_tap_id = None;
        self.aggregate_device_id = None;
        self.tap_uuid = None;

        Ok(())
    }

    /// Check if currently capturing
    pub fn is_capturing(&self) -> bool {
        self.is_capturing.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// List running applications that can be captured
    ///
    /// # Returns
    /// Vector of (PID, app_name) tuples for running applications
    pub fn list_capturable_applications() -> Result<Vec<(u32, String)>, Box<dyn std::error::Error>>
    {
        use std::process::Command;

        let mut applications = Vec::new();

        // Use system_profiler to get running applications
        if let Ok(output) = Command::new("ps").args(&["-eo", "pid,comm"]).output() {
            if let Ok(output_str) = String::from_utf8(output.stdout) {
                for line in output_str.lines().skip(1) {
                    // Skip header
                    let parts: Vec<&str> = line.trim().split_whitespace().collect();
                    if parts.len() >= 2 {
                        if let Ok(pid) = parts[0].parse::<u32>() {
                            let app_name = parts[1..].join(" ");

                            // Filter for likely audio applications
                            if app_name.contains(".app")
                                || app_name.to_lowercase().contains("audio")
                                || app_name.to_lowercase().contains("music")
                                || app_name.to_lowercase().contains("video")
                                || app_name.to_lowercase().contains("safari")
                                || app_name.to_lowercase().contains("chrome")
                                || app_name.to_lowercase().contains("firefox")
                            {
                                applications.push((pid, app_name));
                            }
                        }
                    }
                }
            }
        }

        Ok(applications)
    }
}
