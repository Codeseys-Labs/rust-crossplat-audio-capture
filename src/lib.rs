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
pub mod prelude;
pub mod sink;
// Internal structured-instrumentation shim. Declared before the modules that use
// `rsac_event!`/`rsac_span!` so the `#[macro_export]`ed macros are in scope crate-wide.
// The macros land at the crate root (macro_export); `trace::install_default_tracing`
// is re-exported below behind the `tracing` feature.
pub mod trace;
pub mod utils;

// Core types
pub use crate::core::buffer::AudioBuffer;
pub use crate::core::capabilities::PlatformCapabilities;
pub use crate::core::config::{
    ApplicationId, AudioCaptureConfig, AudioFormat, CaptureTarget, DeviceId, ProcessId,
    SampleFormat, StreamConfig,
};
pub use crate::core::error::{
    AudioError, AudioResult, BackendContext, ErrorKind, Recoverability, UserFacingError,
};
pub use crate::core::interface::{
    AudioDevice, CapturingStream, DeviceEnumerator, DeviceInfo, DeviceKind,
};

// Audio module re-exports
pub use crate::audio::get_device_enumerator;

// Introspection helpers (cross-platform source discovery)
pub use crate::core::introspection::{
    check_audio_capture_permission, list_audio_applications, list_audio_sources, AudioSource,
    AudioSourceKind, BackpressureReport, PermissionStatus, StreamStats,
};

// API types
pub use crate::api::{AudioCapture, AudioCaptureBuilder, RunningCapture};

