//! macOS audio implementation using CoreAudio.
//!
//! This module provides the macOS backend for audio capture, using CoreAudio's
//! AUHAL (Hardware Abstraction Layer) for system audio and ProcessTap for
//! application-specific audio.
//!
//! # Architecture (post-refactoring)
//!
//! Audio capture now flows through `BridgeStream<MacosPlatformStream>`:
//!
//! ```text
//! CoreAudio RT callback → BridgeProducer → [ring buffer] → BridgeConsumer → BridgeStream
//!                                                                             ↓
//!                                                                         CapturingStream
//! ```
//!
//! The old `MacosAudioStream` and `MacosApplicationAudioStream` (which used
//! `VecDeque + Mutex` with priority inversion risk) have been removed.

#[cfg(target_os = "macos")]
pub mod coreaudio;
#[cfg(target_os = "macos")]
pub mod tap;
#[cfg(target_os = "macos")]
pub(crate) mod thread;

// Re-export public types for convenience
#[cfg(target_os = "macos")]
pub use coreaudio::{
    enumerate_audio_applications, ApplicationInfo, MacosAudioDevice, MacosDeviceEnumerator,
};
