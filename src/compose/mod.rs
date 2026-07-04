//! Multi-source channel composition (ADR-0011, opt-in `compose` feature).
//!
//! This module composes **multiple capture sources into one multi-channel
//! stream**. Sources are declared in *groups*; each group contributes a fixed
//! set of output channels:
//!
//! - [`GroupLayout::Mono`] — every source in the group is folded to mono and
//!   gain-weighted-summed into **1** output channel.
//! - [`GroupLayout::Stereo`] — every source is folded to stereo (mono sources
//!   are duplicated to L/R, >2-channel sources are even/odd-averaged) and
//!   summed into **2** output channels.
//! - [`GroupLayout::KeepChannels`] — the group's single source passes its
//!   native channels through unchanged (v1: exactly one source per
//!   keep-channels group).
//!
//! Groups append **in declaration order** into one interleaved-f32 frame; the
//! [`ChannelMap`] reports which output channel belongs to which group.
//!
//! # Rate alignment
//!
//! Every source is delivered at whatever rate its backend negotiated (e.g.
//! Windows process loopback cannot autoconvert). Sources whose delivered rate
//! differs from the session rate (default 48 kHz) are resampled with `rubato`
//! on the dedicated compositor thread — never on an OS callback thread
//! (ADR-0001 is untouched).
//!
//! # Pacing model (master clock + silence padding)
//!
//! One source is the **master clock**: the first source (in declaration order)
//! targeting [`CaptureTarget::SystemDefault`] or [`CaptureTarget::Device`]
//! (device clocks tick through silence; application taps go quiet when the app
//! stops playing), else the first source overall. The composed stream emits a
//! fixed-size tick whenever the master has accumulated a quantum of frames.
//! At each tick, sources that are *behind* are **silence-padded** and sources
//! drifting *ahead* are **bounded-trimmed**; per-source
//! [`SourceStats::padded_frames`] / [`SourceStats::trimmed_frames`] counters
//! expose both. If the master itself stalls past
//! [`CompositionBuilder::stall_timeout`], a wall-clock fallback tick keeps the
//! session alive. Timestamp-based drift correction is deliberately out of
//! scope for v1 (no backend populates `AudioBuffer::timestamp` yet).
//!
//! # Delivery contract
//!
//! [`Composition`] implements the same [`CapturingStream`] contract as a
//! single capture — the composed buffers flow through the standard bridge
//! ring, so terminal semantics (ADR-0003), overrun counters, backpressure
//! reporting, and (with the `async-stream` feature) waker registration all
//! behave exactly like a plain [`AudioCapture`](crate::api::AudioCapture).
//!
//! The composition **owns the consumption of its inner captures** (a bridge
//! ring has a single logical consumer): do not read the composed sources
//! through any other handle while a composition runs.
//!
//! # Example
//!
//! ```rust,no_run
//! use rsac::compose::{CompositionBuilder, Group, GroupLayout};
//! use rsac::core::config::CaptureTarget;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let mut session = CompositionBuilder::new()
//!     .sample_rate(48000)
//!     .group(
//!         Group::new("voice")
//!             .source(CaptureTarget::ApplicationByName("discord".into()))
//!             .source_with_gain(CaptureTarget::ApplicationByName("zoom".into()), 0.8)
//!             .mixdown(GroupLayout::Mono), // → 1 channel
//!     )
//!     .group(
//!         Group::new("system")
//!             .source(CaptureTarget::SystemDefault)
//!             .keep_channels(), // → source's native channels
//!     )
//!     .build()?;
//!
//! session.start()?;
//! let map = session.channel_map().expect("started");
//! println!("composed {} channels: {:?}", map.channels(), map);
//! loop {
//!     match session.read_buffer() {
//!         // interleaved f32, `map.channels()` channels @ 48 kHz
//!         Ok(Some(buffer)) => { let _ = buffer.num_frames(); }
//!         // No data *yet* — not end-of-stream; poll again shortly.
//!         Ok(None) => std::thread::sleep(std::time::Duration::from_millis(1)),
//!         // Fatal terminal (composition ended and drained) ends the loop.
//!         Err(e) if e.is_fatal() => break,
//!         // Transient errors are retryable.
//!         Err(e) => eprintln!("transient read error: {e}"),
//!     }
//! }
//! session.stop()?;
//! # Ok(())
//! # }
//! ```
//!
//! [`CaptureTarget::SystemDefault`]: crate::core::config::CaptureTarget::SystemDefault
//! [`CaptureTarget::Device`]: crate::core::config::CaptureTarget::Device
//! [`CapturingStream`]: crate::core::interface::CapturingStream

mod builder;
mod engine;
mod resample;
mod stream;

#[cfg(test)]
mod tests;

pub use builder::{ChannelMap, ChannelOrigin, CompositionBuilder, Group, GroupLayout};
pub use stream::{Composition, CompositionStats, SourceStats};
