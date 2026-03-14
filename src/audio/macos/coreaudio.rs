// macOS CoreAudio backend implementation.
//
// Provides `MacosAudioDevice`, `MacosDeviceEnumerator`, application enumeration,
// and helper functions for CoreAudio ↔ rsac type conversions.
//
// The old `MacosAudioStream` and `MacosApplicationAudioStream` (VecDeque + Mutex)
// have been REMOVED. Audio capture now flows through `BridgeStream<MacosPlatformStream>`
// via the ring buffer bridge (see `thread.rs`).
//
// CoreAudio OSStatus errors are mapped to `AudioError` via `map_ca_error()`.

#![cfg(target_os = "macos")]

// ── New API imports ──────────────────────────────────────────────────────
use crate::core::buffer::AudioBuffer;
use crate::core::config::{AudioFormat, CaptureTarget, DeviceId, SampleFormat, StreamConfig};
use crate::core::error::{AudioError, AudioResult};
use crate::core::interface::{AudioDevice, CapturingStream, DeviceEnumerator, DeviceKind};

// ── Bridge imports ───────────────────────────────────────────────────────
use crate::bridge::state::StreamState;
use crate::bridge::{calculate_capacity, create_bridge, BridgeStream};

// ── Thread-level imports ─────────────────────────────────────────────────
use super::thread::{create_macos_capture, MacosCaptureConfig};

// ── CoreAudio crate imports ──────────────────────────────────────────────
use coreaudio::audio_unit::macos_helpers::{get_default_device_id, get_device_name};
use coreaudio::Error as CAError;

// ── CoreAudio-sys raw FFI imports ────────────────────────────────────────
use coreaudio_sys::{
    kAudioDevicePropertyStreamFormat, kAudioFormatFlagIsBigEndian, kAudioFormatFlagIsFloat,
    kAudioFormatFlagIsPacked, kAudioFormatFlagIsSignedInteger, kAudioFormatLinearPCM,
    kAudioObjectPropertyElementMaster, kAudioObjectPropertyScopeOutput, AudioObjectGetPropertyData,
    AudioObjectID, AudioObjectPropertyAddress, AudioStreamBasicDescription,
};

/// AudioDeviceID is an alias for AudioObjectID (u32).
type AudioDeviceID = AudioObjectID;

// ── ObjC imports for application enumeration ─────────────────────────────
use cocoa::base::{id, nil};
use cocoa::foundation::{NSArray, NSString};
use objc::{class, msg_send, sel, sel_impl};

use std::time::Duration;

// ══════════════════════════════════════════════════════════════════════════
// ApplicationInfo & enumerate_audio_applications
// ══════════════════════════════════════════════════════════════════════════

/// Information about a running application on macOS, relevant for audio capture.
///
/// Instances of `ApplicationInfo` are returned by [`enumerate_audio_applications()`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationInfo {
    /// The process identifier (PID) of the application.
    pub process_id: u32,
    /// The localized name of the application (e.g., "Safari", "Music").
    pub name: String,
    /// The bundle identifier (e.g., "com.apple.Safari"). `None` for CLI tools.
    pub bundle_id: Option<String>,
}

