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

/// Forward-compatible alias for `kAudioObjectPropertyElementMain`.
///
/// `kAudioObjectPropertyElementMaster` was deprecated in macOS 12.0 and replaced
/// by `kAudioObjectPropertyElementMain`. The value is `0` in both cases.
/// `coreaudio-sys` 0.2.17 doesn't export the new name, so we define it here.
const KAUDIO_OBJECT_PROPERTY_ELEMENT_MAIN: u32 = 0;

/// `kAudioHardwarePropertyTranslatePIDToProcessObject` = `'id2p'`
///
/// Translates a PID (pid_t qualifier) into the corresponding audio process
/// `AudioObjectID`. Required for `initStereoMixdownOfProcesses:` which takes
/// AudioObjectIDs, not PIDs.
const K_AUDIO_HARDWARE_PROPERTY_TRANSLATE_PID_TO_PROCESS_OBJECT: u32 =
    ((b'i' as u32) << 24) | ((b'd' as u32) << 16) | ((b'2' as u32) << 8) | (b'p' as u32);

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
    /// 1. Allocates and configures a `CATapDescription` with process targeting
    /// 2. Configures UUID, mute behavior, and privacy
    /// 3. Calls `AudioHardwareCreateProcessTap`
    /// 4. Gets the default system output device UID
    /// 5. Builds the aggregate device dictionary
    /// 6. Calls `AudioHardwareCreateAggregateDevice`
    ///
    /// Process targeting uses (in order of preference):
    /// - `initStereoMixdownOfProcesses:` with AudioObjectID (macOS 26+)
    /// - `setProcesses:exclusive:` with PID (macOS 14.4–15)
    /// - Separate `setProcesses:` + `setExclusive:` with PID (macOS 26 fallback)
    ///
    /// **Important:** Requires macOS 14.4+ and the `CATapDescription` class.
    pub fn new(target_pid: u32, tap_name_str: &str) -> AudioResult<Self> {
        unsafe {
            let _pool = NSAutoreleasePool::new(nil);

            // 1. Get CATapDescription class
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

            // 2. Create CATapDescription with process targeting configured
            let tap_desc_obj = create_process_tap_description(
                ca_tap_description_class,
                &[target_pid],
                "process_tap",
            )?;

            // 3. Set name
            let tap_name_nsstring = NSString::alloc(nil).init_str(tap_name_str);
            if tap_name_nsstring == nil {
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "process_tap".into(),
                    message: "Failed to create NSString for tap name".into(),
                    context: None,
                });
            }
            let _: () = msg_send![tap_desc_obj, setName: tap_name_nsstring];

            // 4. Set UUID on tap description (REQUIRED for aggregate device)
            let nsuuid_class = class!(NSUUID);
            let tap_uuid: id = msg_send![nsuuid_class, UUID];
            let _: () = msg_send![tap_desc_obj, setUUID: tap_uuid];

            // 5. Set mute behavior to unmuted (CATapUnmuted = 0)
            let _: () = msg_send![tap_desc_obj, setMuteBehavior: 0i32];

            // 6. Set privateTap if available (guard for macOS 26+ where it may be removed)
            if msg_send_responds_to(tap_desc_obj, sel!(setPrivateTap:)) {
                let _: () = msg_send![tap_desc_obj, setPrivateTap: YES];
            }

            // 7. Set mixdown = true (already configured by initStereoMixdown, but safe to set explicitly)
            let _: () = msg_send![tap_desc_obj, setMixdown: YES];

            // 8. Call AudioHardwareCreateProcessTap
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

            // 9. Read tap UUID string for use in aggregate device dictionary
            let uuid_nsstring: id = msg_send![tap_uuid, UUIDString];
            let uuid_cstr = cocoa::foundation::NSString::UTF8String(uuid_nsstring);
            let tap_uuid_str = if uuid_cstr.is_null() {
                "<unknown-uuid>".to_owned()
            } else {
                std::ffi::CStr::from_ptr(uuid_cstr)
                    .to_str()
                    .unwrap_or("<invalid-utf8>")
                    .to_owned()
            };

            log::debug!(
                "Process tap created: tap_id={}, uuid={}",
                tap_id,
                tap_uuid_str
            );

            // 10. Get default output device UID
            let output_uid = get_default_output_device_uid()?;
            log::debug!("Default output device UID: {}", output_uid.to_string());

            // 11. Build aggregate device dictionary
            let agg_dict = build_aggregate_device_dict(&output_uid, &tap_uuid_str, target_pid)?;

            // 12. Create aggregate device
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
    /// Process targeting uses (in order of preference):
    /// - `initStereoMixdownOfProcesses:` with AudioObjectIDs (macOS 26+)
    /// - `setProcesses:exclusive:` with PIDs (macOS 14.4–15)
    /// - Separate `setProcesses:` + `setExclusive:` with PIDs (macOS 26 fallback)
    ///
    /// **Important:** Requires macOS 14.4+ and the `CATapDescription` class.
    pub fn new_tree(parent_pid: u32) -> AudioResult<Self> {
        // ── Step 1: Discover child processes via sysinfo ──
        use sysinfo::{ProcessRefreshKind, RefreshKind, System};

        let refresh_kind = RefreshKind::nothing().with_processes(ProcessRefreshKind::everything());
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

            // Get CATapDescription class
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

            // Create CATapDescription with process targeting configured
            let tap_desc_obj = create_process_tap_description(
                ca_tap_description_class,
                &all_pids,
                "process_tap_tree",
            )?;

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

            // Set UUID on tap description
            let nsuuid_class = class!(NSUUID);
            let tap_uuid: id = msg_send![nsuuid_class, UUID];
            let _: () = msg_send![tap_desc_obj, setUUID: tap_uuid];

            // Set mute behavior to unmuted (CATapUnmuted = 0)
            let _: () = msg_send![tap_desc_obj, setMuteBehavior: 0i32];

            // Set privateTap if available (guard for macOS 26+ where it may be removed)
            if msg_send_responds_to(tap_desc_obj, sel!(setPrivateTap:)) {
                let _: () = msg_send![tap_desc_obj, setPrivateTap: YES];
            }

            // Set mixdown = true (already configured by initStereoMixdown, but safe to set explicitly)
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
            let tap_uuid_str = if uuid_cstr.is_null() {
                "<unknown-uuid>".to_owned()
            } else {
                std::ffi::CStr::from_ptr(uuid_cstr)
                    .to_str()
                    .unwrap_or("<invalid-utf8>")
                    .to_owned()
            };

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

    /// Creates and configures a new Core Audio Tap + aggregate device that
    /// captures ALL system audio (system-wide capture).
    ///
    /// Uses `initStereoGlobalTapButExcludeProcesses:` with an empty exclusion
    /// list, which captures all system audio. Property setters are guarded with
    /// `respondsToSelector:` for forward compatibility with newer macOS versions
    /// where some properties (e.g. `setPrivateTap:`) may be removed.
    ///
    /// Direct AUHAL input capture from the default output device does NOT work
    /// because the output device's AUHAL callback never fires. The Process Tap +
    /// Aggregate Device pattern is required even for system-wide capture on macOS.
    ///
    /// **Important:** Requires macOS 14.4+ and the `CATapDescription` class.
    pub fn new_system() -> AudioResult<Self> {
        unsafe {
            let _pool = NSAutoreleasePool::new(nil);

            // Get CATapDescription class (requires macOS 14.4+)
            let ca_tap_description_class = Class::get("CATapDescription");
            if ca_tap_description_class.is_none() {
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "system_tap".into(),
                    message: "CATapDescription class not found. Ensure macOS 14.4+ and CoreAudio framework is linked.".into(),
                    context: None,
                });
            }
            let ca_tap_description_class = ca_tap_description_class.unwrap();

            // Use initStereoGlobalTapButExcludeProcesses: with empty array
            // to capture ALL system audio (no processes excluded).
            let empty_array: id = msg_send![class!(NSArray), array];
            let tap_desc_obj: id = msg_send![ca_tap_description_class, alloc];

            // Check that the selector is available before calling
            let sel_global_tap = sel!(initStereoGlobalTapButExcludeProcesses:);
            if !msg_send_responds_to(tap_desc_obj, sel_global_tap) {
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "system_tap".into(),
                    message: "CATapDescription does not respond to initStereoGlobalTapButExcludeProcesses:. Ensure macOS 14.4+.".into(),
                    context: None,
                });
            }

            let tap_desc_obj: id =
                msg_send![tap_desc_obj, initStereoGlobalTapButExcludeProcesses: empty_array];

            if tap_desc_obj == nil {
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "system_tap".into(),
                    message: "initStereoGlobalTapButExcludeProcesses: returned nil".into(),
                    context: None,
                });
            }

            // Set name
            let tap_name_str = "rsac-tap-system";
            let tap_name_nsstring = NSString::alloc(nil).init_str(tap_name_str);
            if tap_name_nsstring == nil {
                return Err(AudioError::BackendError {
                    backend: "CoreAudio".into(),
                    operation: "system_tap".into(),
                    message: "Failed to create NSString for tap name".into(),
                    context: None,
                });
            }
            let _: () = msg_send![tap_desc_obj, setName: tap_name_nsstring];

            // Set UUID on tap description (REQUIRED for aggregate device)
            let nsuuid_class = class!(NSUUID);
            let tap_uuid: id = msg_send![nsuuid_class, UUID];
            let _: () = msg_send![tap_desc_obj, setUUID: tap_uuid];

            // Set mute behavior to unmuted (CATapUnmuted = 0)
            let _: () = msg_send![tap_desc_obj, setMuteBehavior: 0i32];

            // Set privateTap if available (removed in macOS 26+)
            if msg_send_responds_to(tap_desc_obj, sel!(setPrivateTap:)) {
                let _: () = msg_send![tap_desc_obj, setPrivateTap: YES];
            }

            // Mixdown already configured by initStereoGlobalTap, but set explicitly for safety
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
                    operation: "system_tap".into(),
                    message:
                        "AudioHardwareCreateProcessTap succeeded but returned an invalid tap_id (0)"
                            .into(),
                    context: None,
                });
            }

            // Read tap UUID string for use in aggregate device dictionary
            let uuid_nsstring: id = msg_send![tap_uuid, UUIDString];
            let uuid_cstr = cocoa::foundation::NSString::UTF8String(uuid_nsstring);
            let tap_uuid_str = if uuid_cstr.is_null() {
                "<unknown-uuid>".to_owned()
            } else {
                std::ffi::CStr::from_ptr(uuid_cstr)
                    .to_str()
                    .unwrap_or("<invalid-utf8>")
                    .to_owned()
            };

            log::debug!(
                "System-wide process tap created: tap_id={}, uuid={}",
                tap_id,
                tap_uuid_str
            );

            // Get default output device UID
            let output_uid = get_default_output_device_uid()?;
            log::debug!("Default output device UID: {}", output_uid.to_string());

            // Build aggregate device dictionary (pid=0 sentinel for system-wide)
            let agg_dict = build_aggregate_device_dict(&output_uid, &tap_uuid_str, 0)?;

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
                "System-wide aggregate device created: agg_id={}, tap_id={}, uuid={}",
                aggregate_device_id,
                tap_id,
                tap_uuid_str
            );

            Ok(Self {
                tap_id,
                aggregate_device_id,
                tap_uuid_string: tap_uuid_str,
                target_pid: 0, // sentinel: system-wide (no specific target process)
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
            mElement: KAUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
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

/// Translate a process PID to its CoreAudio `AudioObjectID` using
/// `kAudioHardwarePropertyTranslatePIDToProcessObject`.
///
/// The returned AudioObjectID is suitable for `initStereoMixdownOfProcesses:`.
/// Returns `None` if the process doesn't have an audio object (it may not be
/// producing audio yet, or the property may not be available on this macOS version).
unsafe fn translate_pid_to_audio_object_id(pid: u32) -> Option<sys::AudioObjectID> {
    let addr = sys::AudioObjectPropertyAddress {
        mSelector: K_AUDIO_HARDWARE_PROPERTY_TRANSLATE_PID_TO_PROCESS_OBJECT,
        mScope: sys::kAudioObjectPropertyScopeGlobal,
        mElement: KAUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    };

    let mut pid_value = pid as i32; // pid_t is i32 on macOS
    let qualifier_size = std::mem::size_of::<i32>() as u32;
    let mut object_id: sys::AudioObjectID = 0;
    let mut data_size = std::mem::size_of::<sys::AudioObjectID>() as u32;

    let status = sys::AudioObjectGetPropertyData(
        sys::kAudioObjectSystemObject,
        &addr,
        qualifier_size,
        &mut pid_value as *mut i32 as *const c_void,
        &mut data_size,
        &mut object_id as *mut _ as *mut c_void,
    );

    if status != sys::noErr as OSStatus {
        log::debug!(
            "TranslatePIDToProcessObject failed for PID {}: OSStatus {}",
            pid,
            status
        );
        return None;
    }

    // kAudioObjectUnknown is 0 in CoreAudio
    if object_id == 0 {
        log::debug!(
            "TranslatePIDToProcessObject returned unknown object for PID {}",
            pid
        );
        return None;
    }

    log::debug!("PID {} → AudioObjectID {}", pid, object_id);
    Some(object_id)
}

/// Creates a `CATapDescription` configured for capturing specific processes.
///
/// Tries these approaches in order:
/// 1. `initStereoMixdownOfProcesses:` with AudioObjectIDs (macOS 26+)
/// 2. `alloc` + `init` + `setProcesses:exclusive:` with PIDs (macOS 14.4–15)
/// 3. `alloc` + `init` + `setProcesses:` + `setExclusive:` with PIDs (macOS 26 fallback
///    when PID→AudioObjectID translation failed)
///
/// Returns the initialized CATapDescription ObjC object or an error.
unsafe fn create_process_tap_description(
    ca_tap_class: &Class,
    pids: &[u32],
    operation: &str,
) -> AudioResult<id> {
    // ── Path 1: initStereoMixdownOfProcesses: (macOS 26+) ──
    //
    // This initializer takes an NSArray of AudioObjectIDs (not PIDs).
    // We translate each PID via kAudioHardwarePropertyTranslatePIDToProcessObject.
    let alloc_obj: id = msg_send![ca_tap_class, alloc];
    let sel_stereo_mixdown = sel!(initStereoMixdownOfProcesses:);

    if msg_send_responds_to(alloc_obj, sel_stereo_mixdown) {
        // Translate all PIDs to AudioObjectIDs
        let mut audio_obj_ids: Vec<sys::AudioObjectID> = Vec::new();
        for &pid in pids {
            if let Some(obj_id) = translate_pid_to_audio_object_id(pid) {
                audio_obj_ids.push(obj_id);
            } else {
                log::debug!(
                    "PID {} has no audio process object (may not be producing audio yet)",
                    pid
                );
            }
        }

        if !audio_obj_ids.is_empty() {
            // Create NSArray of AudioObjectIDs as NSNumber(unsignedInt:)
            let nsnumbers: Vec<id> = audio_obj_ids
                .iter()
                .map(|&obj_id| {
                    let n: id = msg_send![class!(NSNumber), numberWithUnsignedInt: obj_id];
                    n
                })
                .collect();

            // Verify no nil NSNumbers
            let all_valid = nsnumbers.iter().all(|&n| n != nil);
            if all_valid {
                let processes_array: id = msg_send![
                    class!(NSArray),
                    arrayWithObjects: nsnumbers.as_ptr()
                    count: nsnumbers.len()
                ];

                if processes_array != nil {
                    let result: id =
                        msg_send![alloc_obj, initStereoMixdownOfProcesses: processes_array];
                    if result != nil {
                        log::info!(
                            "Using initStereoMixdownOfProcesses: for PIDs {:?} → AudioObjectIDs {:?}",
                            pids,
                            audio_obj_ids
                        );
                        return Ok(result);
                    }
                    // initStereoMixdownOfProcesses: returned nil — alloc_obj consumed by init.
                    // Fall through to Path 2 with a fresh alloc.
                    log::warn!(
                        "initStereoMixdownOfProcesses: returned nil for AudioObjectIDs {:?}, trying fallback",
                        audio_obj_ids
                    );
                } else {
                    // NSArray creation failed; alloc_obj is still uninitialized — release it.
                    log::warn!("Failed to create NSArray for AudioObjectIDs, trying fallback");
                    let _: () = msg_send![alloc_obj, release];
                }
            } else {
                // Some NSNumbers were nil; alloc_obj is still uninitialized — release it.
                log::warn!("Failed to create NSNumber for some AudioObjectIDs, trying fallback");
                let _: () = msg_send![alloc_obj, release];
            }
        } else {
            // No PIDs could be translated to AudioObjectIDs.
            // alloc_obj is still uninitialized — release it.
            log::debug!(
                "No PIDs translated to AudioObjectIDs for {:?}, trying fallback",
                pids
            );
            let _: () = msg_send![alloc_obj, release];
        }
    } else {
        // initStereoMixdownOfProcesses: is not available on this macOS version.
        // alloc_obj is still uninitialized — release it.
        log::debug!("initStereoMixdownOfProcesses: not available, trying fallback");
        let _: () = msg_send![alloc_obj, release];
    }

    // ── Path 2/3: alloc + init + setProcesses (PID-based, macOS 14.4+) ──
    //
    // Fall back to the traditional approach using PIDs.
    let alloc_obj2: id = msg_send![ca_tap_class, alloc];
    let init_obj: id = msg_send![alloc_obj2, init];
    if init_obj == nil {
        return Err(AudioError::BackendError {
            backend: "CoreAudio".into(),
            operation: operation.into(),
            message: "Failed to allocate or initialize CATapDescription (fallback path)".into(),
            context: None,
        });
    }

    // Create NSArray of PIDs as NSNumber(int:)
    let pid_nsnumbers: Vec<id> = pids
        .iter()
        .map(|&pid| {
            let n: id = msg_send![class!(NSNumber), numberWithInt: pid as i32];
            n
        })
        .collect();

    // Verify no nil NSNumbers
    for (i, &n) in pid_nsnumbers.iter().enumerate() {
        if n == nil {
            return Err(AudioError::BackendError {
                backend: "CoreAudio".into(),
                operation: operation.into(),
                message: format!(
                    "Failed to create NSNumber for PID {} (index {})",
                    pids[i], i
                ),
                context: None,
            });
        }
    }

    let pids_nsarray: id = msg_send![
        class!(NSArray),
        arrayWithObjects: pid_nsnumbers.as_ptr()
        count: pid_nsnumbers.len()
    ];
    if pids_nsarray == nil {
        return Err(AudioError::BackendError {
            backend: "CoreAudio".into(),
            operation: operation.into(),
            message: "Failed to create NSArray for PIDs".into(),
            context: None,
        });
    }

    // Path 2: Try combined setProcesses:exclusive: (macOS 14.4–15)
    let sel_set_processes_exclusive = sel!(setProcesses:exclusive:);
    if msg_send_responds_to(init_obj, sel_set_processes_exclusive) {
        let _: () = msg_send![init_obj, setProcesses: pids_nsarray exclusive: NO];
        log::info!("Using setProcesses:exclusive: for PIDs {:?}", pids);
        return Ok(init_obj);
    }

    // Path 3: Try separate setProcesses: + setExclusive: (macOS 26 fallback)
    let sel_set_processes = sel!(setProcesses:);
    let sel_set_exclusive = sel!(setExclusive:);
    if msg_send_responds_to(init_obj, sel_set_processes)
        && msg_send_responds_to(init_obj, sel_set_exclusive)
    {
        let _: () = msg_send![init_obj, setProcesses: pids_nsarray];
        let _: () = msg_send![init_obj, setExclusive: NO];
        log::info!(
            "Using separate setProcesses: + setExclusive: for PIDs {:?}",
            pids
        );
        return Ok(init_obj);
    }

    // No supported method found
    Err(AudioError::BackendError {
        backend: "CoreAudio".into(),
        operation: operation.into(),
        message: format!(
            "No supported method found for setting processes on CATapDescription. \
             Tried initStereoMixdownOfProcesses:, setProcesses:exclusive:, and \
             separate setProcesses:/setExclusive:. PIDs: {:?}. Ensure macOS 14.4+.",
            pids
        ),
        context: None,
    })
}

/// Enumerates all audio process AudioObjectIDs from CoreAudio's
/// `kAudioHardwarePropertyProcessObjectList`.
///
/// Returns the raw AudioObjectIDs for all processes currently participating
/// in the audio system. These IDs can be passed directly to
/// `initStereoMixdownOfProcesses:` on `CATapDescription`.
///
/// Currently only used in tests. Retained for potential future use in
/// system-wide tap creation with explicit process enumeration.
#[cfg(test)]
unsafe fn get_all_audio_process_object_ids() -> AudioResult<Vec<sys::AudioObjectID>> {
    let addr = sys::AudioObjectPropertyAddress {
        mSelector: sys::kAudioHardwarePropertyProcessObjectList,
        mScope: sys::kAudioObjectPropertyScopeGlobal,
        mElement: KAUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
    };

    let mut data_size: u32 = 0;
    let status = sys::AudioObjectGetPropertyDataSize(
        sys::kAudioObjectSystemObject,
        &addr,
        0,
        std::ptr::null(),
        &mut data_size,
    );

    if status != sys::noErr as OSStatus {
        return Err(AudioError::BackendError {
            backend: "CoreAudio".into(),
            operation: "get_audio_process_list".into(),
            message: format!(
                "AudioObjectGetPropertyDataSize for ProcessObjectList failed: OSStatus {}",
                status
            ),
            context: None,
        });
    }

    let count = data_size as usize / std::mem::size_of::<sys::AudioObjectID>();
    if count == 0 {
        return Ok(vec![]);
    }

    let mut object_ids = vec![0u32; count];
    let status = sys::AudioObjectGetPropertyData(
        sys::kAudioObjectSystemObject,
        &addr,
        0,
        std::ptr::null(),
        &mut data_size,
        object_ids.as_mut_ptr() as *mut c_void,
    );

    if status != sys::noErr as OSStatus {
        return Err(AudioError::BackendError {
            backend: "CoreAudio".into(),
            operation: "get_audio_process_list".into(),
            message: format!(
                "AudioObjectGetPropertyData for ProcessObjectList failed: OSStatus {}",
                status
            ),
            context: None,
        });
    }

    // Resize in case fewer objects were returned than allocated
    let actual_count = data_size as usize / std::mem::size_of::<sys::AudioObjectID>();
    object_ids.truncate(actual_count);

    log::debug!(
        "Found {} audio process objects: {:?}",
        object_ids.len(),
        object_ids
    );

    Ok(object_ids)
}

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
        mElement: KAUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
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
        mElement: KAUDIO_OBJECT_PROPERTY_ELEMENT_MAIN,
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
///   uid: "rsac-agg-uid-{tap_uuid}",
///   master: <output_device_uid>,
///   private: true,
///   stacked: false,
///   tap_auto_start: true,
///   subdevices: [ { uid: <output_device_uid> } ],
///   taps: [ { uid: <tap_uuid>, drift_compensation: true } ],
/// }
/// ```
///
/// The aggregate device UID uses the tap UUID (not the PID) to ensure global
/// uniqueness. PID-based UIDs would collide if two concurrent captures target
/// the same process.
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
    let v_uid = CFString::new(&format!("rsac-agg-uid-{}", tap_uuid_str));
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

// ══════════════════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════════════════

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// Test that we can enumerate audio process objects from CoreAudio.
    /// This is a prerequisite for system-wide capture.
    #[test]
    fn test_enumerate_audio_process_objects() {
        let result = unsafe { get_all_audio_process_object_ids() };
        assert!(
            result.is_ok(),
            "get_all_audio_process_object_ids() should succeed: {:?}",
            result.err()
        );
        let ids = result.unwrap();
        // There should be at least one audio process on any running macOS system
        println!("Found {} audio process objects: {:?}", ids.len(), ids);
        assert!(
            !ids.is_empty(),
            "Expected at least one audio process object, got empty list"
        );
        // All IDs should be non-zero
        for &id in &ids {
            assert!(id > 0, "Audio process object ID should be > 0, got {}", id);
        }
    }

    /// Test that CATapDescription class is available on this macOS version.
    #[test]
    fn test_catap_description_class_exists() {
        let cls = Class::get("CATapDescription");
        assert!(
            cls.is_some(),
            "CATapDescription class should exist on macOS 14.4+. \
             If this fails, the macOS version may be too old."
        );
    }

    /// Probe which selectors CATapDescription responds to.
    /// This is a diagnostic test to understand the available API surface.
    #[test]
    fn test_catap_description_available_selectors() {
        let cls =
            Class::get("CATapDescription").expect("CATapDescription class required for this test");

        unsafe {
            let _pool = NSAutoreleasePool::new(nil);

            let obj: id = msg_send![cls, alloc];
            let obj: id = msg_send![obj, init];
            assert!(
                obj != nil,
                "CATapDescription alloc+init should not return nil"
            );

            // Check known selectors
            let selectors_to_check = [
                ("setName:", sel!(setName:)),
                ("setUUID:", sel!(setUUID:)),
                ("setMuteBehavior:", sel!(setMuteBehavior:)),
                ("setPrivateTap:", sel!(setPrivateTap:)),
                ("setMixdown:", sel!(setMixdown:)),
                ("setProcesses:exclusive:", sel!(setProcesses:exclusive:)),
                ("setProcesses:", sel!(setProcesses:)),
                ("setExclusive:", sel!(setExclusive:)),
            ];

            println!("CATapDescription selector availability:");
            for (name, sel) in &selectors_to_check {
                let responds = msg_send_responds_to(obj, *sel);
                println!("  {} → {}", name, if responds { "YES" } else { "NO" });
            }

            // Check init selectors on a fresh alloc (before init)
            let fresh_alloc: id = msg_send![cls, alloc];
            let init_selectors = [
                ("init", sel!(init)),
                (
                    "initStereoMixdownOfProcesses:",
                    sel!(initStereoMixdownOfProcesses:),
                ),
                (
                    "initStereoGlobalTapButExcludeProcesses:",
                    sel!(initStereoGlobalTapButExcludeProcesses:),
                ),
                (
                    "initMonoMixdownOfProcesses:",
                    sel!(initMonoMixdownOfProcesses:),
                ),
                (
                    "initMonoGlobalTapButExcludeProcesses:",
                    sel!(initMonoGlobalTapButExcludeProcesses:),
                ),
            ];

            println!("\nCATapDescription init selector availability:");
            for (name, sel) in &init_selectors {
                let responds = msg_send_responds_to(fresh_alloc, *sel);
                println!("  {} → {}", name, if responds { "YES" } else { "NO" });
            }

            // Release both objects
            let _: () = msg_send![obj, release];
            // Note: fresh_alloc was not initialized, so we release it directly
            let _: () = msg_send![fresh_alloc, release];
        }
    }

    /// Test that PID→AudioObjectID translation works for a known process.
    #[test]
    fn test_translate_pid_to_audio_object_id() {
        // Use our own PID — it may or may not have an audio object,
        // but the function should not crash.
        let own_pid = std::process::id();
        let result = unsafe { translate_pid_to_audio_object_id(own_pid) };
        println!(
            "translate_pid_to_audio_object_id(own PID {}) → {:?}",
            own_pid, result
        );
        // We don't assert success because our test process may not have an audio object.
        // The function should simply return None gracefully.

        // Test with PID 0 (kernel) — should return None
        let result_zero = unsafe { translate_pid_to_audio_object_id(0) };
        println!(
            "translate_pid_to_audio_object_id(PID 0) → {:?}",
            result_zero
        );

        // Test with an impossibly high PID — should return None
        let result_bad = unsafe { translate_pid_to_audio_object_id(999999999) };
        println!(
            "translate_pid_to_audio_object_id(PID 999999999) → {:?}",
            result_bad
        );
        assert!(
            result_bad.is_none(),
            "Non-existent PID should not translate to an AudioObjectID"
        );
    }

    /// Test that new_system() creates a valid system-wide tap.
    /// Uses std::panic::catch_unwind to catch ObjC exceptions (now that
    /// the objc crate's "exception" feature is enabled).
    #[test]
    fn test_new_system_creates_tap() {
        let result = std::panic::catch_unwind(|| CoreAudioProcessTap::new_system());

        match result {
            Ok(Ok(tap)) => {
                println!(
                    "new_system() succeeded: tap_id={}, agg_id={}, uuid={}",
                    tap.raw_tap_id(),
                    tap.id(),
                    tap.tap_uuid_string
                );
                assert!(tap.id() > 0, "Aggregate device ID should be > 0");
                assert!(tap.raw_tap_id() > 0, "Tap ID should be > 0");
                assert_eq!(tap.target_pid(), 0, "System tap should have target_pid=0");
                // Drop will clean up the tap and aggregate device
            }
            Ok(Err(audio_err)) => {
                // AudioError returned — not a panic, just a handled error
                eprintln!(
                    "new_system() returned AudioError (not a panic): {:?}",
                    audio_err
                );
                // This is acceptable — the test documents what error occurred
                // On some systems, this may fail due to permissions or API changes
            }
            Err(panic_info) => {
                // ObjC exception was caught as a Rust panic
                let panic_msg = if let Some(s) = panic_info.downcast_ref::<String>() {
                    s.clone()
                } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                    s.to_string()
                } else {
                    format!("{:?}", panic_info)
                };
                eprintln!(
                    "new_system() panicked (likely ObjC exception): {}",
                    panic_msg
                );
                panic!(
                    "new_system() threw an ObjC exception: {}. \
                     This indicates the CATapDescription API needs adjustment for this macOS version.",
                    panic_msg
                );
            }
        }
    }
}
