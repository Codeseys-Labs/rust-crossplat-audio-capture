// macOS CoreAudio backend implementation.
// CoreAudio OSStatus errors, typically wrapped in `coreaudio_rs::Error`,
// are consistently mapped to this crate's `AudioError::BackendSpecificError`
// using the `map_ca_error` utility function within this module. This ensures
// uniform error reporting from the CoreAudio backend.

use crate::audio::core::{
    AudioCaptureConfig, AudioDevice, AudioError, AudioFormat, AudioResult, CapturingStream,
    DeviceEnumerator, DeviceId, DeviceKind, SampleFormat,
    // AudioBuffer trait is removed, struct will be imported from crate::core::buffer
};
use crate::core::buffer::AudioBuffer; // This is the new AudioBuffer struct
use coreaudio_rs::audio_buffer::AudioBufferList as CAAudioBufferList;
use coreaudio_rs::audio_object::{
    AudioObject, AudioObjectPropertyAddress, AudioObjectPropertyElement, AudioObjectPropertyScope,
};
use coreaudio_rs::audio_unit::audio_device::AudioDeviceID;
use coreaudio_rs::audio_unit::audio_unit_element::{self as au_element, INPUT_BUS, OUTPUT_BUS};
use coreaudio_rs::audio_unit::{
    AudioComponent, AudioComponentDescription, AudioUnit, Element, RenderArgs, Scope, StreamFormat,
};
use coreaudio_rs::sys::{
    self, kAudioDevicePropertyDeviceIsAlive, kAudioDevicePropertyStreamFormat,
    kAudioDevicePropertyStreamFormatSupported, kAudioFormatFlagIsBigEndian,
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved, kAudioFormatFlagIsPacked,
    kAudioFormatFlagIsSignedInteger, kAudioFormatLinearPCM, kAudioObjectPropertyElementMaster,
    kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput,
    kAudioObjectPropertyScopeOutput, kAudioOutputUnitProperty_CurrentDevice,
    kAudioOutputUnitProperty_EnableIO, kAudioUnitManufacturer_Apple,
    kAudioUnitProperty_StreamFormat, kAudioUnitSubType_HALOutput, kAudioUnitType_Output,
    AudioStreamBasicDescription, AudioUnitRenderActionFlags, OSStatus,
};
use coreaudio_rs::Error as CAError;
use std::collections::VecDeque;
use std::os::raw::c_void;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::pin::Pin; // Required for Pin<Box<...>> in to_async_stream
use std::time::Instant; // Added for timestamping
use futures_channel::mpsc;
use futures_core::Stream as FuturesStream; // Alias to avoid conflict if Stream is defined elsewhere

// IMPORTANT: Applications using this library for audio capture on macOS MUST include the
// `NSAudioCaptureUsageDescription` key in their `Info.plist` file. This key provides a
// string explaining to the user why the application needs to capture audio. Without it,
// audio capture will fail silently or with a permissions error on macOS 10.14 Mojave and later.
//
// For application-level audio capture using Core Audio Taps (via `CoreAudioProcessTap`
// and `AudioCaptureBuilder::target_application_pid()`), macOS 14.4 or newer is required.
// The system will prompt the user for permission to record the screen and system audio
// for the specific application being targeted.
//
// Example for `Info.plist`:
// ```xml
// <key>NSAudioCaptureUsageDescription</key>
// <string>This app requires audio capture to process system or application audio.</string>
// ```

pub mod tap;
// Imports for application enumeration
use cocoa::base::{id, nil};
use cocoa::foundation::{NSArray, NSString};
use objc::runtime::{Class, Object, Sel, YES};
use objc::{class, msg_send, sel, sel_impl};

/// Information about a running application on macOS, relevant for audio capture.
///
/// This struct provides details such as the process ID (PID), localized name,
/// and bundle identifier of an application. This information is crucial for
/// identifying and targeting specific applications for audio capture using
/// Core Audio Taps on macOS.
///
/// Instances of `ApplicationInfo` are returned by [`enumerate_audio_applications()`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationInfo {
    /// The process identifier (PID) of the application.
    /// This PID is used with [`crate::api::AudioCaptureBuilder::target_application_pid()`]
    /// to specify the application from which to capture audio on macOS.
    pub process_id: u32, // pid_t is i32 on macOS, but u32 is fine for positive PIDs
    /// The localized name of the application as displayed to the user (e.g., "Safari", "Music").
    pub name: String,
    /// The bundle identifier of the application (e.g., "com.apple.Safari", "com.apple.Music").
    /// This can be `None` if the application does not have a bundle identifier (e.g., some command-line tools).
    pub bundle_id: Option<String>,
}

/// Enumerates running applications on macOS that are potential audio sources for application-level capture.
///
/// This function queries the system for a list of currently running applications
/// and returns a vector of [`ApplicationInfo`] structs. Each struct contains the
/// application's process ID (PID), localized name, and bundle identifier.
///
/// The PID obtained from `ApplicationInfo` can be used with
/// [`crate::api::AudioCaptureBuilder::target_application_pid()`] to attempt to capture
/// audio specifically from that application using Core Audio Taps.
///
/// # Platform Requirements
/// - **macOS**: This function is specific to macOS.
/// - For application audio capture to succeed, macOS 14.4+ is generally required,
///   and the application using this library must have the `NSAudioCaptureUsageDescription`
///   key in its `Info.plist`.
///
/// # Notes
/// - This function lists *running* applications. It does not guarantee that these
///   applications are currently producing audio.
/// - The list of applications can change as applications are launched or quit.
/// - PIDs can be reused by the operating system. If an application quits and another
///   starts, the new application might have the same PID.
///
/// # Returns
///
/// An `AudioResult` containing a `Vec<ApplicationInfo>` on success.
/// Returns `AudioError::BackendSpecificError` if an issue occurs during enumeration
/// (e.g., failure to communicate with system services).
///
/// # Example
///
/// ```rust,no_run
/// # use rust_crossplat_audio_capture::audio::macos::{enumerate_audio_applications, ApplicationInfo};
/// # use rust_crossplat_audio_capture::core::error::AudioResult;
/// fn list_apps() -> AudioResult<()> {
///     println!("Available running applications on macOS:");
///     let apps = enumerate_audio_applications()?;
///     if apps.is_empty() {
///         println!("No running applications found.");
///     } else {
///         for app_info in apps {
///             println!(
///                 "  PID: {}, Name: {}, Bundle ID: {:?}",
///                 app_info.process_id, app_info.name, app_info.bundle_id
///             );
///         }
///     }
///     Ok(())
/// }
/// ```
pub fn enumerate_audio_applications() -> AudioResult<Vec<ApplicationInfo>> {
    let mut app_infos: Vec<ApplicationInfo> = Vec::new();

    unsafe {
        // Get the shared NSWorkspace instance
        let workspace_class = class!(NSWorkspace);
        let shared_workspace: id = msg_send![workspace_class, sharedWorkspace];

        // Get the array of running applications
        // runningApplications returns an NSArray<NSRunningApplication *>
        let running_apps_nsarray: id = msg_send![shared_workspace, runningApplications];

        if running_apps_nsarray == nil {
            // This would be unusual, but good to check.
            return Err(AudioError::BackendSpecificError(
                "Failed to get running applications array from NSWorkspace (nil returned)".to_string(),
            ));
        }

        let count: usize = msg_send![running_apps_nsarray, count];

        for i in 0..count {
            let app: id = msg_send![running_apps_nsarray, objectAtIndex: i]; // app is an NSRunningApplication

            if app == nil {
                // Skip if somehow a nil object is in the array
                continue;
            }

            // Get processIdentifier (pid_t, which is i32 on macOS)
            let pid: i32 = msg_send![app, processIdentifier];

            // Get localizedName (NSString *)
            let name_nsstring: id = msg_send![app, localizedName];
            let name_str: String = if name_nsstring != nil {
                let c_str_name_ptr = NSString::UTF8String(name_nsstring);
                if !c_str_name_ptr.is_null() {
                    std::ffi::CStr::from_ptr(c_str_name_ptr).to_string_lossy().into_owned()
                } else {
                    // Fallback if UTF8String returns null (e.g., invalid UTF-8 or empty)
                    String::from("<Invalid Name>")
                }
            } else {
                String::from("<Unknown Name>") // Should not happen for localizedName if app object is valid
            };

            // Get bundleIdentifier (NSString *)
            let bundle_id_nsstring: id = msg_send![app, bundleIdentifier];
            let bundle_id: Option<String> = if bundle_id_nsstring != nil {
                let c_str_bundle_ptr = NSString::UTF8String(bundle_id_nsstring);
                if !c_str_bundle_ptr.is_null() {
                    let bundle_str = std::ffi::CStr::from_ptr(c_str_bundle_ptr).to_string_lossy().into_owned();
                    if bundle_str.is_empty() { // Treat empty string as None for bundle_id
                        None
                    } else {
                        Some(bundle_str)
                    }
                } else {
                     // Fallback if UTF8String returns null
                    None // Or Some("<Invalid Bundle ID>".to_string()) if explicit error string is preferred
                }
            } else {
                None // bundleIdentifier can legitimately be nil
            };

            app_infos.push(ApplicationInfo {
                process_id: pid as u32, // pid_t is i32, casting to u32 for positive PIDs
                name: name_str,
                bundle_id,
            });
        }
    } // unsafe block ends

    Ok(app_infos)
}
/// A representation of a CoreAudio audio device.
///
/// This struct holds the `AudioDeviceID` and potentially other information
/// like the device name or UID if fetched.
#[derive(Debug)] // Added Debug for easier development, device_id is u32.
pub(crate) struct MacosAudioDevice {
    device_id: AudioDeviceID,
    // TODO: Potentially store name/UID if fetched during enumeration or lookup.
}