/// Enumerates running applications on macOS that are potential audio sources.
///
/// The returned PIDs can be used with [`CaptureTarget::Application`] via
/// [`AudioCaptureBuilder`](crate::api::AudioCaptureBuilder) to capture
/// application-specific audio using CoreAudio Process Taps (macOS 14.4+).
pub fn enumerate_audio_applications() -> AudioResult<Vec<ApplicationInfo>> {
    let mut app_infos: Vec<ApplicationInfo> = Vec::new();

    unsafe {
        let workspace_class = class!(NSWorkspace);
        let shared_workspace: id = msg_send![workspace_class, sharedWorkspace];
        let running_apps_nsarray: id = msg_send![shared_workspace, runningApplications];

        if running_apps_nsarray == nil {
            return Err(AudioError::BackendError {
                backend: "CoreAudio".to_string(),
                operation: "enumerate_applications".to_string(),
                message: "Failed to get running applications array from NSWorkspace".to_string(),
                context: None,
            });
        }

        let count: usize = msg_send![running_apps_nsarray, count];

        for i in 0..count {
            let app: id = msg_send![running_apps_nsarray, objectAtIndex: i];
            if app == nil {
                continue;
            }

            let pid: i32 = msg_send![app, processIdentifier];

            let name_nsstring: id = msg_send![app, localizedName];
            let name_str: String = if name_nsstring != nil {
                let c_str_ptr = NSString::UTF8String(name_nsstring);
                if !c_str_ptr.is_null() {
                    std::ffi::CStr::from_ptr(c_str_ptr)
                        .to_string_lossy()
                        .into_owned()
                } else {
                    String::from("<Invalid Name>")
                }
            } else {
                String::from("<Unknown Name>")
            };

            let bundle_id_nsstring: id = msg_send![app, bundleIdentifier];
            let bundle_id: Option<String> = if bundle_id_nsstring != nil {
                let c_str_ptr = NSString::UTF8String(bundle_id_nsstring);
                if !c_str_ptr.is_null() {
                    let s = std::ffi::CStr::from_ptr(c_str_ptr)
                        .to_string_lossy()
                        .into_owned();
                    if s.is_empty() {
                        None
                    } else {
                        Some(s)
                    }
                } else {
                    None
                }
            } else {
                None
            };

            app_infos.push(ApplicationInfo {
                process_id: pid as u32,
                name: name_str,
                bundle_id,
            });
        }
    }

    Ok(app_infos)
}

// ══════════════════════════════════════════════════════════════════════════
// MacosAudioDevice — implements the NEW AudioDevice trait
// ══════════════════════════════════════════════════════════════════════════

/// A representation of a CoreAudio audio device.
///
/// Wraps an `AudioDeviceID` and implements the new [`AudioDevice`] trait
/// from `crate::core::interface`.
#[derive(Debug)]
pub struct MacosAudioDevice {
    pub(crate) device_id: AudioDeviceID,
}

impl AudioDevice for MacosAudioDevice {
    fn id(&self) -> DeviceId {
        DeviceId(self.device_id.to_string())
    }

    fn name(&self) -> String {
        get_device_name(self.device_id).unwrap_or_else(|_| "Unknown CoreAudio Device".to_string())
    }

    fn is_default(&self) -> bool {
        // Compare against default output device ID
        // get_default_device_id returns Option<AudioDeviceID>
        match get_default_device_id(false) {
            Some(default_id) => self.device_id == default_id,
            None => false,
        }
    }

    fn supported_formats(&self) -> Vec<AudioFormat> {
        // Query the device's default stream format using raw CoreAudio API
        let address = AudioObjectPropertyAddress {
            mSelector: kAudioDevicePropertyStreamFormat,
            mScope: kAudioObjectPropertyScopeOutput,
            mElement: kAudioObjectPropertyElementMaster,
        };

        unsafe {
            let mut asbd: AudioStreamBasicDescription = std::mem::zeroed();
            let mut size = std::mem::size_of::<AudioStreamBasicDescription>() as u32;
            let status = AudioObjectGetPropertyData(
                self.device_id,
                &address,
                0,
                std::ptr::null(),
                &mut size,
                &mut asbd as *mut _ as *mut std::ffi::c_void,
            );
            if status != 0 {
                return vec![];
            }
            match asbd_to_audio_format(&asbd) {
                Ok(fmt) => vec![fmt],
                Err(_) => vec![],
            }
        }
    }

    fn create_stream(&self, config: &StreamConfig) -> AudioResult<Box<dyn CapturingStream>> {
        // 1. Build AudioFormat from StreamConfig
        let format = config.to_audio_format();

        // 2. Use the capture target from StreamConfig (propagated from builder)
        let target = config.capture_target.clone();

        // 3. Create the ring buffer bridge
        let capacity = calculate_capacity(None, 4);
        let (producer, consumer) = create_bridge(capacity, format.clone());

        // 4. Transition bridge state Created → Running
        consumer
            .shared()
            .state
            .transition(StreamState::Created, StreamState::Running)
            .map_err(|actual| AudioError::InternalError {
                message: format!(
                    "Failed to transition bridge state to Running (was {:?})",
                    actual
                ),
                source: None,
            })?;

        // 5. Build MacosCaptureConfig
        let capture_config = MacosCaptureConfig {
            target,
            sample_rate: format.sample_rate,
            channels: format.channels,
        };

        // 6. Create the CoreAudio capture (registers callback, starts AudioUnit)
        let platform_stream = create_macos_capture(capture_config, producer)?;

        // 7. Create BridgeStream wrapping consumer + platform stream
        let bridge_stream =
            BridgeStream::new(consumer, platform_stream, format, Duration::from_secs(1));

        Ok(Box::new(bridge_stream))
    }
}

