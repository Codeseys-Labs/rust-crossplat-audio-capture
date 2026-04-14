// src/core/mod.rs

//! The `core` module provides the central traits, types, and interfaces
//! for the audio capture library.
//!
//! It defines platform-agnostic abstractions for audio devices, streams,
//! formats, and error handling, allowing backend implementations to plug
//! into a common framework.

pub mod buffer;
pub mod capabilities;
pub mod config;
pub mod error;
pub mod interface;
pub mod introspection;
pub mod processing;

// ── Re-exports ───────────────────────────────────────────────────────────

pub use buffer::AudioBuffer;
pub use capabilities::PlatformCapabilities;
pub use error::{AudioError, AudioResult, BackendContext, ErrorKind, ProcessError, Recoverability};
pub use processing::AudioProcessor;

// Config types
pub use config::{
    ApplicationId, AudioCaptureConfig, AudioFileFormat, AudioFormat, CaptureTarget, DeviceId,
    LatencyMode, ProcessId, SampleFormat, StreamConfig,
};

// Legacy / compat (deprecated)
#[allow(deprecated)]
pub use config::DeviceSelector;
