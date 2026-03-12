//! macOS CoreAudio capture infrastructure using BridgeStream.
//!
//! This module provides `MacosPlatformStream` and `create_macos_capture()` for
//! wiring CoreAudio AUHAL capture into the lock-free ring buffer bridge.
//!
//! # Architecture
//!
//! Unlike Linux (PipeWire) and Windows (WASAPI), macOS CoreAudio manages its own
//! real-time audio thread. The AUHAL `set_input_callback` fires on CoreAudio's
//! internal thread. There is **no dedicated capture thread** — the OS callback
//! pushes audio directly into the `BridgeProducer`.
//!
//! ```text
//! CoreAudio RT Thread                   Consumer Thread
//! ──────────────────                    ───────────────
//! AUHAL input callback                  CapturingStream (BridgeConsumer)
//! BridgeProducer::push_or_drop()        BridgeStream::read_chunk()
//! ```
//!
//! The `MacosPlatformStream` wraps the `AudioUnit` handle and optional
//! `CoreAudioProcessTap` for lifecycle management.

#![cfg(target_os = "macos")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::bridge::ring_buffer::BridgeProducer;
use crate::bridge::stream::PlatformStream;
use crate::core::buffer::AudioBuffer;
use crate::core::config::CaptureTarget;
use crate::core::error::{AudioError, AudioResult};

use super::coreaudio::{map_ca_error, MacosAudioDevice};
use super::tap::CoreAudioProcessTap;

use coreaudio::audio_unit::audio_unit_element;
use coreaudio::audio_unit::{AudioComponent, AudioComponentDescription, AudioUnit, Element, Scope};
use coreaudio::sys::{
    self, kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved, kAudioFormatFlagIsPacked,
    kAudioFormatLinearPCM, kAudioOutputUnitProperty_CurrentDevice,
    kAudioOutputUnitProperty_EnableIO, kAudioUnitManufacturer_Apple,
    kAudioUnitProperty_StreamFormat, kAudioUnitSubType_HALOutput, kAudioUnitType_Output,
    AudioStreamBasicDescription, AudioUnitRenderActionFlags, OSStatus,
};
use coreaudio::Error as CAError;

// ── MacosCaptureConfig ───────────────────────────────────────────────────

/// Resolved capture parameters passed to the CoreAudio capture setup.
///
/// This is a subset of [`AudioCaptureConfig`](crate::core::config::AudioCaptureConfig)
/// containing only the fields needed by the macOS backend to create a stream.
#[derive(Debug)]
pub(crate) struct MacosCaptureConfig {
    /// What to capture (system default, specific device, application, process tree, etc.).
    pub target: CaptureTarget,
    /// Desired sample rate in Hz (e.g., 48000).
    pub sample_rate: u32,
    /// Desired number of audio channels (e.g., 2 for stereo).
    pub channels: u16,
}

// ── MacosPlatformStream ──────────────────────────────────────────────────

/// Platform-specific stream handle for macOS (CoreAudio backend).
///
/// Wraps an `AudioUnit` (AUHAL) and optionally a `CoreAudioProcessTap`.
/// Implements [`PlatformStream`] so it can be used with
/// [`BridgeStream`](crate::bridge::stream::BridgeStream).
///
/// # Thread Safety
///
/// `MacosPlatformStream` is `Send` (required by `PlatformStream`). The inner
/// `AudioUnit` is protected by a `Mutex` for safe access from the consumer thread.
/// The `is_active` flag is atomic for lock-free status checks.
pub(crate) struct MacosPlatformStream {
    /// The AUHAL AudioUnit, protected by Mutex for interior mutability.
    audio_unit: Mutex<AudioUnit>,
    /// Optional ProcessTap reference — kept alive for the lifetime of the stream.
    /// When dropped, the tap is destroyed via its Drop impl.
    #[allow(dead_code)]
    process_tap: Option<CoreAudioProcessTap>,
    /// Atomic flag: `true` while CoreAudio callbacks are active.
    is_active: AtomicBool,
}