// ══════════════════════════════════════════════════════════════════════════
// MacosDeviceEnumerator — implements the NEW DeviceEnumerator trait
// ══════════════════════════════════════════════════════════════════════════

/// Device enumerator for macOS using CoreAudio.
pub struct MacosDeviceEnumerator;

impl MacosDeviceEnumerator {
    pub fn new() -> Self {
        MacosDeviceEnumerator
    }
}

impl Default for MacosDeviceEnumerator {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceEnumerator for MacosDeviceEnumerator {
    fn enumerate_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice>>> {
        // For now, return the default output device (suitable for loopback capture).
        // TODO: Full enumeration of all output devices.
        match self.default_device() {
            Ok(device) => Ok(vec![device]),
            Err(_) => Ok(vec![]),
        }
    }

    fn default_device(&self) -> AudioResult<Box<dyn AudioDevice>> {
        let device_id = get_default_device_id(false).ok_or_else(|| AudioError::DeviceNotFound {
            device_id: "default_output".into(),
        })?;
        Ok(Box::new(MacosAudioDevice { device_id }))
    }
}

// ══════════════════════════════════════════════════════════════════════════
// Helper Functions
// ══════════════════════════════════════════════════════════════════════════

/// Well-known CoreAudio OSStatus code for permission denied ('hog!' as u32).
/// Not always present in coreaudio-sys, so we define it here.
const KAUDIO_HARDWARE_PERMISSIONS_ERROR: i32 = 0x686F6721_u32 as i32; // 'hog!'

/// Well-known CoreAudio OSStatus code for format not supported.
const KAUDIO_UNIT_ERR_FORMAT_NOT_SUPPORTED: i32 = -10868;

/// Maps a `coreaudio::Error` to an [`AudioError`].
///
/// Used throughout the macOS backend for consistent error reporting.
///
/// In coreaudio-rs 0.14, the `Error` enum wraps typed sub-enums (not raw i32).
/// We use `as_os_status()` to extract the underlying OSStatus code and then
/// map well-known codes to specific `AudioError` variants.
pub(crate) fn map_ca_error(err: CAError) -> AudioError {
    let os_status = err.as_os_status();

    // Determine category from the variant
    let category = match &err {
        CAError::AudioUnit(_) => "AudioUnit",
        CAError::AudioCodec(_) => "AudioCodec",
        CAError::AudioFormat(_) => "AudioFormat",
        CAError::Audio(_) => "Audio",
        CAError::Unknown(_) => "Unknown",
        _ => "Other",
    };

    // Check for well-known CoreAudio OSStatus codes
    if os_status == KAUDIO_HARDWARE_PERMISSIONS_ERROR {
        return AudioError::PermissionDenied {
            operation: "audio_capture".into(),
            details: Some(format!(
                "CoreAudio permission denied (OSStatus: {})",
                os_status
            )),
        };
    }
    if os_status == KAUDIO_UNIT_ERR_FORMAT_NOT_SUPPORTED {
        return AudioError::UnsupportedFormat {
            format: "requested format".into(),
            context: None,
        };
    }

    AudioError::BackendError {
        backend: "CoreAudio".into(),
        operation: category.to_string(),
        message: format!("CoreAudio error ({}): OSStatus {}", category, os_status),
        context: None,
    }
}

/// Converts an `AudioStreamBasicDescription` to the new [`AudioFormat`].
///
/// Only handles Linear PCM formats (float and signed integer).
pub(crate) fn asbd_to_audio_format(asbd: &AudioStreamBasicDescription) -> AudioResult<AudioFormat> {
    if asbd.mFormatID != kAudioFormatLinearPCM {
        return Err(AudioError::UnsupportedFormat {
            format: format!("format_id={}", asbd.mFormatID),
            context: None,
        });
    }

    let sample_format = if (asbd.mFormatFlags & kAudioFormatFlagIsFloat) != 0 {
        if (asbd.mFormatFlags & kAudioFormatFlagIsBigEndian) != 0 {
            return Err(AudioError::UnsupportedFormat {
                format: "F32BE".to_string(),
                context: None,
            });
        }
        SampleFormat::F32
    } else if (asbd.mFormatFlags & kAudioFormatFlagIsSignedInteger) != 0 {
        if (asbd.mFormatFlags & kAudioFormatFlagIsBigEndian) != 0 {
            return Err(AudioError::UnsupportedFormat {
                format: "Signed Int Big Endian".to_string(),
                context: None,
            });
        }
        match asbd.mBitsPerChannel {
            16 => SampleFormat::I16,
            24 => SampleFormat::I24,
            32 => SampleFormat::I32,
            _ => {
                return Err(AudioError::UnsupportedFormat {
                    format: format!("{}-bit signed int", asbd.mBitsPerChannel),
                    context: None,
                });
            }
        }
    } else {
        return Err(AudioError::UnsupportedFormat {
            format: "Unknown sample format".to_string(),
            context: None,
        });
    };

    Ok(AudioFormat {
        sample_rate: asbd.mSampleRate as u32,
        channels: asbd.mChannelsPerFrame as u16,
        sample_format,
    })
}

/// Converts an [`AudioFormat`] to `AudioStreamBasicDescription`.
///
/// Produces interleaved PCM ASBD suitable for AUHAL configuration.
#[allow(dead_code)]
pub(crate) fn audio_format_to_asbd(format: &AudioFormat) -> AudioStreamBasicDescription {
    let mut flags = kAudioFormatFlagIsPacked;
    let bits_per_sample = format.sample_format.bits_per_sample() as u32;

    match format.sample_format {
        SampleFormat::F32 => {
            flags |= kAudioFormatFlagIsFloat;
        }
        SampleFormat::I16 | SampleFormat::I24 | SampleFormat::I32 => {
            flags |= kAudioFormatFlagIsSignedInteger;
        }
    }

    let bytes_per_sample = bits_per_sample / 8;
    let bytes_per_frame = bytes_per_sample * format.channels as u32;

    AudioStreamBasicDescription {
        mSampleRate: format.sample_rate as f64,
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: flags,
        mBytesPerPacket: bytes_per_frame,
        mFramesPerPacket: 1,
        mBytesPerFrame: bytes_per_frame,
        mChannelsPerFrame: format.channels as u32,
        mBitsPerChannel: bits_per_sample,
        mReserved: 0,
    }
}

// ══════════════════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════════════════

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::core::config::{AudioFormat, SampleFormat, StreamConfig};
    use crate::core::interface::{AudioDevice, DeviceEnumerator};