// Helper function to convert AudioFormat to AudioStreamBasicDescription
fn audio_format_to_asbd(format: &AudioFormat) -> AudioStreamBasicDescription {
    let mut flags = sys::kAudioFormatFlagIsPacked;
    match format.sample_format {
        SampleFormat::F32LE => {
            flags |= sys::kAudioFormatFlagIsFloat;
        }
        SampleFormat::S16LE | SampleFormat::S32LE => {
            flags |= sys::kAudioFormatFlagIsSignedInteger;
        }
        // Assuming LE, kAudioFormatFlagIsBigEndian would be set otherwise.
        // Other formats might need more specific flag handling.
        _ => {
            // Defaulting to signed int for safety, but this should ideally error or be more specific
            flags |= sys::kAudioFormatFlagIsSignedInteger;
        }
    }
    // For f32, it's common to use non-interleaved in CoreAudio.
    // However, our library aims for interleaved f32.
    // The ASBD set on the AUHAL input bus's output scope determines what CoreAudio gives us.
    // If we request interleaved f32 from CoreAudio, set flags accordingly.
    // If we request non-interleaved f32, we'll have to interleave it ourselves.
    // For now, this helper doesn't assume non-interleaved for f32.
    // flags |= sys::kAudioFormatFlagIsNonInterleaved; // If we want to request non-interleaved

    let bytes_per_sample = format.bits_per_sample / 8;
    let bytes_per_frame = bytes_per_sample as u32 * format.channels as u32;

    AudioStreamBasicDescription {
        mSampleRate: format.sample_rate as f64,
        mFormatID: sys::kAudioFormatLinearPCM,
        mFormatFlags: flags,
        mBytesPerPacket: bytes_per_frame,
        mFramesPerPacket: 1, // For uncompressed PCM
        mBytesPerFrame: bytes_per_frame,
        mChannelsPerFrame: format.channels as u32,
        mBitsPerChannel: format.bits_per_sample as u32,
        mReserved: 0,
    }
}

// Helper function to convert AudioStreamBasicDescription to AudioFormat
fn asbd_to_audio_format(asbd: &AudioStreamBasicDescription) -> AudioResult<AudioFormat> {
    if asbd.mFormatID != sys::kAudioFormatLinearPCM {
        return Err(AudioError::BackendSpecificError(format!(
            "Unsupported format ID: {}",
            asbd.mFormatID
        )));
    }

    let sample_format = if (asbd.mFormatFlags & sys::kAudioFormatFlagIsFloat) != 0 {
        // Assuming Little Endian for float if not specified otherwise by kAudioFormatFlagIsBigEndian
        if (asbd.mFormatFlags & sys::kAudioFormatFlagIsBigEndian) != 0 {
            // Or handle as an error if only F32LE is intended for now
            return Err(AudioError::FormatNotSupported("F32BE not supported".into()));
        }
        SampleFormat::F32LE
    } else if (asbd.mFormatFlags & sys::kAudioFormatFlagIsSignedInteger) != 0 {
        // Assuming Little Endian for int if not specified otherwise
        if (asbd.mFormatFlags & sys::kAudioFormatFlagIsBigEndian) != 0 {
            return Err(AudioError::FormatNotSupported(
                "Signed Int Big Endian not supported".into(),
            ));
        }
        match asbd.mBitsPerChannel {
            16 => SampleFormat::S16LE,
            32 => SampleFormat::S32LE,
            _ => {
                return Err(AudioError::FormatNotSupported(format!(
                    "Unsupported bits per channel for signed int: {}",
                    asbd.mBitsPerChannel
                )))
            }
        }
    } else {
        return Err(AudioError::FormatNotSupported(
            "Unknown sample format type".into(),
        ));
    };

    Ok(AudioFormat {
        sample_rate: asbd.mSampleRate as u32,
        channels: asbd.mChannelsPerFrame as u16,
        bits_per_sample: asbd.mBitsPerChannel as u16,
        sample_format,
    });
}

pub(crate) fn map_ca_error(err: CAError) -> AudioError {
    // err is coreaudio_rs::Error, which is a tuple struct `pub struct Error(pub OSStatus);`
    // So, err.0 gives the OSStatus.
    let os_status = err.0;
    match os_status as u32 {
        // Using `as u32` because OSStatus is i32, but constants are often u32.
        // Ensure these constants are correctly defined and accessible.
        // Might need to use `sys::kAudioHardwarePermissionsError as u32` etc.
        // For now, assuming direct u32 comparison is okay or these are i32.
        // Let's use the direct constants from `sys` which should be i32.
        sys::kAudioHardwarePermissionsError => AudioError::PermissionDenied,
        sys::kAudioUnitErr_FormatNotSupported => AudioError::FormatNotSupported(format!("CoreAudio OSStatus: {}", os_status)),
        // Add other specific mappings here if needed.
        // Example: kAudioUnitErr_InvalidProperty means the property is not supported by the AU
        // sys::kAudioUnitErr_InvalidProperty => AudioError::BackendSpecificError(format!("CoreAudio Invalid Property (OSStatus: {})", os_status)),
        // Example: kAudioUnitErr_InvalidElement means the element is out of range or not supported
        // sys::kAudioUnitErr_InvalidElement => AudioError::BackendSpecificError(format!("CoreAudio Invalid Element (OSStatus: {})", os_status)),
        // Example: kAudioUnitErr_CannotDoInCurrentContext might indicate a tap was invalidated (e.g. process quit)
        // sys::kAudioUnitErr_CannotDoInCurrentContext => AudioError::DeviceDisconnected(format!("CoreAudio CannotDoInCurrentContext (OSStatus: {})", os_status)),

        // Placeholder for a common "device disconnected" or "tap invalidated" error.
        // This needs to be researched for the most appropriate OSStatus code.
        // For now, a generic error is pushed in the callback for AudioUnitRender failures.
        // If a specific OSStatus like `kAudioServerDied` or similar is encountered by AudioUnitRender,
        // it could be mapped to DeviceDisconnected.
        // `kAudioHardwareUnspecifiedError` is -6999.
        // `kAudioHardwareNotRunningError` is -6998.
        // `kAudioHardwareUnsupportedOperationError` is -6997.
        // `kAudioDeviceUnsupportedFormatError` is -6989.
        // `kAudioUnitErr_FailedInitialization` is -10870
        // `kAudioUnitErr_InvalidScope` is -10868
        // `kAudioUnitErr_PropertyNotWritable` is -10867
        // `kAudioUnitErr_CannotDoInCurrentContext` is -10863
        // `kAudioUnitErr_InvalidElement` is -10877
        // `kAudioUnitErr_NoConnection` is -10872 (could be relevant for taps)

        _ => AudioError::BackendSpecificError(format!("CoreAudio error: {:?} (OSStatus: {})", err, os_status)),
    }
}

/// Represents an active audio stream for capturing on macOS.
///
/// This struct manages an `AudioUnit` for capturing system audio,
/// handling the input callback, data buffering, and format conversion.
pub(crate) struct MacosAudioStream {
    audio_unit: AudioUnit,
    /// Indicates if the stream has been started and the callback is active.
    is_started: Arc<AtomicBool>,
    /// The `AudioStreamBasicDescription` (ASBD) that the `AudioUnit` is configured
    /// to deliver on its input bus (Element 1, Output Scope). This is the format
    /// of the raw captured audio from CoreAudio before library conversion.
    current_asbd: Arc<Mutex<Option<sys::AudioStreamBasicDescription>>>,
    /// A queue to store captured audio data, converted to the library's
    /// standard `AudioFormat` (interleaved `f32` samples). Each element is
    /// an `AudioResult` wrapping an `AudioBuffer` struct.
    data_queue: Arc<Mutex<VecDeque<AudioResult<AudioBuffer>>>>, // Changed to AudioBuffer struct
    stream_start_time: Instant, // Epoch for timestamping audio buffers
    // _input_callback_handle: Option<Box<dyn Any + Send + Sync>>, // Not strictly needed if closure is 'static
}

// Custom Debug implementation because AudioUnit might not be Debug.
impl std::fmt::Debug for MacosAudioStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacosAudioStream")
            .field("is_started", &self.is_started.load(Ordering::Relaxed))
            .field("current_asbd", &self.current_asbd.lock().unwrap())
            .field("data_queue_len", &self.data_queue.lock().unwrap().len())
            // Not displaying audio_unit itself to avoid issues if it's not Debug
            .field("audio_unit", &"<AudioUnit instance>")
            .finish()
    }
}

