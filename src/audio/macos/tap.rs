//! Manages the lifecycle of a Core Audio Process Tap and its aggregate device
//! for a specific process on macOS. Requires macOS 14.4+.
//!
//! The Process Tap API creates a tap on a process's audio output. To actually
//! capture audio from this tap, it must be wrapped in an aggregate device.
//! The aggregate device combines the tap with the system's default output
//! device, enabling AUHAL to read from it.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────┐     ┌──────────────────┐     ┌────────────────┐
//! │ Target App  │────▶│  Process Tap      │────▶│  Aggregate     │────▶ AUHAL
//! │ (PID)       │     │  (CATapDescription│     │  Device        │
//! └─────────────┘     │   + tap_id)       │     │  (agg_id)      │
//!                     └──────────────────┘     └────────────────┘
//! ```
//!
//! Both reference implementations (Swift AudioCap + C++ audio-rec) use this
//! pattern. Passing the raw `tap_id` directly to AUHAL does NOT work.

#![cfg(target_os = "macos")]

use super::coreaudio::map_ca_error;
use crate::core::error::{AudioError, AudioResult};
use cocoa::base::{id, nil};
use cocoa::foundation::{NSAutoreleasePool, NSString};
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFMutableDictionary;
use core_foundation::string::CFString;
use core_foundation_sys::array::{kCFTypeArrayCallBacks, CFArrayCreate};
use core_foundation_sys::base::{kCFAllocatorDefault, CFRelease, CFTypeRef, OSStatus};
use core_foundation_sys::dictionary::CFDictionaryRef;
use coreaudio::Error as CAError;
use coreaudio_sys as sys;
use objc::runtime::{Class, Sel, BOOL, NO, YES};
use objc::{class, msg_send, sel, sel_impl};
use std::ffi::c_void;

// ── Aggregate device dictionary keys (from CoreAudio/AudioHardware.h) ────

/// `kAudioAggregateDeviceNameKey`
const AGG_DEVICE_NAME_KEY: &str = "name";
/// `kAudioAggregateDeviceUIDKey`
const AGG_DEVICE_UID_KEY: &str = "uid";
/// `kAudioAggregateDeviceMainSubDeviceKey`
const AGG_DEVICE_MAIN_SUBDEVICE_KEY: &str = "master";
/// `kAudioAggregateDeviceIsPrivateKey`
const AGG_DEVICE_IS_PRIVATE_KEY: &str = "private";
/// `kAudioAggregateDeviceIsStackedKey`
const AGG_DEVICE_IS_STACKED_KEY: &str = "stacked";
/// `kAudioAggregateDeviceTapAutoStartKey` (macOS 14.4+)
const AGG_DEVICE_TAP_AUTO_START_KEY: &str = "tap_auto_start";
/// `kAudioAggregateDeviceSubDeviceListKey`
const AGG_DEVICE_SUBDEVICE_LIST_KEY: &str = "subdevices";
/// `kAudioAggregateDeviceTapListKey` (macOS 14.4+)
const AGG_DEVICE_TAP_LIST_KEY: &str = "taps";

/// `kAudioSubDeviceUIDKey`
const SUB_DEVICE_UID_KEY: &str = "uid";
/// `kAudioSubTapUIDKey` (macOS 14.4+)
const SUB_TAP_UID_KEY: &str = "uid";
/// `kAudioSubTapDriftCompensationKey` (macOS 14.4+)
const SUB_TAP_DRIFT_COMPENSATION_KEY: &str = "drift_compensation";

// ── CoreAudioProcessTap ──────────────────────────────────────────────────

/// Represents a Core Audio Tap + aggregate device targeting a specific process.
///
/// This struct handles the creation, configuration, and destruction of:
/// 1. A `CATapDescription` + `AudioHardwareCreateProcessTap` to create a tap
/// 2. An aggregate device wrapping the tap + default output device
///
/// The aggregate device is what AUHAL should read from — not the raw tap.
/// Call `.id()` to get the aggregate device's `AudioObjectID` for AUHAL.
///
/// **Cleanup order:** On drop, the aggregate device is destroyed first, then
/// the process tap. This matches both reference implementations.
///
/// Requires macOS 14.4+.
#[derive(Debug)]
pub struct CoreAudioProcessTap {
    tap_id: sys::AudioObjectID,
    aggregate_device_id: sys::AudioObjectID,
    #[allow(dead_code)]
    tap_uuid_string: String,
    target_pid: u32,
}

