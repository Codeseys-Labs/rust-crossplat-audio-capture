pub mod api;
pub mod audio;
pub mod bridge;
pub mod core;
pub mod sink;
pub mod utils;

// Core types
pub use crate::core::buffer::AudioBuffer;
pub use crate::core::capabilities::PlatformCapabilities;
pub use crate::core::config::{
    ApplicationId, AudioCaptureConfig, AudioFormat, CaptureTarget, DeviceId, ProcessId,
    SampleFormat, StreamConfig,
};
pub use crate::core::error::{AudioError, AudioResult, BackendContext, ErrorKind, Recoverability};
pub use crate::core::interface::{AudioDevice, CapturingStream, DeviceEnumerator};

// API types
pub use crate::api::{AudioCapture, AudioCaptureBuilder};

// Bridge types (stream state is useful for consumers to check stream lifecycle)
pub use crate::bridge::state::{AtomicStreamState, StreamState};

// Sink types
pub use crate::sink::AudioSink;
pub use crate::sink::ChannelSink;
pub use crate::sink::NullSink;

#[cfg(feature = "sink-wav")]
pub use crate::sink::WavFileSink;

// Re-export test utils if the feature is enabled
#[cfg(feature = "test-utils")]
pub use utils::test_utils;