impl MacosAudioStream {
    /// Creates a new `MacosAudioStream`.
    ///
    /// # Arguments
    ///
    /// * `audio_unit`: A configured `AudioUnit` instance ready for capture.
    fn new(audio_unit: AudioUnit) -> Self {
        Self {
            audio_unit,
            is_started: Arc::new(AtomicBool::new(false)),
            current_asbd: Arc::new(Mutex::new(None)),
            // TODO: Consider making queue capacity configurable.
            data_queue: Arc::new(Mutex::new(VecDeque::with_capacity(10))),
            stream_start_time: Instant::now(), // Record stream start time as epoch
            // _input_callback_handle: None,
        }
    }
}

impl CapturingStream for MacosAudioStream {
    /// Starts the audio capture stream.
    ///
    /// This method retrieves the configured stream format (ASBD) from the `AudioUnit`,
    /// sets up the input callback for receiving audio data, and starts the `AudioUnit`.
    /// The callback will be invoked periodically by CoreAudio. Inside the callback,
    /// `AudioUnitRender` is called on the AUHAL's input bus (Element 1) to pull
    /// captured audio data. This data is then converted to interleaved `f32` samples
    /// and enqueued.
    fn start(&mut self) -> AudioResult<()> {
        if self.is_started.load(Ordering::SeqCst) {
            return Ok(());
        }

        // 1. Retrieve and store the ASBD configured on the AudioUnit's input bus (output scope).
        // This is the format CoreAudio will provide.
        let asbd: sys::AudioStreamBasicDescription = self
            .audio_unit
            .get_property(
                sys::kAudioUnitProperty_StreamFormat,
                Scope::Output, // Data flowing OUT of the INPUT bus
                Element::INPUT_BUS, // The bus providing captured audio (Element 1)
            )
            .map_err(map_ca_error)?;
        *self.current_asbd.lock().unwrap() = Some(asbd);

        // 2. Clone Arcs for the callback closure.
        let data_queue_clone = self.data_queue.clone();
        let current_asbd_clone = self.current_asbd.clone();
        let is_started_clone = self.is_started.clone();
        let stream_start_time_clone = self.stream_start_time; // Clone for the closure
        // let audio_unit_instance_clone = self.audio_unit.clone(); // AudioUnit is not Clone

        // 3. Set the input callback.
        // This callback is set on the output bus (Element 0), input scope.
        // It's called when the (disabled) output bus would need data. We use it as a periodic hook.
        self.audio_unit
            .set_input_callback(move |mut args: RenderArgs| -> Result<(), OSStatus> {
                if !is_started_clone.load(Ordering::Relaxed) {
                    return Ok(()); // Stream stopped
                }

                let au_instance = args.audio_unit_ref.instance();
                let num_frames = args.num_frames;
                let timestamp = args.timestamp; // *const sys::AudioTimeStamp

                let locked_asbd_opt = current_asbd_clone.lock().unwrap();
                let input_asbd = match locked_asbd_opt.as_ref() {
                    Some(val) => val,
                    None => {
                        // Should not happen if start() logic is correct
                        // Consider logging an error or pushing an error to the queue
                        eprintln!("Error: ASBD not available in callback.");
                        return Err(sys::kAudio_ParamError as OSStatus); // Indicate an error
                    }
                };

                // Allocate AudioBufferList for capturing data from Element 1
                let is_input_interleaved =
                    (input_asbd.mFormatFlags & sys::kAudioFormatFlagIsNonInterleaved) == 0;
                
                // `coreaudio_rs::audio_buffer::AudioBufferList::allocate` creates a Box<sys::AudioBufferList>
                // and allocates mData for each buffer if the last param is true.
                let captured_abl_boxed_result = CAAudioBufferList::allocate(
                    input_asbd.mChannelsPerFrame, // Number of channels in the ASBD
                    num_frames,                   // Number of frames to render
                    is_input_interleaved,         // Whether the format is interleaved
                    true,                         // True to allocate mData pointers
                );

                let mut captured_abl_boxed = match captured_abl_boxed_result.map_err(map_ca_error) {
                    Ok(abl) => abl,
                    Err(audio_err) => {
                        eprintln!("Callback Error: Failed to allocate AudioBufferList for capture: {:?}", audio_err);
                        let mut queue = data_queue_clone.lock().unwrap();
                        if queue.len() == queue.capacity() {
                            queue.pop_front(); // Make space if full
                        }
                        queue.push_back(Err(audio_err));
                        return Ok(()); // Error pushed to queue, callback returns OK to CoreAudio
                    }
                };
                
                let captured_abl_ptr: *mut sys::AudioBufferList = &mut *captured_abl_boxed;

                let mut render_action_flags: AudioUnitRenderActionFlags = 0;

                // Call AudioUnitRender on the INPUT BUS (Element 1) to get captured data.
                let os_status = unsafe {
                    sys::AudioUnitRender(
                        au_instance,
                        &mut render_action_flags,
                        timestamp,
                        au_element::INPUT_BUS, // Capture from input bus
                        num_frames,
                        captured_abl_ptr,
                    )
                };

                if os_status == sys::noErr {
                    // Process and enqueue the data
                    // For simplicity, let's assume input_asbd.mFormatFlags has kAudioFormatFlagIsFloat
                    // and input_asbd.mBitsPerChannel == 32.
                    // A more robust solution would handle various ASBD formats.
                    if (input_asbd.mFormatFlags & sys::kAudioFormatFlagIsFloat) == 0 ||
                       input_asbd.mBitsPerChannel != 32 {
                        eprintln!("Callback Error: Captured ASBD is not 32-bit float. Actual flags: {}, bits: {}", input_asbd.mFormatFlags, input_asbd.mBitsPerChannel);
                        let mut queue = data_queue_clone.lock().unwrap();
                        queue.push_back(Err(AudioError::FormatNotSupported(
                            "Captured data is not 32-bit float as expected by current conversion logic".into(),
                        )));
                        return Ok(()); // Or return an error status?
                    }

                    let num_channels = input_asbd.mChannelsPerFrame as usize;
                    let num_frames_usize = num_frames as usize;
                    let mut interleaved_f32: Vec<f32> = vec![0.0f32; num_frames_usize * num_channels];

                    let buffers_slice = unsafe {
                        std::slice::from_raw_parts(
                            (*captured_abl_ptr).mBuffers.as_ptr(),
                            (*captured_abl_ptr).mNumberBuffers as usize,
                        )
                    };

                    if is_input_interleaved {
                        // Data is already interleaved: mNumberBuffers = 1
                        if !buffers_slice.is_empty() {
                            let source_buffer = &buffers_slice[0];
                            let samples_in_buffer = source_buffer.mDataByteSize as usize / std::mem::size_of::<f32>();
                            if samples_in_buffer == interleaved_f32.len() {
                                let source_slice = unsafe {
                                    std::slice::from_raw_parts(source_buffer.mData as *const f32, samples_in_buffer)
                                };
                                interleaved_f32.copy_from_slice(source_slice);
                            } else {
                                eprintln!("Callback Error: Interleaved buffer size mismatch.");
                                // Push error to queue
                            }
                        }
                    } else {
                        // Data is non-interleaved: mNumberBuffers = num_channels
                        // Each buffer in buffers_slice is a channel
                        if buffers_slice.len() == num_channels {
                            for frame_idx in 0..num_frames_usize {
                                for ch_idx in 0..num_channels {
                                    let source_buffer = &buffers_slice[ch_idx];
                                    // Ensure mData is not null and mDataByteSize is sufficient
                                    if !source_buffer.mData.is_null() && source_buffer.mDataByteSize >= ((frame_idx + 1) * std::mem::size_of::<f32>()) as u32 {
                                        let sample_ptr = source_buffer.mData as *const f32;
                                        interleaved_f32[frame_idx * num_channels + ch_idx] =
                                            unsafe { *sample_ptr.add(frame_idx) };
                                    } else {
                                         eprintln!("Callback Error: Non-interleaved buffer access issue at frame {}, channel {}.", frame_idx, ch_idx);
                                         // Handle error, maybe fill with 0 or stop
                                    }
                                }
                            }
                        } else {
                             eprintln!("Callback Error: Non-interleaved buffer count mismatch.");
                             // Push error to queue
                        }
                    }
                    
                    let target_format = AudioFormat {
                        sample_rate: input_asbd.mSampleRate as u32,
                        channels: input_asbd.mChannelsPerFrame as u16,
                        bits_per_sample: 32, // We converted to f32
                        sample_format: SampleFormat::F32LE,
                    };
                    let audio_buffer_struct = AudioBuffer {
                        data: interleaved_f32,
                        channels: target_format.channels,
                        sample_rate: target_format.sample_rate,
                        format: target_format,
                        timestamp: Instant::now().duration_since(stream_start_time_clone), // Timestamp relative to stream start
                    };
                    
                    let mut queue = data_queue_clone.lock().unwrap();
                    if queue.len() == queue.capacity() {
                        queue.pop_front(); // Make space if full (simple strategy)
                    }
                    queue.push_back(Ok(audio_buffer_struct)); // Changed to AudioBuffer struct

                } else {
                    eprintln!("AudioUnitRender error in callback: {}", os_status);
                    let mut queue = data_queue_clone.lock().unwrap();
                    queue.push_back(Err(AudioError::BackendSpecificError(format!(
                        "AudioUnitRender failed in callback with status: {}",
                        os_status
                    ))));
                }
                Ok(())
            })
            .map_err(map_ca_error)?;

        // 4. Start the AudioUnit
        self.audio_unit
            .start()
            .map_err(map_ca_error)?;
        self.is_started.store(true, Ordering::SeqCst);

        Ok(())
    }

