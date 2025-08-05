//! Linux-specific audio capture backend using PipeWire.
//!
//! NOTE: This is currently a stub implementation to allow compilation.
//! Full PipeWire integration is being worked on.

use crate::api::AudioCaptureConfig;
use crate::core::error::{AudioError, Result as AudioResult};
use crate::core::interface::{
    AudioDevice, CapturingStream, DeviceEnumerator, DeviceKind,
};
use crate::AudioFormat;

/// Represents the detected status of PipeWire on the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipewireStatus {
    /// PipeWire is active, and `pipewire-pulse` (or equivalent) is managing PulseAudio clients.
    ActiveAndPrimary,
    /// Only the core PipeWire daemon is detected; `pipewire-pulse` might not be active.
    OnlyPipeWireCore,
    /// PipeWire does not appear to be available or active.
    NotAvailable,
}

/// Checks the availability and status of PipeWire on the system.
pub fn check_pipewire_availability() -> PipewireStatus {
    // Stub implementation - always return not available for now
    PipewireStatus::NotAvailable
}

/// Stub Linux audio device ID
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LinuxDeviceId(String);

/// Stub Linux audio device
#[derive(Debug, Clone)]
pub struct LinuxAudioDevice {
    id: LinuxDeviceId,
    name: String,
    is_input: bool,
}

impl AudioDevice for LinuxAudioDevice {
    type DeviceId = LinuxDeviceId;

    fn get_id(&self) -> Self::DeviceId {
        self.id.clone()
    }

    fn get_name(&self) -> String {
        self.name.clone()
    }

    fn get_supported_formats(&self) -> AudioResult<Vec<AudioFormat>> {
        // Stub implementation
        Ok(vec![])
    }

    fn get_default_format(&self) -> AudioResult<AudioFormat> {
        Err(AudioError::BackendError(
            "Linux audio device not yet implemented".to_string(),
        ))
    }

    fn is_input(&self) -> bool {
        self.is_input
    }

    fn is_output(&self) -> bool {
        !self.is_input
    }

    fn is_active(&self) -> bool {
        false
    }

    fn is_format_supported(&self, _format: &AudioFormat) -> AudioResult<bool> {
        Ok(false)
    }

    fn create_stream(
        &mut self,
        _config: &AudioCaptureConfig,
    ) -> AudioResult<Box<dyn CapturingStream + 'static>> {
        Err(AudioError::BackendError(
            "Linux audio stream creation not yet implemented".to_string(),
        ))
    }
}

/// Linux device enumerator (stub implementation)
pub struct LinuxDeviceEnumerator;

impl LinuxDeviceEnumerator {
    pub fn new() -> AudioResult<Self> {
        Ok(LinuxDeviceEnumerator)
    }
}

impl DeviceEnumerator for LinuxDeviceEnumerator {
    type Device = LinuxAudioDevice;

    fn enumerate_devices(&self) -> AudioResult<Vec<Self::Device>> {
        // Stub implementation
        Ok(vec![])
    }

    fn get_default_device(&self, _kind: DeviceKind) -> AudioResult<Self::Device> {
        Err(AudioError::BackendError(
            "No default device available".to_string(),
        ))
    }

    fn get_input_devices(&self) -> AudioResult<Vec<Self::Device>> {
        Ok(vec![])
    }

    fn get_output_devices(&self) -> AudioResult<Vec<Self::Device>> {
        Ok(vec![])
    }

    fn get_device_by_id(&self, _id: &LinuxDeviceId) -> AudioResult<Self::Device> {
        Err(AudioError::BackendError("Device not found".to_string()))
    }
}

/// PipeWire backend (stub implementation)
pub struct PipeWireBackend;

impl PipeWireBackend {
    pub fn new() -> AudioResult<Self> {
        Ok(PipeWireBackend)
    }
}

// AudioBackend trait doesn't exist in core::interface, so no implementation needed

/// Stub implementation for Linux application info
#[derive(Debug, Clone)]
pub struct LinuxApplicationInfo {
    pub process_id: Option<u32>,
    pub name: Option<String>,
    pub executable_path: Option<String>,
    pub pipewire_node_id: Option<u32>,
    pub stream_description: Option<String>,
    pub pulseaudio_sink_input_index: Option<u32>,
}

/// Stub function for enumerating audio applications
pub fn enumerate_audio_applications_pipewire() -> AudioResult<Vec<LinuxApplicationInfo>> {
    Ok(vec![])
}

/// Stub PipeWire core context
pub struct PipewireCoreContext;

impl PipewireCoreContext {
    pub fn new() -> AudioResult<Self> {
        Err(AudioError::BackendError(
            "PipeWire core context not yet implemented".to_string(),
        ))
    }
}