    // ── Helper function tests: asbd_to_audio_format ──────────────────

    #[test]
    fn asbd_to_audio_format_f32_stereo_48k() {
        let asbd = AudioStreamBasicDescription {
            mSampleRate: 48000.0,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
            mBytesPerPacket: 8,
            mFramesPerPacket: 1,
            mBytesPerFrame: 8,
            mChannelsPerFrame: 2,
            mBitsPerChannel: 32,
            mReserved: 0,
        };

        let fmt = asbd_to_audio_format(&asbd).expect("should parse F32 stereo ASBD");
        assert_eq!(fmt.sample_rate, 48000);
        assert_eq!(fmt.channels, 2);
        assert_eq!(fmt.sample_format, SampleFormat::F32);
    }

    #[test]
    fn asbd_to_audio_format_i16_mono_44100() {
        let asbd = AudioStreamBasicDescription {
            mSampleRate: 44100.0,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked,
            mBytesPerPacket: 2,
            mFramesPerPacket: 1,
            mBytesPerFrame: 2,
            mChannelsPerFrame: 1,
            mBitsPerChannel: 16,
            mReserved: 0,
        };

        let fmt = asbd_to_audio_format(&asbd).expect("should parse I16 mono ASBD");
        assert_eq!(fmt.sample_rate, 44100);
        assert_eq!(fmt.channels, 1);
        assert_eq!(fmt.sample_format, SampleFormat::I16);
    }