impl PlatformStream for MacosPlatformStream {
    fn stop_capture(&self) -> AudioResult<()> {
        let au = self
            .audio_unit
            .lock()
            .map_err(|_| AudioError::InternalError {
                message: "AudioUnit mutex poisoned".to_string(),
                source: None,
            })?;
        au.stop().map_err(map_ca_error)?;
        self.is_active.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_active(&self) -> bool {
        self.is_active.load(Ordering::SeqCst)
    }
}

// ── Factory Function ─────────────────────────────────────────────────────

/// Creates and starts a CoreAudio capture session, returning a `MacosPlatformStream`.
///
/// This is the primary entry point for the macOS backend. It:
/// 1. Matches on the `CaptureTarget` variant to determine the device/tap to use
/// 2. Creates and configures an AUHAL `AudioUnit`
/// 3. Registers an input callback that pushes audio into the `BridgeProducer` (lock-free)
/// 4. Starts the AudioUnit
/// 5. Returns the `MacosPlatformStream` handle
///
/// # Arguments
///
/// * `config` — Resolved capture parameters.
/// * `producer` — The `BridgeProducer` to push captured audio into.
///
/// # Errors
///
/// Returns `AudioError` if any CoreAudio operation fails (component lookup,
/// AudioUnit creation, property setting, initialization, or start).
pub(crate) fn create_macos_capture(
    config: MacosCaptureConfig,
    mut producer: BridgeProducer,
) -> AudioResult<MacosPlatformStream> {
    // ── Step 1: Resolve target to device ID and optional ProcessTap ──

    let (device_id, process_tap) = resolve_capture_target(&config)?;

    // ── Step 2: Create AUHAL AudioUnit ──

    let desc = AudioComponentDescription {
        component_type: kAudioUnitType_Output,
        component_sub_type: kAudioUnitSubType_HALOutput,
        component_manufacturer: kAudioUnitManufacturer_Apple,
        component_flags: 0,
        component_flags_mask: 0,
    };

    let component = AudioComponent::find(Some(&desc), None)
        .ok_or_else(|| AudioError::BackendInitializationFailed {
            backend: "CoreAudio".to_string(),
            reason: "Failed to find AUHAL component".to_string(),
        })?
        .into_owned();

    let mut audio_unit = component.new_instance().map_err(map_ca_error)?;

    // ── Step 3: Configure the AudioUnit ──

    // Set current device
    audio_unit
        .set_property(
            kAudioOutputUnitProperty_CurrentDevice,
            Scope::Global,
            audio_unit_element::OUTPUT_BUS,
            Some(&device_id),
        )
        .map_err(map_ca_error)?;

    // Enable IO for input (capture) on input bus
    let enable_io: u32 = 1;
    audio_unit
        .set_property(
            kAudioOutputUnitProperty_EnableIO,
            Scope::Input,
            audio_unit_element::INPUT_BUS,
            Some(&enable_io),
        )
        .map_err(map_ca_error)?;

    // Disable IO for output on output bus
    let disable_io: u32 = 0;
    audio_unit
        .set_property(
            kAudioOutputUnitProperty_EnableIO,
            Scope::Output,
            audio_unit_element::OUTPUT_BUS,
            Some(&disable_io),
        )
        .map_err(map_ca_error)?;

    // Build ASBD for interleaved F32
    let asbd = build_f32_asbd(config.sample_rate, config.channels);

    // Set stream format on OUTPUT scope of INPUT bus (what CoreAudio delivers to us)
    audio_unit
        .set_property(
            kAudioUnitProperty_StreamFormat,
            Scope::Output,
            audio_unit_element::INPUT_BUS,
            Some(&asbd),
        )
        .map_err(map_ca_error)?;

    // Set stream format on INPUT scope of OUTPUT bus (matching format)
    audio_unit
        .set_property(
            kAudioUnitProperty_StreamFormat,
            Scope::Input,
            audio_unit_element::OUTPUT_BUS,
            Some(&asbd),
        )
        .map_err(map_ca_error)?;

    // Initialize the AudioUnit
    audio_unit.initialize().map_err(map_ca_error)?;

    // ── Step 4: Register input callback that pushes to BridgeProducer ──

    let channels = config.channels;
    let sample_rate = config.sample_rate;

    audio_unit
        .set_input_callback(move |args| -> Result<(), OSStatus> {
            // REAL-TIME SAFETY:
            // - BridgeProducer::push_or_drop() is lock-free (rtrb)
            // - Vec allocation is acceptable for initial impl
            //   (optimize with scratch buffer later)
            // - No locks, no blocking I/O

            let au_instance = args.audio_unit_ref.instance();
            let num_frames = args.num_frames;
            let timestamp = args.timestamp;

            // Allocate AudioBufferList for interleaved capture
            let captured_abl_result = coreaudio::audio_buffer::AudioBufferList::allocate(
                channels as u32,
                num_frames,
                true, // interleaved
                true, // allocate mData
            );

            let mut captured_abl = match captured_abl_result {
                Ok(abl) => abl,
                Err(_) => {
                    // Cannot allocate — skip this callback invocation
                    return Ok(());
                }
            };

            let captured_abl_ptr: *mut sys::AudioBufferList = &mut *captured_abl;
            let mut render_action_flags: AudioUnitRenderActionFlags = 0;

            // Call AudioUnitRender on INPUT BUS to get captured data
            let os_status = unsafe {
                sys::AudioUnitRender(
                    au_instance,
                    &mut render_action_flags,
                    timestamp,
                    audio_unit_element::INPUT_BUS,
                    num_frames,
                    captured_abl_ptr,
                )
            };

            if os_status != sys::noErr {
                // AudioUnitRender failed — log and skip this callback
                // Do NOT propagate error through ring buffer (it only carries AudioBuffer)
                return Ok(());
            }

            // Convert raw buffer to interleaved f32 samples
            let num_channels = channels as usize;
            let num_frames_usize = num_frames as usize;
            let total_samples = num_frames_usize * num_channels;

            let buffers_slice = unsafe {
                std::slice::from_raw_parts(
                    (*captured_abl_ptr).mBuffers.as_ptr(),
                    (*captured_abl_ptr).mNumberBuffers as usize,
                )
            };

            if buffers_slice.is_empty() {
                return Ok(());
            }

            // Check if data is interleaved (single buffer) or non-interleaved (one per channel)
            let is_non_interleaved = buffers_slice.len() > 1;

            let mut samples = Vec::with_capacity(total_samples);

            if !is_non_interleaved {
                // Interleaved: single buffer contains all channels
                let source = &buffers_slice[0];
                let n_samples = source.mDataByteSize as usize / std::mem::size_of::<f32>();
                if !source.mData.is_null() && n_samples > 0 {
                    let source_slice = unsafe {
                        std::slice::from_raw_parts(source.mData as *const f32, n_samples)
                    };
                    samples.extend_from_slice(source_slice);
                }
            } else {
                // Non-interleaved: interleave manually
                for frame_idx in 0..num_frames_usize {
                    for ch_idx in 0..num_channels {
                        if ch_idx < buffers_slice.len() {
                            let source = &buffers_slice[ch_idx];
                            if !source.mData.is_null()
                                && source.mDataByteSize
                                    >= ((frame_idx + 1) * std::mem::size_of::<f32>()) as u32
                            {
                                let sample_ptr = source.mData as *const f32;
                                samples.push(unsafe { *sample_ptr.add(frame_idx) });
                            } else {
                                samples.push(0.0); // Silence for missing data
                            }
                        } else {
                            samples.push(0.0);
                        }
                    }
                }
            }

            if !samples.is_empty() {
                let audio_buffer = AudioBuffer::new(samples, channels, sample_rate);
                // Push to ring buffer — if full, silently dropped (back-pressure)
                producer.push_or_drop(audio_buffer);
            }

            Ok(())
        })
        .map_err(map_ca_error)?;

    // ── Step 5: Start the AudioUnit ──

    audio_unit.start().map_err(map_ca_error)?;

    log::debug!(
        "CoreAudio: capture started (target={:?}, {}Hz, {}ch)",
        config.target,
        config.sample_rate,
        config.channels
    );

    // ── Step 6: Return the platform stream handle ──

    Ok(MacosPlatformStream {
        audio_unit: Mutex::new(audio_unit),
        process_tap,
        is_active: AtomicBool::new(true),
    })
}

// ── Helper: Resolve CaptureTarget ────────────────────────────────────────

/// Resolves a [`CaptureTarget`] to a CoreAudio `AudioDeviceID` and optional
/// `CoreAudioProcessTap`.
///
/// | Target                 | Strategy                                                |
/// |------------------------|---------------------------------------------------------|
/// | `SystemDefault`        | Default output device ID (for loopback)                 |
/// | `Device(id)`           | Parse `DeviceId.0` as `u32` → `AudioDeviceID`          |
/// | `Application(pid)`     | `CoreAudioProcessTap::new(pid)` → tap's AudioObjectID  |
/// | `ApplicationByName(n)` | `enumerate_audio_applications()` → find PID → tap       |
/// | `ProcessTree(pid)`     | Same as Application for initial implementation          |
fn resolve_capture_target(
    config: &MacosCaptureConfig,
) -> AudioResult<(
    coreaudio::audio_unit::audio_device::AudioDeviceID,
    Option<CoreAudioProcessTap>,
)> {
    use coreaudio::audio_object::AudioObject;

    match &config.target {
        CaptureTarget::SystemDefault => {
            // Get macro default output device for loopback capture
            let device_id = AudioObject::default_output_device().map_err(map_ca_error)?;
            log::debug!("CoreAudio: SystemDefault → device_id={}", device_id);
            Ok((device_id, None))
        }

        CaptureTarget::Device(device_id) => {
            let id: u32 = device_id
                .0
                .parse()
                .map_err(|_| AudioError::DeviceNotFound {
                    device_id: device_id.0.clone(),
                })?;
            log::debug!("CoreAudio: Device target → device_id={}", id);
            Ok((id, None))
        }

        CaptureTarget::Application(app_id) => {
            let pid: u32 = app_id
                .0
                .parse()
                .map_err(|_| AudioError::ApplicationNotFound {
                    identifier: format!(
                        "Cannot parse PID from ApplicationId '{}': expected numeric PID",
                        app_id.0
                    ),
                })?;

            let tap = CoreAudioProcessTap::new(pid, &format!("rsac-tap-{}", pid))?;
            let tap_device_id = tap.id();
            log::debug!(
                "CoreAudio: Application target (PID={}) → tap_id={}",
                pid,
                tap_device_id
            );
            Ok((tap_device_id, Some(tap)))
        }

        CaptureTarget::ApplicationByName(name) => {
            // Enumerate running applications and find the first match
            let apps = super::coreaudio::enumerate_audio_applications()?;
            let app = apps
                .iter()
                .find(|a| a.name.to_lowercase().contains(&name.to_lowercase()))
                .ok_or_else(|| AudioError::ApplicationNotFound {
                    identifier: format!("No running application matching name '{}'", name),
                })?;

            let pid = app.process_id;
            let tap = CoreAudioProcessTap::new(pid, &format!("rsac-tap-{}", pid))?;
            let tap_device_id = tap.id();
            log::debug!(
                "CoreAudio: ApplicationByName('{}') → PID={}, tap_id={}",
                name,
                pid,
                tap_device_id
            );
            Ok((tap_device_id, Some(tap)))
        }

        CaptureTarget::ProcessTree(pid) => {
            // For initial implementation, treat same as single Application capture
            let tap = CoreAudioProcessTap::new(pid.0, &format!("rsac-tap-tree-{}", pid.0))?;
            let tap_device_id = tap.id();
            log::debug!(
                "CoreAudio: ProcessTree (PID={}) → tap_id={} (single-process for now)",
                pid.0,
                tap_device_id
            );
            Ok((tap_device_id, Some(tap)))
        }
    }
}

// ── Helper: Build F32 ASBD ───────────────────────────────────────────────

/// Builds an `AudioStreamBasicDescription` for interleaved F32 PCM.
fn build_f32_asbd(sample_rate: u32, channels: u16) -> AudioStreamBasicDescription {
    let bytes_per_sample: u32 = 4; // f32
    let bytes_per_frame = bytes_per_sample * channels as u32;

    AudioStreamBasicDescription {
        mSampleRate: sample_rate as f64,
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
        mBytesPerPacket: bytes_per_frame,
        mFramesPerPacket: 1, // Uncompressed PCM
        mBytesPerFrame: bytes_per_frame,
        mChannelsPerFrame: channels as u32,
        mBitsPerChannel: 32,
        mReserved: 0,
    }
}

// ── Compile-time assertions ──────────────────────────────────────────────

/// Assert that `MacosPlatformStream` is `Send` (required by `PlatformStream`).
fn _assert_macos_platform_stream_send() {
    fn _assert<T: Send>() {}
    _assert::<MacosPlatformStream>();
}

// ══════════════════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════════════════

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::core::config::{ApplicationId, CaptureTarget, DeviceId, ProcessId};

