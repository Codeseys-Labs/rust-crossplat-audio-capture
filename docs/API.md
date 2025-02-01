# API Documentation

## Core Types and Traits

### AudioCapture Trait
The main trait for audio capture functionality:

```rust
pub trait AudioCapture: Send {
    /// Start audio capture
    fn start(&mut self) -> Result<(), AudioError>;
    
    /// Stop audio capture
    fn stop(&mut self) -> Result<(), AudioError>;
    
    /// Check if currently capturing
    fn is_capturing(&self) -> bool;
    
    /// Get available audio devices
    fn get_available_devices(&self) -> Result<Vec<AudioDevice>, AudioError>;
    
    /// Set the capture device
    fn set_device(&mut self, device: AudioDevice) -> Result<(), AudioError>;
}
```

### AudioDevice
Represents an audio capture device:

```rust
#[derive(Debug, Clone)]
pub struct AudioDevice {
    /// Unique device identifier
    pub id: String,
    
    /// Human-readable device name
    pub name: String,
    
    /// Device type (input/output/loopback)
    pub device_type: DeviceType,
    
    /// Number of audio channels
    pub channels: u32,
    
    /// Sample rate in Hz
    pub sample_rate: u32,
    
    /// Audio backend type
    pub backend: AudioBackend,
}
```

### AudioCaptureConfig
Configuration for audio capture:

```rust
#[derive(Debug, Clone)]
pub struct AudioCaptureConfig {
    /// Target audio device
    device: Option<AudioDevice>,
    
    /// Sample rate in Hz
    sample_rate: u32,
    
    /// Number of channels
    channels: u32,
    
    /// Buffer size in frames
    buffer_size: usize,
    
    /// Target application name for app-specific capture
    app_name: Option<String>,
}

impl AudioCaptureConfig {
    /// Create new configuration with default values
    pub fn new() -> Self;
    
    /// Set the target device
    pub fn device(self, device: AudioDevice) -> Self;
    
    /// Set the sample rate
    pub fn sample_rate(self, rate: u32) -> Self;
    
    // ... other builder methods
}
```

## Platform-Specific Implementations

### Windows (WASAPI)
```rust
#[cfg(target_os = "windows")]
pub struct WasapiCapture {
    // ... implementation details
}

#[cfg(target_os = "windows")]
impl AudioCapture for WasapiCapture {
    // ... implementation
}
```

### macOS (CoreAudio)
```rust
#[cfg(target_os = "macos")]
pub struct CoreAudioCapture {
    // ... implementation details
}

#[cfg(target_os = "macos")]
impl AudioCapture for CoreAudioCapture {
    // ... implementation
}
```

### Linux (PipeWire/PulseAudio)
```rust
#[cfg(target_os = "linux")]
pub struct LinuxCapture {
    // ... implementation details
}

#[cfg(target_os = "linux")]
impl AudioCapture for LinuxCapture {
    // ... implementation
}
```

## Async Support

### AsyncAudioCapture Trait
```rust
#[async_trait]
pub trait AsyncAudioCapture: Send + Sync {
    /// Start audio capture asynchronously
    async fn start(&mut self) -> Result<(), AudioError>;
    
    /// Stop audio capture asynchronously
    async fn stop(&mut self) -> Result<(), AudioError>;
    
    /// Get audio data stream
    async fn capture_stream(&mut self) -> Result<mpsc::Receiver<Vec<f32>>, AudioError>;
}
```

## Error Handling

### AudioError
```rust
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("Device not found: {0}")]
    DeviceNotFound(String),
    
    #[error("Device initialization failed: {0}")]
    DeviceInitError(String),
    
    #[error("Capture error: {0}")]
    CaptureError(String),
    
    #[error("Backend error: {0}")]
    BackendError(String),
    
    #[error("Invalid configuration: {0}")]
    ConfigError(String),
}
```

## Usage Examples

### Basic Usage
```rust
use audio_capture::{AudioCapture, AudioCaptureConfig};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create configuration
    let config = AudioCaptureConfig::new()
        .sample_rate(48000)
        .channels(2);
    
    // Create capture instance
    let mut capture = AudioCapture::new(config)?;
    
    // Start capturing
    capture.start()?;
    
    // ... capture audio ...
    
    // Stop capturing
    capture.stop()?;
    
    Ok(())
}
```

### Async Usage
```rust
use audio_capture::{AsyncAudioCapture, AudioCaptureConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = AudioCaptureConfig::new();
    let mut capture = AsyncAudioCapture::new(config).await?;
    
    let mut stream = capture.capture_stream().await?;
    
    while let Some(data) = stream.recv().await {
        // Process audio data
    }
    
    Ok(())
}
```