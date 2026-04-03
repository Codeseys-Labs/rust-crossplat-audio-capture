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

// Note: WindowsCaptureConfig, WindowsCaptureThread, WindowsPlatformStream are
// imported directly via `super::thread::*` in wasapi.rs, not through this re-export.
