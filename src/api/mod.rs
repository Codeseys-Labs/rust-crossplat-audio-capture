//! Public builder/handle facade: [`AudioCaptureBuilder`] → [`AudioCapture`].
//!
//! This module defines the library's primary entry points. Consumers interact
//! with rsac through [`AudioCaptureBuilder`] (configuration) and
//! [`AudioCapture`] (the lifecycle handle returned from `build()`).
//!
//! # Thread safety
//!
//! [`AudioCapture`] is `Send + Sync`. Its internal state guards the
//! platform-specific stream behind an [`Arc<Mutex<_>>`] so the handle can be
//! moved across threads or shared behind an [`Arc`]. The underlying data plane
//! (ring buffer between OS callback and consumer) is lock-free; see
//! [`crate::bridge`] for the full description.
//!
//! # Multiple concurrent captures
//!
//! Multiple [`AudioCapture`] instances can run in the same process; each has
//! its own isolated ring buffer bridge (see [`crate::bridge`]), so they
//! cannot interfere.
//!
//! [`Arc`]: std::sync::Arc
//! [`Arc<Mutex<_>>`]: std::sync::Arc

// Internal layout (every public item is re-exported below, so the public
// `crate::api::*` paths do not depend on it):
// - `builder`     — AudioCaptureBuilder: config, preflight, device resolution
// - `capture`     — AudioCapture handle, callback pump, AudioBufferIterator
// - `running`     — RunningCapture RAII guard
// - `delivery`    — drain / subscribe pumps shared with `compose`, DrainHandle
// - `negotiation` — non-Linux device-format negotiation
// - `recovery`    — `hotplug-recovery` supervisor (RecoveryStream)
mod builder;
mod capture;
mod delivery;
#[cfg(not(target_os = "linux"))]
mod negotiation;
#[cfg(feature = "hotplug-recovery")]
mod recovery;
mod running;
#[cfg(test)]
mod tests;

pub use builder::AudioCaptureBuilder;
pub use capture::{AudioBufferIterator, AudioCapture};
pub use delivery::DrainHandle;
pub use running::RunningCapture;

// Re-export AudioCaptureConfig from core::config so downstream code
// that uses `crate::api::AudioCaptureConfig` still resolves.
pub use crate::core::config::AudioCaptureConfig;

// The `compose` feature's `Composition` drives its sinks and subscribers
// through the same pumps as `AudioCapture` (ADR-0011).
#[cfg(feature = "compose")]
pub(crate) use delivery::{
    spawn_drain_thread, spawn_subscribe_thread, spawn_subscribe_with_errors_thread,
};
