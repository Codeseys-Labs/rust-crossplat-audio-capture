use crate::audio::core::{
    AudioCaptureConfig, AudioDevice, AudioError, AudioFormat, AudioResult, CapturingStream,
    DeviceEnumerator, DeviceId, DeviceKind, SampleFormat, AudioBuffer,
};
use crate::core::buffer::VecAudioBuffer; // For creating AudioBuffer instances
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
    })
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
    /// an `AudioResult` wrapping a boxed `AudioBuffer`.
    data_queue: Arc<Mutex<VecDeque<AudioResult<Box<dyn AudioBuffer<Sample = f32>>>>>>,
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
            .map_err(MacosDeviceEnumerator::map_ca_error)?;
        *self.current_asbd.lock().unwrap() = Some(asbd);

        // 2. Clone Arcs for the callback closure.
        let data_queue_clone = self.data_queue.clone();
        let current_asbd_clone = self.current_asbd.clone();
        let is_started_clone = self.is_started.clone();
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
                let mut captured_abl_boxed = CAAudioBufferList::allocate(
                    input_asbd.mChannelsPerFrame, // Number of channels in the ASBD
                    num_frames,                   // Number of frames to render
                    is_input_interleaved,         // Whether the format is interleaved
                    true,                         // True to allocate mData pointers
                )
                .map_err(|e| {
                    eprintln!("Failed to allocate AudioBufferList: {:?}", e);
                    sys::kAudio_MemFullError as OSStatus // Or another appropriate error
                })?;
                
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
                    let audio_buffer = VecAudioBuffer::new(interleaved_f32, target_format);
                    
                    let mut queue = data_queue_clone.lock().unwrap();
                    if queue.len() == queue.capacity() {
                        queue.pop_front(); // Make space if full (simple strategy)
                    }
                    queue.push_back(Ok(Box::new(audio_buffer)));

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
            .map_err(MacosDeviceEnumerator::map_ca_error)?;

        // 4. Start the AudioUnit
        self.audio_unit
            .start()
            .map_err(MacosDeviceEnumerator::map_ca_error)?;
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
            .map_err(MacosDeviceEnumerator::map_ca_error)?;
        self.is_started.store(false, Ordering::SeqCst);
        // Optionally, clear the callback? Or clear the queue?
        // For now, just stop. The callback checks `is_started`.
        Ok(())
    }

    /// Reads a chunk of audio data from the stream.
    /// This is a placeholder and needs to be implemented to pull from `data_queue`.
    fn read_chunk(&mut self, _timeout_ms: u32) -> AudioResult<Option<Box<dyn AudioBuffer<Sample = f32>>>> {
        // TODO: Implement audio data reading from data_queue.
        // This should pop from self.data_queue, handle timeout, etc.
        // The return type in the trait is Option<Vec<u8>>, but task asks for Box<dyn AudioBuffer>
        // For now, let's align with the task's implied type for data_queue.
        // The trait might need an update or this method needs to convert.
        // For now, returning what would come from the queue.
        if let Some(audio_result) = self.data_queue.lock().unwrap().pop_front() {
            audio_result.map(Some)
        } else {
            Ok(None)
        }
        // todo!("Implement read_chunk for MacosAudioStream by reading from data_queue")
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
        AudioObject::name(&self.device_id).map_err(MacosDeviceEnumerator::map_ca_error)
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
        AudioObject::is_alive(&self.device_id).map_err(MacosDeviceEnumerator::map_ca_error)
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
            .map_err(MacosDeviceEnumerator::map_ca_error)?;
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
            .map_err(MacosDeviceEnumerator::map_ca_error)?;

        // 4. Set current device on AUHAL
        audio_unit
            .set_property(
                kAudioOutputUnitProperty_CurrentDevice,
                Scope::Global,
                audio_unit_element::OUTPUT_BUS, // Global scope usually uses output bus for device selection
                Some(&self.device_id),
            )
            .map_err(MacosDeviceEnumerator::map_ca_error)?;

        // 5. Enable IO for input (capture) on the output unit's input bus
        let enable_io: u32 = 1;
        audio_unit
            .set_property(
                kAudioOutputUnitProperty_EnableIO,
                Scope::Input,                  // Scope for enabling input
                audio_unit_element::INPUT_BUS, // Element is the input bus (capture side)
                Some(&enable_io),
            )
            .map_err(MacosDeviceEnumerator::map_ca_error)?;

        // 6. Disable IO for output (to prevent sound passthrough from this AU instance)
        let disable_io: u32 = 0;
        audio_unit
            .set_property(
                kAudioOutputUnitProperty_EnableIO,
                Scope::Output,                  // Scope for enabling output
                audio_unit_element::OUTPUT_BUS, // Element is the output bus (playback side)
                Some(&disable_io),
            )
            .map_err(MacosDeviceEnumerator::map_ca_error)?;

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
            .map_err(MacosDeviceEnumerator::map_ca_error)?;

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
            .map_err(MacosDeviceEnumerator::map_ca_error)?;

        // 9. Initialize AudioUnit
        audio_unit
            .initialize()
            .map_err(MacosDeviceEnumerator::map_ca_error)?;

        // 10. Define MacosAudioStream struct skeleton (done above)
        // 11. Return Ok(Box::new(MacosAudioStream::new(audio_unit)))
        Ok(Box::new(MacosAudioStream::new(audio_unit)))
    }
}

/// Device enumerator for macOS using CoreAudio.
///
/// This enumerator is responsible for listing available audio devices
/// and providing access to the default system output device for loopback capture.
pub(crate) struct MacosDeviceEnumerator;

impl MacosDeviceEnumerator {
    // Renamed from map_ca_error to avoid conflict if we make it public,
    // though it's fine as a private static method.
    // Keeping it as is since it's used by MacosAudioDevice impl.
    pub(crate) fn map_ca_error(err: CAError) -> AudioError {
        AudioError::BackendSpecificError(format!("CoreAudio error: {}", err))
    }
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
                    Err(err) => Err(Self::map_ca_error(err)),
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
