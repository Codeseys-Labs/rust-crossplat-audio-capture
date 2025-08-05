//! Linux-specific audio capture backend using PipeWire.
//!
//! NOTE: This is currently a stub implementation to allow compilation.
//! Full PipeWire integration is being worked on.
#![cfg(target_os = "linux")]

use crate::api::AudioCaptureConfig;
use crate::core::buffer::AudioBuffer;
use crate::core::config::StreamConfig;
use crate::core::error::{AudioError, Result as AudioResult};
use crate::core::interface::{
    AudioBackend, AudioDevice, CapturingStream, DeviceEnumerator, DeviceKind,
};
use crate::{AudioFormat, SampleFormat};
use futures_channel::mpsc;
use futures_core::Stream;
use std::{pin::Pin, sync::Arc, time::Instant};

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

/// Linux device enumerator (stub implementation)
pub struct LinuxDeviceEnumerator;

impl LinuxDeviceEnumerator {
    pub fn new() -> AudioResult<Self> {
        Ok(LinuxDeviceEnumerator)
    }
}

impl DeviceEnumerator for LinuxDeviceEnumerator {
    fn enumerate_devices(&self) -> AudioResult<Vec<AudioDevice>> {
        // Stub implementation
        Ok(vec![])
    }

    fn get_default_device(&self, _kind: DeviceKind) -> AudioResult<Option<AudioDevice>> {
        // Stub implementation
        Ok(None)
    }
}

/// PipeWire backend (stub implementation)
pub struct PipeWireBackend;

impl PipeWireBackend {
    pub fn new() -> AudioResult<Self> {
        Ok(PipeWireBackend)
    }
}

impl AudioBackend for PipeWireBackend {
    fn create_capturing_stream(
        &self,
        _device: &AudioDevice,
        _config: &AudioCaptureConfig,
    ) -> AudioResult<Box<dyn CapturingStream>> {
        Err(AudioError::BackendError(
            "PipeWire backend not yet implemented".to_string(),
        ))
    }

    fn enumerate_devices(&self) -> AudioResult<Vec<AudioDevice>> {
        Ok(vec![])
    }

    fn get_default_device(&self, _kind: DeviceKind) -> AudioResult<Option<AudioDevice>> {
        Ok(None)
    }
}

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