    /// Stops the audio capture stream.
    ///
    /// This method stops the `AudioUnit` and sets the internal state to not running.
    fn stop(&mut self) -> AudioResult<()> {
        if !self.is_started.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.audio_unit
            .stop()
            .map_err(map_ca_error)?;
        self.is_started.store(false, Ordering::SeqCst);
        // Optionally, clear the callback? Or clear the queue?
        // For now, just stop. The callback checks `is_started`.
        Ok(())
    }

    /// Reads a chunk of captured audio data from the stream synchronously.
    ///
    /// This method attempts to retrieve one audio buffer from an internal queue
    /// populated by the CoreAudio input callback.
    ///
    /// # Behavior
    /// - If the stream is not running, it returns `Err(AudioError::InvalidOperation)`.
    /// - If a buffer is available in the queue, it returns `Ok(Some(buffer))`.
    ///   The `buffer` itself is an `AudioResult<AudioBuffer>` (the struct)
    ///   from the queue, so if an error occurred during capture in the callback
    ///   (e.g., `AudioUnitRender` failure), this method will propagate that error
    ///   by returning `Err(AudioError::...)`.
    /// - If the queue is empty (buffer underrun), it returns `Ok(None)`. This indicates
    ///   that no data is currently available.
    /// - If the internal data queue's mutex is poisoned, it returns
    ///   `Err(AudioError::MutexLockError)`.
    ///
    /// # Timeout
    /// The `_timeout_ms` parameter is currently **ignored**. This method performs a
    /// non-blocking check of the queue. Future implementations may use this
    /// parameter to enable blocking reads with a timeout.
    ///
    /// # Returns
    /// - `Ok(Some(AudioBuffer))`: A buffer of audio data (the struct).
    /// - `Ok(None)`: No data currently available in the queue (non-blocking behavior).
    /// - `Err(AudioError)`: An error occurred, such as the stream not running,
    ///   an error propagated from the audio callback, or a mutex lock failure.
    fn read_chunk(&mut self, _timeout_ms: Option<u32>) -> AudioResult<Option<AudioBuffer>> { // Changed return type
        if !self.is_running() {
            return Err(AudioError::InvalidOperation("Stream is not running or not started.".to_string()));
        }

        // TODO: Implement proper timeout logic if _timeout_ms is Some.
        // For now, this is a non-blocking pop.
        match self.data_queue.lock().map_err(|_| AudioError::MutexLockError("data_queue".to_string()))?.pop_front() {
            Some(audio_result) => audio_result.map(Some), // Propagates Ok(buffer) or Err(error_from_callback)
            None => Ok(None), // Queue is empty, no data available
        }
    }

    /// Gets the format of the audio stream as delivered by `read_chunk`.
    ///
    /// This is the format after conversion to the library's standard (interleaved `f32`).
    fn get_format(&self) -> AudioResult<AudioFormat> {
        let locked_asbd = self.current_asbd.lock().unwrap();
        if let Some(asbd) = locked_asbd.as_ref() {
            Ok(AudioFormat {
                sample_rate: asbd.mSampleRate as u32,
                channels: asbd.mChannelsPerFrame as u16,
                bits_per_sample: 32, // Output is f32
                sample_format: SampleFormat::F32LE,
            })
        } else {
            Err(AudioError::NotInitialized(
                "Stream not started or ASBD not available".into(),
            ))
        }
    }

    /// Checks if the stream is currently started and attempting to capture.
    fn is_running(&self) -> bool {
        self.is_started.load(Ordering::SeqCst)
    }

    /// Converts the synchronous `CapturingStream` into an asynchronous `Stream`.
    ///
    /// This method sets up an MPSC (multi-producer, single-consumer) channel.
    /// A new helper thread is spawned which continuously attempts to read audio data
    /// chunks from the internal `data_queue` (populated by the CoreAudio callback).
    /// These chunks (or errors) are then sent through the MPSC channel's sender.
    ///
    /// The returned `Stream` is the receiver part of this MPSC channel. Consumers
    /// can await items from this stream to get audio data asynchronously.
    ///
    /// # Helper Thread Logic
    /// The helper thread performs the following actions in a loop:
    /// 1. Tries to pop an `AudioResult<AudioBuffer>` from `data_queue`.
    /// 2. If data is popped:
    ///    - It sends the data via the MPSC sender (`tx`).
    ///    - If sending fails (e.g., the receiver `rx` is dropped), the thread assumes
    ///      the consumer is no longer interested and terminates its loop.
    /// 3. If `data_queue` is empty:
    ///    - It checks if the main CoreAudio stream (`self.is_started`) is still active.
    ///    - If the main stream is stopped AND the queue is empty, it means all data has
    ///      been drained, so the helper thread terminates its loop.
    ///    - If the main stream is still active but the queue is empty (temporary underrun),
    ///      the thread sleeps for a short duration (10ms) to avoid busy-waiting before
    ///      checking the queue again.
    ///
    /// # Returns
    /// - `Ok(Pin<Box<dyn futures_core::Stream<Item = AudioResult<AudioBuffer>> + Send + Sync + 'a>>)`:
    ///   An asynchronous stream of audio buffers (structs).
    /// - `Err(AudioError::InvalidOperation)`: If the stream has not been started.
    fn to_async_stream<'a>(
        &'a mut self,
    ) -> AudioResult<
        Pin<Box<dyn futures_core::Stream<Item = AudioResult<AudioBuffer>> + Send + Sync + 'a>>, // Changed to AudioBuffer struct
    > {
        if !self.is_running() {
            return Err(AudioError::InvalidOperation(
                "Stream not started or not in streaming state".to_string(),
            ));
        }

        let (tx, rx) = futures_channel::mpsc::unbounded::<AudioResult<AudioBuffer>>(); // Changed to AudioBuffer struct

        let data_queue_clone = self.data_queue.clone();
        let is_started_clone = self.is_started.clone();

        std::thread::spawn(move || {
            loop {
                let mut queue_guard = data_queue_clone.lock().unwrap();
                match queue_guard.pop_front() {
                    Some(audio_result) => {
                        // Drop the lock before sending to avoid holding it if send blocks or errors.
                        drop(queue_guard);
                        if tx.unbounded_send(audio_result).is_err() {
                            // Receiver was dropped, consumer is gone.
                            eprintln!("Async stream receiver dropped, helper thread terminating.");
                            break;
                        }
                    }
                    None => {
                        // Queue is empty, drop lock before potentially sleeping.
                        drop(queue_guard);
                        if !is_started_clone.load(Ordering::SeqCst) {
                            // Stream stopped and queue is empty, so we are done.
                            eprintln!("Main stream stopped and queue empty, async helper thread terminating.");
                            break;
                        } else {
                            // Stream is running but queue is empty, wait a bit.
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                    }
                }
            }
            // tx is dropped here when thread exits, closing the stream.
            eprintln!("Async audio streaming helper thread finished.");
        });

        Ok(Box::pin(rx))
    }
}

impl AudioDevice for MacosAudioDevice {
    /// Gets the unique identifier of the audio device.
    fn get_id(&self) -> DeviceId {
        self.device_id.to_string()
    }

    /// Gets the human-readable name of the audio device.
    ///
    /// This queries `kAudioDevicePropertyDeviceNameCFString`.
    fn get_name(&self) -> AudioResult<String> {
        AudioObject::name(&self.device_id).map_err(map_ca_error)
    }

    /// Gets a human-readable description of the audio device.
    ///
    /// TODO: Implement this, potentially combining name and other properties.
    fn get_description(&self) -> AudioResult<String> {
        todo!("Implement get_description for MacosAudioDevice")
    }

    /// Gets the kind of the audio device (Input/Output).
    ///
    /// For loopback capture on macOS, we treat the output device (e.g., system speakers)
    /// as an input source from the perspective of the capture API.
    fn kind(&self) -> AudioResult<DeviceKind> {
        Ok(DeviceKind::Input) // For loopback, the output device is the input source.
    }

    /// Checks if this device is the default device for the given kind.
    ///
    /// TODO: Implement this by comparing with default device IDs from CoreAudio.
    fn is_default(&self, _kind: DeviceKind) -> AudioResult<bool> {
        todo!("Implement is_default for MacosAudioDevice")
    }

    /// Checks if the audio device is currently active or running.
    ///
    /// This queries `kAudioDevicePropertyDeviceIsAlive`.
    fn is_active(&self) -> AudioResult<bool> {
        // kAudioDevicePropertyDeviceIsAlive is a standard property.
        // The `AudioObject::alive()` method directly queries this.
        AudioObject::is_alive(&self.device_id).map_err(map_ca_error)
    }