impl CoreAudioProcessTap {
    /// Creates and configures a new Core Audio Tap + aggregate device for
    /// the given `target_pid`.
    ///
    /// `tap_name_str` is a descriptive name for the tap (e.g., "rsac-tap-1234").
    ///
    /// This method:
    /// 1. Creates ObjC objects for the tap name and PID
    /// 2. Allocates and initializes a `CATapDescription`
    /// 3. Configures process targeting, UUID, mute behavior, and privacy
    /// 4. Calls `AudioHardwareCreateProcessTap`
    /// 5. Gets the default system output device UID
    /// 6. Builds the aggregate device dictionary
    /// 7. Calls `AudioHardwareCreateAggregateDevice`
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

            // 7. Set UUID on tap description (REQUIRED for aggregate device)
            let nsuuid_class = class!(NSUUID);
            let tap_uuid: id = msg_send![nsuuid_class, UUID];
            let _: () = msg_send![tap_desc_obj, setUUID: tap_uuid];

            // 8. Set mute behavior to unmuted (CATapUnmuted = 0)
            let _: () = msg_send![tap_desc_obj, setMuteBehavior: 0i32];

            // 9. Set privateTap = true
            let _: () = msg_send![tap_desc_obj, setPrivateTap: YES];

            // 10. Set mixdown = true (stereo mixdown)
            let _: () = msg_send![tap_desc_obj, setMixdown: YES];

            // 11. Call AudioHardwareCreateProcessTap
            let mut tap_id: sys::AudioObjectID = 0;
            let status: OSStatus = AudioHardwareCreateProcessTap(tap_desc_obj, &mut tap_id);

