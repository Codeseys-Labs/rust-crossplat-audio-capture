//! Linux audio implementation using PipeWire

pub mod pipewire;

// Re-export for convenience
pub use pipewire::{PipeWireApplicationCapture, ApplicationSelector};

// Stub implementation for LinuxDeviceEnumerator to fix compilation
use crate::{AudioApplication, AudioCaptureStream, Result};
use crate::core::config::StreamConfig;

pub struct LinuxDeviceEnumerator;

impl LinuxDeviceEnumerator {
    pub fn new() -> Self {
        LinuxDeviceEnumerator
    }

    pub fn list_applications(&self) -> Result<Vec<AudioApplication>> {
        Ok(vec![])
    }

    pub fn capture_application(
        &self,
        _app: &AudioApplication,
        _config: StreamConfig,
    ) -> Result<Box<dyn AudioCaptureStream>> {
        Err(crate::core::error::AudioError::UnsupportedPlatform(
            "Linux application capture not yet fully implemented".to_string(),
        ).into())
    }
}

// Simple Linux audio device implementation
#[derive(Debug, Clone)]
pub struct LinuxAudioDevice {
    pub id: String,
    pub name: String,
}

impl crate::core::interface::AudioDevice for LinuxAudioDevice {
    type DeviceId = String;

    fn get_id(&self) -> Self::DeviceId {
        self.id.clone()
    }

    fn get_name(&self) -> String {
        self.name.clone()
    }

    fn get_supported_formats(&self) -> crate::core::error::Result<Vec<crate::core::config::AudioFormat>> {
        Ok(vec![])
    }

    fn get_default_format(&self) -> crate::core::error::Result<crate::core::config::AudioFormat> {
        Ok(crate::core::config::AudioFormat {
            sample_rate: 44100,
            channels: 2,
            bits_per_sample: 16,
            sample_format: crate::core::config::SampleFormat::S16LE,
        })
    }

    fn is_input(&self) -> bool {
        false
    }

    fn is_output(&self) -> bool {
        true
    }

    fn is_active(&self) -> bool {
        false
    }

    fn is_format_supported(&self, _format: &crate::core::config::AudioFormat) -> crate::core::error::Result<bool> {
        Ok(false)
    }

    fn create_stream(&mut self, _config: &crate::api::AudioCaptureConfig) -> crate::core::error::Result<Box<dyn crate::core::interface::CapturingStream>> {
        Err(crate::core::error::AudioError::UnsupportedPlatform(
            "Linux audio streams not yet implemented".to_string(),
        ))
    }
}

// Implement DeviceEnumerator trait
impl crate::core::interface::DeviceEnumerator for LinuxDeviceEnumerator {
    type Device = LinuxAudioDevice;

    fn enumerate_devices(&self) -> crate::core::error::Result<Vec<Self::Device>> {
        Ok(vec![])
    }

    fn get_default_device(&self, _kind: crate::core::interface::DeviceKind) -> crate::core::error::Result<Self::Device> {
        Ok(LinuxAudioDevice {
            id: "default".to_string(),
            name: "Default Linux Audio Device".to_string(),
        })
    }

    fn get_input_devices(&self) -> crate::core::error::Result<Vec<Self::Device>> {
        Ok(vec![])
    }

    fn get_output_devices(&self) -> crate::core::error::Result<Vec<Self::Device>> {
        Ok(vec![])
    }

    fn get_device_by_id(&self, id: &String) -> crate::core::error::Result<Self::Device> {
        Ok(LinuxAudioDevice {
            id: id.clone(),
            name: format!("Linux Audio Device {}", id),
        })
    }
}

// Stub for PipeWireBackend
pub struct PipeWireBackend;

impl PipeWireBackend {
    pub fn new() -> crate::core::error::Result<Self> {
        Ok(PipeWireBackend)
    }

    pub fn is_available() -> bool {
        false // Simplified for now
    }
}

// Implement AudioCaptureBackend trait
impl crate::audio::core::AudioCaptureBackend for PipeWireBackend {
    fn name(&self) -> &'static str {
        "PipeWire"
    }

    fn list_applications(&self) -> crate::core::error::Result<Vec<AudioApplication>> {
        Ok(vec![])
    }

    fn capture_application(
        &self,
        _app: &AudioApplication,
        _config: StreamConfig,
    ) -> crate::core::error::Result<Box<dyn crate::audio::core::AudioCaptureStream>> {
        Err(crate::core::error::AudioError::UnsupportedPlatform(
            "PipeWire backend not yet implemented".to_string(),
        ))
    }
}