    // ── build_f32_asbd tests ─────────────────────────────────────────

    #[test]
    fn build_f32_asbd_48k_stereo() {
        let asbd = build_f32_asbd(48000, 2);

        assert_eq!(asbd.mSampleRate, 48000.0);
        assert_eq!(asbd.mFormatID, kAudioFormatLinearPCM);
        assert_ne!(
            asbd.mFormatFlags & kAudioFormatFlagIsFloat,
            0,
            "should have Float flag"
        );
        assert_ne!(
            asbd.mFormatFlags & kAudioFormatFlagIsPacked,
            0,
            "should have Packed flag"
        );
        assert_eq!(asbd.mChannelsPerFrame, 2);
        assert_eq!(asbd.mBitsPerChannel, 32);
        assert_eq!(asbd.mBytesPerFrame, 8); // 4 bytes * 2 channels
        assert_eq!(asbd.mBytesPerPacket, 8);
        assert_eq!(asbd.mFramesPerPacket, 1); // uncompressed PCM
        assert_eq!(asbd.mReserved, 0);
    }

    #[test]
    fn build_f32_asbd_44100_mono() {
        let asbd = build_f32_asbd(44100, 1);

        assert_eq!(asbd.mSampleRate, 44100.0);
        assert_eq!(asbd.mChannelsPerFrame, 1);
        assert_eq!(asbd.mBytesPerFrame, 4); // 4 bytes * 1 channel
        assert_eq!(asbd.mBytesPerPacket, 4);
        assert_eq!(asbd.mBitsPerChannel, 32);
    }