/// Construct an [`AudioCaptureBuilder`] in one expression with named,
/// order-independent fields.
///
/// `capture!` flattens the usual `AudioCaptureBuilder::new().with_target(..)
/// .sample_rate(..).channels(..)` chain into a single declarative call. It
/// **always evaluates to an [`AudioCaptureBuilder`]** (never a `Result`), so the
/// caller chooses the terminal step — [`build`](AudioCaptureBuilder::build) for
/// the handle, or [`start`](AudioCaptureBuilder::start) for a started
/// [`RunningCapture`](crate::api::RunningCapture). Every field is optional;
/// omitting one leaves the builder default (system-default target, 48 kHz, 2
/// channels, F32). Fields may appear in any order, separated by commas.
///
/// Because it is `#[macro_export]`ed, the macro is reachable as `rsac::capture!`
/// and through the [prelude](crate::prelude) (`use rsac::prelude::*;`). It only
/// ever calls **public** builder methods, so it works unchanged in downstream
/// crates.
///
/// # Target forms
///
/// A target may be given either with the explicit `target:`/`target_str:` key or
/// with a convenience scheme shorthand:
///
/// | Shorthand                    | Equivalent target                                   |
/// |------------------------------|-----------------------------------------------------|
/// | `capture!(system)`           | [`CaptureTarget::SystemDefault`]                    |
/// | `capture!(device: id)`       | [`CaptureTarget::Device`]`(DeviceId(id.into()))`    |
/// | `capture!(app: pid)`         | [`CaptureTarget::Application`]`(ApplicationId(..))`  |
/// | `capture!(name: n)`          | [`CaptureTarget::ApplicationByName`]`(n.into())`     |
/// | `capture!(tree: pid)`        | [`CaptureTarget::ProcessTree`] via [`CaptureTarget::pid`] |
///
/// The `app:`/`device:`/`name:` shorthands accept any value that is stringified
/// via [`ToString`]/[`Display`](std::fmt::Display) (so `&str`, `String`, and
/// numeric pids for `app:` all work), and `tree:` takes a `u32` pid. `target_str:`
/// parses the canonical string grammar
/// (`"system"`, `"app:1234"`, `"device:hw:0,0"`, …) via
/// [`AudioCaptureBuilder::try_target_str`] — an *infallible* best-effort parse
/// that keeps the default target if the string is malformed (use the
/// [`AudioCaptureBuilder::target_str`] method directly when you need the parse
/// error).
///
/// # Config keys
///
/// `sample_rate:` / `rate:` (aliases), `channels:`, `sample_format:`, and
/// `buffer_size:` set the corresponding builder fields.
///
/// # Examples
///
/// ```
/// use rsac::capture;
/// use rsac::core::config::{CaptureTarget, SampleFormat};
///
/// // System capture at 48 kHz stereo, then build the handle:
/// let builder = capture!(system, rate: 48000, channels: 2);
/// assert_eq!(builder.config().sample_rate, 48000);
///
/// // Per-application shorthand (pid 1234):
/// let app = capture!(app: 1234);
/// assert!(matches!(app.target(), CaptureTarget::Application(_)));
///
/// // Device shorthand + explicit format, any field order:
/// let dev = capture!(sample_format: SampleFormat::I16, device: "hw:0,0");
/// assert_eq!(dev.config().sample_format, SampleFormat::I16);
///
/// // Explicit target key is equivalent to the shorthand:
/// let sys = capture!(target: CaptureTarget::SystemDefault);
/// assert_eq!(*sys.target(), CaptureTarget::SystemDefault);
/// ```
///
/// Use the prelude and go straight to a running capture:
///
/// ```no_run
/// use rsac::prelude::*;
/// use rsac::capture;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut running = capture!(system, rate: 44100).start()?;
/// if let Some(buf) = running.read_buffer()? {
///     let _ = buf.data().len();
/// }
/// # Ok(())
/// # }
/// ```
#[macro_export]
macro_rules! capture {
    // NOTE on arm order: the internal `@munch` arms MUST precede the public
    // entry arms below. The `($($rest:tt)+)` entry arm matches *any* non-empty
    // token stream — including an `@munch (..) ..` internal call — so if it came
    // first it would re-wrap every recursive step and blow the recursion limit.
    // macro_rules tries arms top-to-bottom, so the specific `@munch` arms win.

    // ── Terminal: no more tokens → yield the accumulated builder ─────────
    (@munch ($builder:expr)) => { $builder };
    (@munch ($builder:expr) ,) => { $builder };

    // ── Target shorthands (scheme: value) ───────────────────────────────
    // `system` (no value).
    (@munch ($builder:expr) system $(, $($rest:tt)*)?) => {
        $crate::capture!(@munch
            ($builder.with_target($crate::core::config::CaptureTarget::SystemDefault))
            $($($rest)*)?)
    };
    // `device: <expr>` → Device(DeviceId(expr.to_string()))
    // Accepts anything `Display` (a `&str`/`String` device id, or e.g. a number).
    (@munch ($builder:expr) device : $val:expr $(, $($rest:tt)*)?) => {
        $crate::capture!(@munch
            ($builder.with_target($crate::core::config::CaptureTarget::Device(
                $crate::core::config::DeviceId(::std::string::ToString::to_string(&$val)))))
            $($($rest)*)?)
    };
    // `app: <expr>` → Application(ApplicationId(expr.to_string()))
    // So `app: 1234` (a numeric pid) and `app: "vlc"` both work, matching the
    // `"app:<id>"` string grammar where the id is the stringified value.
    (@munch ($builder:expr) app : $val:expr $(, $($rest:tt)*)?) => {
        $crate::capture!(@munch
            ($builder.with_target($crate::core::config::CaptureTarget::Application(
                $crate::core::config::ApplicationId(::std::string::ToString::to_string(&$val)))))
            $($($rest)*)?)
    };
    // `name: <expr>` → ApplicationByName(expr.to_string())
    (@munch ($builder:expr) name : $val:expr $(, $($rest:tt)*)?) => {
        $crate::capture!(@munch
            ($builder.with_target($crate::core::config::CaptureTarget::ApplicationByName(
                ::std::string::ToString::to_string(&$val))))
            $($($rest)*)?)
    };
    // `tree: <expr>` (a u32 pid) → ProcessTree via CaptureTarget::pid
    (@munch ($builder:expr) tree : $val:expr $(, $($rest:tt)*)?) => {
        $crate::capture!(@munch
            ($builder.with_target($crate::core::config::CaptureTarget::pid($val)))
            $($($rest)*)?)
    };

    // ── Explicit target keys ─────────────────────────────────────────────
    // `target: <CaptureTarget expr>`
    (@munch ($builder:expr) target : $val:expr $(, $($rest:tt)*)?) => {
        $crate::capture!(@munch ($builder.with_target($val)) $($($rest)*)?)
    };
    // `target_str: <expr>` → infallible best-effort parse (keeps default on error)
    (@munch ($builder:expr) target_str : $val:expr $(, $($rest:tt)*)?) => {
        $crate::capture!(@munch ($builder.try_target_str($val)) $($($rest)*)?)
    };

    // ── Config keys ──────────────────────────────────────────────────────
    (@munch ($builder:expr) sample_rate : $val:expr $(, $($rest:tt)*)?) => {
        $crate::capture!(@munch ($builder.sample_rate($val)) $($($rest)*)?)
    };
    // `rate:` is an alias for `sample_rate:`.
    (@munch ($builder:expr) rate : $val:expr $(, $($rest:tt)*)?) => {
        $crate::capture!(@munch ($builder.sample_rate($val)) $($($rest)*)?)
    };
    (@munch ($builder:expr) channels : $val:expr $(, $($rest:tt)*)?) => {
        $crate::capture!(@munch ($builder.channels($val)) $($($rest)*)?)
    };
    (@munch ($builder:expr) sample_format : $val:expr $(, $($rest:tt)*)?) => {
        $crate::capture!(@munch ($builder.sample_format($val)) $($($rest)*)?)
    };
    (@munch ($builder:expr) buffer_size : $val:expr $(, $($rest:tt)*)?) => {
        $crate::capture!(@munch ($builder.buffer_size($val)) $($($rest)*)?)
    };

    // ── Public entry points (kept LAST; see arm-order note above) ────────
    // Start from a fresh builder and munch the comma-separated field list,
    // threading the partially-built builder expression through `@munch`.
    () => {
        $crate::api::AudioCaptureBuilder::new()
    };
    ($($rest:tt)+) => {
        $crate::capture!(@munch ($crate::api::AudioCaptureBuilder::new()) $($rest)+)
    };
}

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