    /// Gets the default audio format for the device.
    ///
    /// This queries `kAudioDevicePropertyStreamFormat` on the output scope.
    fn get_default_format(&self) -> AudioResult<AudioFormat> {
        let address = AudioObjectPropertyAddress {
            mSelector: kAudioDevicePropertyStreamFormat,
            mScope: kAudioObjectPropertyScopeOutput, // For loopback, we inspect the output device's format
            mElement: kAudioObjectPropertyElementMaster,
        };
        let asbd: AudioStreamBasicDescription = AudioObject::get_property(&self.device_id, address)
            .map_err(map_ca_error)?;
        asbd_to_audio_format(&asbd)
    }

    /// Gets a list of supported audio formats for the device.
    ///
    /// Currently, this returns a vector containing only the default format.
    /// TODO: Implement full CoreAudio format enumeration (e.g., using `kAudioStreamPropertyAvailablePhysicalFormats`).
    fn get_supported_formats(&self) -> AudioResult<Vec<AudioFormat>> {
        // TODO: Implement full CoreAudio format enumeration.
        // For now, just return the default format as per simplification.
        let default_format = self.get_default_format()?;
        Ok(vec![default_format])
    }

    /// Checks if a given audio format is supported by the device.
    ///
    /// Simplified: Checks if the format matches the device's default format.
    /// TODO: Implement a proper check using `kAudioDevicePropertyStreamFormatSupported`.
    fn is_format_supported(&self, format_to_check: &AudioFormat) -> AudioResult<bool> {
        // TODO: Implement kAudioDevicePropertyStreamFormatSupported check.
        // This involves converting format_to_check to ASBD and querying the property.
        // For now, simplified check:
        let default_format = self.get_default_format()?;
        Ok(format_to_check == &default_format)
    }

    /// Creates an audio stream for capturing from this device.
    ///
    /// This sets up an `AudioUnit` (AUHAL) configured for capturing audio
    /// from the specified device.
    fn create_stream(
        &mut self,
        capture_config: &AudioCaptureConfig,
    ) -> AudioResult<Box<dyn CapturingStream>> {
        // 1. Create AudioComponentDescription for an Output Unit (AUHAL)
        let desc = AudioComponentDescription {
            component_type: kAudioUnitType_Output,
            component_sub_type: kAudioUnitSubType_HALOutput,
            component_manufacturer: kAudioUnitManufacturer_Apple,
            component_flags: 0,
            component_flags_mask: 0,
        };

        // 2. Find component
        let component = AudioComponent::find(Some(&desc), None)
            .ok_or_else(|| {
                AudioError::BackendSpecificError("Failed to find AUHAL component".into())
            })?
            .into_owned(); // into_owned is important if AudioComponent is a Cow

        // 3. Create AudioUnit instance
        let mut audio_unit = component
            .new_instance()
            .map_err(map_ca_error)?;

        // 4. Set current device on AUHAL
        audio_unit
            .set_property(
                kAudioOutputUnitProperty_CurrentDevice,
                Scope::Global,
                audio_unit_element::OUTPUT_BUS, // Global scope usually uses output bus for device selection
                Some(&self.device_id),
            )
            .map_err(map_ca_error)?;

        // 5. Enable IO for input (capture) on the output unit's input bus
        let enable_io: u32 = 1;
        audio_unit
            .set_property(
                kAudioOutputUnitProperty_EnableIO,
                Scope::Input,                  // Scope for enabling input
                audio_unit_element::INPUT_BUS, // Element is the input bus (capture side)
                Some(&enable_io),
            )
            .map_err(map_ca_error)?;

        // 6. Disable IO for output (to prevent sound passthrough from this AU instance)
        let disable_io: u32 = 0;
        audio_unit
            .set_property(
                kAudioOutputUnitProperty_EnableIO,
                Scope::Output,                  // Scope for enabling output
                audio_unit_element::OUTPUT_BUS, // Element is the output bus (playback side)
                Some(&disable_io),
            )
            .map_err(map_ca_error)?;

        // 7. Convert capture_config.stream_config.format to an AudioStreamBasicDescription (ASBD)
        let asbd = audio_format_to_asbd(&capture_config.stream_config.format);

        // 8. Set stream format for the captured audio.
        // This is set on the OUTPUT scope of the INPUT bus (Element 1).
        // This defines the format of the audio data that the AudioUnit will make available
        // *from* its input bus (i.e., the captured audio stream from the device).
        audio_unit
            .set_property(
                kAudioUnitProperty_StreamFormat,
                Scope::Output,                 // Data flowing OUT of the INPUT bus
                audio_unit_element::INPUT_BUS, // The bus providing captured audio
                Some(&asbd),
            )
            .map_err(map_ca_error)?;

        // Set the "client" format on the INPUT scope of the OUTPUT bus (Element 0).
        // This defines the format that the AudioUnit's output bus would expect on its input side
        // if it were rendering audio (which it isn't, as output IO is disabled).
        // For loopback capture, it's common to set this to the same format as the capture format.
        audio_unit
            .set_property(
                kAudioUnitProperty_StreamFormat,
                Scope::Input,                   // Data flowing INTO the OUTPUT bus
                audio_unit_element::OUTPUT_BUS, // The bus that would normally render to speakers
                Some(&asbd),
            )
            .map_err(map_ca_error)?;

        // 9. Initialize AudioUnit
        audio_unit
            .initialize()
            .map_err(map_ca_error)?;

        // 10. Define MacosAudioStream struct skeleton (done above)
        // 11. Return Ok(Box::new(MacosAudioStream::new(audio_unit)))
        Ok(Box::new(MacosAudioStream::new(audio_unit)))
    }
}

// --- MacosApplicationAudioStream ---
// Struct and implementations for application-level audio capture using CoreAudioProcessTap.

use crate::audio::macos::tap::CoreAudioProcessTap;
// Note: Other necessary imports like AudioUnit, Element, Scope, sys, Arc, Mutex, etc.,
// are assumed to be covered by existing imports at the top of the file.
// Explicitly listing some that are definitely needed for clarity in this block:
// use crate::core::buffer::VecAudioBuffer; // This will be removed or unused
use crate::audio::core::{AudioFormat, AudioResult, CapturingStream, SampleFormat}; // AudioBuffer trait removed from here
// AudioBuffer struct is already imported at the top of the module.
use coreaudio_rs::audio_unit::{AudioUnit, Element, Scope, RenderArgs};
use coreaudio_rs::sys::{
    self, kAudioUnitType_Output, kAudioUnitSubType_HALOutput, kAudioUnitManufacturer_Apple,
    kAudioOutputUnitProperty_CurrentDevice, kAudioOutputUnitProperty_EnableIO,
    kAudioUnitProperty_StreamFormat, AudioStreamBasicDescription, OSStatus,
    AudioUnitRenderActionFlags, kAudioFormatFlagIsNonInterleaved, kAudioFormatFlagIsFloat,
};
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use std::collections::VecDeque;
// Pin is already imported at the top level of the module.
// use std::pin::Pin;
// mpsc and FuturesStream are also imported at the top level.
// use futures_channel::mpsc;
// use futures_core::Stream as FuturesStream;


/// Represents an active audio stream for capturing application-specific audio on macOS
/// using a `CoreAudioProcessTap`.
///
/// This struct manages an `AudioUnit` (AUHAL) instance configured to receive audio
/// from a specific application process via its tap. It handles the input callback,
/// data buffering, and format conversion from the tap's native format to the
/// library's standard interleaved F32LE format.
pub struct MacosApplicationAudioStream {
    audio_unit: AudioUnit,
    /// The `CoreAudioProcessTap` providing the audio data. This stream takes ownership.
    #[allow(dead_code)] // May be used later for tap management or info
    process_tap: CoreAudioProcessTap,
    /// Indicates if the stream has been started and the callback is active.
    is_started: Arc<AtomicBool>,
    /// The native `AudioStreamBasicDescription` (ASBD) of the tap.
    /// The `AudioUnit` is configured to read data in this format from the tap.
    native_tap_asbd: Arc<Mutex<Option<sys::AudioStreamBasicDescription>>>,
    /// A queue to store captured audio data, converted to the library's
    /// standard `AudioFormat` (interleaved `f32` samples). Each element is
    /// an `AudioResult` wrapping an `AudioBuffer` struct.
    data_queue: Arc<Mutex<VecDeque<AudioResult<AudioBuffer>>>>, // Changed to AudioBuffer struct
    stream_start_time: Instant, // Epoch for timestamping audio buffers
    // _input_callback_handle: Option<Box<dyn std::any::Any + Send + Sync>>, // If needed for callback lifetime
}

impl std::fmt::Debug for MacosApplicationAudioStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MacosApplicationAudioStream")
            .field("process_tap_id", &self.process_tap.id())
            .field("is_started", &self.is_started.load(Ordering::Relaxed))
            .field("native_tap_asbd", &self.native_tap_asbd.lock().unwrap())
            .field("data_queue_len", &self.data_queue.lock().unwrap().len())
            .field("audio_unit", &"<AudioUnit instance>")
            .finish()
    }
}

