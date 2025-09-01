//! Windows audio implementation using WASAPI

pub mod wasapi;

// Re-export for convenience
pub use wasapi::{
    WindowsApplicationCapture, WindowsAudioDevice, WindowsAudioStream, 
    WindowsDeviceEnumerator, ComInitializer
};

// Legacy backend exports (to be deprecated)
pub use wasapi::WindowsApplicationCapture as WasapiBackend;

/// Application audio session information for Windows
#[derive(Debug, Clone)]
pub struct ApplicationAudioSessionInfo {
    pub process_id: u32,
    pub session_id: String,
    pub display_name: String,
    pub is_system_sounds: bool,
}

/// Enumerate application audio sessions on Windows
pub fn enumerate_application_audio_sessions() -> crate::core::error::Result<Vec<ApplicationAudioSessionInfo>> {
    // This would use WASAPI session enumeration
    // For now, return empty list as placeholder
    Ok(vec![])
}
