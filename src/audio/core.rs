// use std::fmt; // No longer needed directly as AudioError handles its own Display

// Import types from the new core modules
use crate::core::config::StreamConfig; // Renamed from AudioConfig
use crate::core::error::{AudioError, Result as CoreResult}; // Using the aliased Result

#[derive(Debug, Clone)]
pub struct AudioApplication {
    pub name: String,
    pub id: String,
    pub executable_name: String,
    pub pid: u32,
}

// AudioConfig struct has been removed (now StreamConfig from core::config)
// AudioFormat enum has been removed (now part of AudioFormat struct in core::config)
// AudioError enum has been removed (now in core::error)
// Display and Error impls for local AudioError have been removed.
// From<std::io::Error> for local AudioError has been removed.
// Consider adding From<std::io::Error> for crate::core::error::AudioError if needed globally,
// or handle it at the call sites. For now, we'll assume call sites will map errors.

pub trait AudioCaptureBackend: Send {
    fn name(&self) -> &'static str;

    fn list_applications(&self) -> CoreResult<Vec<AudioApplication>>;

    fn capture_application(
        &self,
        app: &AudioApplication,
        config: StreamConfig, // Updated to use StreamConfig
    ) -> CoreResult<Box<dyn AudioCaptureStream>>;
}

pub trait AudioCaptureStream: Send {
    fn start(&mut self) -> CoreResult<()>;
    fn stop(&mut self) -> CoreResult<()>;
    fn read(&mut self, buffer: &mut [u8]) -> CoreResult<usize>;
    fn config(&self) -> &StreamConfig; // Updated to use StreamConfig
}
