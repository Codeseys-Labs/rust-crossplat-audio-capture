//! Manages the lifecycle of a Core Audio Tap for a specific process on macOS.
//! Requires macOS 14.4+.

#![cfg(target_os = "macos")]

use super::coreaudio::map_ca_error;
use crate::core::error::{AudioError, AudioResult};
use cocoa::base::{id, nil};
use cocoa::foundation::{NSArray, NSAutoreleasePool, NSString};
use core_foundation_sys::base::OSStatus;
use coreaudio::Error as CAError;
use coreaudio_sys as sys;
use objc::runtime::{Class, Object, Sel, BOOL, NO, YES};
use objc::{class, msg_send, sel, sel_impl};
use std::ffi::c_void;

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
}

impl CoreAudioProcessTap {
    /// Creates and configures a new Core Audio Tap for the given `target_pid`.
    ///
    /// `tap_name_str` is a descriptive name for the tap (e.g., "rsac-tap-1234").
    ///
    /// This method:
    /// 1. Creates ObjC objects for the tap name and PID
    /// 2. Allocates and initializes a `CATapDescription`
    /// 3. Configures process targeting via `setProcesses:exclusive:`
    /// 4. Calls `AudioHardwareCreateProcessTap`
    ///
    /// **Important:** Requires macOS 14.4+ and the `CATapDescription` class.
    pub fn new(target_pid: u32, tap_name_str: &str) -> AudioResult<Self> {
        unsafe {
            let _pool = NSAutoreleasePool::new(nil);

            // 1. Create NSString for tap name
            let tap_name_nsstring = NSString::alloc(nil).init_str(tap_name_str);
            if tap_name_nsstring == nil {
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "process_tap".into(),
                    message: "Failed to create NSString for tap name".into(),
                    context: None,
                });
            }

            // 2. Create NSNumber for target_pid
            let pid_nsnumber: id = msg_send![class!(NSNumber), numberWithInt: target_pid as i32];
            if pid_nsnumber == nil {
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "process_tap".into(),
                    message: "Failed to create NSNumber for PID".into(),
                    context: None,
                });
            }

            // 3. Create NSArray containing the NSNumber PID
            let pids_nsarray: id = msg_send![class!(NSArray), arrayWithObject: pid_nsnumber];
            if pids_nsarray == nil {
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "process_tap".into(),
                    message: "Failed to create NSArray for PIDs".into(),
                    context: None,
                });
            }

            // 4. Allocate and initialize CATapDescription
            let ca_tap_description_class = Class::get("CATapDescription");
            if ca_tap_description_class.is_none() {
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "process_tap".into(),
                    message: "CATapDescription class not found. Ensure macOS 14.4+ and CoreAudio framework is linked.".into(),
                    context: None,
                });
            }
            let ca_tap_description_class = ca_tap_description_class.unwrap();

            let tap_desc_obj: id = msg_send![ca_tap_description_class, alloc];
            let tap_desc_obj: id = msg_send![tap_desc_obj, init];

            if tap_desc_obj == nil {
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "process_tap".into(),
                    message: "Failed to allocate or initialize CATapDescription".into(),
                    context: None,
                });
            }

            // 5. Set name
            let _: () = msg_send![tap_desc_obj, setName: tap_name_nsstring];

            // 6. Set processes and exclusive
            let sel_set_processes_exclusive = sel!(setProcesses:exclusive:);
            if msg_send_responds_to(tap_desc_obj, sel_set_processes_exclusive) {
                let _: () = msg_send![tap_desc_obj, setProcesses: pids_nsarray exclusive: NO];
            } else {
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "process_tap".into(),
                    message:
                        "CATapDescription does not respond to setProcesses:exclusive:. Check API."
                            .into(),
                    context: None,
                });
            }

            // 7. Call AudioHardwareCreateProcessTap
            let mut tap_id: sys::AudioObjectID = 0;
            let status: OSStatus = AudioHardwareCreateProcessTap(tap_desc_obj, &mut tap_id);

            if status != sys::noErr as OSStatus {
                return Err(map_ca_error(CAError(status)));
            }

            if tap_id == 0 {
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "process_tap".into(),
                    message:
                        "AudioHardwareCreateProcessTap succeeded but returned an invalid tap_id (0)"
                            .into(),
                    context: None,
                });
            }

            Ok(Self { tap_id, target_pid })
        }
    }

    /// Returns the underlying `AudioObjectID` of the tap.
    ///
    /// This ID can be used as a device ID for an AUHAL AudioUnit.
    pub fn id(&self) -> sys::AudioObjectID {
        self.tap_id
    }

    /// Returns the PID of the target process.
    #[allow(dead_code)]
    pub fn target_pid(&self) -> u32 {
        self.target_pid
    }

    /// Queries the virtual stream format of the tap.
    ///
    /// Uses `kAudioStreamPropertyVirtualFormat` on the tap's `AudioObjectID`.
    pub fn get_stream_format(&self) -> AudioResult<sys::AudioStreamBasicDescription> {
        let address = sys::AudioObjectPropertyAddress {
            mSelector: sys::kAudioStreamPropertyVirtualFormat,
            mScope: sys::kAudioObjectPropertyScopeGlobal,
            mElement: sys::kAudioObjectPropertyElementMaster,
        };
        let mut asbd: sys::AudioStreamBasicDescription = unsafe { std::mem::zeroed() };
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
    fn drop(&mut self) {
        if self.tap_id != 0 {
            let status = unsafe { AudioHardwareDestroyProcessTap(self.tap_id) };
            if status != sys::noErr as OSStatus {
                log::warn!(
                    "Error destroying CoreAudioProcessTap (tap_id: {}): OSStatus {}",
                    self.tap_id,
                    status
                );
            } else {
                log::debug!(
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
        description: id,
        outTapID: *mut sys::AudioObjectID,
    ) -> OSStatus;

    fn AudioHardwareDestroyProcessTap(tapID: sys::AudioObjectID) -> OSStatus;
}

/// Helper function to check if an object responds to a selector.
unsafe fn msg_send_responds_to(obj: id, sel: Sel) -> bool {
    let responds: BOOL = msg_send![obj, respondsToSelector: sel];
    responds == YES
}
