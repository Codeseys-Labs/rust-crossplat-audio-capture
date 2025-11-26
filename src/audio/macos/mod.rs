//! macOS audio implementation using CoreAudio

pub mod coreaudio;
pub mod tap;

// Re-export for convenience
pub use coreaudio::{
    enumerate_audio_applications, ApplicationInfo, MacosApplicationAudioStream, MacosAudioDevice,
    MacosAudioStream,
};

// Legacy backend exports (to be deprecated)
pub use coreaudio::MacosAudioDevice as CoreAudioBackend;

/// Device enumerator for macOS
pub struct MacosDeviceEnumerator;

impl MacosDeviceEnumerator {
    pub fn new() -> Self {
        MacosDeviceEnumerator
    }
}