impl MacosApplicationAudioStream {
    /// Creates a new `MacosApplicationAudioStream` for capturing audio from a specific application process.
    ///
    /// This function configures an `AudioUnit` (AUHAL) to connect to the provided `CoreAudioProcessTap`.
    /// The AUHAL is set up to:
    /// 1. Use the tap's `AudioObjectID` as its current device.
    /// 2. Enable input IO on its input bus (Element 1) and disable output IO on its output bus (Element 0).
    /// 3. Expect audio data from the tap in the tap's native `AudioStreamBasicDescription` (ASBD).
    ///    Both the input bus's output scope and the output bus's input scope are configured with this ASBD.
    ///
    /// # Arguments
    ///
    /// * `process_tap`: The `CoreAudioProcessTap` instance representing the connection to the target application's audio.
    ///                  This stream will take ownership of the tap.
    /// * `_desired_output_format`: The audio format desired by the library user. Currently, this parameter is noted
    ///                             but the stream internally converts to interleaved F32LE based on the tap's native
    ///                             channel count and sample rate. Future enhancements might use this for more complex
    ///                             format negotiations or direct output format settings if supported by CoreAudio.
    ///
    /// # Returns
    ///
    /// An `AudioResult` containing the new `MacosApplicationAudioStream` on success, or an `AudioError` on failure.
    pub fn new(
        process_tap: CoreAudioProcessTap,
        _desired_output_format: &AudioFormat, // Marked as unused for now as per current logic
    ) -> AudioResult<Self> {
        // 1. Create AudioComponentDescription for an Output Unit (AUHAL)
        let desc = AudioComponentDescription {
            component_type: kAudioUnitType_Output,
            component_sub_type: kAudioUnitSubType_HALOutput,
            component_manufacturer: kAudioUnitManufacturer_Apple,
            component_flags: 0,
            component_flags_mask: 0,
        };

        // 2. Find component
        let component = AudioComponent::find(Some(&desc), None)
            .ok_or_else(|| {
                AudioError::BackendSpecificError("Failed to find AUHAL component for tap stream".into())
            })?
            .into_owned();

        // 3. Create AudioUnit instance
        let mut audio_unit = component.new_instance().map_err(map_ca_error)?;

        // 4. Set current device on AUHAL to the tap's AudioObjectID
        let tap_device_id = process_tap.id();
        audio_unit
            .set_property(
                kAudioOutputUnitProperty_CurrentDevice,
                Scope::Global,
                Element::OUTPUT_BUS, // Global device selection typically uses output bus
                Some(&tap_device_id),
            )
            .map_err(map_ca_error)?;

        // 5. Enable IO for input (capture) and disable for output
        let enable_io: u32 = 1;
        audio_unit
            .set_property(
                kAudioOutputUnitProperty_EnableIO,
                Scope::Input,      // Scope for enabling input
                Element::INPUT_BUS, // Element is the input bus (capture side from tap)
                Some(&enable_io),
            )
            .map_err(map_ca_error)?;

        let disable_io: u32 = 0;
        audio_unit
            .set_property(
                kAudioOutputUnitProperty_EnableIO,
                Scope::Output,      // Scope for enabling output
                Element::OUTPUT_BUS, // Element is the output bus (playback side, disabled)
                Some(&disable_io),
            )
            .map_err(map_ca_error)?;

        // 6. Get the tap's native stream format (ASBD)
        let tap_asbd = process_tap.get_stream_format()?;

        // 7. Set stream format for the captured audio from the tap.
        // This is set on the OUTPUT scope of the INPUT bus (Element 1).
        // It defines the format of audio data the AUHAL provides from the tap.
        audio_unit
            .set_property(
                kAudioUnitProperty_StreamFormat,
                Scope::Output,     // Data flowing OUT of the INPUT bus
                Element::INPUT_BUS, // The bus providing captured audio from the tap
                Some(&tap_asbd),
            )
            .map_err(map_ca_error)?;

        // 8. Set the "client" stream format on the INPUT scope of the OUTPUT bus (Element 0).
        // This defines the format the AUHAL's output bus would expect if it were rendering.
        // For tap capture, this is often set to the same ASBD as the tap's output.
        audio_unit
            .set_property(
                kAudioUnitProperty_StreamFormat,
                Scope::Input,       // Data flowing INTO the OUTPUT bus
                Element::OUTPUT_BUS, // The bus that would normally render to speakers
                Some(&tap_asbd),
            )
            .map_err(map_ca_error)?;

        // 9. Initialize AudioUnit
        audio_unit.initialize().map_err(map_ca_error)?;

        Ok(Self {
            audio_unit,
            process_tap,
            is_started: Arc::new(AtomicBool::new(false)),
            native_tap_asbd: Arc::new(Mutex::new(Some(tap_asbd))),
            data_queue: Arc::new(Mutex::new(VecDeque::with_capacity(10))), // Default capacity
            stream_start_time: Instant::now(), // Record stream start time as epoch
        })
    }
}