// Optional `tracing` integration: best-effort default subscriber installer for
// binaries/examples. The `rsac_event!`/`rsac_span!` macros are always available
// at the crate root (they fall back to `log::` when this feature is off).
#[cfg(feature = "tracing")]
pub use crate::trace::install_default_tracing;

// Re-export test utils if the feature is enabled
#[cfg(feature = "test-utils")]
pub use utils::test_utils;

// ── capture! macro tests (rsac-44dc) ───────────────────────────────────────
#[cfg(test)]
mod capture_macro_tests {
    use crate::core::config::{ApplicationId, CaptureTarget, DeviceId, ProcessId, SampleFormat};

    /// `capture!()` with no fields yields a default builder (system default).
    #[test]
    fn empty_yields_default_builder() {
        let b = capture!();
        assert_eq!(*b.target(), CaptureTarget::SystemDefault);
        // Defaults preserved: 48 kHz / 2ch / F32.
        assert_eq!(b.config().sample_rate, 48000);
        assert_eq!(b.config().channels, 2);
        assert_eq!(b.config().sample_format, SampleFormat::F32);
    }

    /// `capture!(target: ..)` sets the target (acceptance criterion 1).
    #[test]
    fn explicit_target_key_sets_target() {
        let b = capture!(target: CaptureTarget::SystemDefault);
        assert_eq!(*b.target(), CaptureTarget::SystemDefault);
    }

    /// `capture!(sample_rate: .., channels: ..)` sets those fields; an omitted
    /// field keeps the default (acceptance criterion 2).
    #[test]
    fn config_keys_set_only_named_fields() {
        let b = capture!(sample_rate: 44100, channels: 1);
        assert_eq!(b.config().sample_rate, 44100);
        assert_eq!(b.config().channels, 1);
        // sample_format untouched → default F32.
        assert_eq!(b.config().sample_format, SampleFormat::F32);
        // target untouched → default SystemDefault.
        assert_eq!(*b.target(), CaptureTarget::SystemDefault);
    }

    /// `rate:` is an accepted alias for `sample_rate:`.
    #[test]
    fn rate_alias_sets_sample_rate() {
        let b = capture!(rate: 96000);
        assert_eq!(b.config().sample_rate, 96000);
    }

