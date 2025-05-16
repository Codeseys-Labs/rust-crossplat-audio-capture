use crate::audio::core::{
    AudioCaptureConfig, AudioDevice, AudioError, AudioFormat, AudioResult, CapturingStream,
    DeviceEnumerator, DeviceId, DeviceKind, SampleFormat,
};
use coreaudio_rs::audio_object::{
    AudioObject, AudioObjectPropertyAddress, AudioObjectPropertyElement, AudioObjectPropertyScope,
};
use coreaudio_rs::audio_unit::audio_device::AudioDeviceID;
use coreaudio_rs::audio_unit::audio_unit_element;
use coreaudio_rs::audio_unit::{
    AudioComponent, AudioComponentDescription, AudioUnit, Scope, StreamFormat,
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
    AudioStreamBasicDescription,
};
use coreaudio_rs::Error as CAError;

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
#[derive(Debug)] // AudioUnit might not be Debug. This is a placeholder.
                 // Consider a custom Debug impl or removing it if AudioUnit is not Debug.
pub(crate) struct MacosAudioStream {
    audio_unit: AudioUnit,
    // TODO: Add other necessary fields: is_started, format, buffer, callback_data etc.
}

impl MacosAudioStream {
    fn new(audio_unit: AudioUnit) -> Self {
        Self { audio_unit }
    }
}

impl CapturingStream for MacosAudioStream {
    /// Starts the audio stream.
    fn start(&mut self) -> AudioResult<()> {
        // TODO: Implement stream start (e.g., AudioOutputUnitStart)
        // self.audio_unit.start().map_err(MacosDeviceEnumerator::map_ca_error)?;
        todo!("Implement start for MacosAudioStream")
    }

    /// Stops the audio stream.
    fn stop(&mut self) -> AudioResult<()> {
        // TODO: Implement stream stop (e.g., AudioOutputUnitStop)
        // self.audio_unit.stop().map_err(MacosDeviceEnumerator::map_ca_error)?;
        todo!("Implement stop for MacosAudioStream")
    }

    /// Reads a chunk of audio data from the stream.
    fn read_chunk(&mut self, _timeout_ms: u32) -> AudioResult<Option<Vec<u8>>> {
        // TODO: Implement audio data reading using the render callback mechanism.
        todo!("Implement read_chunk for MacosAudioStream")
    }

    /// Gets the format of the audio stream.
    fn format(&self) -> AudioResult<AudioFormat> {
        // TODO: Store and return the format configured during create_stream.
        todo!("Implement format for MacosAudioStream")
    }

    /// Checks if the stream is currently active.
    fn is_active(&self) -> AudioResult<bool> {
        // TODO: Implement is_active, potentially by checking AudioUnit state.
        todo!("Implement is_active for MacosAudioStream")
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

        // 8. Set stream format on AU's input scope, input bus (for the captured audio)
        // Note: Some examples set this on the OUTPUT scope of the INPUT bus.
        // For AUHAL capture, it's typically the input scope of the input bus, or output scope of input bus.
        // Let's try input scope of input bus first.
        audio_unit
            .set_property(
                kAudioUnitProperty_StreamFormat,
                Scope::Output, // Output scope of the input bus for capture format
                audio_unit_element::INPUT_BUS,
                Some(&asbd),
            )
            .map_err(MacosDeviceEnumerator::map_ca_error)?;

        // Also set the client format on the input scope of the output bus if needed,
        // but for capture, the above should define what we get from the input bus.
        // Let's also set it on the input scope of the output bus as per some examples for client format.
        // This defines the format the AudioUnit expects from its input side (which is our capture source).
        audio_unit
            .set_property(
                kAudioUnitProperty_StreamFormat,
                Scope::Input, // Input scope of the output bus for client data format
                audio_unit_element::OUTPUT_BUS, // This is where the AUHAL *outputs* captured data
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