    #[test]
    fn build_f32_asbd_96k_8ch() {
        let asbd = build_f32_asbd(96000, 8);

        assert_eq!(asbd.mSampleRate, 96000.0);
        assert_eq!(asbd.mChannelsPerFrame, 8);
        assert_eq!(asbd.mBytesPerFrame, 32); // 4 bytes * 8 channels
        assert_eq!(asbd.mBytesPerPacket, 32);
    }

    #[test]
    fn build_f32_asbd_does_not_set_non_interleaved() {
        let asbd = build_f32_asbd(48000, 2);
        assert_eq!(
            asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved,
            0,
            "should NOT have NonInterleaved flag (we use interleaved)"
        );
    }

    // ── MacosCaptureConfig construction ──────────────────────────────

    #[test]
    fn capture_config_debug_format() {
        let config = MacosCaptureConfig {
            target: CaptureTarget::SystemDefault,
            sample_rate: 48000,
            channels: 2,
        };
        let debug = format!("{:?}", config);
        assert!(debug.contains("SystemDefault"));
        assert!(debug.contains("48000"));
        assert!(debug.contains("2"));
    }

    // ── resolve_capture_target tests (require audio hardware) ────────

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn resolve_system_default_returns_valid_device_id() {
        let config = MacosCaptureConfig {
            target: CaptureTarget::SystemDefault,
            sample_rate: 48000,
            channels: 2,
        };

        let result = resolve_capture_target(&config);
        assert!(
            result.is_ok(),
            "resolve SystemDefault should succeed: {:?}",
            result.err()
        );

        let (device_id, process_tap) = result.unwrap();
        assert!(device_id > 0, "device_id should be > 0, got {}", device_id);
        assert!(
            process_tap.is_none(),
            "SystemDefault should not create a ProcessTap"
        );
    }

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn resolve_device_by_id_succeeds_for_default() {
        // First, get the default device ID
        use coreaudio::audio_object::AudioObject;
        let default_id = AudioObject::default_output_device().expect("should get default device");

        let config = MacosCaptureConfig {
            target: CaptureTarget::Device(DeviceId(default_id.to_string())),
            sample_rate: 48000,
            channels: 2,
        };

        let result = resolve_capture_target(&config);
        assert!(
            result.is_ok(),
            "resolve Device should succeed: {:?}",
            result.err()
        );

        let (device_id, process_tap) = result.unwrap();
        assert_eq!(
            device_id, default_id,
            "resolved device_id should match requested"
        );
        assert!(
            process_tap.is_none(),
            "Device target should not create a ProcessTap"
        );
    }

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn resolve_device_invalid_id_returns_error() {
        let config = MacosCaptureConfig {
            target: CaptureTarget::Device(DeviceId("not-a-number".to_string())),
            sample_rate: 48000,
            channels: 2,
        };

        let result = resolve_capture_target(&config);
        assert!(result.is_err(), "invalid device ID should return error");
        match result.unwrap_err() {
            AudioError::DeviceNotFound { device_id } => {
                assert_eq!(device_id, "not-a-number");
            }
            other => panic!("Expected DeviceNotFound, got: {:?}", other),
        }
    }

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn resolve_application_by_name_nonexistent_returns_error() {
        let config = MacosCaptureConfig {
            target: CaptureTarget::ApplicationByName(
                "ThisApplicationDefinitelyDoesNotExist_12345".to_string(),
            ),
            sample_rate: 48000,
            channels: 2,
        };

        let result = resolve_capture_target(&config);
        assert!(result.is_err(), "nonexistent app name should return error");
        match result.unwrap_err() {
            AudioError::ApplicationNotFound { identifier } => {
                assert!(
                    identifier.contains("ThisApplicationDefinitelyDoesNotExist"),
                    "error should contain the app name, got: {}",
                    identifier
                );
            }
            other => panic!("Expected ApplicationNotFound, got: {:?}", other),
        }
    }

