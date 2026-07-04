//! The rsac prelude: a curated set of the most commonly used types.
//!
//! Glob-import this module to bring the everyday capture surface into scope in
//! one line instead of naming each type individually:
//!
//! ```
//! use rsac::prelude::*;
//! ```
//!
//! Everything re-exported here is also available at the crate root (this module
//! is purely additive — it introduces no new names and removes none of the
//! existing top-level re-exports). Each re-export is feature-gated identically
//! to the crate root, so the prelude compiles under `--no-default-features`.
//!
//! # Quick start
//!
//! ```no_run
//! use rsac::prelude::*;
//!
//! let mut capture = AudioCaptureBuilder::new()
//!     .with_target(CaptureTarget::SystemDefault)
//!     .sample_rate(48_000)
//!     .channels(2)
//!     .build()?;
//!
//! capture.start()?;
//! if let Some(buffer) = capture.read_buffer()? {
//!     let _samples: &[f32] = buffer.data();
//! }
//! capture.stop()?;
//! # Ok::<(), AudioError>(())
//! ```

// ── Capture lifecycle: the builder/handle facade ───────────────────────────
pub use crate::api::{AudioCapture, AudioCaptureBuilder, RunningCapture};

// The `capture!` one-line builder macro. `#[macro_export]` places it at the
// crate root; re-exporting it here makes `use rsac::prelude::*;` bring it into
// scope too, matching the prelude's "everyday capture surface in one import"
// contract (rsac-44dc).
pub use crate::capture;

// ── Capture target + audio format configuration ────────────────────────────
pub use crate::core::config::{AudioFormat, CaptureTarget, SampleFormat};

// ── Audio data ─────────────────────────────────────────────────────────────
pub use crate::core::buffer::AudioBuffer;

// ── Errors: result alias, taxonomy, and classification ─────────────────────
pub use crate::core::error::{AudioError, AudioResult, ErrorKind, Recoverability, UserFacingError};

// ── Capabilities + device/source introspection ────────────────────────────
pub use crate::core::capabilities::PlatformCapabilities;
pub use crate::core::interface::{AudioDevice, DeviceInfo, DeviceKind};
pub use crate::core::introspection::{
    AudioSource, AudioSourceKind, BackpressureReport, StreamStats,
};

// ── Sinks: downstream consumer adapters ────────────────────────────────────
pub use crate::sink::{AudioSink, ChannelSink, NullSink};

#[cfg(feature = "sink-wav")]
pub use crate::sink::WavFileSink;

// ── Async stream support (feature-gated, mirrors the crate root) ───────────
#[cfg(feature = "async-stream")]
pub use crate::bridge::AsyncAudioStream;

// ── Multi-source channel composition (feature-gated; ADR-0011) ─────────────
#[cfg(feature = "compose")]
pub use crate::compose::{ChannelMap, Composition, CompositionBuilder, Group, GroupLayout};