            if status != sys::noErr as OSStatus {
                return Err(map_ca_error(CAError::Unknown(status)));
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

            // 12. Read tap UUID string for use in aggregate device dictionary
            let uuid_nsstring: id = msg_send![tap_uuid, UUIDString];
            let uuid_cstr = cocoa::foundation::NSString::UTF8String(uuid_nsstring);
            let tap_uuid_str = std::ffi::CStr::from_ptr(uuid_cstr)
                .to_string_lossy()
                .into_owned();

            log::debug!(
                "Process tap created: tap_id={}, uuid={}",
                tap_id,
                tap_uuid_str
            );

            // 13. Get default output device UID
            let output_uid = get_default_output_device_uid()?;
            log::debug!("Default output device UID: {}", output_uid.to_string());

            // 14. Build aggregate device dictionary
            let agg_dict = build_aggregate_device_dict(&output_uid, &tap_uuid_str, target_pid)?;

            // 15. Create aggregate device
            let mut aggregate_device_id: sys::AudioObjectID = 0;
            let agg_status = AudioHardwareCreateAggregateDevice(
                agg_dict.as_concrete_TypeRef() as CFDictionaryRef,
                &mut aggregate_device_id,
            );

            if agg_status != sys::noErr as OSStatus {
                // Clean up: destroy the already-created tap before returning error
                AudioHardwareDestroyProcessTap(tap_id);
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "create_aggregate_device".into(),
                    message: format!(
                        "AudioHardwareCreateAggregateDevice failed: OSStatus {}",
                        agg_status
                    ),
                    context: None,
                });
            }

            log::info!(
                "Aggregate device created: agg_id={}, tap_id={}, uuid={}",
                aggregate_device_id,
                tap_id,
                tap_uuid_str
            );

            Ok(Self {
                tap_id,
                aggregate_device_id,
                tap_uuid_string: tap_uuid_str,
                target_pid,
            })
        }
    }

    /// Creates and configures a new Core Audio Tap + aggregate device that
    /// captures audio from a process tree (parent + all direct children).
    ///
    /// Uses `sysinfo` to enumerate child processes of `parent_pid`, then
    /// creates a `CATapDescription` targeting all discovered PIDs.
    ///
    /// If no child processes are found, the tap is created with just the
    /// parent PID (graceful degradation to single-process capture).
    ///
    /// **Important:** Requires macOS 14.4+ and the `CATapDescription` class.
    pub fn new_tree(parent_pid: u32) -> AudioResult<Self> {
        // ── Step 1: Discover child processes via sysinfo ──
        use sysinfo::{ProcessRefreshKind, RefreshKind, System, UpdateKind};

        let refresh_kind = RefreshKind::nothing()
            .with_processes(ProcessRefreshKind::nothing().with_parent(UpdateKind::Always));
        let sys = System::new_with_specifics(refresh_kind);

        let parent_sysinfo_pid = sysinfo::Pid::from_u32(parent_pid);

        // Collect parent + direct children
        let mut all_pids: Vec<u32> = vec![parent_pid];
        for (pid, process) in sys.processes() {
            if let Some(ppid) = process.parent() {
                if ppid == parent_sysinfo_pid && *pid != parent_sysinfo_pid {
                    all_pids.push(pid.as_u32());
                }
            }
        }
        all_pids.sort();
        all_pids.dedup();

        log::info!(
            "ProcessTree capture: parent_pid={}, discovered {} total PIDs: {:?}",
            parent_pid,
            all_pids.len(),
            all_pids
        );

        // ── Step 2: Create the tap with all discovered PIDs ──
        unsafe {
            let _pool = NSAutoreleasePool::new(nil);

            // Build NSArray of NSNumber PIDs
            let pid_nsnumbers: Vec<id> = all_pids
                .iter()
                .map(|&pid| {
                    let n: id = msg_send![class!(NSNumber), numberWithInt: pid as i32];
                    n
                })
                .collect();

            // Check none are nil
            for (i, &n) in pid_nsnumbers.iter().enumerate() {
                if n == nil {
                    return Err(AudioError::BackendError {
                        backend: "CoreAudio".into(),
                        operation: "process_tap_tree".into(),
                        message: format!(
                            "Failed to create NSNumber for PID {} (index {})",
                            all_pids[i], i
                        ),
                        context: None,
                    });
                }
            }

            // Create NSArray from the Vec of NSNumber objects
            let pids_nsarray: id = msg_send![
                class!(NSArray),
                arrayWithObjects: pid_nsnumbers.as_ptr()
                count: pid_nsnumbers.len()
            ];
            if pids_nsarray == nil {
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "process_tap_tree".into(),
                    message: "Failed to create NSArray for PIDs".into(),
                    context: None,
                });
            }

            // Allocate and initialize CATapDescription
            let ca_tap_description_class = Class::get("CATapDescription");
            if ca_tap_description_class.is_none() {
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "process_tap_tree".into(),
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
                    operation: "process_tap_tree".into(),
                    message: "Failed to allocate or initialize CATapDescription".into(),
                    context: None,
                });
            }

            // Set name
            let tap_name_str = format!("rsac-tap-tree-{}", parent_pid);
            let tap_name_nsstring = NSString::alloc(nil).init_str(&tap_name_str);
            if tap_name_nsstring == nil {
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "process_tap_tree".into(),
                    message: "Failed to create NSString for tap name".into(),
                    context: None,
                });
            }
            let _: () = msg_send![tap_desc_obj, setName: tap_name_nsstring];

            // Set processes and exclusive (non-exclusive: hear audio + capture it)
            let sel_set_processes_exclusive = sel!(setProcesses:exclusive:);
            if msg_send_responds_to(tap_desc_obj, sel_set_processes_exclusive) {
                let _: () = msg_send![tap_desc_obj, setProcesses: pids_nsarray exclusive: NO];
            } else {
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "process_tap_tree".into(),
                    message:
                        "CATapDescription does not respond to setProcesses:exclusive:. Check API."
                            .into(),
                    context: None,
                });
            }

            // Set UUID on tap description
            let nsuuid_class = class!(NSUUID);
            let tap_uuid: id = msg_send![nsuuid_class, UUID];
            let _: () = msg_send![tap_desc_obj, setUUID: tap_uuid];

            // Set mute behavior to unmuted (CATapUnmuted = 0)
            let _: () = msg_send![tap_desc_obj, setMuteBehavior: 0i32];

            // Set privateTap = true
            let _: () = msg_send![tap_desc_obj, setPrivateTap: YES];

            // Set mixdown = true (stereo mixdown)
            let _: () = msg_send![tap_desc_obj, setMixdown: YES];

            // Call AudioHardwareCreateProcessTap
            let mut tap_id: sys::AudioObjectID = 0;
            let status: OSStatus = AudioHardwareCreateProcessTap(tap_desc_obj, &mut tap_id);

            if status != sys::noErr as OSStatus {
                return Err(map_ca_error(CAError::Unknown(status)));
            }

            if tap_id == 0 {
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "process_tap_tree".into(),
                    message:
                        "AudioHardwareCreateProcessTap succeeded but returned an invalid tap_id (0)"
                            .into(),
                    context: None,
                });
            }

            // Read tap UUID string for use in aggregate device dictionary
            let uuid_nsstring: id = msg_send![tap_uuid, UUIDString];
            let uuid_cstr = cocoa::foundation::NSString::UTF8String(uuid_nsstring);
            let tap_uuid_str = std::ffi::CStr::from_ptr(uuid_cstr)
                .to_string_lossy()
                .into_owned();

            log::debug!(
                "Process tree tap created: tap_id={}, uuid={}, pids={:?}",
                tap_id,
                tap_uuid_str,
                all_pids
            );

            // Get default output device UID
            let output_uid = get_default_output_device_uid()?;
            log::debug!("Default output device UID: {}", output_uid.to_string());

            // Build aggregate device dictionary
            let agg_dict = build_aggregate_device_dict(&output_uid, &tap_uuid_str, parent_pid)?;

            // Create aggregate device
            let mut aggregate_device_id: sys::AudioObjectID = 0;
            let agg_status = AudioHardwareCreateAggregateDevice(
                agg_dict.as_concrete_TypeRef() as CFDictionaryRef,
                &mut aggregate_device_id,
            );

            if agg_status != sys::noErr as OSStatus {
                // Clean up: destroy the already-created tap before returning error
                AudioHardwareDestroyProcessTap(tap_id);
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "create_aggregate_device".into(),
                    message: format!(
                        "AudioHardwareCreateAggregateDevice failed: OSStatus {}",
                        agg_status
                    ),
                    context: None,
                });
            }

            log::info!(
                "Aggregate device created for process tree: agg_id={}, tap_id={}, uuid={}, pids={:?}",
                aggregate_device_id,
                tap_id,
                tap_uuid_str,
                all_pids
            );

            Ok(Self {
                tap_id,
                aggregate_device_id,
                tap_uuid_string: tap_uuid_str,
                target_pid: parent_pid,
            })
        }
    }

    /// Returns the aggregate device's `AudioObjectID`.
    ///
    /// This is the device ID to use with AUHAL's `kAudioOutputUnitProperty_CurrentDevice`.
    /// Do NOT use the raw tap ID with AUHAL — it must go through the aggregate device.
    pub fn id(&self) -> sys::AudioObjectID {
        self.aggregate_device_id
    }

    /// Returns the raw tap `AudioObjectID` (for debugging or format queries).
    #[allow(dead_code)]
    pub fn raw_tap_id(&self) -> sys::AudioObjectID {
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
    /// Note: This queries the tap directly, not the aggregate device.
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
            return Err(map_ca_error(CAError::Unknown(status)));
        }
        Ok(asbd)
    }
}