    #[test]
    fn asbd_to_audio_format_i24() {
        let asbd = AudioStreamBasicDescription {
            mSampleRate: 96000.0,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked,
            mBytesPerPacket: 6,
            mFramesPerPacket: 1,
            mBytesPerFrame: 6,
            mChannelsPerFrame: 2,
            mBitsPerChannel: 24,
            mReserved: 0,
        };

        let fmt = asbd_to_audio_format(&asbd).expect("should parse I24 stereo ASBD");
        assert_eq!(fmt.sample_rate, 96000);
        assert_eq!(fmt.channels, 2);
        assert_eq!(fmt.sample_format, SampleFormat::I24);
    }

    #[test]
    fn asbd_to_audio_format_i32() {
        let asbd = AudioStreamBasicDescription {
            mSampleRate: 48000.0,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked,
            mBytesPerPacket: 8,
            mFramesPerPacket: 1,
            mBytesPerFrame: 8,
            mChannelsPerFrame: 2,
            mBitsPerChannel: 32,
            mReserved: 0,
        };

        let fmt = asbd_to_audio_format(&asbd).expect("should parse I32 stereo ASBD");
        assert_eq!(fmt.sample_format, SampleFormat::I32);
    }

    #[test]
    fn asbd_to_audio_format_rejects_non_pcm() {
        let asbd = AudioStreamBasicDescription {
            mSampleRate: 48000.0,
            mFormatID: 0x61616320, // 'aac ' — not Linear PCM
            mFormatFlags: 0,
            mBytesPerPacket: 0,
            mFramesPerPacket: 1024,
            mBytesPerFrame: 0,
            mChannelsPerFrame: 2,
            mBitsPerChannel: 0,
            mReserved: 0,
        };

        let result = asbd_to_audio_format(&asbd);
        assert!(result.is_err(), "non-PCM format should be rejected");
    }

    #[test]
    fn asbd_to_audio_format_rejects_big_endian_float() {
        let asbd = AudioStreamBasicDescription {
            mSampleRate: 48000.0,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsBigEndian,
            mBytesPerPacket: 8,
            mFramesPerPacket: 1,
            mBytesPerFrame: 8,
            mChannelsPerFrame: 2,
            mBitsPerChannel: 32,
            mReserved: 0,
        };

        let result = asbd_to_audio_format(&asbd);
        assert!(result.is_err(), "big endian float should be rejected");
    }

    #[test]
    fn asbd_to_audio_format_rejects_unsupported_bit_depth() {
        let asbd = AudioStreamBasicDescription {
            mSampleRate: 48000.0,
            mFormatID: kAudioFormatLinearPCM,
            mFormatFlags: kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked,
            mBytesPerPacket: 1,
            mFramesPerPacket: 1,
            mBytesPerFrame: 1,
            mChannelsPerFrame: 1,
            mBitsPerChannel: 8,
            mReserved: 0,
        };

        let result = asbd_to_audio_format(&asbd);
        assert!(result.is_err(), "8-bit signed int should be rejected");
    }

    // ── Helper function tests: audio_format_to_asbd ──────────────────

    #[test]
    fn audio_format_to_asbd_f32_stereo() {
        let fmt = AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
        };

