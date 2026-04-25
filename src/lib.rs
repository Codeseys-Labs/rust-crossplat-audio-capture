#![allow(clippy::result_large_err)]
#![deny(rustdoc::broken_intra_doc_links)]
//! # rsac — cross-platform audio capture
//!
//! Streaming-first audio capture for Rust. Captures system audio,
//! per-application audio, and process-tree audio on Windows (WASAPI),
//! Linux (PipeWire), and macOS (CoreAudio Process Tap) through a single
//! unified API.
//!
//! ## Entry points
//!
//! - [`AudioCaptureBuilder`] — configure a capture session (target, format).
//! - [`AudioCapture`] — the lifecycle handle returned by `build()`; exposes
//!   `start()`, `stop()`, `read_buffer()`, `subscribe()`, and (behind the
//!   `async-stream` feature) `audio_data_stream()`.
//! - [`CaptureTarget`] — unified capture-target enum: [`CaptureTarget::SystemDefault`],
//!   [`CaptureTarget::Device`], [`CaptureTarget::Application`],
//!   [`CaptureTarget::ApplicationByName`], [`CaptureTarget::ProcessTree`].
//! - [`PlatformCapabilities::query`] — runtime capability probe; tells you
//!   what the current OS + backend actually supports before you build a capture.
//! - [`get_device_enumerator`] — device enumeration facade.
//!
//! ## Module layout
//!
//! The crate follows a strict layering DAG with no reverse dependencies:
//!
//! ```text
//! core/ → bridge/ → audio/ (platform backends) → api/
//! ```
//!
//! - [`core`] — platform-agnostic types: [`AudioBuffer`], [`CaptureTarget`],
//!   [`AudioError`], [`PlatformCapabilities`], the [`CapturingStream`] and
//!   [`AudioDevice`] traits, and runtime introspection helpers.
//! - [`bridge`] — lock-free SPSC ring-buffer bridge (`rtrb`) connecting OS
//!   callback threads to consumer threads, plus the [`StreamState`]
//!   lifecycle machine and the internal `BridgeStream` adapter used by
//!   every backend.
//! - [`audio`] — per-OS backends (WASAPI, PipeWire, CoreAudio), each gated by
//!   `#[cfg(target_os = "…")]` + a matching `feat_*` Cargo feature.
//! - [`api`] — the public builder/handle facade.
//! - [`sink`] — downstream sink adapters ([`NullSink`], [`ChannelSink`],
//!   [`WavFileSink`] behind `sink-wav`).
//!
//! ## Quick start
//!
//! ```no_run
//! use rsac::{AudioCaptureBuilder, CaptureTarget};
//!
//! let mut capture = AudioCaptureBuilder::new()
//!     .with_target(CaptureTarget::SystemDefault)
//!     .sample_rate(48000)
//!     .channels(2)
//!     .build()?;
//!
//! capture.start()?;
//! while let Some(buffer) = capture.read_buffer()? {
//!     let samples: &[f32] = buffer.data();
//!     let _frames = buffer.num_frames();
//!     // process audio…
//!     # break;
//! }
//! capture.stop()?;
//! # Ok::<(), rsac::AudioError>(())
//! ```
//!
//! ## Feature flags
//!
//! See [`docs/features.md`](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/blob/master/docs/features.md)
//! for the full matrix. Summary:
//!
//! - `feat_windows`, `feat_linux`, `feat_macos` — platform backends
//!   (all enabled by default; pair with matching `target_os` to compile).
//! - `async-stream` — enables [`AudioCapture::audio_data_stream`] returning
//!   a [`futures_core::Stream`].
//! - `sink-wav` — enables [`WavFileSink`].
//! - `test-utils` — exposes shared test helpers used by integration tests
//!   and the binding crates.
//!
//! ## Errors and recoverability
//!
//! Every fallible operation returns [`AudioResult<T>`] (alias for
//! `Result<T, AudioError>`). [`AudioError`] variants are tagged with an
//! [`ErrorKind`] and a [`Recoverability`] hint so callers can decide whether
//! to retry, fall back, or surface the failure.
//!
//! ## Further reading
//!
//! - [`VISION.md`](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/blob/master/VISION.md) — scope and non-goals.
//! - [`docs/ARCHITECTURE.md`](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/blob/master/docs/ARCHITECTURE.md) — 3-layer architecture overview.
//! - [`docs/CI_AUDIO_TESTING.md`](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture/blob/master/docs/CI_AUDIO_TESTING.md) — how audio integration tests run in CI.

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
pub use crate::core::interface::{AudioDevice, CapturingStream, DeviceEnumerator, DeviceKind};

// Audio module re-exports
pub use crate::audio::get_device_enumerator;

// Introspection helpers (cross-platform source discovery)
pub use crate::core::introspection::{
    check_audio_capture_permission, list_audio_applications, list_audio_sources, AudioSource,
    AudioSourceKind, PermissionStatus, StreamStats,
};

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

// Async stream support
#[cfg(feature = "async-stream")]
pub use crate::bridge::AsyncAudioStream;

// Re-export test utils if the feature is enabled
#[cfg(feature = "test-utils")]
pub use utils::test_utils;
