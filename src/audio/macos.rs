use crate::audio::core::{
    AudioDevice, AudioError, AudioResult, DeviceEnumerator, DeviceId, DeviceKind,
};
use coreaudio_rs::audio_unit::audio_device::{AudioDevice as CAAudioDevice, AudioDeviceID};
use coreaudio_rs::Error as CAError;

/// A representation of a CoreAudio audio device.
///
/// This struct holds the `AudioDeviceID` and potentially other information
/// like the device name or UID if fetched.
pub(crate) struct MacosAudioDevice {
    device_id: AudioDeviceID,
    // TODO: Potentially store name/UID if fetched during enumeration or lookup.
}

impl AudioDevice for MacosAudioDevice {
    fn get_id(&self) -> DeviceId {
        self.device_id.to_string()
    }

    fn get_name(&self) -> AudioResult<String> {
        todo!("Implement get_name for MacosAudioDevice")
    }

    fn get_description(&self) -> AudioResult<String> {
        todo!("Implement get_description for MacosAudioDevice")
    }

    fn get_kind(&self) -> AudioResult<DeviceKind> {
        todo!("Implement get_kind for MacosAudioDevice")
    }

    fn is_default(&self, kind: DeviceKind) -> AudioResult<bool> {
        todo!("Implement is_default for MacosAudioDevice")
    }

    fn get_supported_formats(&self) -> AudioResult<Vec<crate::audio::core::AudioFormat>> {
        todo!("Implement get_supported_formats for MacosAudioDevice")
    }

    fn get_default_format(&self) -> AudioResult<crate::audio::core::AudioFormat> {
        todo!("Implement get_default_format for MacosAudioDevice")
    }
}

/// Device enumerator for macOS using CoreAudio.
///
/// This enumerator is responsible for listing available audio devices
/// and providing access to the default system output device for loopback capture.
pub(crate) struct MacosDeviceEnumerator;

impl MacosDeviceEnumerator {
    fn map_ca_error(err: CAError) -> AudioError {
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
