//! Sink adapters for consuming audio data from a CapturingStream.
//!
//! Sinks provide different ways to process/store audio buffers:
//! - [`NullSink`] — discards all data (for testing/benchmarking)
//! - [`ChannelSink`] — sends buffers over a std::sync::mpsc channel
//! - [`WavFileSink`] — writes audio to WAV files (requires `sink-wav` feature)

pub mod channel;
pub mod null;
pub mod traits;

#[cfg(feature = "sink-wav")]
pub mod wav;

// Re-exports
pub use channel::ChannelSink;
pub use null::NullSink;
pub use traits::AudioSink;

#[cfg(feature = "sink-wav")]
pub use wav::WavFileSink;
