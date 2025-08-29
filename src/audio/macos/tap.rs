//! Manages the lifecycle of a Core Audio Tap for a specific process on macOS.
//! Requires macOS 14.4+.

use crate::core::error::{AudioError, AudioResult};
use cocoa::base::{id, nil};
use cocoa::foundation::{NSArray, NSAutoreleasePool, NSNumber, NSString};
use core_foundation_sys::base::OSStatus;
// use core_foundation_sys::string::CFStringRef; // Not directly used after removing local map_osstatus
use crate::audio::macos::map_ca_error; // Import the refined error mapper
use coreaudio_rs::Error as CAError; // To wrap OSStatus for map_ca_error
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
    ///
    /// # TODO
    /// - Implement runtime macOS version check
    /// - Use NSProcessInfo or similar to check version
    pub fn is_process_tap_available() -> bool {
        // TODO: Implement version check
        // if #available(macOS 14.4, *) { true } else { false }
        // In Rust, this would be a runtime check using system APIs
        false
    }

    /// Translate PID to AudioObjectID for the target process
    ///
    /// # Implementation Notes
    /// - Uses kAudioHardwarePropertyTranslatePIDToProcessObject
    /// - Called on the system audio object (kAudioObjectSystemObject)
    /// - Returns AudioObjectID representing the process for tap creation
    ///
    /// # TODO
    /// - Implement AudioObjectGetPropertyData call
    /// - Handle process not found or not audio-capable
    /// - Add proper error handling for invalid PIDs
    pub fn translate_pid_to_process_object(&self) -> AudioResult<sys::AudioObjectID> {
        // TODO: Implement PID to AudioObjectID translation
        //
        // Key steps based on research:
        // 1. Set up AudioObjectPropertyAddress with:
        //    - mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject
        //    - mScope: kAudioObjectPropertyScopeGlobal
        //    - mElement: kAudioObjectPropertyElementMain
        // 2. Call AudioObjectGetPropertyData with:
        //    - inObjectID: kAudioObjectSystemObject
        //    - inQualifierData: &target_pid (as qualifier)
        //    - outData: &mut process_object_id
        // 3. Return the resulting AudioObjectID

        Err(AudioError::NotImplemented("PID to AudioObjectID translation not yet implemented".to_string()))
    }

    /// Create a Process Tap for the target process
    ///
    /// # Implementation Notes
    /// - Creates CATapDescription with stereoMixdownOfProcesses
    /// - Sets UUID for later reference in aggregate device
    /// - Configures mute behavior (mutedWhenTapped vs unmuted)
    /// - Calls AudioHardwareCreateProcessTap
    ///
    /// # TODO
    /// - Implement CATapDescription creation and configuration
    /// - Add proper UUID generation and storage
    /// - Handle tap creation errors (process not found, permission denied)
    pub fn create_process_tap(&mut self) -> AudioResult<sys::AudioObjectID> {
        // TODO: Implement process tap creation
        //
        // Key steps based on research:
        // 1. Get process AudioObjectID via translate_pid_to_process_object
        // 2. Create CATapDescription:
        //    - stereoMixdownOfProcesses: [process_object_id]
        //    - uuid: generate new UUID
        //    - muteBehavior: .mutedWhenTapped or .unmuted
        // 3. Call AudioHardwareCreateProcessTap(tapDescription, &tapID)
        // 4. Store tap ID and return it

        Err(AudioError::NotImplemented("Process tap creation not yet implemented".to_string()))
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

        Err(AudioError::NotImplemented("Aggregate device creation not yet implemented".to_string()))
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

        Err(AudioError::NotImplemented("Tap format reading not yet implemented".to_string()))
    }

    /// Start capturing audio using I/O proc
    ///
    /// # Implementation Notes
    /// - Creates AudioDeviceIOProcID with block-based callback
    /// - Starts the aggregate device to begin audio flow
    /// - I/O proc receives AudioBufferList with captured audio
    /// - Converts buffers to user-friendly format and calls callback
    ///
    /// # TODO
    /// - Implement AudioDeviceCreateIOProcIDWithBlock
    /// - Add proper AudioBufferList processing
    /// - Handle device start/stop and error conditions
    /// - Convert audio data to f32 samples for callback
    pub fn start_capture<F>(&mut self, callback: F) -> AudioResult<()>
    where
        F: Fn(&[f32]) + Send + 'static,
    {
        // TODO: Implement capture start
        //
        // Key steps based on research:
        // 1. Ensure process tap and aggregate device are created
        // 2. Create I/O proc with AudioDeviceCreateIOProcIDWithBlock:
        //    - Use aggregate device ID
        //    - Provide dispatch queue for callback
        //    - Implement ioBlock to process AudioBufferList
        // 3. Start device with AudioDeviceStart(aggregateDeviceID, procID)
        // 4. In I/O block:
        //    - Extract audio data from inInputData AudioBufferList
        //    - Convert to f32 samples
        //    - Call user callback with samples

        self.is_capturing.store(true, std::sync::atomic::Ordering::SeqCst);
        Err(AudioError::NotImplemented("macOS capture start not yet implemented".to_string()))
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
    pub fn stop_capture(&mut self) -> AudioResult<()> {
        self.is_capturing.store(false, std::sync::atomic::Ordering::SeqCst);

        // TODO: Implement proper cleanup sequence
        //
        // Key steps based on research:
        // 1. Stop aggregate device: AudioDeviceStop(aggregateDeviceID, procID)
        // 2. Destroy I/O proc: AudioDeviceDestroyIOProcID(aggregateDeviceID, procID)
        // 3. Destroy aggregate device: AudioHardwareDestroyAggregateDevice(aggregateDeviceID)
        // 4. Destroy process tap: AudioHardwareDestroyProcessTap(processTapID)
        // 5. Clear stored IDs

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
    ///
    /// # TODO
    /// - Implement NSRunningApplication enumeration
    /// - Filter for applications that produce audio
    /// - Extract PID and localized name
    pub fn list_capturable_applications() -> AudioResult<Vec<(i32, String)>> {
        // TODO: Implement application listing
        //
        // Key steps:
        // 1. Use NSWorkspace.shared.runningApplications
        // 2. Filter applications (exclude system processes if desired)
        // 3. Extract processIdentifier and localizedName
        // 4. Return list of (PID, name) tuples

        Err(AudioError::NotImplemented("Application listing not yet implemented".to_string()))
    }
}