impl CapturingStream for MacosApplicationAudioStream {
    /// Starts the audio capture stream from the application tap.
    ///
    /// This method performs the following key operations:
    /// 1. Checks if the stream is already started.
    /// 2. Sets up an input callback on the `AudioUnit`. This callback is associated with the
    ///    output of the `AudioUnit`'s input bus (`Element::INPUT_BUS`, `Scope::Output`),
    ///    which is where data from the `CoreAudioProcessTap` arrives.
    /// 3. Inside the callback:
    ///    a. It checks if the stream is marked as started.
    ///    b. It calls `AudioUnitRender` on the input bus (`Element::INPUT_BUS`) to pull
    ///       audio data from the tap into a temporary `AudioBufferList`.
    ///    c. It retrieves the native `AudioStreamBasicDescription` (ASBD) of the tap.
    ///    d. It converts the raw audio data from the `AudioBufferList` (which is in the
    ///       tap's native format) into an interleaved `Vec<f32>`. This primarily handles
    ///       non-interleaved float data from the tap, converting it to interleaved.
    ///    e. It creates an `AudioBuffer` struct with the converted data, an `AudioFormat`
    ///       reflecting F32LE (with channel count and sample rate from the tap's ASBD),
    ///       and a current `Instant::now()` timestamp.
    ///    f. This `AudioResult<AudioBuffer>` is then pushed to an internal queue.
    /// 4. Starts the `AudioUnit` to begin invoking the callback.
    /// 5. Marks the stream as started.
    ///
    /// # Returns
    /// `Ok(())` on success, or an `AudioError` if starting fails (e.g., callback setup error, AU start error).
    fn start(&mut self) -> AudioResult<()> {
        if self.is_started.load(Ordering::SeqCst) {
            return Ok(());
        }

        let data_queue_clone = self.data_queue.clone();
        let native_tap_asbd_clone = self.native_tap_asbd.clone();
        let is_started_clone = self.is_started.clone();
        let stream_start_time_clone = self.stream_start_time; // Clone for the closure
        // AudioUnit is not Clone, will be accessed via RenderArgs.audio_unit_ref

        self.audio_unit.set_input_callback(
            Element::INPUT_BUS, // Callback for the input bus
            Scope::Output,      // Specifically, for data being output by the input bus (from the tap)
            move |mut args: RenderArgs| -> Result<(), OSStatus> {
                if !is_started_clone.load(Ordering::Relaxed) {
                    return Ok(()); // Stream stopped
                }

                let au_instance = args.audio_unit_ref.instance(); // Get the AudioUnit instance
                let num_frames = args.num_frames;
                let timestamp = args.timestamp; // *const sys::AudioTimeStamp

                let locked_asbd_opt = native_tap_asbd_clone.lock().unwrap();
                let tap_asbd = match locked_asbd_opt.as_ref() {
                    Some(val) => val,
                    None => {
                        eprintln!("MacosApplicationAudioStream Error: Native tap ASBD not available in callback.");
                        // Push error to queue? For now, return error to CoreAudio.
                        return Err(sys::kAudio_ParamError as OSStatus);
                    }
                };

                // Allocate AudioBufferList for capturing data from the tap via Element 1
                let is_tap_data_non_interleaved = (tap_asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved) != 0;
                
                let captured_abl_boxed_result = CAAudioBufferList::allocate(
                    tap_asbd.mChannelsPerFrame,
                    num_frames,
                    !is_tap_data_non_interleaved, // `allocate` wants `is_interleaved`
                    true, // allocate mData pointers
                );

                let mut captured_abl_boxed = match captured_abl_boxed_result.map_err(map_ca_error) {
                    Ok(abl) => abl,
                    Err(audio_err) => {
                        eprintln!("Callback Error: Failed to allocate AudioBufferList for tap capture: {:?}", audio_err);
                        let mut queue = data_queue_clone.lock().unwrap();
                        if queue.len() == queue.capacity() { queue.pop_front(); }
                        queue.push_back(Err(audio_err));
                        return Ok(()); // Error pushed, callback returns OK
                    }
                };
                let captured_abl_ptr: *mut sys::AudioBufferList = &mut *captured_abl_boxed;
                let mut render_action_flags: AudioUnitRenderActionFlags = 0;

                // Call AudioUnitRender on the INPUT BUS (Element 1) to get captured data from the tap.
                let os_status = unsafe {
                    sys::AudioUnitRender(
                        au_instance,
                        &mut render_action_flags,
                        timestamp,
                        Element::INPUT_BUS, // Render data from the input bus
                        num_frames,
                        captured_abl_ptr,
                    )
                };

                if os_status == sys::noErr {
                    // Ensure tap data is 32-bit float as expected by current conversion logic
                    if (tap_asbd.mFormatFlags & kAudioFormatFlagIsFloat) == 0 || tap_asbd.mBitsPerChannel != 32 {
                        let err_msg = format!(
                            "Tap data is not 32-bit float. Flags: {}, Bits: {}",
                            tap_asbd.mFormatFlags, tap_asbd.mBitsPerChannel
                        );
                        eprintln!("Callback Error: {}", err_msg);
                        let mut queue = data_queue_clone.lock().unwrap();
                        if queue.len() == queue.capacity() { queue.pop_front(); }
                        queue.push_back(Err(AudioError::FormatNotSupported(err_msg)));
                        return Ok(());
                    }

                    let num_channels = tap_asbd.mChannelsPerFrame as usize;
                    let num_frames_usize = num_frames as usize;
                    let mut interleaved_f32: Vec<f32> = vec![0.0f32; num_frames_usize * num_channels];

                    let buffers_slice = unsafe {
                        std::slice::from_raw_parts(
                            (*captured_abl_ptr).mBuffers.as_ptr(),
                            (*captured_abl_ptr).mNumberBuffers as usize,
                        )
                    };

                    if !is_tap_data_non_interleaved { // Data is interleaved
                        if !buffers_slice.is_empty() {
                            let source_buffer = &buffers_slice[0];
                            let samples_in_buffer = source_buffer.mDataByteSize as usize / std::mem::size_of::<f32>();
                            if samples_in_buffer == interleaved_f32.len() {
                                let source_slice = unsafe {
                                    std::slice::from_raw_parts(source_buffer.mData as *const f32, samples_in_buffer)
                                };
                                interleaved_f32.copy_from_slice(source_slice);
                            } else {
                                eprintln!("Callback Error: Interleaved tap buffer size mismatch. Expected {}, got {}", interleaved_f32.len(), samples_in_buffer);
                                // Push error or fill with silence
                            }
                        }
                    } else { // Data is non-interleaved
                        if buffers_slice.len() == num_channels {
                            for frame_idx in 0..num_frames_usize {
                                for ch_idx in 0..num_channels {
                                    let source_buffer = &buffers_slice[ch_idx];
                                    if !source_buffer.mData.is_null() && source_buffer.mDataByteSize >= ((frame_idx + 1) * std::mem::size_of::<f32>()) as u32 {
                                        let sample_ptr = source_buffer.mData as *const f32;
                                        interleaved_f32[frame_idx * num_channels + ch_idx] =
                                            unsafe { *sample_ptr.add(frame_idx) };
                                    } else {
                                        eprintln!("Callback Error: Non-interleaved tap buffer access issue at frame {}, channel {}.", frame_idx, ch_idx);
                                        // Fill with silence or push error
                                        interleaved_f32[frame_idx * num_channels + ch_idx] = 0.0;
                                    }
                                }
                            }
                        } else {
                             eprintln!("Callback Error: Non-interleaved tap buffer count mismatch. Expected {}, got {}.", num_channels, buffers_slice.len());
                             // Push error or fill with silence
                        }
                    }
                    
                    let output_audio_format = AudioFormat {
                        sample_rate: tap_asbd.mSampleRate as u32,
                        channels: tap_asbd.mChannelsPerFrame as u16,
                        bits_per_sample: 32, // We converted to f32
                        sample_format: SampleFormat::F32LE,
                    };
                    let audio_buffer_struct = AudioBuffer {
                        data: interleaved_f32,
                        channels: output_audio_format.channels,
                        sample_rate: output_audio_format.sample_rate,
                        format: output_audio_format,
                        timestamp: Instant::now().duration_since(stream_start_time_clone), // Timestamp relative to stream start
                    };
                    
                    let mut queue = data_queue_clone.lock().unwrap();
                    if queue.len() == queue.capacity() { queue.pop_front(); }
                    queue.push_back(Ok(audio_buffer_struct)); // Changed to AudioBuffer struct

                } else {
                    // Handle AudioUnitRender error
                    eprintln!("AudioUnitRender error in tap callback: OSStatus {}", os_status);
                    let audio_error = match os_status as u32 {
                        // Consider specific OSStatus codes that might indicate the tap is gone
                        // or the target process quit. For example:
                        // sys::kAudioUnitErr_NoConnection might be relevant.
                        // sys::kAudioUnitErr_CannotDoInCurrentContext could also occur.
                        // For now, map common errors or use a general one.
                        // If the tap is invalidated because the process quit, AudioUnitRender might return
                        // an error like kAudioUnitErr_NoConnection or kAudioUnitErr_InvalidElement if the
                        // tap AudioObjectID becomes invalid.
                        // Let's use map_ca_error to be consistent, wrapping the OSStatus.
                        // If os_status is kAudioUnitErr_NoConnection or similar, map_ca_error might
                        // eventually map it to DeviceDisconnected if we enhance it further.
                        // For now, it will be BackendSpecificError.
                        // The prompt asks to consider AudioError::DeviceDisconnected.
                        // Let's make a specific check here for a known error if possible,
                        // otherwise use map_ca_error.
                        // A common error if the underlying device/tap is gone is kAudioHardwareIllegalOperationError (-50)
                        // or kAudioUnitErr_UnspecifiedError (-10875) or kAudioUnitErr_InvalidElement (-10877)
                        // or kAudioUnitErr_NoConnection (-10872)
                        sys::kAudioUnitErr_NoConnection | sys::kAudioUnitErr_InvalidElement | sys::kAudioHardwareIllegalOperationError => {
                            AudioError::DeviceDisconnected(format!(
                                "AudioUnitRender failed, possibly due to target process exit or tap invalidation (OSStatus: {})",
                                os_status
                            ))
                        }
                        _ => map_ca_error(CAError(os_status)), // Wrap OSStatus in CAError for map_ca_error
                    };

                    let mut queue = data_queue_clone.lock().unwrap();
                    if queue.len() == queue.capacity() {
                        queue.pop_front(); // Make space
                    }
                    queue.push_back(Err(audio_error));

                    // As per prompt, just enqueuing the error is sufficient for now.
                    // If we wanted to stop the stream:
                    // is_started_clone.store(false, Ordering::SeqCst);
                }
                Ok(())
            }
        ).map_err(map_ca_error)?;

        self.audio_unit.start().map_err(map_ca_error)?;
        self.is_started.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&mut self) -> AudioResult<()> {
        if !self.is_started.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.audio_unit.stop().map_err(map_ca_error)?;
        self.is_started.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.is_started.load(Ordering::SeqCst)
    }

    fn get_format(&self) -> AudioResult<AudioFormat> {
        let locked_asbd = self.native_tap_asbd.lock().unwrap();
        if let Some(asbd) = locked_asbd.as_ref() {
            Ok(AudioFormat {
                sample_rate: asbd.mSampleRate as u32,
                channels: asbd.mChannelsPerFrame as u16,
                bits_per_sample: 32, // Output is f32
                sample_format: SampleFormat::F32LE,
            })
        } else {
            Err(AudioError::NotInitialized(
                "Tap stream not started or native ASBD not available".into(),
            ))
        }
    }

    /// Reads a chunk of captured audio data from the application tap stream synchronously.
    ///
    /// This method attempts to retrieve one audio buffer from an internal queue
    /// (`self.data_queue`) which is populated by the CoreAudio input callback connected
    /// to the application's process tap.
    ///
    /// # Behavior
    /// - If the stream is not running (i.e., `start()` has not been called or `stop()` has been called),
    ///   it returns `Err(AudioError::InvalidOperation("Stream not started or not in streaming state".to_string()))`.
    /// - If a buffer is available in the queue, it pops the `AudioResult<AudioBuffer>`.
    ///   - If the popped item is `Ok(buffer)`, it returns `Ok(Some(buffer))`.
    ///   - If the popped item is `Err(error_from_callback)`, this method propagates that error
    ///     by returning `Err(error_from_callback)`. This allows errors occurring during
    ///     audio capture or processing within the callback (e.g., `AudioUnitRender` failure,
    ///     format conversion issues) to be communicated to the caller of `read_chunk`.
    /// - If the queue is empty (meaning no new audio data has been processed by the callback
    ///   since the last call, or the callback is not producing data fast enough), it returns
    ///   `Ok(None)`. This signifies a non-blocking read attempt where no data is immediately available.
    /// - If the internal data queue's mutex is poisoned (which is unlikely but possible if a
    ///   thread panics while holding the lock), it returns `Err(AudioError::MutexLockError)`.
    ///
    /// # Timeout
    /// The `_timeout_ms` parameter is currently **ignored**. This method always performs a
    /// non-blocking check of the queue. Future implementations might utilize this parameter
    /// to enable blocking reads with a specified timeout.
    ///
    /// # Returns
    /// - `Ok(Some(AudioBuffer))`: A buffer containing the captured audio data (the struct).
    /// - `Ok(None)`: No data is currently available in the queue (non-blocking behavior).
    /// - `Err(AudioError)`: An error occurred. This could be due to the stream not running,
    ///   an error propagated from the audio callback (e.g., capture or processing error),
    ///   or a mutex lock failure.
    fn read_chunk(&mut self, _timeout_ms: Option<u32>) -> AudioResult<Option<AudioBuffer>> { // Changed return type
        if !self.is_running() {
            return Err(AudioError::InvalidOperation("Stream not started or not in streaming state".to_string()));
        }

        // TODO: Implement timeout logic if _timeout_ms is Some.
        // For now, this is a non-blocking pop.
        match self.data_queue.lock().map_err(|_| AudioError::MutexLockError("data_queue".to_string()))?.pop_front() {
            Some(audio_result) => audio_result.map(Some), // Propagates Ok(buffer) or Err(error_from_callback)
            None => Ok(None), // Queue is empty, no data available
        }
    }