impl Drop for CoreAudioProcessTap {
    /// Destroys the aggregate device and process tap when the struct goes out of scope.
    ///
    /// **Order matters:** The aggregate device must be destroyed before the tap.
    /// This matches both reference implementations (Swift AudioCap + C++ audio-rec).
    fn drop(&mut self) {
        unsafe {
            // Destroy aggregate device first
            if self.aggregate_device_id != 0 {
                let status = AudioHardwareDestroyAggregateDevice(self.aggregate_device_id);
                if status != sys::noErr as OSStatus {
                    log::warn!(
                        "Failed to destroy aggregate device {}: OSStatus {}",
                        self.aggregate_device_id,
                        status
                    );
                } else {
                    log::debug!(
                        "Aggregate device {} destroyed successfully.",
                        self.aggregate_device_id
                    );
                }
            }

            // Then destroy the process tap
            if self.tap_id != 0 {
                let status = AudioHardwareDestroyProcessTap(self.tap_id);
                if status != sys::noErr as OSStatus {
                    log::warn!(
                        "Failed to destroy process tap {}: OSStatus {}",
                        self.tap_id,
                        status
                    );
                } else {
                    log::debug!("Process tap {} destroyed successfully.", self.tap_id);
                }
            }
        }
    }
}

// ── FFI declarations ─────────────────────────────────────────────────────

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioHardwareCreateProcessTap(
        description: id,
        outTapID: *mut sys::AudioObjectID,
    ) -> OSStatus;

    fn AudioHardwareDestroyProcessTap(tapID: sys::AudioObjectID) -> OSStatus;

    fn AudioHardwareCreateAggregateDevice(
        inDescription: CFDictionaryRef,
        outDeviceID: *mut sys::AudioObjectID,
    ) -> OSStatus;

    fn AudioHardwareDestroyAggregateDevice(inDeviceID: sys::AudioObjectID) -> OSStatus;
}

// ── Helper functions ─────────────────────────────────────────────────────

