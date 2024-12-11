use std::fmt;

#[derive(Debug, Clone)]
pub struct AudioApplication {
    pub name: String,
    pub id: String,
    pub executable_name: String,
    pub pid: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub format: AudioFormat,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            channels: 2,
            format: AudioFormat::F32LE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    F32LE,  // 32-bit float, little endian
    S16LE,  // 16-bit signed integer, little endian
    S32LE,  // 32-bit signed integer, little endian
}

#[derive(Debug)]
pub enum AudioError {
    BackendUnavailable(&'static str),
    InitializationFailed(String),
    DeviceNotFound(String),
    ApplicationNotFound(String),
    CaptureError(String),
    InvalidFormat(String),
    IoError(std::io::Error),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::BackendUnavailable(backend) => 
                write!(f, "Audio backend '{}' is not available", backend),
            AudioError::InitializationFailed(msg) => 
                write!(f, "Failed to initialize audio backend: {}", msg),
            AudioError::DeviceNotFound(msg) => 
                write!(f, "Audio device not found: {}", msg),
            AudioError::ApplicationNotFound(msg) => 
                write!(f, "Application not found: {}", msg),
            AudioError::CaptureError(msg) => 
                write!(f, "Audio capture error: {}", msg),
            AudioError::InvalidFormat(msg) => 
                write!(f, "Invalid audio format: {}", msg),
            AudioError::IoError(err) => 
                write!(f, "IO error: {}", err),
        }
    }
}

impl std::error::Error for AudioError {}

impl From<std::io::Error> for AudioError {
    fn from(err: std::io::Error) -> Self {
        AudioError::IoError(err)
    }
}

pub trait AudioCaptureBackend: Send {
    fn name(&self) -> &'static str;
    
    fn list_applications(&self) -> Result<Vec<AudioApplication>, AudioError>;
    
    fn capture_application(
        &self,
        app: &AudioApplication,
        config: AudioConfig,
    ) -> Result<Box<dyn AudioCaptureStream>, AudioError>;
}

pub trait AudioCaptureStream: Send {
    fn start(&mut self) -> Result<(), AudioError>;
    fn stop(&mut self) -> Result<(), AudioError>;
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, AudioError>;
    fn config(&self) -> &AudioConfig;
}