    /// Converts the synchronous `CapturingStream` into an asynchronous `Stream`
    /// for application-level audio capture.
    ///
    /// This method facilitates asynchronous consumption of audio data captured from a specific
    /// macOS application via a `CoreAudioProcessTap`. It operates by:
    /// 1. Checking if the underlying `MacosApplicationAudioStream` is currently running. If not,
    ///    it returns an `AudioError::InvalidOperation`.
    /// 2. Creating an unbounded MPSC (multi-producer, single-consumer) channel using `futures_channel::mpsc`.
    ///    The sender (`tx`) part of this channel will be used by a helper thread, and the
    ///    receiver (`rx`) part will form the basis of the returned asynchronous stream.
    /// 3. Spawning a new `std::thread`. This thread is responsible for continuously polling
    ///    the internal `data_queue` of the `MacosApplicationAudioStream`. This queue is
    ///    populated by the CoreAudio callback with `AudioResult<AudioBuffer>` items.
    /// 4. Inside the helper thread's loop:
    ///    a. It attempts to pop an item from `data_queue` (after acquiring a lock).
    ///    b. If an `AudioResult` (containing either an audio buffer or an error) is popped:
    ///       - The lock on `data_queue` is dropped.
    ///       - The `AudioResult` is sent via the MPSC sender (`tx`).
    ///       - If `tx.unbounded_send()` fails (e.g., because the `rx` stream has been dropped
    ///         by the consumer), it indicates that the consumer is no longer interested in
    ///         the data. The helper thread then breaks its loop and terminates.
    ///    c. If `data_queue` is empty:
    ///       - The lock on `data_queue` is dropped.
    ///       - The thread checks the `is_started` status of the main `MacosApplicationAudioStream`.
    ///       - If `is_started` is `false` (meaning the main stream has been stopped) AND the
    ///         queue is empty, it implies that all available data has been processed and
    ///         no new data will arrive. The helper thread breaks its loop and terminates.
    ///       - If `is_started` is `true` but the queue is empty (a temporary underrun),
    ///         the thread sleeps for a short duration (e.g., 10 milliseconds) to avoid
    ///         busy-waiting, then continues its loop to check the queue again.
    /// 5. The method returns `Ok(Box::pin(rx))`, where `rx` is the MPSC receiver, now wrapped
    ///    as a `Pin<Box<dyn futures_core::Stream<...>>>`. This stream can be consumed
    ///    asynchronously by awaiting its items.
    ///
    /// # Error Handling
    /// - If the stream is not running when `to_async_stream` is called, `Err(AudioError::InvalidOperation)` is returned.
    /// - Errors encountered during audio capture within the CoreAudio callback (e.g., `AudioUnitRender` failures,
    ///   format conversion issues) are wrapped in `AudioResult::Err` and pushed onto the `data_queue`.
    ///   The helper thread forwards these `Err` variants through the MPSC channel, allowing the
    ///   asynchronous consumer to handle them.
    ///
    /// # Thread Safety and Lifetimes
    /// - `Arc` and `Mutex` are used for `data_queue` and `is_started` to ensure safe sharing
    ///   between the CoreAudio callback thread, the `MacosApplicationAudioStream` methods,
    ///   and the new helper thread spawned by `to_async_stream`.
    /// - The lifetime `'a` ensures that the returned stream does not outlive the `MacosApplicationAudioStream` instance.
    fn to_async_stream<'a>(
        &'a mut self,
    ) -> AudioResult<
        Pin<Box<dyn FuturesStream<Item = AudioResult<AudioBuffer>> + Send + Sync + 'a>>, // Changed to AudioBuffer struct
    > {
        if !self.is_running() {
            return Err(AudioError::InvalidOperation(
                "Stream not started or not in streaming state for async conversion".to_string(),
            ));
        }

        let (tx, rx) = mpsc::unbounded::<AudioResult<AudioBuffer>>(); // Changed to AudioBuffer struct

        let data_queue_clone = self.data_queue.clone();
        let is_started_clone = self.is_started.clone();

        std::thread::spawn(move || {
            loop {
                let mut queue_guard = data_queue_clone.lock().unwrap();
                match queue_guard.pop_front() {
                    Some(audio_result) => {
                        // Drop the lock before sending to avoid holding it if send blocks or errors.
                        drop(queue_guard);
                        if tx.unbounded_send(audio_result).is_err() {
                            // Receiver was dropped, consumer is gone.
                            eprintln!("Async stream receiver dropped for MacosApplicationAudioStream, helper thread terminating.");
                            break;
                        }
                    }
                    None => {
                        // Queue is empty, drop lock before potentially sleeping.
                        drop(queue_guard);
                        if !is_started_clone.load(Ordering::SeqCst) {
                            // Stream stopped and queue is empty, so we are done.
                            eprintln!("Main MacosApplicationAudioStream stopped and queue empty, async helper thread terminating.");
                            break;
                        } else {
                            // Stream is running but queue is empty, wait a bit.
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                    }
                }
            }
            // tx is dropped here when thread exits, closing the stream.
            eprintln!("MacosApplicationAudioStream async audio streaming helper thread finished.");
        });

        Ok(Box::pin(rx))
    }

    fn close(&mut self) -> AudioResult<()> {
        self.stop()?;
        // AudioUnit resources (uninitialize, dispose) are managed by its Drop implementation.
        // If explicit error handling for uninitialize/dispose is needed,
        // self.audio_unit would need to be Option<AudioUnit> to take() and dispose,
        // or MacosApplicationAudioStream would need its own Drop impl.
        Ok(())
    }
}
/// Device enumerator for macOS using CoreAudio.
///
/// This enumerator is responsible for listing available audio devices
/// and providing access to the default system output device for loopback capture.
pub(crate) struct MacosDeviceEnumerator;

impl MacosDeviceEnumerator {
}

impl DeviceEnumerator for MacosDeviceEnumerator {
    /// Gets the default audio device for the specified kind.
    ///
    /// For `DeviceKind::Input` (system audio capture), this attempts to get the
    /// default *output* device, as that's the target for loopback.
    /// For `DeviceKind::Output`, this currently returns `Ok(None)`.
    fn get_default_device(&self, kind: DeviceKind) -> AudioResult<Option<Box<dyn AudioDevice>>> {
        match kind {
            DeviceKind::Input => {
                // For system capture, we target the default output device for loopback.
                match CAAudioDevice::default_output_device() {
                    Ok(device_id) => {
                        let macos_audio_device = MacosAudioDevice { device_id };
                        Ok(Some(Box::new(macos_audio_device)))
                    }
                    Err(err) => Err(map_ca_error(err)),
                }
            }
            DeviceKind::Output => Ok(None), // Not implemented for output selection yet.
        }
    }

    /// Enumerates available audio devices.
    ///
    /// Currently, this only returns the default output device (if available)
    /// as a stand-in for full enumeration.
    /// TODO: Implement full enumeration of all output devices suitable for loopback capture.
    fn enumerate_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice>>> {
        // TODO: Implement full enumeration of all output devices suitable for loopback capture.
        match self.get_default_device(DeviceKind::Input)? {
            Some(device) => Ok(vec![device]),
            None => Ok(vec![]),
        }
    }

    /// Gets a specific audio device by its ID.
    ///
    /// Currently, this only checks if the provided ID matches the default output device's ID.
    /// TODO: Implement lookup for arbitrary device IDs.
    fn get_device_by_id(
        &self,
        id_str: &DeviceId,
        _kind: Option<DeviceKind>,
    ) -> AudioResult<Option<Box<dyn AudioDevice>>> {
        // TODO: Implement lookup for arbitrary device IDs.
        let target_id = match id_str.parse::<u32>() {
            Ok(id) => id,
            Err(_) => return Ok(None), // Invalid ID format
        };

        if let Some(default_dev_boxed) = self.get_default_device(DeviceKind::Input)? {
            if let Ok(default_id_u32) = default_dev_boxed.get_id().parse::<u32>() {
                if default_id_u32 == target_id {
                    return Ok(Some(default_dev_boxed));
                }
            }
        }
        Ok(None)
    }

    /// Gets a list of available input audio devices.
    ///
    /// This currently calls `enumerate_devices` which, for now, only returns the default output device.
    fn get_input_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice>>> {
        self.enumerate_devices() // For loopback, the "input" is the system's output.
    }

    /// Gets a list of available output audio devices.
    ///
    /// This currently returns an empty vector.
    fn get_output_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice>>> {
        Ok(vec![]) // Not focused on output device enumeration for capture.
    }
}
