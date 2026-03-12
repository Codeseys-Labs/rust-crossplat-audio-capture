//! Windows audio implementation using WASAPI

pub(crate) mod thread;
pub mod wasapi;

// Re-export public types from wasapi module
pub use wasapi::{
    ComInitializer, WindowsApplicationCapture, WindowsAudioDevice, WindowsDeviceEnumerator,
};

// Re-export application session types from wasapi (canonical definitions)
pub use wasapi::enumerate_application_audio_sessions;
pub use wasapi::ApplicationAudioSessionInfo;

// Re-export new types for BridgeStream architecture
pub(crate) use thread::{WindowsCaptureConfig, WindowsCaptureThread, WindowsPlatformStream};