    /// The `system` shorthand sets SystemDefault and composes with config keys
    /// in one call (the brief's `capture!(system, rate: 48000, channels: 2)`).
    #[test]
    fn system_shorthand_with_config() {
        let b = capture!(system, rate: 48000, channels: 2);
        assert_eq!(*b.target(), CaptureTarget::SystemDefault);
        assert_eq!(b.config().sample_rate, 48000);
        assert_eq!(b.config().channels, 2);
    }

    /// `app: <pid>` (numeric) stringifies into an `ApplicationId`, matching the
    /// `"app:<id>"` grammar.
    #[test]
    fn app_shorthand_numeric_pid() {
        let b = capture!(app: 1234);
        assert_eq!(
            *b.target(),
            CaptureTarget::Application(ApplicationId("1234".to_string()))
        );
    }

    /// `device: "hw:0"` builds a `Device` target from a string id.
    #[test]
    fn device_shorthand_string_id() {
        let b = capture!(device: "hw:0");
        assert_eq!(
            *b.target(),
            CaptureTarget::Device(DeviceId("hw:0".to_string()))
        );
    }

    /// `name: ..` builds an `ApplicationByName` target.
    #[test]
    fn name_shorthand() {
        let b = capture!(name: "VLC");
        assert_eq!(
            *b.target(),
            CaptureTarget::ApplicationByName("VLC".to_string())
        );
    }

    /// `tree: <pid>` builds a `ProcessTree` target via `CaptureTarget::pid`.
    #[test]
    fn tree_shorthand() {
        let b = capture!(tree: 42);
        assert_eq!(*b.target(), CaptureTarget::ProcessTree(ProcessId(42)));
    }

    /// Fields are order-independent: target may come before or after config keys.
    #[test]
    fn fields_are_order_independent() {
        let a = capture!(channels: 1, app: 7, sample_rate: 32000);
        let b = capture!(sample_rate: 32000, app: 7, channels: 1);
        assert_eq!(a.target(), b.target());
        assert_eq!(a.config().sample_rate, b.config().sample_rate);
        assert_eq!(a.config().channels, b.config().channels);
        assert_eq!(
            *a.target(),
            CaptureTarget::Application(ApplicationId("7".to_string()))
        );
        assert_eq!(a.config().sample_rate, 32000);
        assert_eq!(a.config().channels, 1);
    }

    /// `target_str:` parses the canonical string grammar (infallibly); a valid
    /// string is applied (acceptance criterion 3: produces a builder).
    #[test]
    fn target_str_valid_applies() {
        let b = capture!(target_str: "app:99", channels: 2);
        assert_eq!(
            *b.target(),
            CaptureTarget::Application(ApplicationId("99".to_string()))
        );
        assert_eq!(b.config().channels, 2);
    }

    /// `target_str:` is best-effort: a malformed string keeps the default target
    /// (it routes through the infallible `try_target_str`).
    #[test]
    fn target_str_invalid_keeps_default() {
        let b = capture!(target_str: "garbage");
        assert_eq!(*b.target(), CaptureTarget::SystemDefault);
    }

    /// `sample_format:` and `buffer_size:` are honored.
    #[test]
    fn format_and_buffer_size_keys() {
        let b = capture!(sample_format: SampleFormat::I16, buffer_size: Some(512usize));
        assert_eq!(b.config().sample_format, SampleFormat::I16);
        assert_eq!(b.config().buffer_size, Some(512));
    }

    /// The macro yields a real `AudioCaptureBuilder`, so `.build()`/`.start()` are
    /// reachable (acceptance criterion 3). We don't run them (no hardware) — we
    /// only assert the produced value is the builder type by chaining a setter.
    #[test]
    fn macro_result_is_a_builder_chainable_to_terminal() {
        let b = capture!(system).channels(2).sample_rate(48000);
        assert_eq!(b.config().channels, 2);
        // Type-checks that `.build()` exists on the produced value without running
        // it (would need a device). Referencing the fn item is enough.
        let _build_fn = crate::api::AudioCaptureBuilder::build;
        let _start_fn = crate::api::AudioCaptureBuilder::start;
    }
}