        let asbd = audio_format_to_asbd(&fmt);
        assert_eq!(asbd.mSampleRate, 48000.0);
        assert_eq!(asbd.mFormatID, kAudioFormatLinearPCM);
        assert_ne!(asbd.mFormatFlags & kAudioFormatFlagIsFloat, 0);
        assert_ne!(asbd.mFormatFlags & kAudioFormatFlagIsPacked, 0);
        assert_eq!(asbd.mChannelsPerFrame, 2);
        assert_eq!(asbd.mBitsPerChannel, 32);
        assert_eq!(asbd.mBytesPerFrame, 8); // 4 bytes * 2 channels
        assert_eq!(asbd.mBytesPerPacket, 8);
        assert_eq!(asbd.mFramesPerPacket, 1);
    }

    #[test]
    fn audio_format_to_asbd_i16_mono() {
        let fmt = AudioFormat {
            sample_rate: 44100,
            channels: 1,
            sample_format: SampleFormat::I16,
        };

        let asbd = audio_format_to_asbd(&fmt);
        assert_eq!(asbd.mSampleRate, 44100.0);
        assert_ne!(asbd.mFormatFlags & kAudioFormatFlagIsSignedInteger, 0);
        assert_eq!(asbd.mChannelsPerFrame, 1);
        assert_eq!(asbd.mBitsPerChannel, 16);
        assert_eq!(asbd.mBytesPerFrame, 2); // 2 bytes * 1 channel
    }

    #[test]
    fn audio_format_to_asbd_i24_stereo() {
        let fmt = AudioFormat {
            sample_rate: 96000,
            channels: 2,
            sample_format: SampleFormat::I24,
        };

        let asbd = audio_format_to_asbd(&fmt);
        assert_eq!(asbd.mSampleRate, 96000.0);
        assert_ne!(asbd.mFormatFlags & kAudioFormatFlagIsSignedInteger, 0);
        assert_eq!(asbd.mBitsPerChannel, 24);
        assert_eq!(asbd.mBytesPerFrame, 6); // 3 bytes * 2 channels
    }

    // ── Round-trip test: AudioFormat → ASBD → AudioFormat ────────────

    #[test]
    fn audio_format_asbd_roundtrip_f32() {
        let original = AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
        };

        let asbd = audio_format_to_asbd(&original);
        let recovered = asbd_to_audio_format(&asbd).expect("roundtrip should succeed");
        assert_eq!(original, recovered);
    }

    #[test]
    fn audio_format_asbd_roundtrip_i16() {
        let original = AudioFormat {
            sample_rate: 44100,
            channels: 1,
            sample_format: SampleFormat::I16,
        };

        let asbd = audio_format_to_asbd(&original);
        let recovered = asbd_to_audio_format(&asbd).expect("roundtrip should succeed");
        assert_eq!(original, recovered);
    }

    #[test]
    fn audio_format_asbd_roundtrip_i32() {
        let original = AudioFormat {
            sample_rate: 96000,
            channels: 6,
            sample_format: SampleFormat::I32,
        };

        let asbd = audio_format_to_asbd(&original);
        let recovered = asbd_to_audio_format(&asbd).expect("roundtrip should succeed");
        assert_eq!(original, recovered);
    }

    // ── map_ca_error tests ───────────────────────────────────────────

    #[test]
    fn map_ca_error_permission_denied() {
        // Construct a CAError::Unknown variant with the permissions error OSStatus
        let err = map_ca_error(CAError::Unknown(KAUDIO_HARDWARE_PERMISSIONS_ERROR));
        assert!(
            matches!(err, AudioError::PermissionDenied { .. }),
            "Expected PermissionDenied, got: {:?}",
            err
        );
    }

    #[test]
    fn map_ca_error_format_not_supported() {
        // Construct a CAError with the format-not-supported OSStatus
        // Use AudioUnitError::FormatNotSupported since that's the typed variant
        use coreaudio::error::AudioUnitError;
        let err = map_ca_error(CAError::AudioUnit(AudioUnitError::FormatNotSupported));
        assert!(
            matches!(err, AudioError::UnsupportedFormat { .. }),
            "Expected UnsupportedFormat, got: {:?}",
            err
        );
    }

    #[test]
    fn map_ca_error_unknown_status() {
        // Use an arbitrary unknown OSStatus (e.g., -50 = paramErr)
        let err = map_ca_error(CAError::Unknown(-50));
        assert!(
            matches!(err, AudioError::BackendError { .. }),
            "Expected BackendError, got: {:?}",
            err
        );
    }

    // ── Device construction tests (require audio hardware) ───────────

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn device_default_has_nonempty_name() {
        let enumerator = MacosDeviceEnumerator::new();
        let device = enumerator
            .default_device()
            .expect("default device should exist");
        let name = device.name();
        assert!(!name.is_empty(), "default device name should not be empty");
        assert_ne!(name, "Unknown CoreAudio Device");
    }

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn device_default_is_default() {
        let enumerator = MacosDeviceEnumerator::new();
        let device = enumerator
            .default_device()
            .expect("default device should exist");
        assert!(
            device.is_default(),
            "default device should report is_default() == true"
        );
    }

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn device_default_has_supported_formats() {
        let enumerator = MacosDeviceEnumerator::new();
        let device = enumerator
            .default_device()
            .expect("default device should exist");
        let formats = device.supported_formats();
        assert!(
            !formats.is_empty(),
            "default device should support at least one format"
        );
    }

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn device_id_is_parseable_u32() {
        let enumerator = MacosDeviceEnumerator::new();
        let device = enumerator
            .default_device()
            .expect("default device should exist");
        let id = device.id();
        let parsed: Result<u32, _> = id.0.parse();
        assert!(
            parsed.is_ok(),
            "macOS device ID should be a parseable u32, got: {}",
            id.0
        );
    }

    // ── DeviceEnumerator tests (require audio hardware) ──────────────

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn enumerator_returns_at_least_one_device() {
        let enumerator = MacosDeviceEnumerator::new();
        let devices = enumerator
            .enumerate_devices()
            .expect("enumerate should succeed");
        assert!(!devices.is_empty(), "should enumerate at least one device");
    }

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn enumerator_default_found_in_enumeration() {
        let enumerator = MacosDeviceEnumerator::new();
        let default = enumerator
            .default_device()
            .expect("default device should exist");
        let devices = enumerator
            .enumerate_devices()
            .expect("enumerate should succeed");

        let default_id = default.id();
        let found = devices.iter().any(|d| d.id() == default_id);
        assert!(found, "default device should appear in enumerated devices");
    }

    #[test]
    fn enumerator_new_and_default_are_equivalent() {
        let a = MacosDeviceEnumerator::new();
        let b = MacosDeviceEnumerator::default();
        // Both constructors produce equivalent enumerators (no internal state)
        let _ = (a, b); // Just verify they compile and are usable
    }

    // ── ApplicationInfo / enumerate_audio_applications ────────────────

    #[test]
    #[ignore = "requires macOS GUI environment"]
    fn enumerate_audio_applications_returns_results() {
        let apps = enumerate_audio_applications().expect("enumeration should succeed");
        // There should be at least one running application on a macOS desktop
        assert!(
            !apps.is_empty(),
            "should find at least one running application"
        );
    }

    #[test]
    #[ignore = "requires macOS GUI environment"]
    fn enumerate_audio_applications_have_nonempty_names() {
        let apps = enumerate_audio_applications().expect("enumeration should succeed");
        for app in &apps {
            assert!(
                !app.name.is_empty(),
                "app name should not be empty (PID={})",
                app.process_id
            );
            assert_ne!(app.name, "<Unknown Name>");
        }
    }

    // ── Stream lifecycle tests (require audio hardware) ──────────────

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn create_stream_system_default() {
        let enumerator = MacosDeviceEnumerator::new();
        let device = enumerator
            .default_device()
            .expect("default device should exist");
        let config = StreamConfig::default();
        let stream = device.create_stream(&config);
        assert!(
            stream.is_ok(),
            "create_stream should succeed: {:?}",
            stream.err()
        );
    }

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn stream_is_running_after_creation() {
        let enumerator = MacosDeviceEnumerator::new();
        let device = enumerator
            .default_device()
            .expect("default device should exist");
        let config = StreamConfig::default();
        let stream = device
            .create_stream(&config)
            .expect("create_stream should succeed");
        assert!(
            stream.is_running(),
            "stream should be running after creation"
        );
    }

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn stream_stop_succeeds() {
        let enumerator = MacosDeviceEnumerator::new();
        let device = enumerator
            .default_device()
            .expect("default device should exist");
        let config = StreamConfig::default();
        let stream = device
            .create_stream(&config)
            .expect("create_stream should succeed");
        let result = stream.stop();
        assert!(result.is_ok(), "stop should succeed: {:?}", result.err());
    }

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn stream_not_running_after_stop() {
        let enumerator = MacosDeviceEnumerator::new();
        let device = enumerator
            .default_device()
            .expect("default device should exist");
        let config = StreamConfig::default();
        let stream = device
            .create_stream(&config)
            .expect("create_stream should succeed");
        stream.stop().expect("stop should succeed");
        assert!(
            !stream.is_running(),
            "stream should not be running after stop"
        );
    }

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn stream_format_matches_config() {
        let enumerator = MacosDeviceEnumerator::new();
        let device = enumerator
            .default_device()
            .expect("default device should exist");
        let config = StreamConfig {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
            buffer_size: None,
            capture_target: CaptureTarget::SystemDefault,
        };
        let stream = device
            .create_stream(&config)
            .expect("create_stream should succeed");
        let fmt = stream.format();
        assert_eq!(fmt.sample_rate, 48000);
        assert_eq!(fmt.channels, 2);
        assert_eq!(fmt.sample_format, SampleFormat::F32);
    }
}