    #[test]
    #[ignore = "requires macOS 14.4+ audio hardware"]
    fn resolve_application_by_pid_smoke_test() {
        // Use the current process PID — it won't necessarily produce audio,
        // but tests the tap creation path. Expect either success or a specific
        // error (e.g., the process isn't an audio source).
        let current_pid = std::process::id();
        let config = MacosCaptureConfig {
            target: CaptureTarget::Application(ApplicationId(current_pid.to_string())),
            sample_rate: 48000,
            channels: 2,
        };

        let result = resolve_capture_target(&config);
        // Either succeeds (tap created) or fails with a backend error
        // (process isn't an audio source). Both are valid outcomes.
        match &result {
            Ok((device_id, tap)) => {
                assert!(*device_id > 0, "tap device_id should be > 0");
                assert!(
                    tap.is_some(),
                    "Application target should create a ProcessTap"
                );
            }
            Err(AudioError::BackendError { .. }) => {
                // Expected: the current process may not be a valid audio source
            }
            Err(AudioError::SystemError(_)) => {
                // Expected: CATapDescription might not be available
            }
            Err(other) => {
                panic!("Unexpected error type for Application target: {:?}", other);
            }
        }
    }

    // ── Full stream creation tests (require audio hardware) ──────────

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn create_macos_capture_system_default() {
        use crate::bridge::calculate_capacity;
        use crate::bridge::ring_buffer::create_bridge;
        use crate::core::config::AudioFormat;

        let format = AudioFormat::default();
        let capacity = calculate_capacity(None, 4);
        let (producer, _consumer) = create_bridge(capacity, format);

        let config = MacosCaptureConfig {
            target: CaptureTarget::SystemDefault,
            sample_rate: 48000,
            channels: 2,
        };

        let result = create_macos_capture(config, producer);
        assert!(
            result.is_ok(),
            "create_macos_capture should succeed: {:?}",
            result.err()
        );

        let stream = result.unwrap();
        assert!(stream.is_active(), "stream should be active after creation");

        // Clean up: stop the stream
        let stop_result = stream.stop_capture();
        assert!(stop_result.is_ok(), "stop should succeed");
        assert!(
            !stream.is_active(),
            "stream should not be active after stop"
        );
    }

    // ── Compile-time trait assertions ────────────────────────────────

    #[test]
    fn macos_platform_stream_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<MacosPlatformStream>();
    }
}