/// Get the UID string of the default system output audio device.
///
/// Returns an owned `CFString` containing the device UID, which is needed
/// for the aggregate device dictionary's sub-device list and master key.
unsafe fn get_default_output_device_uid() -> AudioResult<CFString> {
    // Step 1: Get default output device ID
    let mut default_device_id: sys::AudioObjectID = 0;
    let mut size = std::mem::size_of::<sys::AudioObjectID>() as u32;

    let addr = sys::AudioObjectPropertyAddress {
        mSelector: sys::kAudioHardwarePropertyDefaultSystemOutputDevice,
        mScope: sys::kAudioObjectPropertyScopeGlobal,
        mElement: sys::kAudioObjectPropertyElementMaster,
    };

    let status = sys::AudioObjectGetPropertyData(
        sys::kAudioObjectSystemObject,
        &addr,
        0,
        std::ptr::null(),
        &mut size,
        &mut default_device_id as *mut _ as *mut c_void,
    );

    if status != sys::noErr as OSStatus {
        return Err(AudioError::DeviceNotFound {
            device_id: format!(
                "default_output (kAudioHardwarePropertyDefaultSystemOutputDevice failed: OSStatus {})",
                status
            ),
        });
    }

    if default_device_id == 0 {
        return Err(AudioError::DeviceNotFound {
            device_id: "default_output (no default output device found)".into(),
        });
    }

    // Step 2: Get the device UID string (CFStringRef)
    let mut uid_ref: core_foundation_sys::string::CFStringRef = std::ptr::null();
    let mut uid_size = std::mem::size_of::<core_foundation_sys::string::CFStringRef>() as u32;

    let uid_addr = sys::AudioObjectPropertyAddress {
        mSelector: sys::kAudioDevicePropertyDeviceUID,
        mScope: sys::kAudioObjectPropertyScopeGlobal,
        mElement: sys::kAudioObjectPropertyElementMaster,
    };

    let status = sys::AudioObjectGetPropertyData(
        default_device_id,
        &uid_addr,
        0,
        std::ptr::null(),
        &mut uid_size,
        &mut uid_ref as *mut _ as *mut c_void,
    );

    if status != sys::noErr as OSStatus {
        return Err(AudioError::BackendError {
            backend: "CoreAudio".into(),
            operation: "get_device_uid".into(),
            message: format!(
                "Failed to get device UID for device {}: OSStatus {}",
                default_device_id, status
            ),
            context: None,
        });
    }

    if uid_ref.is_null() {
        return Err(AudioError::BackendError {
            backend: "CoreAudio".into(),
            operation: "get_device_uid".into(),
            message: format!("Device {} returned null UID", default_device_id),
            context: None,
        });
    }

    // Convert CFStringRef to owned CFString (Get Rule: the system owns it, we retain)
    Ok(CFString::wrap_under_get_rule(uid_ref))
}

/// Build the `CFDictionary` for `AudioHardwareCreateAggregateDevice`.
///
/// The dictionary structure follows the CoreAudio aggregate device specification,
/// matching the pattern used by both reference implementations:
///
/// ```text
/// {
///   name: "rsac-agg-{pid}",
///   uid: "rsac-agg-uid-{pid}",
///   master: <output_device_uid>,
///   private: true,
///   stacked: false,
///   tap_auto_start: true,
///   subdevices: [ { uid: <output_device_uid> } ],
///   taps: [ { uid: <tap_uuid>, drift_compensation: true } ],
/// }
/// ```
unsafe fn build_aggregate_device_dict(
    output_uid: &CFString,
    tap_uuid_str: &str,
    pid: u32,
) -> AudioResult<CFMutableDictionary> {
    // --- Build tap inner dict: { uid: tapUUID, drift_compensation: true } ---
    let mut tap_inner = CFMutableDictionary::new();
    let tap_uid_key = CFString::new(SUB_TAP_UID_KEY);
    let tap_uuid_val = CFString::new(tap_uuid_str);
    tap_inner.add(
        &(tap_uid_key.as_concrete_TypeRef() as *const c_void),
        &(tap_uuid_val.as_concrete_TypeRef() as *const c_void),
    );
    let tap_drift_key = CFString::new(SUB_TAP_DRIFT_COMPENSATION_KEY);
    tap_inner.add(
        &(tap_drift_key.as_concrete_TypeRef() as *const c_void),
        &(CFBoolean::true_value().as_concrete_TypeRef() as *const c_void),
    );

    // Wrap tap dict in a single-element CFArray
    let tap_dict_ref = tap_inner.as_concrete_TypeRef() as CFTypeRef;
    let tap_array = CFArrayCreate(
        kCFAllocatorDefault,
        &tap_dict_ref as *const CFTypeRef,
        1,
        &kCFTypeArrayCallBacks,
    );
    if tap_array.is_null() {
        return Err(AudioError::BackendError {
            backend: "CoreAudio".into(),
            operation: "build_aggregate_dict".into(),
            message: "Failed to create tap list CFArray".into(),
            context: None,
        });
    }

    // --- Build sub-device inner dict: { uid: outputUID } ---
    let mut sub_inner = CFMutableDictionary::new();
    let sub_uid_key = CFString::new(SUB_DEVICE_UID_KEY);
    sub_inner.add(
        &(sub_uid_key.as_concrete_TypeRef() as *const c_void),
        &(output_uid.as_concrete_TypeRef() as *const c_void),
    );

    // Wrap sub-device dict in a single-element CFArray
    let sub_dict_ref = sub_inner.as_concrete_TypeRef() as CFTypeRef;
    let sub_array = CFArrayCreate(
        kCFAllocatorDefault,
        &sub_dict_ref as *const CFTypeRef,
        1,
        &kCFTypeArrayCallBacks,
    );
    if sub_array.is_null() {
        CFRelease(tap_array as *const c_void);
        return Err(AudioError::BackendError {
            backend: "CoreAudio".into(),
            operation: "build_aggregate_dict".into(),
            message: "Failed to create sub-device list CFArray".into(),
            context: None,
        });
    }

    // --- Build top-level aggregate device dictionary ---
    let mut dict = CFMutableDictionary::new();

    // name
    let k_name = CFString::new(AGG_DEVICE_NAME_KEY);
    let v_name = CFString::new(&format!("rsac-agg-{}", pid));
    dict.add(
        &(k_name.as_concrete_TypeRef() as *const c_void),
        &(v_name.as_concrete_TypeRef() as *const c_void),
    );

    // uid
    let k_uid = CFString::new(AGG_DEVICE_UID_KEY);
    let v_uid = CFString::new(&format!("rsac-agg-uid-{}", pid));
    dict.add(
        &(k_uid.as_concrete_TypeRef() as *const c_void),
        &(v_uid.as_concrete_TypeRef() as *const c_void),
    );

    // master sub-device = output device UID
    let k_master = CFString::new(AGG_DEVICE_MAIN_SUBDEVICE_KEY);
    dict.add(
        &(k_master.as_concrete_TypeRef() as *const c_void),
        &(output_uid.as_concrete_TypeRef() as *const c_void),
    );

    // private = true
    let k_private = CFString::new(AGG_DEVICE_IS_PRIVATE_KEY);
    dict.add(
        &(k_private.as_concrete_TypeRef() as *const c_void),
        &(CFBoolean::true_value().as_concrete_TypeRef() as *const c_void),
    );

    // stacked = false
    let k_stacked = CFString::new(AGG_DEVICE_IS_STACKED_KEY);
    dict.add(
        &(k_stacked.as_concrete_TypeRef() as *const c_void),
        &(CFBoolean::false_value().as_concrete_TypeRef() as *const c_void),
    );

    // tap_auto_start = true
    let k_auto_start = CFString::new(AGG_DEVICE_TAP_AUTO_START_KEY);
    dict.add(
        &(k_auto_start.as_concrete_TypeRef() as *const c_void),
        &(CFBoolean::true_value().as_concrete_TypeRef() as *const c_void),
    );

    // sub-device list (array of dicts)
    let k_subdevices = CFString::new(AGG_DEVICE_SUBDEVICE_LIST_KEY);
    dict.add(
        &(k_subdevices.as_concrete_TypeRef() as *const c_void),
        &(sub_array as *const c_void),
    );

    // tap list (array of dicts)
    let k_taps = CFString::new(AGG_DEVICE_TAP_LIST_KEY);
    dict.add(
        &(k_taps.as_concrete_TypeRef() as *const c_void),
        &(tap_array as *const c_void),
    );

    // Release the arrays — dict.add() has already retained them via CFDictionaryAddValue
    CFRelease(tap_array as *const c_void);
    CFRelease(sub_array as *const c_void);

    Ok(dict)
}

/// Helper function to check if an object responds to a selector.
unsafe fn msg_send_responds_to(obj: id, sel: Sel) -> bool {
    let responds: BOOL = msg_send![obj, respondsToSelector: sel];
    responds == YES
}
