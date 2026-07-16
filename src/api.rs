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

use crate::audio::get_device_enumerator;
use crate::core::buffer::AudioBuffer;
use crate::core::capabilities::PlatformCapabilities;
#[cfg(target_os = "android")]
use crate::core::config::AndroidProjectionToken;
use crate::core::config::{CaptureTarget, SampleFormat, StreamConfig};
// `AudioFormat` is only referenced by `pick_supported_format` (and its tests),
// which is itself `cfg(not(target_os = "linux"))`; gate the import to match so
// the Linux build stays warning-clean under `-D warnings`.
#[cfg(not(target_os = "linux"))]
use crate::core::config::AudioFormat;
// `format()`/`uptime()` and their helpers must compile on every platform
// (including Linux), but the `AudioFormat` import above is gated to keep the
// Linux build warning-clean for `pick_supported_format`. Reference the fully
// qualified path through this always-available alias so the public read path
// is not accidentally tied to the gated import.
use crate::core::config::AudioFormat as AudioFormatType;
use crate::core::error::{AudioError, AudioResult};
use crate::core::introspection::{BackpressureReport, StreamStats};
use std::fmt;
use std::ops::{Deref, DerefMut};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// Re-export AudioCaptureConfig from core::config so downstream code
// that uses `crate::api::AudioCaptureConfig` still resolves.
pub use crate::core::config::AudioCaptureConfig;

/// The whitelist of sample rates the builder accepts. Shared by
/// [`AudioCaptureBuilder::preflight`] and [`AudioCaptureBuilder::build`] (which
/// calls `preflight`) so the two cannot drift. Device negotiation may still
/// land on a different rate the *hardware* advertises; this gate is purely the
/// config-time contract.
///
/// This is an alias for the canonical
/// [`PlatformCapabilities::SUPPORTED_SAMPLE_RATES`] so the builder and the
/// publicly queryable list are a single source of truth — callers can
/// pre-validate against `PlatformCapabilities::SUPPORTED_SAMPLE_RATES` (or
/// [`PlatformCapabilities::supported_sample_rates()`]) and get exactly what
/// `build()` enforces.
const SUPPORTED_SAMPLE_RATES: [u32; 6] = PlatformCapabilities::SUPPORTED_SAMPLE_RATES;

/// The maximum channel count the builder accepts at config time (the valid
/// range is `1..=MAX_CHANNELS`). Mirrors the most permissive backend's ceiling;
/// a narrower per-platform limit is enforced later by `PlatformCapabilities`.
const MAX_CHANNELS: u16 = 32;

/// Capacity, in buffers, of the **bounded** channel behind
/// [`AudioCapture::subscribe`], [`AudioCapture::subscribe_with_errors`], and
/// the `compose` feature's `Composition::subscribe{,_with_errors}`.
///
/// 128 buffers is deliberately the same order of magnitude as the bridge ring
/// itself (the composed ring uses 128 slots; at the typical ~10 ms buffer
/// cadence this is ~1.3 s of audio). A subscriber that stalls longer than that
/// starts **losing** buffers — by design: the crate-wide backpressure policy is
/// *drop, don't block, and count it* (ADR-0007). The previous unbounded
/// `mpsc::channel()` instead grew without limit while `overrun_count()` /
/// `backpressure_report()` read healthy, because the pump drained the ring
/// promptly even when the subscriber never caught up (rsac-d6a8).
pub(crate) const SUBSCRIBE_CHANNEL_CAPACITY: usize = 128;

/// Minimum interval between two forwarded **same-variant** recoverable errors
/// on a [`AudioCapture::subscribe_with_errors`] channel.
///
/// The pump polls at ~1 ms, so a persistently-recoverable stream would
/// otherwise emit ~1000 identical advisory `Err` items per second, flooding
/// the bounded channel and crowding out audio. Repeated recoverable errors of
/// the same variant are therefore coalesced: forwarded at most once per this
/// interval, while an error of a *different* variant is always forwarded
/// immediately (rsac-d6a8).
pub(crate) const RECOVERABLE_ERROR_FORWARD_INTERVAL: Duration = Duration::from_secs(1);

/// A builder for creating [`AudioCapture`] instances.
///
/// This builder allows for a flexible and clear way to specify audio capture parameters.
/// Once all desired parameters are set, call [`build`](AudioCaptureBuilder::build)
/// to validate the configuration and create an [`AudioCapture`] instance.
///
/// ## Example (new API)
///
/// ```rust,no_run
/// # use rsac::api::AudioCaptureBuilder;
/// # use rsac::core::config::{CaptureTarget, SampleFormat};
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let capture = AudioCaptureBuilder::new()
///     .with_target(CaptureTarget::SystemDefault)
///     .sample_rate(48000)
///     .channels(2)
///     .sample_format(SampleFormat::F32)
///     .build()?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct AudioCaptureBuilder {
    target: CaptureTarget,
    config: StreamConfig,
    /// Android `MediaProjection` consent token (rsac-82d4 / ADR-0013).
    /// Present only on Android builds; playback-capture targets need it once
    /// an Android backend advertises the capability.
    #[cfg(target_os = "android")]
    android_projection: Option<AndroidProjectionToken>,
}

impl Default for AudioCaptureBuilder {
    fn default() -> Self {
        Self {
            target: CaptureTarget::SystemDefault,
            config: StreamConfig::default(),
            #[cfg(target_os = "android")]
            android_projection: None,
        }
    }
}

impl AudioCaptureBuilder {
    /// Creates a new `AudioCaptureBuilder` with default settings.
    ///
    /// Defaults: target = `CaptureTarget::SystemDefault`, config = `StreamConfig::default()`
    /// (48 kHz, 2 channels, F32, no buffer size preference).
    pub fn new() -> Self {
        Self::default()
    }

    // ── CaptureTarget-based API ──────────────────────────────────────

    /// Sets the capture target (system default, device, application, …).
    pub fn with_target(mut self, target: CaptureTarget) -> Self {
        self.target = target;
        self
    }

    /// Supplies the Android `MediaProjection` consent token
    /// *(Android targets only)*.
    ///
    /// Android gates playback capture (system / application / process-tree
    /// targets) behind a user-consent dialog whose result the configuration
    /// must carry — see
    /// [`AndroidProjectionToken`](crate::core::config::AndroidProjectionToken)
    /// and `docs/MOBILE_BACKEND_DESIGN.md`. Obtain the token from the rsac
    /// Android consent helper and pass it here **before**
    /// [`build`](Self::build); once an Android backend advertises
    /// `requires_user_consent`, playback-capture targets without a token fail
    /// the preflight with
    /// [`UserConsentRequired`](crate::core::error::AudioError::UserConsentRequired).
    ///
    /// Microphone ([`CaptureTarget::Device`]) targets never need a token, and
    /// supplying one is harmless (it is simply unused).
    #[cfg(target_os = "android")]
    pub fn with_android_projection(mut self, token: AndroidProjectionToken) -> Self {
        self.android_projection = Some(token);
        self
    }

    /// Supplies the App Group identifier for iOS system-audio capture
    /// *(iOS targets only)*.
    ///
    /// iOS serves [`CaptureTarget::SystemDefault`] through a ReplayKit
    /// Broadcast Upload Extension writing into a ring in the **shared App
    /// Group container** (ADR-0013, rsac-b3aa); the App Group id (e.g.
    /// `"group.com.example.myapp.rsac"`) is the config-time consent artifact
    /// this backend needs to find that container — see
    /// [`StreamConfig::ios_app_group`](crate::core::config::StreamConfig::ios_app_group).
    /// Pass it **before** [`build`](Self::build): `SystemDefault` builds
    /// without one fail the preflight with
    /// [`UserConsentRequired`](crate::core::error::AudioError::UserConsentRequired).
    ///
    /// Microphone ([`CaptureTarget::Device`]) targets never need an App
    /// Group, and supplying one is harmless (it is simply unused). The value
    /// is stored on the builder's [`StreamConfig`], so a later
    /// [`with_config`](Self::with_config) replaces it along with everything
    /// else.
    #[cfg(target_os = "ios")]
    pub fn with_ios_app_group(mut self, group: impl Into<String>) -> Self {
        self.config.ios_app_group = Some(group.into());
        self
    }

    // ── Read-only accessors ──────────────────────────────────────────

    /// Returns the capture target configured so far.
    ///
    /// Read-only view of the builder's current target (the
    /// [`CaptureTarget::SystemDefault`] default for a fresh builder). Useful for
    /// inspecting a builder assembled by the [`capture!`](crate::capture) macro
    /// or for pre-flight UI without consuming the builder.
    pub fn target(&self) -> &CaptureTarget {
        &self.target
    }

    /// Returns the desired [`StreamConfig`] configured so far.
    ///
    /// Read-only view of the builder's current sample rate / channels / sample
    /// format / buffer-size preference (the [`StreamConfig::default()`] values
    /// until overridden). The `capture_target` field of the returned config is
    /// not populated until [`build`](Self::build); use [`target`](Self::target)
    /// for the configured target.
    pub fn config(&self) -> &StreamConfig {
        &self.config
    }

    /// Sets the desired stream config in one shot.
    pub fn with_config(mut self, config: StreamConfig) -> Self {
        self.config = config;
        self
    }

    // ── Individual config setters ────────────────────────────────────

    /// Sets the desired sample rate in Hz (e.g., 44100, 48000).
    pub fn sample_rate(mut self, rate: u32) -> Self {
        self.config.sample_rate = rate;
        self
    }

    /// Sets the desired number of audio channels.
    pub fn channels(mut self, channels: u16) -> Self {
        self.config.channels = channels;
        self
    }

    /// Sets the desired sample format.
    pub fn sample_format(mut self, format: SampleFormat) -> Self {
        self.config.sample_format = format;
        self
    }

    /// Sets the desired ring-buffer depth, in **buffers/slots** (not frames).
    ///
    /// This is the number of `AudioBuffer` slots in the SPSC bridge ring, i.e.
    /// how many captured buffers can queue before the producer drops to
    /// back-pressure — see [`StreamConfig::buffer_size`](crate::core::config::StreamConfig::buffer_size)
    /// and ADR-0007. It is honored on **Windows** today; macOS and Linux
    /// currently derive their ring capacity internally. `None` uses the backend
    /// default.
    pub fn buffer_size(mut self, size: Option<usize>) -> Self {
        self.config.buffer_size = size;
        self
    }

    /// Historical name for [`buffer_size`](Self::buffer_size); the value is the
    /// same ring **slot** count (the `_frames` suffix is a misnomer kept for
    /// backward compatibility — it is not a frame count).
    pub fn buffer_size_frames(mut self, size: Option<u32>) -> Self {
        self.config.buffer_size = size.map(|s| s as usize);
        self
    }

    /// Sets the capture target from a canonical string, parsing it via
    /// [`CaptureTarget`]'s [`FromStr`] implementation.
    ///
    /// This is the string-driven counterpart to
    /// [`with_target`](Self::with_target): it lets a CLI flag or config value
    /// (e.g. `"system"`, `"app:1234"`, `"device:hw:0,0"`, `"name:VLC"`,
    /// `"tree:42"`) feed straight into the builder without hand-rolling the
    /// match. The grammar (and its case-insensitive scheme matching) is exactly
    /// the one documented on [`CaptureTarget`]'s `FromStr` impl.
    ///
    /// # Errors
    ///
    /// Returns the parse error ([`AudioError::InvalidParameter`] with
    /// `param == "capture_target"`) for an unrecognized scheme or a malformed
    /// pid. On error the builder's target is **left unchanged** (the method
    /// consumes `self` and only returns the mutated builder on success), so a
    /// caller that ignores the error keeps the previously configured target.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use rsac::api::AudioCaptureBuilder;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let capture = AudioCaptureBuilder::new()
    ///     .target_str("app:1234")?
    ///     .sample_rate(48000)
    ///     .build()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn target_str(self, s: &str) -> AudioResult<Self> {
        // Parse first; only mutate the builder on success so a failed parse
        // never silently changes the target out from under the caller.
        let target = CaptureTarget::from_str(s)?;
        Ok(self.with_target(target))
    }

    /// Infallible variant of [`target_str`](Self::target_str): parses `s` and,
    /// on a parse failure, returns the builder **unchanged** (the bad string is
    /// ignored) rather than surfacing the error.
    ///
    /// Use this when a caller wants "best effort" target selection from a string
    /// and is content to fall back to whatever target was already configured
    /// (the [`CaptureTarget::SystemDefault`] default for a fresh builder). When
    /// the error matters, prefer [`target_str`](Self::target_str).
    pub fn try_target_str(self, s: &str) -> Self {
        match CaptureTarget::from_str(s) {
            Ok(target) => self.with_target(target),
            Err(_) => self,
        }
    }

    /// Runs the cheap, device-independent validations that
    /// [`build`](Self::build) performs **before** it opens a device.
    ///
    /// This lets a caller fail fast on a misconfigured builder (unsupported
    /// platform feature, out-of-range sample rate or channel count) without
    /// paying for — or requiring — device enumeration / stream creation. It is
    /// the single source of truth for those checks: `build()` calls it first,
    /// so `preflight()` returning `Ok(())` guarantees the configuration will not
    /// be rejected for any of these reasons later (it may still fail at the
    /// device-resolution or format-negotiation step that needs hardware).
    ///
    /// The checks are config-time only and have no real-time impact.
    ///
    /// # Errors
    ///
    /// - [`AudioError::PlatformNotSupported`] when the target is
    ///   [`Application`](CaptureTarget::Application) /
    ///   [`ApplicationByName`](CaptureTarget::ApplicationByName) but the platform
    ///   reports `!supports_application_capture`, or
    ///   [`ProcessTree`](CaptureTarget::ProcessTree) but
    ///   `!supports_process_tree_capture`.
    /// - [`AudioError::InvalidParameter`] (`param == "sample_rate"`) when the
    ///   sample rate is not one of the supported rates
    ///   (22050, 32000, 44100, 48000, 88200, 96000).
    /// - [`AudioError::ConfigurationError`] when the channel count is `0` or
    ///   exceeds the maximum (32).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use rsac::api::AudioCaptureBuilder;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let builder = AudioCaptureBuilder::new().sample_rate(48000).channels(2);
    /// builder.preflight()?; // validates without touching any device
    /// # Ok(())
    /// # }
    /// ```
    pub fn preflight(&self) -> AudioResult<()> {
        // ── Validate target against platform capabilities ────────────
        let caps = PlatformCapabilities::query();
        match &self.target {
            CaptureTarget::Application(_) | CaptureTarget::ApplicationByName(_)
                if !caps.supports_application_capture =>
            {
                return Err(AudioError::PlatformNotSupported {
                    feature: "application capture".to_string(),
                    platform: caps.backend_name.to_string(),
                });
            }
            CaptureTarget::ProcessTree(_) if !caps.supports_process_tree_capture => {
                return Err(AudioError::PlatformNotSupported {
                    feature: "process tree capture".to_string(),
                    platform: caps.backend_name.to_string(),
                });
            }
            _ => {}
        }

        // ── Consent preflight (mobile, ADR-0013 / rsac-82d4) ─────────
        // Live as of rsac-77f1 (the Android playback backend): it fires only
        // when the platform *claims* the requested playback-capture tier AND
        // reports `requires_user_consent` AND no token was supplied. The
        // capability checks above dominate, so a build with no compiled-in
        // backend still reports PlatformNotSupported — never a misleading
        // consent error.
        #[cfg(target_os = "android")]
        {
            let target_needs_consent = match &self.target {
                CaptureTarget::SystemDefault => caps.supports_system_capture,
                CaptureTarget::Application(_) | CaptureTarget::ApplicationByName(_) => {
                    caps.supports_application_capture
                }
                CaptureTarget::ProcessTree(_) => caps.supports_process_tree_capture,
                // Microphone capture never needs a projection token.
                CaptureTarget::Device(_) => false,
            };
            if caps.requires_user_consent
                && target_needs_consent
                && self.android_projection.is_none()
            {
                return Err(AudioError::UserConsentRequired {
                    feature: "Android playback capture".to_string(),
                    missing: "MediaProjection token — obtain one via the rsac Android \
                              consent helper and pass it to \
                              AudioCaptureBuilder::with_android_projection()"
                        .to_string(),
                });
            }
        }

        // ── Consent preflight (iOS, ADR-0013 / rsac-b3aa) ────────────
        // Mirror of the Android block above: it fires only when the platform
        // *claims* the requested capture tier AND reports
        // `requires_user_consent` AND no App Group was supplied. The
        // capability checks above dominate (the per-app arms below are
        // unreachable on iOS today, where those tiers are permanently
        // unsupported), so a build with no compiled-in backend still reports
        // PlatformNotSupported — never a misleading consent error.
        #[cfg(target_os = "ios")]
        {
            let target_needs_consent = match &self.target {
                CaptureTarget::SystemDefault => caps.supports_system_capture,
                CaptureTarget::Application(_) | CaptureTarget::ApplicationByName(_) => {
                    caps.supports_application_capture
                }
                CaptureTarget::ProcessTree(_) => caps.supports_process_tree_capture,
                // Microphone capture never needs the App Group artifact.
                CaptureTarget::Device(_) => false,
            };
            if caps.requires_user_consent
                && target_needs_consent
                && self.config.ios_app_group.is_none()
            {
                return Err(AudioError::UserConsentRequired {
                    feature: "iOS broadcast capture".to_string(),
                    missing: "App Group identifier — call \
                              AudioCaptureBuilder::with_ios_app_group(\"group.…\") and \
                              embed the RsacBroadcastKit extension"
                        .to_string(),
                });
            }
        }

        // ── Validate sample rate ────────────────────────────────────
        if !SUPPORTED_SAMPLE_RATES.contains(&self.config.sample_rate) {
            return Err(AudioError::InvalidParameter {
                param: "sample_rate".into(),
                reason: format!(
                    "Unsupported sample rate: {} Hz. Supported: {}",
                    self.config.sample_rate,
                    PlatformCapabilities::supported_sample_rates_display()
                ),
            });
        }

        // ── Validate channels ───────────────────────────────────────
        if self.config.channels == 0 {
            return Err(AudioError::ConfigurationError {
                message: "Channels must be greater than 0.".to_string(),
            });
        }
        if self.config.channels > MAX_CHANNELS {
            return Err(AudioError::ConfigurationError {
                message: format!(
                    "Number of channels ({}) exceeds the maximum supported ({}).",
                    self.config.channels, MAX_CHANNELS
                ),
            });
        }

        Ok(())
    }

    /// Validates settings and constructs an [`AudioCapture`] instance.
    ///
    /// Runs [`preflight`](Self::preflight) first (capability + sample-rate +
    /// channel-count checks), then resolves the device and negotiates the
    /// format. The device-independent validations are single-sourced in
    /// `preflight()` so the two entry points cannot drift apart.
    pub fn build(self) -> AudioResult<AudioCapture> {
        // ── Device-independent validation (single-sourced) ──────────
        self.preflight()?;

        // ── Build capture config ────────────────────────────────────
        let mut stream_config = self.config;
        stream_config.capture_target = self.target.clone();
        // Propagate the Android consent token into the stream config so the
        // backend's create_stream can resolve it back through JNI
        // (rsac-77f1); mirrors the ios_app_group plumbing.
        #[cfg(target_os = "android")]
        {
            stream_config.android_projection = self.android_projection;
        }
        #[allow(unused_mut)] // mutated only in the non-Linux negotiation block
        let mut capture_config = AudioCaptureConfig {
            target: self.target,
            stream_config,
        };

        // ── Resolve device from target ──────────────────────────────
        let selected_device = Self::resolve_target_device(&capture_config.target)?;

        // ── Format negotiation (non-Linux) ──────────────────────────
        // Devices advertise a fixed set of formats via WASAPI / CoreAudio. If
        // the exact requested format isn't on offer, negotiate to the closest
        // supported one (prefer the requested sample rate, then an F32 sample
        // type) instead of hard-failing — consumers resample/downmix
        // downstream anyway, and the alternative is that perfectly capturable
        // devices (e.g. a virtual surround endpoint that only advertises
        // 8ch/96000, or a 44.1kHz-only interface) are unusable. Only error if
        // the device advertises no formats at all. Single-sourced with
        // `negotiated_format()` via `negotiate_device_format()` so the format a
        // pre-build query reports cannot drift from the one `build()` applies.
        #[cfg(not(target_os = "linux"))]
        {
            let requested = capture_config.stream_config.to_audio_format();
            let negotiated = negotiate_device_format(selected_device.as_ref(), &requested)?;
            if negotiated != requested {
                capture_config.stream_config.sample_rate = negotiated.sample_rate;
                capture_config.stream_config.channels = negotiated.channels;
                capture_config.stream_config.sample_format = negotiated.sample_format;
            }
        }

        Ok(AudioCapture {
            config: capture_config,
            device: Some(selected_device),
            stream: None,
            callback: Mutex::new(None),
            callback_pump: None,
            start_instant: None,
            subscriber_dropped: Arc::new(AtomicU64::new(0)),
        })
    }

    /// Resolves the [`AudioDevice`](crate::core::interface::AudioDevice) a given
    /// [`CaptureTarget`] maps to, exactly as [`build`](Self::build) does.
    ///
    /// Shared by [`build`](Self::build) and [`negotiated_format`](Self::negotiated_format)
    /// so the two cannot diverge on which device a target selects. Takes the
    /// target by reference and returns the boxed device; does not mutate the
    /// builder. This is device enumeration / resolution only — no stream is
    /// created and no real-time path is touched.
    fn resolve_target_device(
        target: &CaptureTarget,
    ) -> AudioResult<Box<dyn crate::core::interface::AudioDevice>> {
        let enumerator = get_device_enumerator()?;
        let selected_device = match target {
            CaptureTarget::SystemDefault => {
                // All backends return the default output device (used for loopback capture).
                enumerator
                    .default_device()
                    .map_err(|e| AudioError::DeviceEnumerationError {
                        reason: format!("Failed to get default device: {}", e),
                        context: None,
                    })?
            }
            CaptureTarget::Device(device_id) => {
                let devices = enumerator.enumerate_devices()?;
                let device = devices
                    .into_iter()
                    .find(|d| d.id() == *device_id)
                    .ok_or_else(|| AudioError::DeviceNotFound {
                        device_id: device_id.0.clone(),
                    })?;
                // Warn users that targeting an output device for capture may
                // not produce data on all platforms.  System capture or
                // Process Tap loopback is required for output-device audio.
                log::info!(
                    "Device capture targeting '{}' (id: {}). Note: if this is \
                     an output-only device, consider using CaptureTarget::SystemDefault \
                     for loopback capture.",
                    device.name(),
                    device_id
                );
                device
            }
            CaptureTarget::Application(_)
            | CaptureTarget::ApplicationByName(_)
            | CaptureTarget::ProcessTree(_) => {
                // Application capture typically uses the default output device
                enumerator
                    .default_device()
                    .map_err(|e| AudioError::DeviceEnumerationError {
                        reason: format!(
                            "Failed to get default output device for app capture: {}",
                            e
                        ),
                        context: None,
                    })?
            }
        };
        Ok(selected_device)
    }

    /// Returns the `AudioFormat` that
    /// [`build`](Self::build) would deliver for this configuration **without**
    /// constructing an [`AudioCapture`] or opening a stream (AEG-8, rsac-0113).
    ///
    /// This lets a downstream learn the *resolved* delivery format ahead of time
    /// — e.g. to pre-size resamplers or downmix tables — without re-enumerating
    /// devices and re-implementing the negotiation policy. It runs the same
    /// [`preflight`](Self::preflight) checks, resolves the same device via the
    /// shared `resolve_target_device` helper, and applies the same
    /// closest-match negotiation (`negotiate_device_format`) that `build()`
    /// uses, so the two cannot drift.
    ///
    /// The returned `sample_format` reflects the *device-advertised* format
    /// chosen at negotiation. Note that the bridge always delivers interleaved
    /// f32 at runtime, so the post-`start()` observable
    /// [`AudioCapture::format`] reports `F32`; this pre-build query instead
    /// reports the endpoint's advertised sample type for the negotiated
    /// rate/channels, which is the input to that conversion.
    ///
    /// # Platform behaviour (negotiation timing)
    ///
    /// - **Windows (WASAPI) / macOS (CoreAudio):** the device advertises a fixed
    ///   set of formats, so negotiation is resolvable at config time and this
    ///   returns the negotiated `AudioFormat`.
    /// - **Linux (PipeWire):** the delivered format is negotiated at
    ///   **stream-open** (in the `param_changed` callback), *after* `build()`,
    ///   so it cannot be known pre-build. This method returns
    ///   [`AudioError::PlatformNotSupported`] there; query
    ///   [`AudioCapture::format`] after [`start`](AudioCapture::start) instead.
    ///
    /// # Errors
    ///
    /// - Any error [`preflight`](Self::preflight) raises (unsupported platform
    ///   feature, out-of-range sample rate or channel count).
    /// - [`AudioError::DeviceEnumerationError`] / [`AudioError::DeviceNotFound`]
    ///   if the target device cannot be resolved.
    /// - [`AudioError::UnsupportedFormat`] if the resolved device advertises no
    ///   usable formats at all (the same hard-fail `build()` would hit).
    /// - [`AudioError::PlatformNotSupported`] on Linux (see above).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use rsac::api::AudioCaptureBuilder;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let _builder = AudioCaptureBuilder::new().sample_rate(48000).channels(2);
    /// # #[cfg(not(target_os = "linux"))]
    /// let _resolved = _builder.negotiated_format()?; // no AudioCapture built
    /// # Ok(())
    /// # }
    /// ```
    pub fn negotiated_format(&self) -> AudioResult<AudioFormatType> {
        // Same device-independent validation build() runs first.
        self.preflight()?;

        #[cfg(not(target_os = "linux"))]
        {
            // to_audio_format() reads only rate/channels/sample_format, so the
            // builder's config gives the requested format directly — no need to
            // stamp capture_target first (build() does that only for the stored
            // AudioCaptureConfig, which this query does not construct).
            let requested = self.config.to_audio_format();
            let selected_device = Self::resolve_target_device(&self.target)?;
            negotiate_device_format(selected_device.as_ref(), &requested)
        }

        // On Linux the delivered format is only known at stream-open; document
        // and surface that rather than guessing a pre-build value.
        #[cfg(target_os = "linux")]
        {
            Err(AudioError::PlatformNotSupported {
                feature: "pre-build negotiated_format query (Linux negotiates at \
                          stream-open; read AudioCapture::format() after start())"
                    .to_string(),
                platform: "linux".to_string(),
            })
        }
    }

    /// Builds **and** starts a capture in one call, returning a
    /// [`RunningCapture`] RAII guard.
    ///
    /// This collapses the usual two-step (`let mut c = builder.build()?;
    /// c.start()?;`) into a single fallible call. The returned guard
    /// [`Deref`]s/[`DerefMut`]s to the wrapped [`AudioCapture`], so every read /
    /// subscribe / stats method (e.g. [`read_buffer`](AudioCapture::read_buffer),
    /// [`stream_stats`](AudioCapture::stream_stats),
    /// [`is_running`](AudioCapture::is_running)) is reachable through it, and
    /// dropping the guard stops the capture deterministically.
    ///
    /// [`build`](Self::build) and [`AudioCapture::start`] remain public and
    /// unchanged for callers who want to hold the capture themselves or defer
    /// `start()`.
    ///
    /// # Errors
    ///
    /// Surfaces any error from [`build`](Self::build) (validation, device
    /// resolution, format negotiation) or from
    /// [`AudioCapture::start`] (stream creation / callback-pump spawn). On a
    /// `start()` failure the partially built [`AudioCapture`] is dropped, whose
    /// `Drop` best-effort stops any stream it managed to create — so a failed
    /// `start()` does not leak a half-running stream.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use rsac::api::AudioCaptureBuilder;
    /// # use rsac::core::config::CaptureTarget;
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut capture = AudioCaptureBuilder::new()
    ///     .with_target(CaptureTarget::SystemDefault)
    ///     .start()?; // builds, starts, and wraps in a RAII guard
    /// // `capture` derefs to AudioCapture (DerefMut for &mut self methods):
    /// if let Some(buffer) = capture.read_buffer()? {
    ///     let _frames = buffer.data().len();
    /// }
    /// // Dropping `capture` stops the stream automatically.
    /// # Ok(())
    /// # }
    /// ```
    pub fn start(self) -> AudioResult<RunningCapture> {
        let mut capture = self.build()?;
        // If start() fails, `capture` drops here; its Drop best-effort stops any
        // stream that was created, so we never leak a half-started capture.
        capture.start()?;
        Ok(RunningCapture(capture))
    }
}

/// An RAII guard wrapping a started [`AudioCapture`].
///
/// Returned by [`AudioCaptureBuilder::start()`]. It exists so that the common
/// "build, start, then use" path is a single call whose result also tears the
/// capture down deterministically when it goes out of scope.
///
/// # Deref
///
/// `RunningCapture` implements [`Deref`]`<Target = `[`AudioCapture`]`>` and
/// [`DerefMut`], so the full [`AudioCapture`] surface (reads, subscriptions,
/// stats, `stop`, …) is callable directly on the guard. There is no wrapper
/// boilerplate to keep in sync — new `AudioCapture` methods are reachable
/// automatically.
///
/// # Drop
///
/// Dropping the guard calls [`AudioCapture::stop`] once. `stop()` is idempotent
/// and `Drop`-safe (calling it explicitly *and* then dropping does not error
/// and does not double-stop the underlying stream — the second call is a no-op
/// once the stream is gone). The guard uses a plain [`Drop`] impl rather than
/// keeping the inner capture in a `ManuallyDrop` and reconstructing teardown by
/// hand: the wrapped `AudioCapture`'s own `Drop` already best-effort stops the
/// stream, and this guard's `Drop` simply makes the stop explicit and
/// authoritative on the ergonomic path.
///
/// Use [`into_inner`](Self::into_inner) to take ownership of the wrapped
/// [`AudioCapture`] without triggering the guard's stop.
pub struct RunningCapture(AudioCapture);

impl RunningCapture {
    /// Drains captured audio into an [`AudioSink`](crate::sink::AudioSink) on a
    /// dedicated background thread, returning a [`DrainHandle`] that owns it.
    ///
    /// This is the library's first path that actually *drives* the
    /// [`AudioSink`](crate::sink::AudioSink) abstraction. It spawns a thread
    /// (named `rsac-drain`) that **owns** `sink`, reads buffers from the same
    /// ring the manual reads use via the *terminal-observable*
    /// [`try_read_chunk`](crate::core::interface::CapturingStream::try_read_chunk)
    /// path, and for each captured buffer calls
    /// [`sink.write(&buf)`](crate::sink::AudioSink::write). When the stream
    /// reaches a **fatal terminal** state (e.g. [`AudioError::StreamEnded`]) the
    /// loop exits and the thread calls [`sink.flush()`](crate::sink::AudioSink::flush)
    /// then [`sink.close()`](crate::sink::AudioSink::close) so the sink is
    /// finalized exactly once.
    ///
    /// The pattern mirrors the callback pump (`spawn_callback_pump`): the sink
    /// runs on this dedicated thread, **never** the OS real-time audio thread, so
    /// a slow sink (disk I/O, encoding) only delays draining — it never stalls
    /// the audio callback (ADR-0001).
    ///
    /// # Error handling inside the drain loop
    ///
    /// - A **recoverable** read error (transient
    ///   [`AudioError::StreamReadError`], overrun/underrun) is logged and retried
    ///   — it never ends draining.
    /// - A **fatal** read error ends the loop, after which `flush()`/`close()`
    ///   still run so a file sink's header is finalized.
    /// - A **recoverable** `write()` error is logged and the loop continues; a
    ///   **fatal** `write()` error (e.g. a `WavFileSink` format mismatch, or
    ///   disk-full mapped to a fatal `AudioError`) ends the loop — but
    ///   `flush()`/`close()` are still attempted so the sink is left in the
    ///   most-finalized state possible.
    ///
    /// # Lifecycle
    ///
    /// The returned [`DrainHandle`] joins the thread on
    /// [`shutdown()`](DrainHandle::shutdown) or `Drop` (signal + join,
    /// self-join-guarded like the callback pump). Because the stream reaching a
    /// terminal state ends the loop on its own, you do not have to call
    /// `shutdown()` for the thread to exit — but doing so (or dropping the
    /// handle) joins it deterministically and guarantees `flush()`/`close()` have
    /// completed.
    ///
    /// # Competes with other readers
    ///
    /// The drain thread competes with [`read_buffer`](AudioCapture::read_buffer),
    /// [`subscribe`](AudioCapture::subscribe), and the callback pump for buffers
    /// from the same ring (a single logical consumer per buffer). **Do not** mix
    /// `drain_to` with manual reads on the same capture.
    ///
    /// # Accepted stream states (drain-the-tail; rsac-7aa2)
    ///
    /// Accepted whenever a stream exists: `Running`, the drainable `Stopping`
    /// window (a gracefully-ended stream's buffered tail is drained into the
    /// sink before finalization), or even an already-terminal stream — the
    /// drain thread then exits on its first read and still finalizes the sink
    /// (`flush()` then `close()`), so e.g. a WAV header is written for an empty
    /// capture. The gate is stream **presence** only, chosen over a
    /// `Running || Stopping` state check because (a) `CapturingStream` cannot
    /// distinguish `Stopping` from terminal without consuming a buffer, and
    /// (b) a state check here merely races the stream's own lifecycle — the
    /// drain loop already handles every state honestly, so rejecting at the
    /// gate could strand a drainable tail while accepting adds no hazard.
    /// `Composition::drain_to` (behind the `compose` feature) applies the same
    /// policy, so the two handles agree.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::StreamReadError`] if the capture has no stream
    /// (never started, or stopped — `stop()` releases the stream), or
    /// [`AudioError::InternalError`] if the drain thread cannot be spawned.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[cfg(feature = "sink-wav")]
    /// # fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use rsac::{AudioCaptureBuilder, CaptureTarget};
    /// use rsac::core::config::{AudioFormat, SampleFormat};
    /// use rsac::sink::WavFileSink;
    ///
    /// let capture = AudioCaptureBuilder::new()
    ///     .with_target(CaptureTarget::SystemDefault)
    ///     .start()?;
    /// let format = capture.format().unwrap_or(AudioFormat {
    ///     sample_rate: 48000,
    ///     channels: 2,
    ///     sample_format: SampleFormat::F32,
    /// });
    /// let sink = WavFileSink::new("capture.wav", &format)?;
    /// let drain = capture.drain_to(sink)?;
    /// std::thread::sleep(std::time::Duration::from_secs(2));
    /// drain.shutdown(); // flushes + closes the sink, joins the thread
    /// # Ok(())
    /// # }
    /// # #[cfg(not(feature = "sink-wav"))]
    /// # fn main() {}
    /// ```
    pub fn drain_to<S>(&self, sink: S) -> AudioResult<DrainHandle>
    where
        S: crate::sink::AudioSink + 'static,
    {
        // Presence gate only (rsac-7aa2, same policy as subscribe()): Running
        // AND the drainable Stopping window are accepted — drain-the-tail. On a
        // stream already at its fatal terminal the drain thread exits on its
        // first read and finalizes (flush + close) the sink immediately, which
        // is the honest outcome of racing a natural end. Read via `&self`
        // (DerefMut not needed) so we never form a `&mut` alias to the handle —
        // the drain thread only needs a stream Arc clone.
        let stream_ref = self
            .0
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: "No active stream: the capture was never started, or has been \
                         stopped (stop() releases the stream). Call start() to begin \
                         (or restart) capturing."
                    .to_string(),
            })?;
        spawn_drain_thread(Arc::clone(stream_ref), sink)
    }

    /// Consumes the guard and returns the wrapped [`AudioCapture`], **without**
    /// stopping it.
    ///
    /// Use this to escape the RAII lifecycle — e.g. to move the capture into a
    /// longer-lived owner or to manage `stop()` manually. Because the returned
    /// `AudioCapture` is moved out before the guard's [`Drop`] runs, the guard
    /// does **not** stop the capture; the caller becomes responsible for its
    /// lifecycle (the `AudioCapture`'s own `Drop` still best-effort stops it).
    pub fn into_inner(self) -> AudioCapture {
        // Move the AudioCapture out of the guard without running RunningCapture's
        // Drop (which would stop the still-running capture). We destructure via
        // ptr::read under ManuallyDrop so the guard's Drop is suppressed but the
        // AudioCapture itself is NOT dropped — it is returned to the caller.
        let this = std::mem::ManuallyDrop::new(self);
        // SAFETY: `this` is a ManuallyDrop, so RunningCapture::drop never runs.
        // We read the single field out exactly once and never touch `this.0`
        // again, so there is no double-move and no double-drop. `this` itself is
        // a ManuallyDrop wrapping a now-moved-from value, so it is never dropped.
        unsafe { std::ptr::read(&this.0) }
    }
}

impl Deref for RunningCapture {
    type Target = AudioCapture;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for RunningCapture {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for RunningCapture {
    fn drop(&mut self) {
        // Deterministic teardown. `stop()` is idempotent and Drop-safe, so an
        // explicit `stop()` before this drop (or the wrapped AudioCapture's own
        // Drop running afterwards) does not double-stop or error.
        let _ = self.0.stop();
    }
}

impl fmt::Debug for RunningCapture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RunningCapture").field(&self.0).finish()
    }
}

/// Shared drain-thread spawner behind [`RunningCapture::drain_to`] and the
/// `compose` feature's `Composition::drain_to`: reads the stream's
/// *terminal-observable* `try_read_chunk` path on a dedicated `rsac-drain`
/// thread that **owns** the sink, writing each buffer and finalizing
/// (`flush()` then `close()`) exactly once when the loop exits.
///
/// Policy (identical for both callers):
/// - recoverable read/write errors are logged and retried — they never end
///   draining;
/// - a **fatal** read error (e.g. the terminal
///   [`AudioError::StreamEnded`]) or a fatal write error ends the loop, after
///   which `flush()`/`close()` still run so a file sink's header is written;
/// - `Ok(None)` sleeps ~1 ms to avoid busy-spinning (mirrors the callback
///   pump's idle poll).
pub(crate) fn spawn_drain_thread<S>(
    stream: Arc<dyn crate::core::interface::CapturingStream>,
    mut sink: S,
) -> AudioResult<DrainHandle>
where
    S: crate::sink::AudioSink + 'static,
{
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_thread = Arc::clone(&stop_flag);
    let handle = std::thread::Builder::new()
        .name("rsac-drain".into())
        .spawn(move || {
            loop {
                if stop_flag_thread.load(Ordering::SeqCst) {
                    break;
                }
                match stream.try_read_chunk() {
                    // The drain thread OWNS `sink`, so no lock is held while
                    // user sink code runs (it may block on I/O without
                    // stalling anything else).
                    Ok(Some(buffer)) => {
                        if let Err(e) = sink.write(&buffer) {
                            if e.is_fatal() {
                                log::error!(
                                    "Drain sink write failed fatally; \
                                     stopping drain: {:?}",
                                    e
                                );
                                break;
                            }
                            // Recoverable write error: log and keep draining
                            // (mirrors the read-side recoverable policy).
                            log::warn!("Drain sink write error (continuing): {:?}", e);
                        }
                    }
                    Ok(None) => {
                        // No data right now — avoid busy-spinning. Mirrors
                        // spawn_callback_pump's idle poll.
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    // Only a FATAL read error (e.g. StreamEnded — terminal)
                    // ends the drain. A recoverable read error is logged and
                    // retried, mirroring the callback pump and subscribe().
                    Err(e) if e.is_fatal() => break,
                    Err(e) => {
                        log::warn!("Drain pump read error (retrying): {:?}", e);
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            }
            // Loop exited (terminal stream, fatal write, or stop signal):
            // finalize the sink. flush() then close() so a file sink's header
            // is written even on the error paths. Both are best-effort here
            // because the thread cannot return a Result; log any failure.
            if let Err(e) = sink.flush() {
                log::error!("Drain sink flush failed: {:?}", e);
            }
            if let Err(e) = sink.close() {
                log::error!("Drain sink close failed: {:?}", e);
            }
        })
        .map_err(|e| AudioError::InternalError {
            message: format!("Failed to spawn drain thread: {}", e),
            source: None,
        })?;

    Ok(DrainHandle {
        stop_flag,
        handle: Some(handle),
    })
}

/// Shared push-subscription pump behind [`AudioCapture::subscribe`] and the
/// `compose` feature's `Composition::subscribe`: a background `rsac-subscribe`
/// thread reads the stream's terminal-observable `try_read_chunk` path and
/// forwards each buffer over a **bounded** [`mpsc`] channel
/// ([`SUBSCRIBE_CHANNEL_CAPACITY`] buffers).
///
/// Policy (identical for both callers; mirrors the callback pump and the
/// iterator):
/// - buffers are forwarded with a **non-blocking** `try_send`: when the
///   subscriber has stalled long enough to fill the channel, the buffer is
///   **dropped and counted** in `dropped` — the crate-wide "drop, don't
///   block, and count it" backpressure policy. An unbounded channel would
///   instead grow memory without limit while the ring-side `overrun_count()`
///   read healthy (rsac-d6a8);
/// - only a **fatal** terminal (e.g. [`AudioError::StreamEnded`]) ends the
///   subscription — the channel then disconnects, which the receiver observes
///   as a [`RecvError`](std::sync::mpsc::RecvError);
/// - a **recoverable** read error is logged and retried, never ending delivery
///   (FH-1/BP-6);
/// - a momentarily-empty ring sleeps ~1 ms (the documented latency floor);
/// - a dropped receiver ends the thread (observed on every send attempt,
///   including the `Full` path's `Disconnected` variant).
pub(crate) fn spawn_subscribe_thread(
    stream: Arc<dyn crate::core::interface::CapturingStream>,
    dropped: Arc<AtomicU64>,
) -> AudioResult<mpsc::Receiver<AudioBuffer>> {
    let (tx, rx) = mpsc::sync_channel(SUBSCRIBE_CHANNEL_CAPACITY);

    std::thread::Builder::new()
        .name("rsac-subscribe".into())
        .spawn(move || loop {
            match stream.try_read_chunk() {
                Ok(Some(buffer)) => match tx.try_send(buffer) {
                    Ok(()) => {}
                    // Subscriber slower than real time: the bounded channel is
                    // full. Drop the buffer (never block the pump, never queue
                    // unbounded memory) and count the loss so it is visible via
                    // subscriber_dropped_count() — the ring-side counters can't
                    // see it, because the pump drained the ring successfully.
                    Err(mpsc::TrySendError::Full(_)) => {
                        dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => break, // Receiver dropped
                },
                Ok(None) => {
                    // No data available, sleep briefly to avoid busy-spinning
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                // Only a FATAL terminal (e.g. StreamEnded) ends the
                // subscription — the channel then disconnects, which the
                // receiver observes as a RecvError. A recoverable read error
                // (transient StreamReadError, BufferOverrun/Underrun) must
                // NOT kill delivery; log and retry after a brief pause,
                // mirroring the callback pump (spawn_callback_pump) and the
                // iterator. This closes the prior bug where ANY error broke
                // the loop and silently ended the subscription (FH-1/BP-6).
                Err(e) if e.is_fatal() => break,
                Err(e) => {
                    log::warn!("Subscribe thread read error (retrying): {:?}", e);
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        })
        .map_err(|e| AudioError::InternalError {
            message: format!("Failed to spawn subscribe thread: {}", e),
            source: None,
        })?;

    Ok(rx)
}

/// Error-carrying sibling of [`spawn_subscribe_thread`] behind
/// [`AudioCapture::subscribe_with_errors`] and the `compose` feature's
/// `Composition::subscribe_with_errors`: each item is an
/// [`AudioResult<AudioBuffer>`], and the **fatal terminal** error is delivered
/// as the final channel item *before* the disconnect, so the consumer never
/// has to race a bare `RecvError` to learn why the stream ended.
///
/// On top of [`spawn_subscribe_thread`]'s bounded-channel policy (`try_send`
/// buffers; on `Full` drop-and-count into `dropped`):
/// - repeated **recoverable** errors are coalesced: a same-variant error is
///   forwarded at most once per [`RECOVERABLE_ERROR_FORWARD_INTERVAL`] (a
///   persistently-recoverable stream errors once per ~1 ms poll and would
///   otherwise flood the channel with identical advisory items); an error of a
///   *different* variant is always forwarded immediately. A recoverable error
///   item that meets a full channel is simply not forwarded that round (it is
///   advisory; it is **not** counted as a dropped buffer);
/// - the **fatal terminal** is sent with a **blocking** `send` so it can never
///   be lost to the `Full` path: the pump exits right after, so blocking on
///   this one final item is acceptable, and delivery is guaranteed unless the
///   receiver is already gone (in which case nobody is listening anyway).
pub(crate) fn spawn_subscribe_with_errors_thread(
    stream: Arc<dyn crate::core::interface::CapturingStream>,
    dropped: Arc<AtomicU64>,
) -> AudioResult<mpsc::Receiver<AudioResult<AudioBuffer>>> {
    let (tx, rx) = mpsc::sync_channel(SUBSCRIBE_CHANNEL_CAPACITY);

    std::thread::Builder::new()
        .name("rsac-subscribe-err".into())
        .spawn(move || {
            // Coalescing state: the variant of the last forwarded recoverable
            // error and when it was forwarded. Variant identity (discriminant)
            // rather than full equality keeps the hot path allocation-free.
            let mut last_recoverable: Option<(std::mem::Discriminant<AudioError>, Instant)> = None;
            loop {
                match stream.try_read_chunk() {
                    Ok(Some(buffer)) => match tx.try_send(Ok(buffer)) {
                        Ok(()) => {}
                        // Full channel: drop the buffer, count it (see
                        // spawn_subscribe_thread — same policy).
                        Err(mpsc::TrySendError::Full(_)) => {
                            dropped.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(mpsc::TrySendError::Disconnected(_)) => break, // Receiver dropped
                    },
                    Ok(None) => {
                        // No data available, sleep briefly to avoid busy-spinning
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    // Fatal terminal: forward the error as the FINAL item, THEN
                    // exit. This is deliberately the BLOCKING send: the channel
                    // may be full of unread buffers, and the terminal must never
                    // be silently lost to the Full path (the consumer would then
                    // see a bare disconnect and never learn why the stream
                    // ended). Blocking here is fine — the pump exits right after
                    // this send, and send() errors out (ignored) if the receiver
                    // is already gone.
                    Err(e) if e.is_fatal() => {
                        let _ = tx.send(Err(e));
                        break;
                    }
                    // Recoverable: surface it (best-effort, coalesced) AND keep
                    // delivering — a transient hiccup must not end the
                    // subscription.
                    Err(e) => {
                        let variant = std::mem::discriminant(&e);
                        let now = Instant::now();
                        let forward = match last_recoverable {
                            Some((last_variant, last_at)) => {
                                variant != last_variant
                                    || now.duration_since(last_at)
                                        >= RECOVERABLE_ERROR_FORWARD_INTERVAL
                            }
                            None => true,
                        };
                        if forward {
                            // Advisory item: non-blocking. If the channel is
                            // full the error is not forwarded this round (and
                            // NOT counted as a dropped buffer); a disconnect
                            // still ends the pump.
                            match tx.try_send(Err(e)) {
                                Ok(()) => {
                                    // rsac-0055: arm the coalescing cooldown
                                    // only on a DELIVERED error — a Full drop
                                    // must not suppress the next report.
                                    last_recoverable = Some((variant, now));
                                }
                                Err(mpsc::TrySendError::Full(_)) => {}
                                Err(mpsc::TrySendError::Disconnected(_)) => {
                                    break; // Receiver dropped
                                }
                            }
                        }
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            }
        })
        .map_err(|e| AudioError::InternalError {
            message: format!("Failed to spawn subscribe thread: {}", e),
            source: None,
        })?;

    Ok(rx)
}

/// Handle to the background thread spawned by
/// [`RunningCapture::drain_to`](RunningCapture::drain_to).
///
/// The thread owns the [`AudioSink`](crate::sink::AudioSink) and drains captured
/// buffers into it until the stream reaches a terminal state (or this handle is
/// shut down / dropped). On exit the thread flushes and closes the sink, so a
/// file sink's header is finalized exactly once.
///
/// Modeled on the callback pump's handle: it holds a stop flag and the thread's
/// [`JoinHandle`](std::thread::JoinHandle), and joins on
/// [`shutdown`](Self::shutdown) / [`Drop`] (self-join-guarded, so a join from the
/// drain thread itself is skipped rather than dead-locking). Because the loop
/// also exits on its own when the stream becomes terminal, you do not strictly
/// have to call `shutdown()` for the thread to end — but doing so joins it
/// deterministically and guarantees the sink's `flush()`/`close()` have run.
pub struct DrainHandle {
    stop_flag: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl DrainHandle {
    /// Signal the drain thread to stop and join it. Idempotent.
    ///
    /// Sets the stop flag (so the loop breaks on its next pass, after which the
    /// thread flushes + closes the sink) and joins the thread so the finalize
    /// has completed by the time this returns. If called from the drain thread
    /// itself the join is skipped — a thread cannot join itself — and the
    /// `JoinHandle` is retained so a later `shutdown()`/`Drop` from another
    /// thread can still join it (mirrors `CallbackPump::shutdown`).
    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    /// Internal shutdown shared by [`shutdown`](Self::shutdown) and [`Drop`].
    /// Takes `&mut self` so `Drop` can call it; the public `shutdown` consumes
    /// the handle so it cannot be used after finalizing.
    fn shutdown_inner(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            if handle.thread().id() == std::thread::current().id() {
                // Re-entrant teardown from the drain thread: don't join self.
                // The stop flag is set; the loop will break on its next pass.
                // Put the handle back so a later shutdown()/Drop from another
                // thread can still join it deterministically.
                self.handle = Some(handle);
            } else {
                let _ = handle.join();
            }
        }
    }
}

impl Drop for DrainHandle {
    fn drop(&mut self) {
        // Deterministic teardown: signal + join (self-join-guarded). Ensures the
        // drain thread is never leaked and the sink's flush()/close() have run.
        self.shutdown_inner();
    }
}

impl fmt::Debug for DrainHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DrainHandle")
            .field("stopping", &self.stop_flag.load(Ordering::Relaxed))
            .field("joined", &self.handle.is_none())
            .finish()
    }
}

/// Resolve the delivery [`AudioFormat`] a device will produce for `requested`,
/// applying the same closest-match negotiation [`build`](AudioCaptureBuilder::build)
/// uses.
///
/// Single source of truth shared by [`build`](AudioCaptureBuilder::build) and
/// [`negotiated_format`](AudioCaptureBuilder::negotiated_format) so a pre-build
/// query and the actual build cannot drift:
///
/// - If the device advertises no formats at all, returns the requested format
///   unchanged (the backend will open with the request as-is; nothing to
///   negotiate against).
/// - If the device advertises the exact requested format, returns it.
/// - Otherwise returns the closest supported format per
///   [`pick_supported_format`]'s preference order.
/// - Returns [`AudioError::UnsupportedFormat`] only when the device advertises
///   some formats but [`pick_supported_format`] still finds nothing usable
///   (which it does not for a non-empty list, but the hard-fail is preserved to
///   match `build()`'s contract exactly).
///
/// The device name is read via the trait object only for the error/log
/// message; this is a config-time path and touches no real-time code.
#[cfg(not(target_os = "linux"))]
fn negotiate_device_format(
    device: &dyn crate::core::interface::AudioDevice,
    requested: &AudioFormat,
) -> AudioResult<AudioFormat> {
    let supported = device.supported_formats();
    // No advertised formats → nothing to negotiate against; the backend opens
    // with the request as-is. (build() only hard-fails on a non-empty list that
    // yields no usable pick, which pick_supported_format never does.)
    if supported.is_empty() {
        return Ok(requested.clone());
    }
    if supported.contains(requested) {
        return Ok(requested.clone());
    }
    match pick_supported_format(&supported, requested) {
        Some(f) => {
            log::warn!(
                "Device '{}' does not support requested format {:?}; negotiated to {:?}",
                device.name(),
                requested,
                f
            );
            Ok(f)
        }
        None => Err(AudioError::UnsupportedFormat {
            format: format!(
                "The selected device '{}' advertises no usable audio formats (requested {:?})",
                device.name(),
                requested
            ),
            context: None,
        }),
    }
}

/// Pick a device-supported format closest to `requested`.
///
/// Used by [`AudioCaptureBuilder::build()`] to negotiate when a device does
/// not advertise the exact requested format. Preference order:
/// 1. F32 at the requested sample rate (any channel count).
/// 2. F32 at the requested channel count (any sample rate).
/// 3. Any F32 format (fewest channels first — cheapest to downmix).
/// 4. The device's first advertised format (last resort).
///
/// Returns `None` only when `supported` is empty.
#[cfg(not(target_os = "linux"))]
fn pick_supported_format(
    supported: &[AudioFormat],
    requested: &AudioFormat,
) -> Option<AudioFormat> {
    if supported.is_empty() {
        return None;
    }
    if supported.contains(requested) {
        return Some(requested.clone());
    }
    let is_f32 = |f: &&AudioFormat| f.sample_format == SampleFormat::F32;
    if let Some(f) = supported
        .iter()
        .filter(is_f32)
        .find(|f| f.sample_rate == requested.sample_rate)
    {
        return Some(f.clone());
    }
    if let Some(f) = supported
        .iter()
        .filter(is_f32)
        .find(|f| f.channels == requested.channels)
    {
        return Some(f.clone());
    }
    if let Some(f) = supported.iter().filter(is_f32).min_by_key(|f| f.channels) {
        return Some(f.clone());
    }
    supported.first().cloned()
}

#[cfg(all(test, not(target_os = "linux")))]
mod format_negotiation_tests {
    use super::*;

    fn fmt(sample_rate: u32, channels: u16, sample_format: SampleFormat) -> AudioFormat {
        AudioFormat {
            sample_rate,
            channels,
            sample_format,
        }
    }

    #[test]
    fn empty_supported_returns_none() {
        assert!(pick_supported_format(&[], &fmt(48000, 2, SampleFormat::F32)).is_none());
    }

    #[test]
    fn surround_only_device_negotiates() {
        // The exact field failure: default output is an 8ch/96000-only endpoint.
        let supported = [
            fmt(96000, 8, SampleFormat::F32),
            fmt(96000, 8, SampleFormat::I16),
        ];
        let chosen = pick_supported_format(&supported, &fmt(48000, 2, SampleFormat::F32)).unwrap();
        assert_eq!(chosen, fmt(96000, 8, SampleFormat::F32));
    }

    #[test]
    fn prefers_requested_sample_rate_f32() {
        let supported = [
            fmt(44100, 2, SampleFormat::F32),
            fmt(48000, 2, SampleFormat::F32),
        ];
        let chosen = pick_supported_format(&supported, &fmt(48000, 1, SampleFormat::F32)).unwrap();
        assert_eq!(chosen, fmt(48000, 2, SampleFormat::F32));
    }

    #[test]
    fn exact_match_passthrough() {
        let supported = [fmt(48000, 2, SampleFormat::F32)];
        let chosen = pick_supported_format(&supported, &fmt(48000, 2, SampleFormat::F32)).unwrap();
        assert_eq!(chosen, fmt(48000, 2, SampleFormat::F32));
    }

    // ── negotiate_device_format() tests (AEG-8, rsac-0113) ────────────

    /// A minimal AudioDevice whose `supported_formats()` is configurable, so we
    /// can exercise `negotiate_device_format` (the shared build()/
    /// negotiated_format() helper) without hardware.
    struct MockDevice {
        formats: Vec<AudioFormat>,
    }

    impl crate::core::interface::AudioDevice for MockDevice {
        fn id(&self) -> crate::core::config::DeviceId {
            crate::core::config::DeviceId("mock".to_string())
        }
        fn name(&self) -> String {
            "MockDevice".to_string()
        }
        fn is_default(&self) -> bool {
            true
        }
        fn supported_formats(&self) -> Vec<AudioFormat> {
            self.formats.clone()
        }
        fn create_stream(
            &self,
            _config: &StreamConfig,
        ) -> AudioResult<Box<dyn crate::core::interface::CapturingStream>> {
            Err(AudioError::StreamCreationFailed {
                reason: "mock device cannot create a stream".to_string(),
                context: None,
            })
        }
    }

    /// An exact-match request passes through unchanged.
    #[test]
    fn negotiate_returns_requested_when_exact_match() {
        let dev = MockDevice {
            formats: vec![fmt(48000, 2, SampleFormat::F32)],
        };
        let requested = fmt(48000, 2, SampleFormat::F32);
        let got = negotiate_device_format(&dev, &requested).expect("ok");
        assert_eq!(got, requested);
    }

    /// A device advertising NO formats negotiates to the requested format
    /// unchanged — mirroring build()'s prior behavior (it skipped negotiation
    /// entirely when supported_formats() was empty). This is the property the
    /// build()/negotiated_format() single-sourcing depends on.
    #[test]
    fn negotiate_empty_formats_returns_requested() {
        let dev = MockDevice { formats: vec![] };
        let requested = fmt(44100, 1, SampleFormat::F32);
        let got = negotiate_device_format(&dev, &requested).expect("ok");
        assert_eq!(
            got, requested,
            "empty advertisement must pass the request through (no hard-fail)"
        );
    }

    /// The field-failure case: a surround-only endpoint negotiates the request
    /// to its advertised 8ch/96000 F32 — proving negotiate_device_format applies
    /// the same closest-match policy pick_supported_format does.
    #[test]
    fn negotiate_surround_only_device() {
        let dev = MockDevice {
            formats: vec![
                fmt(96000, 8, SampleFormat::F32),
                fmt(96000, 8, SampleFormat::I16),
            ],
        };
        let requested = fmt(48000, 2, SampleFormat::F32);
        let got = negotiate_device_format(&dev, &requested).expect("ok");
        assert_eq!(got, fmt(96000, 8, SampleFormat::F32));
        assert_ne!(
            got, requested,
            "a forced negotiation diverges from requested"
        );
    }
}

/// Represents an active audio capture session.
///
/// Created via [`AudioCaptureBuilder::build()`]. Provides methods to start/stop
/// audio capture and read audio data via a pull-based streaming model.
/// The user audio callback type. Boxed `FnMut` invoked once per captured buffer.
type AudioCallback = Box<dyn FnMut(&AudioBuffer) + Send + 'static>;

/// A registered-but-not-yet-running callback, stored in [`AudioCapture`] until
/// [`start()`](AudioCapture::start) moves it into a pump thread. Held behind a
/// `Mutex<Option<...>>` only so `&self`-style set/clear can mutate it before the
/// pump owns it — the pump thread does **not** lock this while invoking the
/// callback (it takes ownership), so a callback can freely re-enter the handle.
type PendingCallback = Mutex<Option<AudioCallback>>;

/// Handle to a running callback pump thread.
///
/// The pump *owns* the callback (it was moved out of [`PendingCallback`] at
/// [`start()`](AudioCapture::start)), so no lock is held while the user closure
/// runs — a callback may call back into `AudioCapture` without deadlocking. The
/// pump exits when `stop_flag` is set or the stream errors; the [`JoinHandle`]
/// lets `stop()`/`Drop` join it deterministically rather than leaking the thread.
struct CallbackPump {
    stop_flag: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl CallbackPump {
    /// Signal the pump to stop and join it. Idempotent.
    ///
    /// If called from the pump's own thread (i.e. the user closure re-entered
    /// `AudioCapture` and triggered teardown), the join is skipped — a thread
    /// cannot join itself — and only the stop flag is set; the pump will exit at
    /// the next loop iteration. This makes "clear the callback from within the
    /// callback" safe rather than a self-join deadlock.
    fn shutdown(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            if handle.thread().id() == std::thread::current().id() {
                // Re-entrant teardown from the pump thread: don't join self.
                // The stop flag is set; the loop will break on its next pass.
                // Put the handle back so a later stop()/Drop from another thread
                // can still join it.
                self.handle = Some(handle);
            } else {
                let _ = handle.join();
            }
        }
    }
}

/// The lifecycle handle for a configured capture session, returned by
/// [`AudioCaptureBuilder::build`].
///
/// `AudioCapture` owns the platform stream and exposes the full capture
/// lifecycle: [`start()`](AudioCapture::start), [`stop()`](AudioCapture::stop),
/// pull-based reads ([`read_buffer()`](AudioCapture::read_buffer)), push-based
/// delivery ([`subscribe()`](AudioCapture::subscribe),
/// [`set_callback()`](AudioCapture::set_callback)), and runtime introspection
/// ([`stream_stats()`](AudioCapture::stream_stats),
/// [`overrun_count()`](AudioCapture::overrun_count),
/// [`uptime()`](AudioCapture::uptime)).
///
/// The handle is `Send + Sync` (asserted at compile time below); see the
/// [module docs](self) for the threading model and the multiple-captures
/// guarantee.
pub struct AudioCapture {
    config: AudioCaptureConfig,
    device: Option<Box<dyn crate::core::interface::AudioDevice>>,
    stream: Option<Arc<dyn crate::core::interface::CapturingStream + 'static>>,
    /// Callback registered via [`set_callback`](AudioCapture::set_callback)
    /// before the capture starts. Moved into the pump thread on `start()`.
    callback: PendingCallback,
    /// Active callback pump, if a callback was set when `start()` ran. `None`
    /// means no pump is running (so `start()` will never double-spawn).
    callback_pump: Option<CallbackPump>,
    /// Monotonic timestamp captured the first time `start()` actually creates
    /// the OS stream. `Some` while a stream is live; reset to `None` by
    /// `stop()`/`Drop` when the stream is torn down. Set exactly once on the
    /// non-RT control path (a single `Instant` store), never on an idempotent
    /// restart of an already-running stream. Drives [`uptime`](AudioCapture::uptime).
    start_instant: Option<Instant>,
    /// Total buffers dropped by this handle's subscribe pumps because a
    /// subscriber's bounded channel was full (rsac-d6a8). Shared (aggregated)
    /// across every subscription created from this handle and monotone for
    /// the handle's lifetime — it survives `stop()`/restart. Surfaced via
    /// [`subscriber_dropped_count`](AudioCapture::subscriber_dropped_count).
    subscriber_dropped: Arc<AtomicU64>,
}

// ── Send + Sync assertion (AEG-5, rsac-6f1f) ──────────────────────────────
//
// `AudioCapture` holds only `Send + Sync` parts: an owned `AudioCaptureConfig`;
// `Option<Box<dyn AudioDevice>>` and `Option<Arc<dyn CapturingStream>>` whose
// trait objects are bounded `Send + Sync` (see `crate::core::interface`); a
// `Mutex<Option<Box<dyn FnMut(&AudioBuffer) + Send>>>` (a `Mutex` of a `Send`
// payload is `Send + Sync`); an `Option<CallbackPump>` (an `Arc<AtomicBool>`
// plus a `JoinHandle<()>`, both `Send + Sync`); and an `Option<Instant>`. The
// compiler therefore derives both auto-traits.
//
// We deliberately do NOT hand-write `unsafe impl Send/Sync`: that would suppress
// the compiler's safety net if a non-`Send`/`Sync` field were ever added (e.g. a
// raw pointer or `Rc`), silently making the handle unsound to share. The
// compile-time assertion below pins the type-level guarantee the module doc
// promises (and that downstreams previously had to guess at — some assumed
// `!Sync`) without `unsafe`. Mirrors `buffer.rs`'s `_assert_send_sync`.
//
// All public read paths (`read_buffer`, `read_buffer_blocking`,
// `read_chunk_nonblocking`, `read_chunk_blocking`, `subscribe`,
// `subscribe_with_errors`, `request_stop`, stats/format) take `&self` and reach
// the underlying stream through its own `&self` methods, so an `Arc<AudioCapture>`
// shared across threads needs no external lock for reads. The only `&mut self`
// entry points are lifecycle mutators (`start`/`stop`/`set_callback`/
// `clear_callback`) and `buffers_iter`, whose iterator holds a `&mut AudioCapture`
// borrow; none of these contradict the `Send + Sync` guarantee.
const _: () = {
    const fn _assert_send_sync<T: Send + Sync>() {}
    _assert_send_sync::<AudioCapture>();
};

impl AudioCapture {
    /// Starts the audio capture stream.
    ///
    /// Creates the underlying OS stream (if not already created) and marks
    /// the capture as running. In the new `CapturingStream` contract, the
    /// stream starts producing data upon creation.
    ///
    /// # Restart-by-recreation (rsac-7aa2)
    ///
    /// A handle whose stream was released by [`stop`](Self::stop) **can** be
    /// started again: `start()` creates a *fresh* stream from the same
    /// resolved device/config, so `stop() → start() → read` works. Notes:
    ///
    /// - the uptime anchor re-anchors at the restart —
    ///   [`uptime`](Self::uptime) reports time since the *current* stream was
    ///   created, not cumulative capture time;
    /// - per-stream counters ([`overrun_count`](Self::overrun_count),
    ///   [`stream_stats`](Self::stream_stats)) reset with the new stream,
    ///   while [`subscriber_dropped_count`](Self::subscriber_dropped_count)
    ///   is handle-lifetime and survives.
    ///
    /// Calling `start()` while a stream already exists is state-dependent:
    /// a **running** stream makes it an idempotent no-op (`Ok`); a stream
    /// that is present but **no longer running** (a naturally-ended stream
    /// that was never `stop()`ped) returns
    /// [`AudioError::StreamStartFailed`] — an individual *stream* can never be
    /// restarted; call `stop()` first to release it, then `start()` to
    /// restart with a fresh one.
    pub fn start(&mut self) -> AudioResult<()> {
        // If a stream already exists, decide based on its state:
        // - running  → no-op (idempotent restart of an active capture).
        // - stopped  → error. An individual stream cannot be restarted (the OS
        //   capture thread has exited); the caller must first release it via
        //   stop(), after which start() creates a fresh stream
        //   (restart-by-recreation — see the rustdoc above). Previously this
        //   fell through and spawned a callback pump on a dead stream, then
        //   read_buffer() would fail confusingly (audit L8).
        if let Some(stream) = self.stream.as_ref() {
            if stream.is_running() {
                return Ok(());
            }
            return Err(AudioError::StreamStartFailed {
                reason: "Stream already created and is no longer running; an individual \
                         stream cannot be restarted — call stop() to release it, then \
                         start() again to restart with a fresh stream."
                    .to_string(),
            });
        }

        if self.stream.is_none() {
            let device_ref =
                self.device
                    .as_ref()
                    .ok_or_else(|| AudioError::StreamCreationFailed {
                        reason: "Audio device not available to create stream (was None)."
                            .to_string(),
                        context: None,
                    })?;
            let capturing_stream_obj = device_ref.create_stream(&self.config.stream_config)?;
            self.stream = Some(Arc::from(capturing_stream_obj));
            // Record the start time exactly once, the first time a real stream
            // is created. The idempotent-restart paths above (running → Ok,
            // stopped → Err) return before reaching here, so a second start()
            // on a live stream never resets this. Single non-RT Instant store.
            self.start_instant = Some(Instant::now());
        }

        // Verify stream is available
        let stream_ref = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamCreationFailed {
                reason: "Stream not initialized before starting.".to_string(),
                context: None,
            })?;

        // If a callback was registered via set_callback() AND no pump is already
        // running, spawn a pump thread that delivers captured buffers to it.
        // Without this the stored closure is never invoked (the callback
        // delivery mode would silently do nothing). See
        // docs/designs/0002-callback-delivery.md.
        //
        // Guarding on `self.callback_pump.is_none()` makes a second start() a
        // no-op for the pump — two pumps must never race for the same ring. The
        // callback is *moved* into the pump (taken out of the pending slot), so
        // the pump never holds a lock while running the user closure.
        if self.callback_pump.is_none() {
            // A poisoned callback mutex must NOT silently drop a registered
            // callback (which would leave start() returning Ok with no
            // delivery); surface it like set_callback/clear_callback do.
            let taken = match self.callback.lock() {
                Ok(mut guard) => guard.take(),
                Err(poisoned) => {
                    return Err(AudioError::InternalError {
                        message: format!("Failed to lock callback mutex: {}", poisoned),
                        source: None,
                    });
                }
            };
            if let Some(callback) = taken {
                let pump = Self::spawn_callback_pump(Arc::clone(stream_ref), callback)?;
                self.callback_pump = Some(pump);
            }
        }

        Ok(())
    }

    /// Spawns the callback pump thread and returns a [`CallbackPump`] handle.
    ///
    /// The pump **owns** `callback` (moved in), reads buffers from `stream`, and
    /// invokes the closure on this dedicated thread — **not** the OS real-time
    /// audio thread — so a slow callback only delays delivery, it never stalls
    /// the audio callback, and the closure may freely call back into
    /// `AudioCapture` (no lock is held during invocation). The thread exits when:
    /// - `stop_flag` is set (via [`stop`](Self::stop)/[`clear_callback`](Self::clear_callback)/`Drop`), or
    /// - the stream reaches a terminal state (a **fatal** read error such as
    ///   [`AudioError::StreamEnded`]). Transient/recoverable read errors are
    ///   logged and retried — they must not permanently stop delivery.
    ///
    /// The pump competes with [`read_buffer`](Self::read_buffer) and
    /// [`subscribe`](Self::subscribe) for buffers from the same ring; avoid
    /// mixing a callback with manual reads.
    fn spawn_callback_pump(
        stream: Arc<dyn crate::core::interface::CapturingStream + 'static>,
        mut callback: AudioCallback,
    ) -> AudioResult<CallbackPump> {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_thread = Arc::clone(&stop_flag);
        let handle = std::thread::Builder::new()
            .name("rsac-callback".into())
            .spawn(move || loop {
                if stop_flag_thread.load(Ordering::SeqCst) {
                    break;
                }
                match stream.try_read_chunk() {
                    // No lock held: the pump owns `callback`, so the user closure
                    // can re-enter AudioCapture (e.g. clear_callback) without
                    // deadlocking, and a panic here cannot poison a shared mutex.
                    Ok(Some(buffer)) => callback(&buffer),
                    Ok(None) => {
                        // No data right now — avoid busy-spinning.
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    // Only a FATAL error (e.g. StreamEnded — terminal) stops the
                    // pump. A transient/recoverable read error (StreamReadError,
                    // BufferOverrun/Underrun) must NOT kill delivery — mirror the
                    // iterator/read-loop contract and retry after a brief pause.
                    Err(e) if e.is_fatal() => break,
                    Err(e) => {
                        log::warn!("Callback pump read error (retrying): {:?}", e);
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
            })
            .map_err(|e| AudioError::InternalError {
                message: format!("Failed to spawn callback pump thread: {}", e),
                source: None,
            })?;
        Ok(CallbackPump {
            stop_flag,
            handle: Some(handle),
        })
    }

    /// Stops the audio capture stream.
    ///
    /// Stops the underlying OS stream and **releases** it: the handle's stream
    /// slot becomes empty and the uptime anchor is cleared. The handle itself
    /// remains usable — a subsequent [`start`](Self::start) creates a fresh
    /// stream from the same resolved device/config (*restart-by-recreation*;
    /// see [`start`](Self::start) for what carries over). An individual
    /// *stream* can never be restarted; the handle restarts by creating a new
    /// one.
    ///
    /// Any active subscriber threads will terminate once they detect the stream
    /// has stopped. The underlying stream is released when all references
    /// (including subscriber threads) are dropped.
    pub fn stop(&mut self) -> AudioResult<()> {
        // Shut down the callback pump first (signal + join) so it stops
        // consuming buffers and releases its stream clone before we drop ours.
        // Joining here makes stop() authoritative for the pump thread rather
        // than leaking it until try_read_chunk happens to observe the stop.
        if let Some(mut pump) = self.callback_pump.take() {
            pump.shutdown();
        }

        // Nothing to stop if there is no stream (idempotent).
        if self.stream.is_none() {
            return Ok(());
        }

        if let Some(stream) = self.stream.as_ref() {
            if let Err(e) = stream.stop() {
                log::warn!("Error stopping stream: {:?}", e);
            }
        }
        // Drop our Arc reference. The stream will be fully deallocated once all
        // subscriber threads also drop their clones.
        self.stream.take();
        // The stream is gone, so there is no longer an uptime to report.
        self.start_instant = None;

        Ok(())
    }

    /// Returns `true` if the stream is currently capturing.
    ///
    /// Delegates to the underlying stream's state machine — the single source
    /// of truth for running status. Returns `false` if no stream has been
    /// created yet.
    pub fn is_running(&self) -> bool {
        self.stream
            .as_ref()
            .map(|s| s.is_running())
            .unwrap_or(false)
    }

    /// Returns a reference to the capture configuration.
    pub fn config(&self) -> &AudioCaptureConfig {
        &self.config
    }

    /// Reads a buffer of audio data synchronously (non-blocking).
    ///
    /// Uses `CapturingStream::try_read_chunk` for non-blocking reads.
    /// Returns `Ok(None)` if no data is currently available.
    ///
    /// # Terminal semantics (important)
    ///
    /// This is the **simple pull API**: it short-circuits with a **recoverable**
    /// [`AudioError::StreamReadError`] the moment the stream leaves `Running`, so
    /// it **never surfaces the fatal [`AudioError::StreamEnded`]** and does **not**
    /// drain the buffered tail after `stop()`. A loop that branches on
    /// [`is_fatal()`](AudioError::is_fatal) to detect end-of-stream will therefore
    /// **never terminate** on this method (the downgraded error is recoverable);
    /// it is intended for `while let Ok(Some(buf)) = capture.read_buffer()` style
    /// loops that stop when the caller decides to.
    ///
    /// Prefer [`read_chunk_nonblocking`](Self::read_chunk_nonblocking) (its
    /// terminal-observable sibling) when you need to observe the clean
    /// end-of-stream signal or drain the tail after `stop()` — that is the path
    /// the language bindings and the [`AudioBufferIterator`] use. `read_buffer` is
    /// retained unchanged for backward compatibility; new terminal-aware code
    /// should migrate to `read_chunk_nonblocking`.
    ///
    /// Takes `&self` (not `&mut self`): this path only reads `self.stream` and
    /// calls the stream's own `&self` methods, mutating no field of the handle.
    /// Sharing it behind `&self` lets a concurrent [`request_stop`](Self::request_stop)
    /// unblock a parked reader without forming a `&mut` alias to the same handle
    /// (the use-after-free precondition the C/Go bindings previously relied on).
    pub fn read_buffer(&self) -> AudioResult<Option<AudioBuffer>> {
        // Get the stream first — if there's no stream, we're not running.
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: "Stream is not initialized. Call start() first.".to_string(),
            })?;

        // Check running state via the stream itself — single source of truth.
        // This eliminates the TOCTOU window that existed when a separate
        // AtomicBool was consulted before touching the stream.
        if !stream.is_running() {
            return Err(AudioError::StreamReadError {
                reason: "Stream is not running".to_string(),
            });
        }

        stream.try_read_chunk()
    }

    /// Reads a buffer of audio data non-blocking, **without** the
    /// [`read_buffer`](Self::read_buffer) running-state short-circuit.
    ///
    /// This is the read path the binding pumps (Node/napi, Go via the FFI) and
    /// the in-process consumers use when they must observe the stream's
    /// *terminal* state. Unlike [`read_buffer`](Self::read_buffer) — which
    /// returns a **recoverable** [`AudioError::StreamReadError`] as soon as the
    /// stream leaves `Running`, and therefore can never surface the fatal
    /// [`AudioError::StreamEnded`] — this method delegates straight to the
    /// stream's `try_read_chunk`. That preserves the bridge's drain-on-stop
    /// semantics:
    ///
    /// - `Ok(Some(buf))` while data remains (including the buffered tail that is
    ///   still drainable while the stream is `Stopping`),
    /// - `Ok(None)` when the ring is momentarily empty but the stream is not yet
    ///   terminal,
    /// - `Err(`[`AudioError::StreamEnded`]`)` — **fatal/terminal** — once the
    ///   ring is empty *and* the stream has reached a terminal state.
    ///
    /// Consumers branch on [`AudioError::is_fatal`]/[`AudioError::recoverability`]:
    /// a recoverable error must be retried (it must never end consumption), and
    /// only a fatal terminal ends it cleanly. [`read_buffer`](Self::read_buffer)
    /// keeps its `is_running()` guard for the simple pull API; this is the
    /// terminal-observable sibling. The [`AudioBufferIterator`] and the callback
    /// pump read via `try_read_chunk` for exactly this reason.
    ///
    /// Like [`read_buffer`](Self::read_buffer), this takes `&self` (it only reads
    /// `self.stream` and calls the stream's own `&self` method), so it never
    /// forms a `&mut` alias to the handle.
    pub fn read_chunk_nonblocking(&self) -> AudioResult<Option<AudioBuffer>> {
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: "Stream is not initialized. Call start() first.".to_string(),
            })?;
        stream.try_read_chunk()
    }

    /// Blocking read **without** the [`read_buffer_blocking`](Self::read_buffer_blocking)
    /// running-state short-circuit — the *terminal-observable* blocking sibling.
    ///
    /// [`read_buffer_blocking`](Self::read_buffer_blocking) returns a
    /// **recoverable** [`AudioError::StreamReadError`] the moment the stream
    /// leaves `Running`, so it can never surface the fatal
    /// [`AudioError::StreamEnded`]. This method delegates straight to the
    /// stream's `read_chunk`, which blocks until either a buffer is available
    /// (including the drainable tail while `Stopping`) or the stream reaches a
    /// terminal state, in which case it returns `Err(`[`AudioError::StreamEnded`]`)`
    /// **promptly** (a concurrent [`request_stop`](Self::request_stop) unblocks
    /// it). This is the blocking read the C FFI (`rsac_capture_read`) and the Go
    /// binding use so their pumps observe the terminal signal and end cleanly
    /// instead of spinning on a downgraded recoverable error.
    ///
    /// Takes `&self` like the other read paths, so it never forms a `&mut` alias
    /// (the #28 use-after-free precondition).
    pub fn read_chunk_blocking(&self) -> AudioResult<AudioBuffer> {
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: "Stream is not initialized. Call start() first.".to_string(),
            })?;
        stream.read_chunk()
    }

    /// Reads a buffer of audio data, blocking until data is available.
    ///
    /// Uses `CapturingStream::read_chunk` which blocks until data arrives.
    ///
    /// # Terminal semantics (important)
    ///
    /// Like [`read_buffer`](Self::read_buffer), this is the **simple pull API**:
    /// it returns a **recoverable** [`AudioError::StreamReadError`] the instant the
    /// stream leaves `Running`, so it **never surfaces the fatal
    /// [`AudioError::StreamEnded`]**. Use
    /// [`read_chunk_blocking`](Self::read_chunk_blocking) — the terminal-observable
    /// sibling — when you need the clean end-of-stream signal (it delegates
    /// straight to `read_chunk` and returns `StreamEnded` promptly on terminal).
    /// `read_buffer_blocking` is retained unchanged for backward compatibility.
    ///
    /// Takes `&self` (not `&mut self`) for the same reason as
    /// [`read_buffer`](Self::read_buffer): no field of the handle is mutated, so
    /// a parked `read_buffer_blocking` can be unblocked by a concurrent
    /// [`request_stop`](Self::request_stop) without ever aliasing the handle
    /// mutably. When the stream reaches a terminal state the underlying
    /// `read_chunk` returns [`AudioError::StreamEnded`] promptly instead of
    /// blocking forever — but the `is_running()` guard below means that terminal
    /// is only observed when the transition happens *during* an already-parked
    /// read; a call made after the stream is already terminal returns the
    /// recoverable "not running" error instead (use `read_chunk_blocking` to
    /// always see `StreamEnded`).
    pub fn read_buffer_blocking(&self) -> AudioResult<AudioBuffer> {
        // Get the stream first — if there's no stream, we're not running.
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: "Stream is not initialized. Call start() first.".to_string(),
            })?;

        // Check running state via the stream itself — single source of truth.
        if !stream.is_running() {
            return Err(AudioError::StreamReadError {
                reason: "Stream is not running".to_string(),
            });
        }

        stream.read_chunk()
    }

    /// Best-effort request to stop capture, used as the *unblock primitive* for a
    /// parked [`read_buffer_blocking`](Self::read_buffer_blocking).
    ///
    /// Transitions the underlying stream toward its terminal state (via the
    /// stream's own idempotent `stop()`), which flips the bridge to a terminal
    /// state so a blocked `read_buffer_blocking` returns
    /// [`AudioError::StreamEnded`] within roughly a millisecond instead of waiting
    /// out the blocking-read timeout. It is **idempotent** and a no-op when no
    /// stream has been created (or the stream is already stopped).
    ///
    /// Unlike [`stop`](Self::stop), this takes `&self`: it does not tear down the
    /// callback pump, drop the stream `Arc`, or clear the uptime anchor — it only
    /// signals the stream. That makes it safe to call **concurrently with an
    /// in-flight read** (it forms no `&mut` alias). It is **not** safe to call
    /// concurrently with dropping/freeing the handle: callers (e.g. the C/Go
    /// bindings) must order `request_stop` + drain of in-flight reads **before**
    /// freeing the handle.
    pub fn request_stop(&self) {
        if let Some(stream) = self.stream.as_ref() {
            // CapturingStream::stop is &self and idempotent (already-terminal
            // streams no-op). Ignore the result: this is a best-effort unblock,
            // and a stop error on an already-dying stream is not actionable here.
            let _ = stream.stop();
        }
    }

    /// Returns an iterator over synchronously captured audio buffers.
    pub fn buffers_iter(&mut self) -> AudioBufferIterator<'_> {
        AudioBufferIterator { capture: self }
    }

    /// Returns an asynchronous stream of audio data buffers.
    ///
    /// The returned [`AsyncAudioStream`](crate::bridge::AsyncAudioStream) implements
    /// [`futures_core::Stream`] and yields [`AudioBuffer`]s as they become available
    /// from the audio capture backend.
    ///
    /// The capture must be started (via [`start()`](Self::start)) before calling this method.
    ///
    /// # Single async consumer (waker contract; rsac-7aa2)
    ///
    /// The bridge holds exactly **one** waker slot (an
    /// `atomic_waker::AtomicWaker`). Creating two `AsyncAudioStream`s over the
    /// same capture and polling them from two tasks concurrently is
    /// **unsupported**: each poll's waker registration displaces the other
    /// task's waker (concurrent `AtomicWaker::register` is the documented
    /// unsupported case — the displaced waker is dropped *without being
    /// woken*), so the displaced task can park forever on a wake that was
    /// promised but stolen. Use **at most one** async consumer per capture.
    /// Two consumers would in any case compete for buffers from the same ring
    /// (a single logical consumer per buffer), so there is nothing to gain
    /// from a second stream.
    ///
    /// # Feature Flag
    ///
    /// This method is only available when the `async-stream` feature is enabled.
    ///
    /// # Errors
    ///
    /// Returns an error if the capture has not been started.
    #[cfg(feature = "async-stream")]
    pub fn audio_data_stream(&self) -> AudioResult<crate::bridge::AsyncAudioStream<'_>> {
        let stream = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: "Capture not started. Call start() before audio_data_stream().".to_string(),
            })?;

        Ok(crate::bridge::AsyncAudioStream::new(stream.as_ref()))
    }

    /// Returns an asynchronous stream of audio data buffers.
    ///
    /// **Note:** The `async-stream` feature is not enabled. Enable it in `Cargo.toml`
    /// to use async audio streaming.
    #[cfg(not(feature = "async-stream"))]
    pub fn audio_data_stream(
        &mut self,
    ) -> AudioResult<impl futures_core::Stream<Item = AudioResult<AudioBuffer>> + Send + Sync + '_>
    {
        Err::<
            std::pin::Pin<
                Box<dyn futures_core::Stream<Item = AudioResult<AudioBuffer>> + Send + Sync>,
            >,
            AudioError,
        >(AudioError::PlatformNotSupported {
            feature: "async audio streaming".to_string(),
            platform: "enable the 'async-stream' feature".to_string(),
        })
    }

    /// Sets a callback function for captured audio data.
    ///
    /// The callback will be invoked with each captured audio buffer.
    /// Callbacks cannot be set while capture is running.
    pub fn set_callback<F>(&mut self, callback: F) -> AudioResult<()>
    where
        F: FnMut(&AudioBuffer) + Send + 'static,
    {
        if self.is_running() {
            return Err(AudioError::ConfigurationError {
                message: "Cannot set callback after capture has started.".into(),
            });
        }
        match self.callback.lock() {
            Ok(mut guard) => {
                *guard = Some(Box::new(callback));
                Ok(())
            }
            Err(poisoned) => Err(AudioError::InternalError {
                message: format!("Failed to lock callback mutex: {}", poisoned),
                source: None,
            }),
        }
    }

    /// Clears the registered audio callback.
    ///
    /// If a capture is running with an active callback pump, this signals the
    /// pump to stop and joins it (so delivery ceases promptly), in addition to
    /// clearing any pending (not-yet-started) callback. It is safe to call from
    /// outside the callback. Calling it *from within* the callback signals the
    /// pump but does not self-join (the pump only joins on `stop()`/`Drop`),
    /// avoiding a self-join deadlock.
    pub fn clear_callback(&mut self) -> AudioResult<()> {
        // Tear down a running pump (the callback now lives inside it).
        if let Some(mut pump) = self.callback_pump.take() {
            pump.shutdown();
            // If shutdown() ran re-entrantly (called from inside the pump's own
            // callback), it could not self-join and left the JoinHandle in place
            // for a later join. Re-store the pump so stop()/Drop can still join it
            // deterministically instead of detaching the thread (ADR-0002).
            if pump.handle.is_some() {
                self.callback_pump = Some(pump);
            }
        }
        // Also clear any pending callback registered before start().
        match self.callback.lock() {
            Ok(mut guard) => {
                *guard = None;
                Ok(())
            }
            Err(poisoned) => Err(AudioError::InternalError {
                message: format!("Failed to lock callback mutex for clearing: {}", poisoned),
                source: None,
            }),
        }
    }

    /// Creates a subscription channel that delivers audio buffers as they are captured.
    ///
    /// Spawns a background thread that reads from the capture stream and sends
    /// buffers over a **bounded** [`mpsc`] channel (128 buffers — the same
    /// order as the bridge ring itself; ~1.3 s of audio at the typical ~10 ms
    /// buffer cadence). Returns the receiving end of the channel.
    ///
    /// # Backpressure: drop, don't block, and count it (rsac-d6a8)
    ///
    /// If the receiver stalls long enough for the channel to fill, further
    /// buffers are **dropped** — the pump never blocks and never queues
    /// unbounded memory — and each drop is counted in
    /// [`subscriber_dropped_count`](Self::subscriber_dropped_count). This loss
    /// happens *downstream* of the capture ring, so
    /// [`overrun_count`](Self::overrun_count) and
    /// [`backpressure_report`](Self::backpressure_report) do **not** reflect
    /// it; a slow subscriber must watch `subscriber_dropped_count()` as the
    /// complement.
    ///
    /// **Important:** The background thread competes with [`read_buffer()`](Self::read_buffer)
    /// and [`read_buffer_blocking()`](Self::read_buffer_blocking) for audio data
    /// from the same ring buffer. Avoid mixing `subscribe()` with manual buffer reads.
    ///
    /// The background thread exits automatically when:
    /// - The stream reaches a **fatal terminal** state (e.g.
    ///   [`AudioError::StreamEnded`]) — the channel then disconnects, which the
    ///   receiver observes as a [`RecvError`](std::sync::mpsc::RecvError). A
    ///   **recoverable** read error (e.g. a transient
    ///   [`AudioError::StreamReadError`]) is logged and retried — it does **not**
    ///   end the subscription. (Use [`subscribe_with_errors`](Self::subscribe_with_errors)
    ///   if you need to observe the terminal `AudioError` itself rather than a
    ///   bare disconnect.)
    /// - The returned [`Receiver`](mpsc::Receiver) is dropped
    ///
    /// Multiple subscriptions are allowed but each subscriber competes for buffers.
    ///
    /// # Accepted stream states (rsac-7aa2)
    ///
    /// The subscription is accepted whenever a stream exists — while `Running`
    /// **or** in the drainable `Stopping` window: a gracefully-ended stream
    /// still holding a buffered tail can be subscribed to and drained (the old
    /// `is_running()` gate stranded that tail). Subscribing to a stream that
    /// already reached its fatal terminal also succeeds and simply yields a
    /// channel that ends immediately — racing a natural end is inherently
    /// indistinguishable from subscribing just before it. Only a capture with
    /// **no** stream (never started, or [`stop`](Self::stop)ped — `stop()`
    /// releases the stream) is rejected.
    ///
    /// # Latency floor (audit L5)
    ///
    /// The background thread polls with a 1 ms sleep when the ring buffer is
    /// momentarily empty (rather than parking on the async waker), so delivery
    /// of the *first* buffer after an idle period can be delayed by up to ~1 ms.
    /// For most capture workloads (10–20 ms callback periods) this is negligible,
    /// but latency-critical consumers should read via
    /// [`read_buffer_blocking()`](Self::read_buffer_blocking) or the async stream
    /// API (`feature = "async-stream"`), which is waker-driven and has no fixed
    /// poll-interval floor.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::StreamReadError`] if the capture has no stream
    /// (never started, or stopped).
    pub fn subscribe(&self) -> AudioResult<mpsc::Receiver<AudioBuffer>> {
        // Presence gate only (rsac-7aa2): Running and the drainable Stopping
        // window are both accepted; a stream already at its fatal terminal
        // yields an immediately-ending channel. See the rustdoc above.
        let stream_ref = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: "No active stream: the capture was never started, or has been \
                         stopped (stop() releases the stream). Call start() to begin \
                         (or restart) capturing."
                    .to_string(),
            })?;

        spawn_subscribe_thread(Arc::clone(stream_ref), Arc::clone(&self.subscriber_dropped))
    }

    /// Like [`subscribe`](Self::subscribe), but delivers each item as an
    /// [`AudioResult<AudioBuffer>`] so the **terminal** [`AudioError`] reaches
    /// the consumer instead of only a bare channel disconnect.
    ///
    /// This is the error-carrying counterpart to [`subscribe`](Self::subscribe)
    /// (the same `Stream` / `StreamWithErrors` split the Go binding exposes). The
    /// background reader:
    ///
    /// - sends `Ok(buffer)` for each captured buffer,
    /// - on a momentarily-empty ring, sleeps ~1 ms and retries,
    /// - on a **recoverable** read error (transient
    ///   [`AudioError::StreamReadError`], [`AudioError::BufferOverrun`],
    ///   [`AudioError::BufferUnderrun`], …), logs and retries — a recoverable
    ///   error never ends the subscription, and
    /// - on a **fatal terminal** error (e.g. [`AudioError::StreamEnded`]) sends
    ///   one final `Err(e)` **then** exits, so the consumer receives the terminal
    ///   `AudioError` as the last item *before* the channel disconnects.
    ///
    /// The `Item` type matches the async stream
    /// ([`AsyncAudioStream`](crate::bridge::AsyncAudioStream)) and the
    /// [`AudioBufferIterator`] so a consumer can reuse
    /// [`AudioError::is_fatal`]/[`AudioError::recoverability`] uniformly.
    ///
    /// Like [`subscribe`](Self::subscribe), the reader competes with
    /// [`read_buffer`](Self::read_buffer) and the callback pump for buffers from
    /// the same ring — do not mix it with manual reads.
    ///
    /// # Bounded channel, drops, and coalescing (rsac-d6a8)
    ///
    /// The channel is **bounded** (128 buffers, like [`subscribe`](Self::subscribe)):
    /// a stalled receiver causes further buffers to be dropped and counted in
    /// [`subscriber_dropped_count`](Self::subscriber_dropped_count). Repeated
    /// **recoverable** errors of the same variant are *coalesced* — forwarded
    /// at most once per second (a persistently-recoverable stream errors once
    /// per ~1 ms poll and would otherwise flood the channel with identical
    /// advisory items); an error of a different variant is always forwarded
    /// immediately. The **fatal terminal** is exempt from all of this: it is
    /// sent with a *blocking* send as the guaranteed final item, so it is never
    /// lost even when the channel is full of unread buffers (the pump exits
    /// right after; delivery fails only if the receiver is already gone).
    ///
    /// # Accepted stream states (rsac-7aa2)
    ///
    /// Same policy as [`subscribe`](Self::subscribe): accepted whenever a
    /// stream exists (`Running` or the drainable `Stopping` window — the
    /// buffered tail plus the terminal error are then delivered); a stream
    /// already at its fatal terminal yields a channel whose only item is the
    /// final `Err`. Only a capture with no stream is rejected.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::StreamReadError`] if the capture has no stream
    /// (never started, or stopped). Once a subscription exists, the terminal
    /// stream error is delivered as the final channel *item*, not as the
    /// return value of this method.
    ///
    /// # Backend caveat (FH-1)
    ///
    /// On Linux/macOS the producer-side terminal signal is only fully wired on
    /// some backends; the final `Err(StreamEnded)` is end-to-end observable
    /// wherever the producer drives the bridge to a terminal state (Windows, and
    /// Linux/macOS once the producer-terminal-signal work lands). On a backend
    /// that never reaches terminal, the subscription simply keeps delivering
    /// until the receiver is dropped — the recoverable-vs-fatal branching here is
    /// correct regardless.
    pub fn subscribe_with_errors(&self) -> AudioResult<mpsc::Receiver<AudioResult<AudioBuffer>>> {
        // Presence gate only (rsac-7aa2) — see subscribe() for the rationale.
        let stream_ref = self
            .stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: "No active stream: the capture was never started, or has been \
                         stopped (stop() releases the stream). Call start() to begin \
                         (or restart) capturing."
                    .to_string(),
            })?;

        spawn_subscribe_with_errors_thread(
            Arc::clone(stream_ref),
            Arc::clone(&self.subscriber_dropped),
        )
    }

    /// Returns the total number of buffers **dropped by subscribe pumps** on
    /// this handle because a subscriber's bounded channel was full (rsac-d6a8).
    ///
    /// [`subscribe`](Self::subscribe) and
    /// [`subscribe_with_errors`](Self::subscribe_with_errors) deliver over a
    /// bounded channel (128 buffers) and, per the crate-wide backpressure
    /// policy, **drop rather than block** when a subscriber stalls. Those
    /// drops happen *downstream* of the capture ring, so they are invisible to
    /// [`overrun_count`](Self::overrun_count) and
    /// [`backpressure_report`](Self::backpressure_report) — this counter is
    /// the subscriber-side complement. It aggregates across every subscription
    /// created from this handle (mirroring how `overrun_count` aggregates the
    /// ring) and is monotone for the handle's lifetime: unlike the per-stream
    /// counters it survives [`stop`](Self::stop)/restart.
    ///
    /// `0` means no subscriber has ever fallen behind.
    pub fn subscriber_dropped_count(&self) -> u64 {
        self.subscriber_dropped.load(Ordering::Relaxed)
    }

    /// Returns the number of audio buffers dropped due to ring buffer overflow (overruns).
    ///
    /// This counter reflects how many times the OS audio callback had to discard
    /// a buffer because the consumer was not reading fast enough. A non-zero value
    /// indicates the consumer is too slow or the ring buffer capacity is too small.
    ///
    /// Returns `0` if the stream has not been created yet.
    pub fn overrun_count(&self) -> u64 {
        self.stream.as_ref().map(|s| s.overrun_count()).unwrap_or(0)
    }

    /// Returns true if the stream is experiencing sustained backpressure —
    /// the ring buffer has dropped enough consecutive frames to indicate the
    /// consumer cannot keep up with the producer. Consumers should slow down
    /// processing, warn the user, or switch to a lower-cost provider.
    ///
    /// Returns `false` if the stream has not been created yet.
    pub fn is_under_backpressure(&self) -> bool {
        self.stream
            .as_ref()
            .map(|s| s.is_under_backpressure())
            .unwrap_or(false)
    }

    /// Returns how long the capture has been running, or `None` if it has not
    /// been started (or has been stopped).
    ///
    /// The clock is anchored the first time [`start()`](Self::start) actually
    /// creates the OS stream and is cleared by [`stop()`](Self::stop)/`Drop`.
    /// A second `start()` on an already-running stream does not reset it, so a
    /// long-lived capture reports a continuously increasing uptime. Backed by a
    /// monotonic [`Instant`], so the value never goes backwards even if the
    /// wall clock is adjusted.
    pub fn uptime(&self) -> Option<Duration> {
        self.start_instant.map(|t| t.elapsed())
    }

    /// Returns the negotiated *delivery* `AudioFormat` the backend actually
    /// produces, or `None` before [`start()`](Self::start) creates a stream.
    ///
    /// This is the authoritative format atomically published by the bridge: each
    /// backend records the rate/channels it actually delivers at its negotiation
    /// point — PipeWire from its `param_changed` callback, WASAPI at mix-format
    /// open, CoreAudio when the AUHAL stream format is set (PU-1/PERF-07,
    /// `rsac-2c56`; see [`crate::bridge`]). It can therefore differ from the
    /// requested config in [`config()`](Self::config) when the device forced a
    /// negotiation, and the reported `sample_format` is always `F32` because the
    /// bridge payload is always interleaved f32 regardless of the endpoint's
    /// native sample type. Reading it is a single `Acquire` load behind the
    /// underlying stream's `format()` — no allocation and no lock on the data
    /// plane.
    pub fn format(&self) -> Option<AudioFormatType> {
        self.stream.as_ref().map(|s| s.format())
    }

    /// Returns a point-in-time [`StreamStats`] snapshot of this capture.
    ///
    /// Bundles the bridge's diagnostic counters
    /// ([`buffers_captured`](crate::core::interface::CapturingStream::buffers_captured) /
    /// [`buffers_dropped`](crate::core::interface::CapturingStream::buffers_dropped) /
    /// [`buffers_pushed`](crate::core::interface::CapturingStream::buffers_pushed) /
    /// [`overrun_count`](crate::core::interface::CapturingStream::overrun_count))
    /// with [`is_running()`](Self::is_running), [`uptime()`](Self::uptime), and a
    /// human-readable description of the negotiated [`format()`](Self::format).
    ///
    /// All counters are read with cheap `Relaxed` loads on this (non-real-time)
    /// query path — no allocation on or contention with the OS audio callback.
    /// The format description string is built lazily here and is never stored
    /// per-buffer.
    ///
    /// When no stream has been created (before [`start()`](Self::start), or after
    /// [`stop()`](Self::stop)), this returns [`StreamStats::default()`]:
    /// `is_running == false`, `uptime == Duration::ZERO`, zeroed counters, and an
    /// empty `format_description`.
    pub fn stream_stats(&self) -> StreamStats {
        let stream = match self.stream.as_ref() {
            Some(s) => s,
            // No stream → default snapshot (is_running == false, ZERO uptime).
            None => return StreamStats::default(),
        };

        let buffers_dropped = stream.buffers_dropped();
        let format_description = self
            .format()
            .as_ref()
            .map(format_description_string)
            .unwrap_or_default();

        StreamStats {
            // `overruns` is the original field; keep it equal to buffers_dropped
            // (its documented alias) so both read consistently.
            overruns: buffers_dropped,
            buffers_captured: stream.buffers_captured(),
            buffers_dropped,
            buffers_pushed: stream.buffers_pushed(),
            uptime: self.uptime().unwrap_or(Duration::ZERO),
            is_running: self.is_running(),
            format_description,
        }
    }

    /// Returns a windowed [`BackpressureReport`] for this capture.
    ///
    /// Unlike [`is_under_backpressure()`](Self::is_under_backpressure) — an
    /// all-or-nothing flag that trips only on *consecutive* drops and resets on
    /// any successful push — this report exposes a [`drop_rate`](BackpressureReport::drop_rate)
    /// over recent push activity, so sustained partial loss (e.g. a steady 1-in-3
    /// drop pattern) is visible. The legacy bool is carried unchanged inside the
    /// report.
    ///
    /// Returns [`BackpressureReport::default()`] (all-zero, `drop_rate == 0.0`,
    /// `is_under_backpressure == false`) when no stream has been created.
    ///
    /// # Windowing
    ///
    /// The producer maintains a fixed-size, alloc-free ring of per-window
    /// `(pushed, dropped)` snapshots in the bridge (`bridge/ring_buffer.rs`); this
    /// method reads that **windowed** view via
    /// `CapturingStream::drop_window_snapshot`, so the report reflects a
    /// *recent* loss pattern — including a sustained 1-in-N drop that the
    /// consecutive-drop bool resets away — rather than lifetime totals.
    ///
    /// The reported `window` is an honest estimate of the wall-clock the windowed
    /// tallies cover: `(pushed + dropped)` buffers × the per-buffer duration
    /// (`buffer_size` frames at the negotiated `sample_rate`). When the buffer
    /// size or rate is unknown (no stream / not yet negotiated, or a zero rate),
    /// `window` falls back to [`Duration::ZERO`] — the tallies are still valid,
    /// only their span is unattributed. The legacy `is_under_backpressure` bool is
    /// carried unchanged inside the report.
    pub fn backpressure_report(&self) -> BackpressureReport {
        let stream = match self.stream.as_ref() {
            Some(s) => s,
            None => return BackpressureReport::default(),
        };
        let (pushed, dropped) = stream.drop_window_snapshot();
        BackpressureReport::from_counts(
            self.estimate_window_span(pushed + dropped),
            pushed,
            dropped,
            stream.is_under_backpressure(),
        )
    }

    /// Estimate the wall-clock span covered by `buffers` push attempts, from the
    /// configured buffer size and the negotiated (or requested) sample rate.
    ///
    /// Returns [`Duration::ZERO`] when the span cannot be attributed — an unknown
    /// buffer size or a zero/unknown sample rate — so a caller never reads a
    /// fabricated span. Each push delivers one `buffer_size`-frame buffer, so the
    /// window is `buffers × buffer_size / sample_rate` seconds.
    fn estimate_window_span(&self, buffers: u64) -> Duration {
        let frames_per_buffer = match self.config.stream_config.buffer_size {
            Some(n) if n > 0 => n as u64,
            _ => return Duration::ZERO,
        };
        let sample_rate = self
            .format()
            .map(|f| f.sample_rate)
            .filter(|&r| r > 0)
            .unwrap_or(self.config.stream_config.sample_rate);
        if sample_rate == 0 {
            return Duration::ZERO;
        }
        let total_frames = buffers.saturating_mul(frames_per_buffer);
        Duration::from_secs_f64(total_frames as f64 / sample_rate as f64)
    }
}

/// Formats an `AudioFormat` as a compact
/// human-readable string, e.g. `"2ch 48000Hz F32"`.
///
/// Computed lazily on the query path (e.g. by a future `stream_stats()`),
/// never stored per-buffer, so it allocates only when actually called.
#[allow(dead_code)] // Consumed by stream_stats() / diagnostics on the query path.
fn format_description_string(fmt: &AudioFormatType) -> String {
    let sample_fmt = match fmt.sample_format {
        SampleFormat::I16 => "I16",
        SampleFormat::I24 => "I24",
        SampleFormat::I32 => "I32",
        SampleFormat::F32 => "F32",
    };
    format!("{}ch {}Hz {}", fmt.channels, fmt.sample_rate, sample_fmt)
}

// AudioDataStreamWrapper has been removed — async streaming will be
// re-introduced via the BridgeStream layer in a later phase.

// ── Iterator ─────────────────────────────────────────────────────────────

/// An iterator that yields audio buffers by synchronously reading from an [`AudioCapture`].
pub struct AudioBufferIterator<'a> {
    capture: &'a mut AudioCapture,
}

impl<'a> Iterator for AudioBufferIterator<'a> {
    type Item = AudioResult<AudioBuffer>;

    fn next(&mut self) -> Option<Self::Item> {
        // Read directly from the stream (not via read_buffer(), which refuses to
        // read once the stream leaves the Running state). The stream remains
        // *readable* while Stopping, so reading directly lets us DRAIN the buffered
        // tail after stop() rather than discarding it (audit R2-2).
        //
        // Semantics of try_read_chunk():
        //   Ok(Some(buf)) → yield it.
        //   Ok(None)      → no data right now (Running or Stopping with empty ring)
        //                   → wait briefly and retry.
        //   Err(StreamEnded) → terminal (Stopped/Closed/Error) AND nothing more to
        //                   read → end the iterator (return None). Other Err →
        //                   surface to the caller.
        let stream = self.capture.stream.as_ref()?; // no stream → iteration done
        loop {
            match stream.try_read_chunk() {
                Ok(Some(buffer)) => return Some(Ok(buffer)),
                Ok(None) => {
                    // No data yet. If the stream is no longer readable (terminal)
                    // the next try_read_chunk will return StreamEnded; otherwise
                    // we're Running/Stopping with a momentarily-empty ring — wait
                    // and retry so we don't busy-spin.
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                // Terminal end-of-stream: drained and done.
                Err(AudioError::StreamEnded { .. }) => return None,
                // Any other error is surfaced once.
                Err(e) => return Some(Err(e)),
            }
        }
    }
}

// ── Drop ─────────────────────────────────────────────────────────────────

impl Drop for AudioCapture {
    fn drop(&mut self) {
        // Tear down the callback pump first (signal + join) so its thread stops
        // touching the stream before we drop it, and is never leaked.
        if let Some(mut pump) = self.callback_pump.take() {
            pump.shutdown();
        }
        // Best-effort stop of whatever stream we still hold. The stream's own
        // state machine decides whether this is a no-op (already stopped) or
        // a real stop; stop() is idempotent on the stream side.
        if let Some(stream) = self.stream.as_ref() {
            if stream.is_running() {
                if let Err(e) = stream.stop() {
                    log::warn!("Error stopping audio stream during drop: {:?}", e);
                }
            }
        }
        // Drop the Arc reference (stream fully deallocated when last clone is dropped).
        self.stream.take();
        // Clear the uptime anchor alongside the stream teardown.
        self.start_instant = None;
    }
}

// ── Debug ────────────────────────────────────────────────────────────────

impl fmt::Debug for AudioCapture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let device_name = self
            .device
            .as_ref()
            .map(|d| d.name())
            .unwrap_or_else(|| "None".to_string());

        f.debug_struct("AudioCapture")
            .field("config", &self.config)
            .field("device_name", &device_name)
            .field("stream_is_some", &self.stream.is_some())
            .field("is_running", &self.is_running())
            // Never panic inside Debug: a poisoned callback mutex must not take
            // down an infallible formatter. Fall back to reporting the poison.
            .field(
                "callback_is_some",
                &match self.callback.try_lock() {
                    Ok(guard) => guard.is_some(),
                    Err(_) => false,
                },
            )
            .finish()
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{AudioFormat, SampleFormat};
    use crate::core::interface::CapturingStream;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    #[test]
    fn builder_defaults_to_system_default() {
        let builder = AudioCaptureBuilder::new();
        assert_eq!(builder.target, CaptureTarget::SystemDefault);
        assert_eq!(builder.config.sample_rate, 48000);
        assert_eq!(builder.config.channels, 2);
        assert_eq!(builder.config.sample_format, SampleFormat::F32);
        assert_eq!(builder.config.buffer_size, None);
    }

    #[test]
    fn builder_fails_if_channels_is_zero() {
        let result = AudioCaptureBuilder::new()
            .with_target(CaptureTarget::SystemDefault)
            .sample_rate(44100)
            .channels(0)
            .sample_format(SampleFormat::F32)
            .build();
        assert!(result.is_err());
        match result.err().unwrap() {
            AudioError::ConfigurationError { message: msg } => {
                assert_eq!(msg, "Channels must be greater than 0.");
            }
            other_error => panic!("Expected ConfigurationError, got {:?}", other_error),
        }
    }

    #[test]
    fn builder_fails_on_unsupported_sample_rate() {
        let result = AudioCaptureBuilder::new()
            .with_target(CaptureTarget::SystemDefault)
            .sample_rate(11025) // Not supported
            .channels(1)
            .sample_format(SampleFormat::F32)
            .build();
        assert!(result.is_err());
        match result.err().unwrap() {
            AudioError::InvalidParameter { param, reason } => {
                assert_eq!(param, "sample_rate");
                assert!(reason.contains("11025"));
            }
            other_error => panic!("Expected InvalidParameter, got {:?}", other_error),
        }
    }

    #[test]
    fn builder_with_target_overrides_default() {
        let device_id = crate::core::config::DeviceId("test-device".to_string());
        let builder =
            AudioCaptureBuilder::new().with_target(CaptureTarget::Device(device_id.clone()));
        assert_eq!(builder.target, CaptureTarget::Device(device_id));
    }

    #[test]
    fn builder_with_config_sets_all_fields() {
        let config = StreamConfig {
            sample_rate: 44100,
            channels: 1,
            sample_format: SampleFormat::I16,
            buffer_size: Some(1024),
            capture_target: CaptureTarget::SystemDefault,
            #[cfg(target_os = "ios")]
            ios_app_group: None,
            #[cfg(target_os = "android")]
            android_projection: None,
        };
        let builder = AudioCaptureBuilder::new().with_config(config.clone());
        assert_eq!(builder.config, config);
    }

    // ── Builder method chainability & defaults ────────────────────────

    #[test]
    fn builder_is_chainable() {
        // Verify all builder methods return Self and can be chained
        let builder = AudioCaptureBuilder::new()
            .with_target(CaptureTarget::SystemDefault)
            .sample_rate(44100)
            .channels(2)
            .sample_format(SampleFormat::F32)
            .buffer_size(Some(1024))
            .buffer_size_frames(Some(512));
        // Just verifying compilation and chainability — no panic
        assert_eq!(builder.config.sample_rate, 44100);
        assert_eq!(builder.config.channels, 2);
    }

    #[test]
    fn builder_default_trait_matches_new() {
        let from_new = AudioCaptureBuilder::new();
        let from_default = AudioCaptureBuilder::default();
        // Both should produce identical builders
        assert_eq!(from_new.config.sample_rate, from_default.config.sample_rate);
        assert_eq!(from_new.config.channels, from_default.config.channels);
        assert_eq!(
            from_new.config.sample_format,
            from_default.config.sample_format
        );
    }

    // ── Invalid sample rate tests ────────────────────────────────────

    #[test]
    fn builder_rejects_sample_rate_zero() {
        let result = AudioCaptureBuilder::new().sample_rate(0).build();
        assert!(result.is_err());
        match result.unwrap_err() {
            AudioError::InvalidParameter { param, .. } => assert_eq!(param, "sample_rate"),
            e => panic!("Expected InvalidParameter, got: {e:?}"),
        }
    }

    #[test]
    fn builder_rejects_very_high_sample_rate() {
        let result = AudioCaptureBuilder::new().sample_rate(999999).build();
        assert!(result.is_err());
        match result.unwrap_err() {
            AudioError::InvalidParameter { param, .. } => assert_eq!(param, "sample_rate"),
            e => panic!("Expected InvalidParameter, got: {e:?}"),
        }
    }

    #[test]
    fn builder_rejects_nonstandard_sample_rate() {
        // 11025 is a valid audio rate but not in the supported list
        let result = AudioCaptureBuilder::new().sample_rate(11025).build();
        assert!(result.is_err());
    }

    /// rsac-c957: the builder's config-time whitelist is the *same* array as the
    /// public [`PlatformCapabilities::SUPPORTED_SAMPLE_RATES`] — a single source
    /// of truth, so a caller pre-validating against the public const sees exactly
    /// what `build()`/`preflight()` enforce.
    #[test]
    fn builder_whitelist_is_platform_capabilities_const() {
        assert_eq!(
            SUPPORTED_SAMPLE_RATES,
            PlatformCapabilities::SUPPORTED_SAMPLE_RATES
        );
        // And the slice accessor agrees with what preflight checks against.
        assert_eq!(
            PlatformCapabilities::supported_sample_rates(),
            &SUPPORTED_SAMPLE_RATES
        );
    }

    #[test]
    fn builder_accepts_all_supported_sample_rates() {
        // These should NOT fail at the sample_rate validation step
        // They may fail later at device enumeration, which is fine
        for rate in [22050u32, 32000, 44100, 48000, 88200, 96000] {
            let result = AudioCaptureBuilder::new().sample_rate(rate).build();
            // Should NOT be InvalidParameter for sample_rate
            if let Err(AudioError::InvalidParameter { param, .. }) = &result {
                panic!(
                    "Rate {rate} should be valid, but got InvalidParameter {{ param: {param} }}"
                );
            }
            // Other errors (DeviceEnumeration, etc.) are expected without hardware
        }
    }

    // ── Invalid channel count tests ──────────────────────────────────

    #[test]
    fn builder_rejects_channels_above_max() {
        let result = AudioCaptureBuilder::new()
            .channels(33) // MAX_CHANNELS = 32
            .build();
        assert!(result.is_err());
        match result.unwrap_err() {
            AudioError::ConfigurationError { .. } => {} // expected
            e => panic!("Expected ConfigurationError, got: {e:?}"),
        }
    }

    #[test]
    fn builder_rejects_channels_way_above_max() {
        let result = AudioCaptureBuilder::new().channels(u16::MAX).build();
        assert!(result.is_err());
    }

    #[test]
    fn builder_accepts_max_channels() {
        // 32 channels should be accepted (it's the max, not above it)
        let result = AudioCaptureBuilder::new().channels(32).build();
        // Should NOT be ConfigurationError
        if let Err(AudioError::ConfigurationError { .. }) = &result {
            panic!("32 channels (MAX_CHANNELS) should be accepted");
        }
        // Other errors (DeviceEnumeration, etc.) are fine
    }

    #[test]
    fn builder_accepts_mono() {
        let result = AudioCaptureBuilder::new().channels(1).build();
        // Should NOT be ConfigurationError for channels
        if let Err(AudioError::ConfigurationError { message }) = &result {
            if message.contains("hannels") {
                panic!("Mono (1 channel) should be accepted, got ConfigurationError: {message}");
            }
        }
    }

    // ── Sample format tests ──────────────────────────────────────────

    #[test]
    fn builder_with_all_sample_formats() {
        // Verify all sample formats can be set without panic
        for format in [
            SampleFormat::I16,
            SampleFormat::I24,
            SampleFormat::I32,
            SampleFormat::F32,
        ] {
            let builder = AudioCaptureBuilder::new().sample_format(format);
            assert_eq!(builder.config.sample_format, format);
        }
    }

    // ── Buffer size tests ────────────────────────────────────────────

    #[test]
    fn builder_buffer_size_can_be_set_and_cleared() {
        let b1 = AudioCaptureBuilder::new().buffer_size(Some(1024));
        assert_eq!(b1.config.buffer_size, Some(1024));

        let b2 = AudioCaptureBuilder::new().buffer_size(None);
        assert_eq!(b2.config.buffer_size, None);
    }

    #[test]
    fn builder_buffer_size_frames_sets_buffer_size() {
        let builder = AudioCaptureBuilder::new().buffer_size_frames(Some(256));
        assert_eq!(builder.config.buffer_size, Some(256));
    }

    // ── With_config override test ────────────────────────────────────

    #[test]
    fn builder_with_config_overrides_individual_settings() {
        let config = StreamConfig {
            sample_rate: 96000,
            channels: 8,
            sample_format: SampleFormat::I32,
            buffer_size: Some(2048),
            capture_target: CaptureTarget::SystemDefault,
            #[cfg(target_os = "ios")]
            ios_app_group: None,
            #[cfg(target_os = "android")]
            android_projection: None,
        };
        let builder = AudioCaptureBuilder::new()
            .sample_rate(44100) // This should be overridden
            .with_config(config.clone());
        assert_eq!(builder.config.sample_rate, 96000);
        assert_eq!(builder.config.channels, 8);
        assert_eq!(builder.config.sample_format, SampleFormat::I32);
    }

    // ── negotiated_format() tests (AEG-8, rsac-0113) ──────────────────

    /// On Linux, negotiated_format() is intentionally unsupported pre-build
    /// (PipeWire negotiates at stream-open). It must return PlatformNotSupported
    /// rather than guessing a value — and must still run preflight first, so an
    /// invalid config surfaces its config error before the platform gate.
    #[cfg(target_os = "linux")]
    #[test]
    fn negotiated_format_unsupported_on_linux() {
        let builder = AudioCaptureBuilder::new().sample_rate(48000).channels(2);
        match builder.negotiated_format() {
            Err(AudioError::PlatformNotSupported { platform, .. }) => {
                assert_eq!(platform, "linux");
            }
            other => panic!("expected PlatformNotSupported on Linux, got: {other:?}"),
        }
    }

    /// negotiated_format() runs preflight() first on every platform: an invalid
    /// config (unsupported sample rate) is rejected with the same
    /// InvalidParameter error build() would raise, before any device work.
    #[test]
    fn negotiated_format_runs_preflight_first() {
        let builder = AudioCaptureBuilder::new().sample_rate(11025); // unsupported
        match builder.negotiated_format() {
            Err(AudioError::InvalidParameter { param, .. }) => {
                assert_eq!(param, "sample_rate");
            }
            other => panic!("expected InvalidParameter from preflight, got: {other:?}"),
        }
    }

    /// On non-Linux platforms negotiated_format() resolves a real device. With
    /// no hardware it surfaces an honest enumeration/format error; with hardware
    /// it returns a format. Either way it must NOT panic — this asserts the
    /// pre-build query is callable and well-behaved on a device-free CI box.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn negotiated_format_is_callable_and_does_not_panic() {
        let builder = AudioCaptureBuilder::new().sample_rate(48000).channels(2);
        // Valid config → preflight passes; device resolution may succeed (real
        // hardware) or fail (device-free CI). Both are acceptable; a panic is
        // not.
        let _ = builder.negotiated_format();
    }

    // ── Mock CapturingStream for subscribe/overrun_count tests ────────

    /// A mock CapturingStream that serves buffers from an internal Mutex<VecDeque>
    /// and tracks an overrun counter via an AtomicU64.
    struct MockCapturingStream {
        buffers: Mutex<std::collections::VecDeque<AudioBuffer>>,
        running: AtomicBool,
        overruns: AtomicU64,
        /// Bridge counters mirrored for stream_stats()/backpressure_report():
        /// buffers the producer enqueued, buffers delivered to the consumer.
        pushed: AtomicU64,
        captured: AtomicU64,
        /// Legacy consecutive-drop backpressure flag the windowed report carries.
        backpressure: AtomicBool,
        /// The format `format()` reports. Defaults to `AudioFormat::default()`;
        /// `set_negotiated_format()` overwrites it to mirror a backend that
        /// negotiated a different delivery format on the bridge producer.
        format: Mutex<AudioFormat>,
        /// Count of `stop()` calls — lets RunningCapture tests assert that the
        /// guard's Drop stops exactly once and that an explicit stop + drop does
        /// not double-stop the *underlying* stream (stop() is a no-op once the
        /// AudioCapture has already dropped its Arc).
        stop_calls: AtomicU64,
        /// One-shot RECOVERABLE read errors to inject ahead of buffered data.
        /// Each `try_read_chunk`/`read_chunk` pops one off the front and returns
        /// it as an `Err`, modeling a transient hiccup (StreamReadError) that
        /// terminal-delivery consumers must RETRY rather than treat as the end.
        recoverable_errors: Mutex<std::collections::VecDeque<AudioError>>,
    }

    impl MockCapturingStream {
        fn new() -> Self {
            Self {
                buffers: Mutex::new(std::collections::VecDeque::new()),
                running: AtomicBool::new(true),
                overruns: AtomicU64::new(0),
                pushed: AtomicU64::new(0),
                captured: AtomicU64::new(0),
                backpressure: AtomicBool::new(false),
                format: Mutex::new(AudioFormat::default()),
                stop_calls: AtomicU64::new(0),
                recoverable_errors: Mutex::new(std::collections::VecDeque::new()),
            }
        }

        /// Queue one RECOVERABLE error (e.g. a transient StreamReadError) to be
        /// returned by the next read before any buffered data. Used by the
        /// terminal-delivery tests to prove a recoverable error is retried, not
        /// treated as end-of-stream.
        fn inject_recoverable_error(&self, e: AudioError) {
            debug_assert!(
                e.is_recoverable(),
                "inject_recoverable_error expects a recoverable variant"
            );
            self.recoverable_errors.lock().unwrap().push_back(e);
        }

        /// Number of times `stop()` was invoked on this stream.
        fn stop_calls(&self) -> u64 {
            self.stop_calls.load(Ordering::SeqCst)
        }

        /// Mirror the real bridge's `BridgeProducer::set_negotiated_format`:
        /// record the authoritative delivery format the backend produces.
        fn set_negotiated_format(&self, sample_rate: u32, channels: u16) {
            *self.format.lock().unwrap() = AudioFormat {
                sample_rate,
                channels,
                sample_format: SampleFormat::F32,
            };
        }

        /// Push a buffer for the mock to serve on the next try_read_chunk call.
        fn push_buffer(&self, buf: AudioBuffer) {
            self.buffers.lock().unwrap().push_back(buf);
        }

        /// Simulate overruns by incrementing the counter.
        fn add_overruns(&self, count: u64) {
            self.overruns.fetch_add(count, Ordering::Relaxed);
        }

        /// Mirror a producer that enqueued `count` buffers (bumps buffers_pushed).
        fn add_pushed(&self, count: u64) {
            self.pushed.fetch_add(count, Ordering::Relaxed);
        }

        /// Mirror a consumer that popped `count` buffers (bumps buffers_captured).
        fn add_captured(&self, count: u64) {
            self.captured.fetch_add(count, Ordering::Relaxed);
        }

        /// Mirror the producer dropping `count` buffers to overflow: bumps the
        /// drop/overrun counter (buffers_dropped is an alias of overrun_count).
        fn add_dropped(&self, count: u64) {
            self.overruns.fetch_add(count, Ordering::Relaxed);
        }

        /// Set the legacy consecutive-drop backpressure flag.
        fn set_backpressure(&self, on: bool) {
            self.backpressure.store(on, Ordering::SeqCst);
        }

        /// Signal the mock stream is stopped.
        fn signal_stop(&self) {
            self.running.store(false, Ordering::SeqCst);
        }
    }

    impl CapturingStream for MockCapturingStream {
        fn read_chunk(&self) -> AudioResult<AudioBuffer> {
            // Blocking: spin-wait until data or stopped. Mirrors the real bridge,
            // which returns the terminal StreamEnded (Fatal) once stopped.
            loop {
                if let Some(e) = self.recoverable_errors.lock().unwrap().pop_front() {
                    return Err(e);
                }
                if let Some(buf) = self.buffers.lock().unwrap().pop_front() {
                    return Ok(buf);
                }
                if !self.running.load(Ordering::SeqCst) {
                    return Err(AudioError::StreamEnded {
                        reason: "Mock stream stopped".into(),
                    });
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }

        fn try_read_chunk(&self) -> AudioResult<Option<AudioBuffer>> {
            // Surface any injected RECOVERABLE error first (a transient hiccup
            // ahead of real data), so terminal-delivery consumers must retry it
            // rather than end iteration.
            if let Some(e) = self.recoverable_errors.lock().unwrap().pop_front() {
                return Err(e);
            }
            // Drain any buffered data first, even after stop, so the iterator's
            // drain-on-stop path (R2-2) is exercised; only report StreamEnded
            // once the buffer is empty AND the stream has stopped.
            if let Some(buf) = self.buffers.lock().unwrap().pop_front() {
                return Ok(Some(buf));
            }
            if !self.running.load(Ordering::SeqCst) {
                return Err(AudioError::StreamEnded {
                    reason: "Mock stream stopped".into(),
                });
            }
            Ok(None)
        }

        fn stop(&self) -> AudioResult<()> {
            self.stop_calls.fetch_add(1, Ordering::SeqCst);
            self.running.store(false, Ordering::SeqCst);
            Ok(())
        }

        fn format(&self) -> AudioFormat {
            self.format.lock().unwrap().clone()
        }

        fn is_running(&self) -> bool {
            self.running.load(Ordering::SeqCst)
        }

        fn overrun_count(&self) -> u64 {
            self.overruns.load(Ordering::Relaxed)
        }

        fn buffers_pushed(&self) -> u64 {
            self.pushed.load(Ordering::Relaxed)
        }

        fn buffers_captured(&self) -> u64 {
            self.captured.load(Ordering::Relaxed)
        }

        fn buffers_dropped(&self) -> u64 {
            // Alias of overrun_count(), matching the BridgeStream contract.
            self.overruns.load(Ordering::Relaxed)
        }

        fn is_under_backpressure(&self) -> bool {
            self.backpressure.load(Ordering::SeqCst)
        }

        fn drop_window_snapshot(&self) -> (u64, u64) {
            // The mock has no real sliding ring; its lifetime pushed/dropped
            // counters stand in as the windowed view so backpressure_report()
            // tests can drive a known (pushed, dropped) pair through the same
            // read path BridgeStream uses.
            (
                self.pushed.load(Ordering::Relaxed),
                self.overruns.load(Ordering::Relaxed),
            )
        }
    }

    /// Creates an AudioCapture with a mock stream, bypassing the builder (no hardware needed).
    fn make_mock_capture(mock: Arc<MockCapturingStream>) -> AudioCapture {
        AudioCapture {
            config: AudioCaptureConfig {
                target: CaptureTarget::SystemDefault,
                stream_config: StreamConfig::default(),
            },
            device: None,
            stream: Some(mock),
            callback: Mutex::new(None),
            callback_pump: None,
            start_instant: None,
            subscriber_dropped: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Creates an AudioCapture with NO stream — the pre-start / post-stop
    /// state — for gate and default-snapshot tests.
    fn make_capture_without_stream() -> AudioCapture {
        AudioCapture {
            config: AudioCaptureConfig {
                target: CaptureTarget::SystemDefault,
                stream_config: StreamConfig::default(),
            },
            device: None,
            stream: None,
            callback: Mutex::new(None),
            callback_pump: None,
            start_instant: None,
            subscriber_dropped: Arc::new(AtomicU64::new(0)),
        }
    }

    // ── Send + Sync / Arc-shareable tests (AEG-5, rsac-6f1f) ──────────

    /// AEG-5: `AudioCapture` is `Send + Sync` at the type level (the compile-time
    /// `_assert_send_sync` const proves it; this test makes the guarantee visible
    /// in the test surface so a regression that flips it shows up as a failing
    /// test, not just a build break).
    #[test]
    fn audio_capture_is_send_and_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<AudioCapture>();
        assert_sync::<AudioCapture>();
        // RunningCapture wraps it, so it inherits both.
        assert_send::<RunningCapture>();
        assert_sync::<RunningCapture>();
    }

    /// AEG-5: an `Arc<AudioCapture>` can be shared across threads and read
    /// concurrently through `&self` with no external lock — the contract the
    /// type-level `Send + Sync` claim makes good on. Two threads read from the
    /// same shared handle via the `&self` read path; both succeed without a
    /// `Mutex<AudioCapture>` wrapper.
    #[test]
    fn audio_capture_arc_shared_reads_need_no_external_lock() {
        let mock = Arc::new(MockCapturingStream::new());
        for _ in 0..8 {
            mock.push_buffer(AudioBuffer::new(vec![0.25; 4], 2, 48000));
        }
        let capture = Arc::new(make_mock_capture(Arc::clone(&mock)));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let capture = Arc::clone(&capture);
            handles.push(std::thread::spawn(move || {
                // &self read path through a shared Arc — no Mutex<AudioCapture>.
                let _ = capture.read_chunk_nonblocking();
                // Other &self queries are reachable on the shared handle too.
                let _ = capture.is_running();
                let _ = capture.stream_stats();
            }));
        }
        for h in handles {
            h.join().expect("reader thread joins");
        }
        mock.signal_stop();
    }

    // ── subscribe() tests ─────────────────────────────────────────────

    /// rsac-7aa2(1): subscribe is gated on stream PRESENCE only. A stream that
    /// already reached its fatal terminal (stopped, nothing buffered) is
    /// accepted and yields a channel that ends immediately — racing a natural
    /// end is indistinguishable from subscribing just before it, so an error
    /// here would be arbitrary. A capture with NO stream at all still rejects,
    /// with a message that is accurate for both never-started and post-stop.
    #[test]
    fn subscribe_on_terminal_stream_yields_immediately_ending_channel() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.signal_stop(); // terminal, no buffered tail
        let capture = make_mock_capture(mock);

        let rx = capture.subscribe().expect("terminal stream is accepted");
        assert!(
            rx.recv_timeout(std::time::Duration::from_secs(2)).is_err(),
            "channel must end immediately (no data, prompt disconnect)"
        );

        // No stream at all → rejected via the presence check.
        let empty = make_capture_without_stream();
        match empty.subscribe() {
            Err(AudioError::StreamReadError { reason }) => {
                assert!(
                    reason.contains("start()"),
                    "message must direct to start(): {reason}"
                );
            }
            other => panic!("expected StreamReadError, got: {other:?}"),
        }
    }

    /// rsac-7aa2(1): the drainable Stopping window is accepted — a stream that
    /// gracefully ended with a buffered tail can still be subscribed to, and
    /// the tail is delivered before the channel ends. (The old `is_running()`
    /// gate rejected this call and stranded the tail.)
    #[test]
    fn subscribe_accepts_stopping_window_and_drains_tail() {
        let mock = Arc::new(MockCapturingStream::new());
        for i in 0..3 {
            mock.push_buffer(AudioBuffer::new(vec![i as f32 + 1.0; 4], 2, 48000));
        }
        // Not running, but the tail is still drainable — the mock models the
        // bridge's Stopping window (drain first, terminal only when empty).
        mock.signal_stop();
        let capture = make_mock_capture(Arc::clone(&mock));
        assert!(!capture.is_running());

        let rx = capture
            .subscribe()
            .expect("the drainable Stopping window must be accepted");
        let mut got = Vec::new();
        while let Ok(buf) = rx.recv_timeout(std::time::Duration::from_secs(2)) {
            got.push(buf.data()[0]);
        }
        assert_eq!(
            got,
            vec![1.0, 2.0, 3.0],
            "the buffered tail is delivered, then the channel ends"
        );
    }

    /// rsac-7aa2(3): the post-stop subscribe error is accurate — stop()
    /// releases the stream, so subscribe() must not claim the stream "is not
    /// initialized" (the old message) when it was in fact stopped.
    #[test]
    fn post_stop_subscribe_error_is_accurate() {
        let mock = Arc::new(MockCapturingStream::new());
        let mut capture = make_mock_capture(mock);
        capture.stop().unwrap();
        match capture.subscribe() {
            Err(AudioError::StreamReadError { reason }) => {
                assert!(
                    reason.contains("stopped"),
                    "post-stop message must mention the stopped state: {reason}"
                );
                assert!(
                    !reason.contains("not initialized"),
                    "the misleading pre-fix message must be gone: {reason}"
                );
            }
            other => panic!("expected StreamReadError, got: {other:?}"),
        }
    }

    #[test]
    fn subscribe_receives_buffers() {
        let mock = Arc::new(MockCapturingStream::new());
        // Push some test buffers before subscribing
        mock.push_buffer(AudioBuffer::new(vec![0.1; 960], 2, 48000));
        mock.push_buffer(AudioBuffer::new(vec![0.2; 960], 2, 48000));
        mock.push_buffer(AudioBuffer::new(vec![0.3; 960], 2, 48000));

        let capture = make_mock_capture(Arc::clone(&mock));
        let rx = capture.subscribe().expect("subscribe should succeed");

        // Receive the three buffers
        let buf1 = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(buf1.data()[0], 0.1);

        let buf2 = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(buf2.data()[0], 0.2);

        let buf3 = rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(buf3.data()[0], 0.3);

        // Stop the mock so the subscribe thread exits
        mock.signal_stop();
    }

    #[test]
    fn subscribe_thread_stops_when_stream_stops() {
        let mock = Arc::new(MockCapturingStream::new());
        let capture = make_mock_capture(Arc::clone(&mock));
        let rx = capture.subscribe().expect("subscribe should succeed");

        // Signal stop — the subscribe thread should exit
        mock.signal_stop();

        // After a short delay, recv should fail (channel disconnected)
        std::thread::sleep(std::time::Duration::from_millis(50));
        let result = rx.recv_timeout(std::time::Duration::from_millis(100));
        assert!(result.is_err());
    }

    #[test]
    fn subscribe_thread_stops_when_receiver_dropped() {
        let mock = Arc::new(MockCapturingStream::new());
        let capture = make_mock_capture(Arc::clone(&mock));
        let rx = capture.subscribe().expect("subscribe should succeed");

        // Drop the receiver — the subscribe thread should eventually exit
        drop(rx);

        // Push a buffer to trigger the send error in the thread
        mock.push_buffer(AudioBuffer::new(vec![1.0; 960], 2, 48000));

        // Give the thread time to realize the receiver is gone and exit
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Clean up
        mock.signal_stop();
    }

    // ── terminal-error delivery tests (FH-1 / BP-6) ───────────────────

    /// subscribe() must NOT end on a recoverable read error: a transient
    /// StreamReadError ahead of real data is retried, and the queued buffer is
    /// still delivered. Previously ANY Err broke the loop, silently ending the
    /// subscription on a transient hiccup.
    #[test]
    fn subscribe_continues_past_recoverable_error() {
        let mock = Arc::new(MockCapturingStream::new());
        // One transient error, then a real buffer behind it.
        mock.inject_recoverable_error(AudioError::StreamReadError {
            reason: "transient hiccup".into(),
        });
        mock.push_buffer(AudioBuffer::new(vec![0.42; 4], 2, 48000));

        let capture = make_mock_capture(Arc::clone(&mock));
        let rx = capture.subscribe().expect("subscribe should succeed");

        // The recoverable error is swallowed+retried, so the buffer behind it
        // still arrives (subscribe() yields buffers only, not errors).
        let buf = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("buffer must arrive past the recoverable error");
        assert_eq!(buf.data()[0], 0.42);

        mock.signal_stop();
    }

    /// subscribe() ends (channel disconnects) only on a FATAL terminal. After
    /// the mock stops and drains, the reader observes StreamEnded (fatal) and
    /// exits, disconnecting the channel.
    #[test]
    fn subscribe_disconnects_on_fatal_terminal() {
        let mock = Arc::new(MockCapturingStream::new());
        let capture = make_mock_capture(Arc::clone(&mock));
        let rx = capture.subscribe().expect("subscribe should succeed");

        // Stop → next try_read_chunk returns StreamEnded (fatal) → thread breaks.
        mock.signal_stop();

        // The channel disconnects; recv must eventually fail.
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(200))
                .is_err(),
            "channel must disconnect once the stream reaches a fatal terminal"
        );
    }

    /// subscribe_with_errors() delivers buffers as Ok(_) and forwards a
    /// recoverable error as an Err item WITHOUT ending the stream (the buffer
    /// behind it still arrives).
    #[test]
    fn subscribe_with_errors_forwards_recoverable_then_continues() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.inject_recoverable_error(AudioError::StreamReadError {
            reason: "transient hiccup".into(),
        });
        mock.push_buffer(AudioBuffer::new(vec![0.7; 4], 2, 48000));

        let capture = make_mock_capture(Arc::clone(&mock));
        let rx = capture
            .subscribe_with_errors()
            .expect("subscribe_with_errors should succeed");

        // First item: the recoverable error, surfaced but non-terminal.
        match rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(Err(e)) => {
                assert!(e.is_recoverable(), "first item is the recoverable error");
            }
            other => panic!("expected a recoverable Err item first, got: {other:?}"),
        }
        // Second item: the buffer behind it — delivery continued past the error.
        match rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(Ok(buf)) => assert_eq!(buf.data()[0], 0.7),
            other => panic!("expected the buffered Ok item next, got: {other:?}"),
        }

        mock.signal_stop();
    }

    /// subscribe_with_errors() forwards the FATAL terminal AudioError as the
    /// FINAL item before the channel disconnects — the consumer learns *why* the
    /// stream ended rather than racing a bare RecvError.
    #[test]
    fn subscribe_with_errors_delivers_terminal_before_disconnect() {
        let mock = Arc::new(MockCapturingStream::new());
        // Queue one buffer, then stop so the next read is the fatal StreamEnded.
        mock.push_buffer(AudioBuffer::new(vec![1.0; 4], 2, 48000));
        let capture = make_mock_capture(Arc::clone(&mock));
        let rx = capture
            .subscribe_with_errors()
            .expect("subscribe_with_errors should succeed");

        // The buffered item arrives as Ok.
        match rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(Ok(buf)) => assert_eq!(buf.data()[0], 1.0),
            other => panic!("expected the buffered Ok item, got: {other:?}"),
        }

        // Stop → the next read is the fatal terminal, delivered as a final Err.
        mock.signal_stop();
        let mut saw_terminal = false;
        // Drain until we either see the terminal Err or the channel disconnects.
        loop {
            match rx.recv_timeout(std::time::Duration::from_secs(2)) {
                Ok(Ok(_)) => continue, // any straggler buffer
                Ok(Err(e)) => {
                    assert!(
                        e.is_fatal(),
                        "the final delivered Err must be the fatal terminal"
                    );
                    assert!(matches!(e, AudioError::StreamEnded { .. }));
                    saw_terminal = true;
                }
                Err(_) => break, // disconnected
            }
        }
        assert!(
            saw_terminal,
            "subscribe_with_errors must deliver the terminal Err before disconnect"
        );
    }

    // ── bounded subscribe channel tests (rsac-d6a8) ───────────────────

    /// rsac-d6a8: a stalled receiver must NOT grow memory without bound. The
    /// channel depth is capped at SUBSCRIBE_CHANNEL_CAPACITY, the overflow is
    /// dropped, and every drop is counted in subscriber_dropped_count().
    #[test]
    fn subscribe_bounds_channel_and_counts_drops() {
        const OVERFLOW: usize = 72;
        let total = SUBSCRIBE_CHANNEL_CAPACITY + OVERFLOW;

        let mock = Arc::new(MockCapturingStream::new());
        for _ in 0..total {
            mock.push_buffer(AudioBuffer::new(vec![0.5; 4], 2, 48000));
        }
        let capture = make_mock_capture(Arc::clone(&mock));
        let rx = capture.subscribe().expect("subscribe should succeed");
        // Stall the receiver: never recv while the pump floods the channel.

        // The pump drains all `total` buffers from the mock; the first
        // SUBSCRIBE_CHANNEL_CAPACITY fill the channel and the rest are dropped
        // and counted. Poll (bounded) until the counter reaches the overflow.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while capture.subscriber_dropped_count() < OVERFLOW as u64
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(
            capture.subscriber_dropped_count(),
            OVERFLOW as u64,
            "every overflowed buffer is dropped and counted, exactly once"
        );

        // End the stream so the pump exits, then drain: the channel holds
        // EXACTLY the cap — bounded depth, not the full flood.
        mock.signal_stop();
        let mut received = 0usize;
        while rx.recv_timeout(std::time::Duration::from_secs(2)).is_ok() {
            received += 1;
        }
        assert_eq!(
            received, SUBSCRIBE_CHANNEL_CAPACITY,
            "channel depth is capped at the documented bound"
        );
    }

    /// rsac-d6a8: the fatal terminal is NEVER lost to a full channel. After
    /// drops, subscribe_with_errors still delivers the terminal Err as the
    /// final item (the pump uses a blocking send for that one item).
    #[test]
    fn subscribe_with_errors_terminal_survives_full_channel() {
        const OVERFLOW: usize = 10;
        let total = SUBSCRIBE_CHANNEL_CAPACITY + OVERFLOW;

        let mock = Arc::new(MockCapturingStream::new());
        for _ in 0..total {
            mock.push_buffer(AudioBuffer::new(vec![0.25; 4], 2, 48000));
        }
        let capture = make_mock_capture(Arc::clone(&mock));
        let rx = capture
            .subscribe_with_errors()
            .expect("subscribe_with_errors should succeed");

        // Let the pump flood the (unread) channel and drop the overflow.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while capture.subscriber_dropped_count() < OVERFLOW as u64
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(capture.subscriber_dropped_count(), OVERFLOW as u64);

        // Now end the stream: the pump's terminal send blocks on the full
        // channel until we drain — the terminal must arrive as the FINAL item.
        mock.signal_stop();
        let mut oks = 0usize;
        let terminal = loop {
            match rx.recv_timeout(std::time::Duration::from_secs(2)) {
                Ok(Ok(_)) => oks += 1,
                Ok(Err(e)) => break e,
                Err(e) => panic!("channel ended before the terminal Err arrived: {e:?}"),
            }
        };
        assert_eq!(
            oks, SUBSCRIBE_CHANNEL_CAPACITY,
            "all channel-buffered items are delivered before the terminal"
        );
        assert!(
            terminal.is_fatal() && matches!(terminal, AudioError::StreamEnded { .. }),
            "the final item is the fatal terminal, got: {terminal:?}"
        );
        assert!(
            rx.recv_timeout(std::time::Duration::from_secs(1)).is_err(),
            "channel disconnects after the terminal item"
        );
    }

    /// rsac-d6a8: repeated recoverable errors of the same variant are
    /// coalesced (≈1/s), not forwarded once per ~1 ms poll — a persistently-
    /// recoverable stream must not flood the channel with identical advisory
    /// items. The audio behind the error burst is still delivered.
    #[test]
    fn subscribe_with_errors_coalesces_repeated_recoverable_errors() {
        const BURST: usize = 50;
        let mock = Arc::new(MockCapturingStream::new());
        for _ in 0..BURST {
            mock.inject_recoverable_error(AudioError::StreamReadError {
                reason: "persistent transient".into(),
            });
        }
        mock.push_buffer(AudioBuffer::new(vec![0.7; 4], 2, 48000));

        let capture = make_mock_capture(Arc::clone(&mock));
        let rx = capture
            .subscribe_with_errors()
            .expect("subscribe_with_errors should succeed");

        // Collect items until the buffer behind the burst arrives.
        let mut err_items = 0usize;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            assert!(
                std::time::Instant::now() < deadline,
                "the buffer behind the error burst never arrived"
            );
            match rx.recv_timeout(std::time::Duration::from_secs(2)) {
                Ok(Err(e)) => {
                    assert!(e.is_recoverable(), "burst items are recoverable");
                    err_items += 1;
                }
                Ok(Ok(buf)) => {
                    assert_eq!(buf.data()[0], 0.7);
                    break;
                }
                Err(e) => panic!("channel ended prematurely: {e:?}"),
            }
        }
        // 50 identical errors ~1 ms apart span well under the 1 s coalescing
        // interval, so ~1 forwarded item is expected. Allow a little slack for
        // pathological CI scheduling (a >1 s stall between two polls legally
        // forwards again) — the point is "far fewer than the burst".
        assert!(
            (1..=3).contains(&err_items),
            "repeated same-variant recoverable errors must be coalesced \
             (expected 1..=3 forwarded, got {err_items} of {BURST})"
        );
        assert_eq!(
            capture.subscriber_dropped_count(),
            0,
            "advisory error items are never counted as dropped buffers"
        );
        mock.signal_stop();
    }

    // ── read_chunk_nonblocking() tests (terminal-observable read) ─────

    /// read_chunk_nonblocking() yields Ok(Some(buf)) when data is available and
    /// Ok(None) when the (running) ring is momentarily empty.
    #[test]
    fn read_chunk_nonblocking_yields_data_then_none() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.push_buffer(AudioBuffer::new(vec![0.3; 4], 2, 48000));
        let capture = make_mock_capture(Arc::clone(&mock));

        let first = capture
            .read_chunk_nonblocking()
            .expect("read ok")
            .expect("a buffer is available");
        assert_eq!(first.data()[0], 0.3);

        // Ring now empty but still running → Ok(None), NOT an error.
        assert!(
            capture.read_chunk_nonblocking().expect("read ok").is_none(),
            "empty-but-running ring must yield Ok(None), not an error"
        );

        mock.signal_stop();
    }

    /// Unlike read_buffer() (which short-circuits to a RECOVERABLE
    /// StreamReadError once the stream leaves Running), read_chunk_nonblocking()
    /// surfaces the FATAL terminal StreamEnded once the ring is empty AND the
    /// stream has stopped — the property the napi/Go pumps depend on.
    #[test]
    fn read_chunk_nonblocking_surfaces_fatal_terminal() {
        let mock = Arc::new(MockCapturingStream::new());
        let capture = make_mock_capture(Arc::clone(&mock));
        mock.signal_stop();

        match capture.read_chunk_nonblocking() {
            Err(e) => {
                assert!(e.is_fatal(), "terminal read must be fatal");
                assert!(matches!(e, AudioError::StreamEnded { .. }));
            }
            other => panic!("expected fatal StreamEnded, got: {other:?}"),
        }
    }

    /// read_chunk_nonblocking() drains the buffered tail AFTER stop (Stopping
    /// drain semantics) before reporting the fatal terminal — proving it does
    /// not discard data the way read_buffer()'s is_running() guard would.
    #[test]
    fn read_chunk_nonblocking_drains_tail_then_reports_terminal() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.push_buffer(AudioBuffer::new(vec![5.0; 2], 1, 48000));
        mock.signal_stop(); // stopped, but buffered tail remains
        let capture = make_mock_capture(Arc::clone(&mock));

        // The tail is drained first.
        let buf = capture
            .read_chunk_nonblocking()
            .expect("tail read ok")
            .expect("buffered tail is drained after stop");
        assert_eq!(buf.data()[0], 5.0);

        // Then the fatal terminal.
        assert!(
            matches!(
                capture.read_chunk_nonblocking(),
                Err(AudioError::StreamEnded { .. })
            ),
            "terminal reported only after the tail is drained"
        );
    }

    // ── terminal-read propagation: read_buffer* (simple pull) vs
    //    read_chunk_* (terminal-observable) — the documented divergence ─────

    /// The load-bearing contract: on a stopped stream, `read_chunk_nonblocking`
    /// surfaces the FATAL `StreamEnded` while `read_buffer` DOWNGRADES to a
    /// RECOVERABLE `StreamReadError`. A read loop that ends on `is_fatal()` must
    /// therefore terminate on `read_chunk_nonblocking` but would spin forever on
    /// `read_buffer`. This pins the exact behavior their rustdoc promises so it
    /// cannot silently drift.
    #[test]
    fn read_buffer_downgrades_terminal_but_read_chunk_nonblocking_preserves_it() {
        // read_chunk_nonblocking → fatal StreamEnded on a stopped, empty stream.
        let mock = Arc::new(MockCapturingStream::new());
        mock.signal_stop();
        let capture = make_mock_capture(Arc::clone(&mock));
        match capture.read_chunk_nonblocking() {
            Err(e) => {
                assert!(
                    e.is_fatal(),
                    "read_chunk_nonblocking terminal must be fatal"
                );
                assert!(matches!(e, AudioError::StreamEnded { .. }));
            }
            other => panic!("expected fatal StreamEnded, got: {other:?}"),
        }

        // read_buffer → recoverable StreamReadError on the same stopped stream
        // (never StreamEnded), because it short-circuits on !is_running().
        let mock2 = Arc::new(MockCapturingStream::new());
        mock2.signal_stop();
        let capture2 = make_mock_capture(Arc::clone(&mock2));
        match capture2.read_buffer() {
            Err(e) => {
                assert!(
                    e.is_recoverable(),
                    "read_buffer downgrades terminal to a recoverable error"
                );
                assert!(
                    matches!(e, AudioError::StreamReadError { .. }),
                    "read_buffer must report StreamReadError, not StreamEnded"
                );
                assert!(
                    !matches!(e, AudioError::StreamEnded { .. }),
                    "read_buffer must NEVER surface the fatal terminal"
                );
            }
            other => panic!("expected recoverable StreamReadError, got: {other:?}"),
        }
    }

    /// The blocking pair mirrors the non-blocking pair: `read_chunk_blocking`
    /// surfaces the FATAL `StreamEnded` on a stopped stream, while
    /// `read_buffer_blocking` short-circuits to a RECOVERABLE `StreamReadError`.
    #[test]
    fn read_buffer_blocking_downgrades_terminal_but_read_chunk_blocking_preserves_it() {
        // read_chunk_blocking → fatal StreamEnded (delegates to read_chunk,
        // whose MockCapturingStream loop returns StreamEnded once stopped+empty).
        let mock = Arc::new(MockCapturingStream::new());
        mock.signal_stop();
        let capture = make_mock_capture(Arc::clone(&mock));
        match capture.read_chunk_blocking() {
            Err(e) => {
                assert!(e.is_fatal(), "read_chunk_blocking terminal must be fatal");
                assert!(matches!(e, AudioError::StreamEnded { .. }));
            }
            other => panic!("expected fatal StreamEnded, got: {other:?}"),
        }

        // read_buffer_blocking → recoverable StreamReadError (is_running guard).
        let mock2 = Arc::new(MockCapturingStream::new());
        mock2.signal_stop();
        let capture2 = make_mock_capture(Arc::clone(&mock2));
        match capture2.read_buffer_blocking() {
            Err(e) => {
                assert!(
                    e.is_recoverable(),
                    "read_buffer_blocking downgrades terminal to recoverable"
                );
                assert!(matches!(e, AudioError::StreamReadError { .. }));
            }
            other => panic!("expected recoverable StreamReadError, got: {other:?}"),
        }
    }

    /// Both terminal-observable read paths (`read_chunk_nonblocking` and
    /// `read_chunk_blocking`) must NOT end on a *recoverable* read error: a
    /// transient `StreamReadError` injected ahead of data is surfaced as-is (and
    /// is `is_recoverable()`), and the buffer behind it is still delivered on the
    /// retry — so a correct consumer loop that retries recoverable errors keeps
    /// going and only stops on `is_fatal()`. This guards the recoverable-vs-fatal
    /// split end-to-end through the AudioCapture read surface.
    #[test]
    fn read_chunk_paths_treat_recoverable_error_as_retryable_not_terminal() {
        let mock = Arc::new(MockCapturingStream::new());
        // One transient hiccup, then a real buffer behind it.
        mock.inject_recoverable_error(AudioError::StreamReadError {
            reason: "transient hiccup".into(),
        });
        mock.push_buffer(AudioBuffer::new(vec![0.9; 4], 2, 48000));
        let capture = make_mock_capture(Arc::clone(&mock));

        // First read observes the recoverable error (NOT fatal, NOT terminal).
        match capture.read_chunk_nonblocking() {
            Err(e) => {
                assert!(e.is_recoverable(), "injected error must be recoverable");
                assert!(!e.is_fatal());
                assert!(!matches!(e, AudioError::StreamEnded { .. }));
            }
            other => panic!("expected a recoverable error first, got: {other:?}"),
        }
        // Retry delivers the buffer that was queued behind the hiccup.
        let buf = capture
            .read_chunk_nonblocking()
            .expect("read ok on retry")
            .expect("buffer behind the recoverable error is delivered");
        assert_eq!(buf.data()[0], 0.9);

        mock.signal_stop();
    }

    // ── overrun_count() tests ─────────────────────────────────────────

    #[test]
    fn overrun_count_returns_zero_when_no_stream() {
        let capture = make_capture_without_stream();
        assert_eq!(capture.overrun_count(), 0);
    }

    #[test]
    fn overrun_count_returns_zero_initially() {
        let mock = Arc::new(MockCapturingStream::new());
        let capture = make_mock_capture(mock);
        assert_eq!(capture.overrun_count(), 0);
    }

    #[test]
    fn overrun_count_reflects_mock_overruns() {
        let mock = Arc::new(MockCapturingStream::new());
        let capture = make_mock_capture(Arc::clone(&mock));

        assert_eq!(capture.overrun_count(), 0);

        mock.add_overruns(5);
        assert_eq!(capture.overrun_count(), 5);

        mock.add_overruns(3);
        assert_eq!(capture.overrun_count(), 8);
    }

    #[test]
    fn overrun_count_returns_zero_after_stop() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.add_overruns(10);
        let mut capture = make_mock_capture(mock);

        assert_eq!(capture.overrun_count(), 10);

        // Stop drops the stream Arc
        capture.stop().unwrap();

        // After stop, stream is None, so overrun_count returns 0
        assert_eq!(capture.overrun_count(), 0);
    }

    // ── buffers_iter() tests (H2) ─────────────────────────────────────

    /// Regression (audit H2): the iterator must NOT end on a transient empty
    /// poll. With buffers queued, `next()` yields them in order; an interleaved
    /// empty poll (Ok(None)) is retried, not treated as end-of-stream.
    #[test]
    fn buffers_iter_yields_queued_then_continues_past_empty() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.push_buffer(AudioBuffer::new(vec![0.1; 8], 2, 48000));
        mock.push_buffer(AudioBuffer::new(vec![0.2; 8], 2, 48000));
        let mut capture = make_mock_capture(Arc::clone(&mock));

        // First two next() calls must return the queued buffers, even though the
        // mock's try_read_chunk returns Ok(None) once the queue drains (the old
        // iterator would have stopped at the first None instead of these items).
        let mut it = capture.buffers_iter();
        let b1 = it.next().expect("first item").expect("ok");
        assert_eq!(b1.data()[0], 0.1);
        let b2 = it.next().expect("second item").expect("ok");
        assert_eq!(b2.data()[0], 0.2);
        // Queue now empty but stream still running → the iterator is retrying on
        // Ok(None). Stop the stream from another thread so next() observes
        // !is_running and terminates rather than spinning forever.
        let mock2 = Arc::clone(&mock);
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            mock2.signal_stop();
        });
        assert!(
            it.next().is_none(),
            "iterator must end once the stream stops"
        );
        stopper.join().unwrap();
    }

    /// The iterator ends (returns None) when the capture is not running and there
    /// is no stream, rather than panicking or looping.
    #[test]
    fn buffers_iter_ends_when_not_running() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.signal_stop();
        let mut capture = make_mock_capture(mock);
        let mut it = capture.buffers_iter();
        assert!(it.next().is_none());
    }

    /// Regression (review R2-2): the iterator must DRAIN buffered data after the
    /// stream stops, not discard the tail. We queue 5 buffers, stop the stream,
    /// then iterate — all 5 must be yielded before the iterator ends.
    #[test]
    fn buffers_iter_drains_buffered_tail_after_stop() {
        let mock = Arc::new(MockCapturingStream::new());
        for i in 0..5 {
            mock.push_buffer(AudioBuffer::new(vec![i as f32], 1, 48000));
        }
        // Stop BEFORE iterating — the buffered data must still be drained.
        mock.signal_stop();
        let mut capture = make_mock_capture(mock);

        let collected: Vec<f32> = capture
            .buffers_iter()
            .map(|r| r.expect("buffered reads are Ok").data()[0])
            .collect();
        assert_eq!(
            collected,
            vec![0.0, 1.0, 2.0, 3.0, 4.0],
            "iterator must drain all buffered frames after stop, then end"
        );
    }

    // ── callback delivery tests (H1 / ADR-0002) ──────────────────────

    /// Regression (audit H1): a registered callback must actually be invoked.
    /// We drive the pump helper directly against a mock stream and assert the
    /// closure observes the pushed buffers, then that clearing the callback
    /// stops delivery.
    #[test]
    fn callback_pump_invokes_registered_callback() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let mock = Arc::new(MockCapturingStream::new());
        mock.push_buffer(AudioBuffer::new(vec![0.5; 4], 2, 48000));
        mock.push_buffer(AudioBuffer::new(vec![0.6; 4], 2, 48000));

        let seen = Arc::new(AtomicU64::new(0));
        let seen_cb = Arc::clone(&seen);
        // The pump now OWNS the callback (moved in), so no shared mutex.
        let callback: AudioCallback = Box::new(move |buf: &AudioBuffer| {
            // Encode the first sample (scaled) so we can assert we saw real data.
            seen_cb.fetch_add((buf.data()[0] * 10.0) as u64, Ordering::SeqCst);
        });

        let stream: Arc<dyn CapturingStream> = mock.clone();
        let mut pump = AudioCapture::spawn_callback_pump(stream, callback).expect("pump spawns");

        // Wait until both buffers (0.5*10 + 0.6*10 = 5 + 6 = 11) are delivered.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while seen.load(Ordering::SeqCst) < 11 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(
            seen.load(Ordering::SeqCst),
            11,
            "callback should have been invoked with both buffers"
        );

        // Shut the pump down → it stops consuming; further pushes are not seen.
        pump.shutdown();
        mock.push_buffer(AudioBuffer::new(vec![9.9; 4], 2, 48000));
        std::thread::sleep(std::time::Duration::from_millis(30));
        assert_eq!(
            seen.load(Ordering::SeqCst),
            11,
            "no further delivery after pump shutdown"
        );
        mock.signal_stop();
    }

    // ── start() lifecycle tests (L8) ─────────────────────────────────

    /// Regression (audit L8): start() on an existing-but-stopped stream must
    /// return an error rather than silently succeeding (a stopped stream cannot
    /// be restarted). We simulate a stopped stream by signalling the mock.
    #[test]
    fn start_on_stopped_stream_errors() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.signal_stop(); // stream exists but is not running
        let mut capture = make_mock_capture(mock);

        let result = capture.start();
        assert!(result.is_err(), "start() on a stopped stream must error");
        match result.unwrap_err() {
            AudioError::StreamStartFailed { reason } => {
                assert!(
                    reason.contains("restart") || reason.contains("no longer running"),
                    "unexpected reason: {reason}"
                );
            }
            other => panic!("Expected StreamStartFailed, got: {other:?}"),
        }
    }

    /// start() on an already-running stream is a no-op (idempotent), returning Ok.
    #[test]
    fn start_on_running_stream_is_noop() {
        let mock = Arc::new(MockCapturingStream::new()); // starts running
        let mut capture = make_mock_capture(mock);
        assert!(capture.start().is_ok());
        assert!(
            capture.start().is_ok(),
            "second start() on running stream is Ok"
        );
    }

    /// rsac-7aa2(3): restart-by-recreation is the blessed lifecycle — stop()
    /// releases the stream and a later start() creates a FRESH stream from the
    /// same resolved device; reads work again and the uptime anchor re-anchors.
    #[test]
    fn stop_then_start_restarts_by_recreation() {
        /// A device whose create_stream() hands out a fresh running mock with
        /// one buffered chunk, so the restarted capture demonstrably serves
        /// data end-to-end.
        struct RestartDevice;
        impl crate::core::interface::AudioDevice for RestartDevice {
            fn id(&self) -> crate::core::config::DeviceId {
                crate::core::config::DeviceId("restart-mock".to_string())
            }
            fn name(&self) -> String {
                "RestartMockDevice".to_string()
            }
            fn is_default(&self) -> bool {
                true
            }
            fn supported_formats(&self) -> Vec<AudioFormat> {
                vec![]
            }
            fn create_stream(
                &self,
                _config: &StreamConfig,
            ) -> AudioResult<Box<dyn CapturingStream>> {
                let mock = MockCapturingStream::new();
                mock.push_buffer(AudioBuffer::new(vec![0.6; 4], 2, 48000));
                Ok(Box::new(mock))
            }
        }

        let first = Arc::new(MockCapturingStream::new());
        let mut capture = AudioCapture {
            config: AudioCaptureConfig {
                target: CaptureTarget::SystemDefault,
                stream_config: StreamConfig::default(),
            },
            device: Some(Box::new(RestartDevice)),
            stream: Some(first),
            callback: Mutex::new(None),
            callback_pump: None,
            start_instant: Some(Instant::now()),
            subscriber_dropped: Arc::new(AtomicU64::new(0)),
        };
        assert!(capture.is_running());

        capture.stop().expect("stop ok");
        assert!(!capture.is_running());
        assert!(capture.uptime().is_none(), "stop clears the uptime anchor");

        // Restart: start() re-creates a stream from the resolved device.
        capture.start().expect("restart-by-recreation must succeed");
        assert!(capture.is_running(), "fresh stream is running");
        assert!(
            capture.uptime().is_some(),
            "the uptime anchor re-anchors on restart"
        );

        // The recreated stream serves data end-to-end.
        let buf = capture
            .read_chunk_nonblocking()
            .expect("read ok")
            .expect("data from the recreated stream");
        assert_eq!(buf.data()[0], 0.6);

        capture.stop().expect("second stop ok");
    }

    /// The pump thread exits when the stream stops (try_read_chunk → Err), and
    /// shutdown() is safe to call afterwards (idempotent join).
    #[test]
    fn callback_pump_exits_when_stream_stops() {
        let mock = Arc::new(MockCapturingStream::new());
        let callback: AudioCallback = Box::new(|_: &AudioBuffer| {});
        let stream: Arc<dyn CapturingStream> = mock.clone();
        let mut pump = AudioCapture::spawn_callback_pump(stream, callback).expect("pump spawns");
        // Stopping the mock makes try_read_chunk return Err → pump breaks.
        mock.signal_stop();
        std::thread::sleep(std::time::Duration::from_millis(20));
        // Joining a pump whose thread already exited must not hang or panic.
        pump.shutdown();
    }

    /// Regression (wave-1 review R1-#3): a callback that re-enters the capture
    /// handle must not deadlock. Here the callback increments a counter and, on
    /// the first invocation, flips a flag — proving the pump holds no lock across
    /// the user closure (the closure could otherwise not run arbitrary code).
    #[test]
    fn callback_pump_holds_no_lock_during_invocation() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let mock = Arc::new(MockCapturingStream::new());
        for _ in 0..3 {
            mock.push_buffer(AudioBuffer::new(vec![1.0; 2], 2, 48000));
        }
        let count = Arc::new(AtomicU64::new(0));
        let count_cb = Arc::clone(&count);
        // The closure does real work (sleep) to widen any lock-held window; if
        // the pump held a lock across this, a concurrent shutdown join would
        // stall. We assert delivery proceeds and shutdown completes promptly.
        let callback: AudioCallback = Box::new(move |_buf: &AudioBuffer| {
            count_cb.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(1));
        });
        let stream: Arc<dyn CapturingStream> = mock.clone();
        let mut pump = AudioCapture::spawn_callback_pump(stream, callback).expect("pump");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while count.load(Ordering::SeqCst) < 3 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(
            count.load(Ordering::SeqCst),
            3,
            "all three buffers delivered"
        );
        pump.shutdown();
        mock.signal_stop();
    }

    // ── uptime() tests (rsac-76dc) ────────────────────────────────────

    /// Before any start(), there is no stream and therefore no uptime.
    #[test]
    fn uptime_is_none_before_start() {
        let capture = make_capture_without_stream();
        assert!(capture.uptime().is_none());
    }

    /// After a real start (mock has a stream and we set the anchor as start()
    /// would), uptime() is Some and monotonically non-decreasing across two
    /// reads. We construct the capture with start_instant already set to mirror
    /// the post-start() state (start() itself needs a device to create a
    /// stream, which the mock path bypasses).
    #[test]
    fn uptime_is_some_and_nondecreasing_after_start() {
        let mock = Arc::new(MockCapturingStream::new());
        let mut capture = make_mock_capture(mock);
        // Mirror what start() does on first real stream creation.
        capture.start_instant = Some(Instant::now());

        let first = capture.uptime().expect("uptime is Some after start");
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = capture.uptime().expect("uptime is Some after start");
        assert!(
            second >= first,
            "uptime must be monotonically non-decreasing: {second:?} < {first:?}"
        );
    }

    /// stop() tears down the stream and clears the uptime anchor, so uptime()
    /// returns None afterwards.
    #[test]
    fn uptime_is_none_after_stop() {
        let mock = Arc::new(MockCapturingStream::new());
        let mut capture = make_mock_capture(mock);
        capture.start_instant = Some(Instant::now());
        assert!(capture.uptime().is_some());

        capture.stop().unwrap();
        assert!(
            capture.uptime().is_none(),
            "uptime must be None after stop() drops the stream"
        );
    }

    /// A second start() on an already-running stream must NOT reset the uptime
    /// anchor (idempotent restart). We seed an anchor, call start() (the mock
    /// stream is running, so start() returns Ok without touching the anchor),
    /// and assert the original anchor is preserved.
    #[test]
    fn uptime_anchor_not_reset_on_idempotent_restart() {
        let mock = Arc::new(MockCapturingStream::new()); // running
        let mut capture = make_mock_capture(mock);
        let anchor = Instant::now();
        capture.start_instant = Some(anchor);

        // start() on a running stream is a no-op and returns early, before the
        // is_none() branch that would re-anchor start_instant.
        capture
            .start()
            .expect("idempotent start on running stream is Ok");
        assert_eq!(
            capture.start_instant,
            Some(anchor),
            "idempotent restart must not reset the uptime anchor"
        );
    }

    // ── format() tests (rsac-574d) ────────────────────────────────────

    /// Before start() there is no stream, so format() reports None.
    #[test]
    fn format_is_none_before_start() {
        let capture = make_capture_without_stream();
        assert!(capture.format().is_none());
    }

    /// Once started, format() returns Some(negotiated AudioFormat). A backend
    /// that called set_negotiated_format(44100, 1) makes AudioCapture::format()
    /// report sample_rate 44100, channels 1, normalized F32 — mirroring
    /// test_format_reflects_negotiated_delivery_format in stream.rs.
    #[test]
    fn format_reflects_negotiated_delivery_format() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.set_negotiated_format(44100, 1);
        let capture = make_mock_capture(mock);

        let fmt = capture.format().expect("format is Some once started");
        assert_eq!(fmt.sample_rate, 44100);
        assert_eq!(fmt.channels, 1);
        assert_eq!(fmt.sample_format, SampleFormat::F32);
    }

    /// PU-1/PERF-07 (rsac-2c56): the consumer-observable contract that each
    /// backend now wires via `producer.set_negotiated_format(...)` at its
    /// negotiation point (PipeWire `param_changed`, WASAPI mix-format open,
    /// CoreAudio AUHAL stream-format set). `format()` and
    /// `stream_stats().format_description` must report the *delivered* format the
    /// backend recorded, NOT the *requested* config — the exact regression where
    /// they previously reported the requested format because no backend called
    /// `set_negotiated_format`. Here the builder requested the 48k/2ch default
    /// but the backend negotiated (delivered) 44100/1, so both reads track the
    /// delivered values.
    #[test]
    fn format_reports_delivered_not_requested() {
        let requested = StreamConfig::default(); // 48 kHz, 2ch
        assert_eq!(requested.sample_rate, 48000);
        assert_eq!(requested.channels, 2);

        let mock = Arc::new(MockCapturingStream::new());
        // Backend negotiates a different *delivery* rate/channels than requested.
        mock.set_negotiated_format(44100, 1);
        let mut capture = make_mock_capture(Arc::clone(&mock));
        // The handle still carries the *requested* config unchanged...
        assert_eq!(capture.config().stream_config.sample_rate, 48000);
        assert_eq!(capture.config().stream_config.channels, 2);

        // ...but format() reports the DELIVERED format, divergent from requested.
        let delivered = capture.format().expect("format is Some once started");
        assert_eq!(delivered.sample_rate, 44100);
        assert_eq!(delivered.channels, 1);
        assert_eq!(delivered.sample_format, SampleFormat::F32);
        assert_ne!(
            delivered.sample_rate, requested.sample_rate,
            "format() must track delivered, not requested"
        );

        // And the stats description string is built from the delivered format.
        capture.start_instant = Some(Instant::now());
        assert_eq!(capture.stream_stats().format_description, "1ch 44100Hz F32");
    }

    /// format_description_string yields a non-empty 'Nch RHz FMT' string for
    /// use by stream_stats() on the query path.
    #[test]
    fn format_description_string_is_well_formed() {
        let fmt = AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
        };
        let desc = format_description_string(&fmt);
        assert_eq!(desc, "2ch 48000Hz F32");
        assert!(!desc.is_empty());

        // Sanity-check the other sample formats render their tags.
        let i16_desc = format_description_string(&AudioFormat {
            sample_rate: 44100,
            channels: 1,
            sample_format: SampleFormat::I16,
        });
        assert_eq!(i16_desc, "1ch 44100Hz I16");
    }

    // ── stream_stats() tests (rsac-4c07) ──────────────────────────────

    /// stream_stats() on a never-started capture (no stream) returns the default
    /// snapshot: not running, ZERO uptime, zeroed counters, empty format, and
    /// does not panic.
    #[test]
    fn stream_stats_default_when_no_stream() {
        let capture = make_capture_without_stream();
        let s = capture.stream_stats();
        assert!(!s.is_running);
        assert_eq!(s.uptime, Duration::ZERO);
        assert_eq!(s.buffers_captured, 0);
        assert_eq!(s.buffers_dropped, 0);
        assert_eq!(s.buffers_pushed, 0);
        assert_eq!(s.overruns, 0);
        assert!(s.format_description.is_empty());
        assert_eq!(s.dropped_ratio(), 0.0);
    }

    /// After pushing N, dropping M, popping K, stream_stats() reports
    /// buffers_pushed==N, buffers_dropped==M, buffers_captured==K, is_running==true,
    /// a non-empty format description, and a non-decreasing uptime across two reads.
    #[test]
    fn stream_stats_reflects_counters_and_running() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.set_negotiated_format(44100, 1);
        // N pushed, M dropped, K captured.
        mock.add_pushed(10);
        mock.add_dropped(4);
        mock.add_captured(7);
        let mut capture = make_mock_capture(Arc::clone(&mock));
        // Mirror start()'s anchor so uptime() reports Some.
        capture.start_instant = Some(Instant::now());

        let s1 = capture.stream_stats();
        assert!(s1.is_running, "mock stream is running");
        assert_eq!(s1.buffers_pushed, 10);
        assert_eq!(s1.buffers_dropped, 4);
        assert_eq!(s1.buffers_captured, 7);
        // overruns aliases buffers_dropped.
        assert_eq!(s1.overruns, 4);
        // dropped_ratio = 4 / (7 + 4) = 4/11.
        assert!((s1.dropped_ratio() - 4.0 / 11.0).abs() < f64::EPSILON);
        assert_eq!(s1.format_description, "1ch 44100Hz F32");
        assert!(!s1.format_description.is_empty());

        std::thread::sleep(std::time::Duration::from_millis(2));
        let s2 = capture.stream_stats();
        assert!(
            s2.uptime >= s1.uptime,
            "uptime must be non-decreasing across reads: {:?} < {:?}",
            s2.uptime,
            s1.uptime
        );
    }

    /// After stop(), the stream is dropped, so stream_stats() falls back to the
    /// default snapshot (not running, ZERO uptime).
    #[test]
    fn stream_stats_default_after_stop() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.add_pushed(5);
        let mut capture = make_mock_capture(mock);
        capture.start_instant = Some(Instant::now());
        assert!(capture.stream_stats().is_running);

        capture.stop().unwrap();
        let s = capture.stream_stats();
        assert!(!s.is_running);
        assert_eq!(s.uptime, Duration::ZERO);
        assert_eq!(s.buffers_pushed, 0, "no stream → zeroed counters");
    }

    // ── backpressure_report() tests (rsac-cfe4) ───────────────────────

    /// backpressure_report() on a capture with no stream is the all-zero default.
    #[test]
    fn backpressure_report_default_when_no_stream() {
        let capture = make_capture_without_stream();
        let r = capture.backpressure_report();
        assert_eq!(r.pushed, 0);
        assert_eq!(r.dropped, 0);
        assert_eq!(r.drop_rate, 0.0);
        assert!(!r.is_under_backpressure);
    }

    /// A drop,push,drop,push pattern that never trips the consecutive-threshold
    /// bool (is_under_backpressure stays false) still reports drop_rate ~0.5 in
    /// the windowed report — the core motivation of rsac-cfe4.
    #[test]
    fn backpressure_report_surfaces_partial_loss_bool_misses() {
        let mock = Arc::new(MockCapturingStream::new());
        // Interleaved drop,push,drop,push → 2 pushed, 2 dropped, and the legacy
        // consecutive-drop bool never trips (each push resets the run).
        mock.add_pushed(2);
        mock.add_dropped(2);
        mock.set_backpressure(false);
        let capture = make_mock_capture(mock);

        let r = capture.backpressure_report();
        assert_eq!(r.pushed, 2);
        assert_eq!(r.dropped, 2);
        assert!(
            (r.drop_rate - 0.5).abs() < f64::EPSILON,
            "drop_rate should be ~0.5, got {}",
            r.drop_rate
        );
        assert!(
            !r.is_under_backpressure,
            "consecutive-drop bool stays false while windowed drop_rate shows loss"
        );
    }

    /// rsac-cfe4: `estimate_window_span` derives the report's `window` from the
    /// buffer size and negotiated rate, falling back to ZERO when unattributable.
    #[test]
    fn estimate_window_span_derives_and_falls_back() {
        // The mock's format() reports the negotiated rate, which takes precedence
        // over config.sample_rate; set it so the two agree on 48000.
        let mock = Arc::new(MockCapturingStream::new());
        mock.format.lock().unwrap().sample_rate = 48000;
        let mut cap = make_mock_capture(mock);
        cap.config.stream_config.buffer_size = Some(1024);
        cap.config.stream_config.sample_rate = 48000;

        // Known buffer_size + rate → exact span: 100 buffers × 1024 frames @ 48000.
        let span = cap.estimate_window_span(100);
        let expected = 100.0 * 1024.0 / 48000.0; // ≈ 2.133s
        assert!(
            (span.as_secs_f64() - expected).abs() < 1e-6,
            "span {} should equal {}",
            span.as_secs_f64(),
            expected
        );

        // Zero buffers → zero span (no fabricated duration).
        assert_eq!(cap.estimate_window_span(0), Duration::ZERO);

        // Unknown buffer_size → ZERO (span unattributable).
        cap.config.stream_config.buffer_size = None;
        assert_eq!(cap.estimate_window_span(100), Duration::ZERO);
    }

    /// rsac-cfe4: when neither the negotiated format nor the config carries a
    /// usable rate, `estimate_window_span` falls back to ZERO (div-by-zero guard)
    /// rather than fabricating a span.
    #[test]
    fn estimate_window_span_zero_rate_falls_back() {
        let mock = Arc::new(MockCapturingStream::new());
        // Negotiated format rate 0 AND config rate 0 → no usable rate anywhere.
        mock.format.lock().unwrap().sample_rate = 0;
        let mut cap = make_mock_capture(mock);
        cap.config.stream_config.buffer_size = Some(512);
        cap.config.stream_config.sample_rate = 0;
        assert_eq!(cap.estimate_window_span(100), Duration::ZERO);
    }

    /// rsac-cfe4: `backpressure_report()` reports a non-zero `window` when the
    /// buffer size and rate are known, matching `estimate_window_span`.
    #[test]
    fn backpressure_report_populates_window_span() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.add_pushed(10);
        mock.add_dropped(2);
        mock.format.lock().unwrap().sample_rate = 48000;
        let mut cap = make_mock_capture(mock);
        cap.config.stream_config.buffer_size = Some(960);
        cap.config.stream_config.sample_rate = 48000;

        let r = cap.backpressure_report();
        assert_eq!(r.pushed, 10);
        assert_eq!(r.dropped, 2);
        // window == (pushed + dropped=12) × 960 / 48000 = 0.24s, NOT Duration::ZERO.
        let expected = 12.0 * 960.0 / 48000.0;
        assert!(
            (r.window.as_secs_f64() - expected).abs() < 1e-6,
            "window {} should equal {}",
            r.window.as_secs_f64(),
            expected
        );
        assert_ne!(
            r.window,
            Duration::ZERO,
            "window must be populated, not lifetime-ZERO"
        );
    }

    /// The legacy bool is carried through unchanged when it is set.
    #[test]
    fn backpressure_report_carries_legacy_bool() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.add_pushed(1);
        mock.add_dropped(9);
        mock.set_backpressure(true);
        let capture = make_mock_capture(mock);

        let r = capture.backpressure_report();
        assert!(r.is_under_backpressure, "legacy bool carried through");
        assert!((r.drop_rate - 0.9).abs() < f64::EPSILON);
    }

    // ── target_str() tests (rsac-0f75) ────────────────────────────────

    /// target_str("system") parses to SystemDefault and the builder carries it.
    #[test]
    fn target_str_system_sets_system_default() {
        let builder = AudioCaptureBuilder::new()
            .target_str("system")
            .expect("'system' parses");
        assert_eq!(builder.target, CaptureTarget::SystemDefault);
    }

    /// target_str("app:55") → Application(ApplicationId("55")).
    #[test]
    fn target_str_app_sets_application() {
        let builder = AudioCaptureBuilder::new()
            .target_str("app:55")
            .expect("'app:55' parses");
        assert_eq!(
            builder.target,
            CaptureTarget::Application(crate::core::config::ApplicationId("55".to_string()))
        );
    }

    /// target_str("name:VLC") → ApplicationByName("VLC").
    #[test]
    fn target_str_name_sets_application_by_name() {
        let builder = AudioCaptureBuilder::new()
            .target_str("name:VLC")
            .expect("'name:VLC' parses");
        assert_eq!(
            builder.target,
            CaptureTarget::ApplicationByName("VLC".to_string())
        );
    }

    /// A device string with embedded colons round-trips (first-colon split).
    #[test]
    fn target_str_device_preserves_colons() {
        let builder = AudioCaptureBuilder::new()
            .target_str("device:hw:0,0")
            .expect("'device:hw:0,0' parses");
        assert_eq!(
            builder.target,
            CaptureTarget::Device(crate::core::config::DeviceId("hw:0,0".to_string()))
        );
    }

    /// target_str("garbage") returns InvalidParameter and (because the method
    /// consumes self and only returns the builder on success) the caller's
    /// builder is untouched — we verify the error shape and that a fresh
    /// builder still defaults to SystemDefault.
    #[test]
    fn target_str_garbage_errors_and_does_not_mutate_target() {
        let builder = AudioCaptureBuilder::new();
        // Sanity: starts at the default target.
        assert_eq!(builder.target, CaptureTarget::SystemDefault);

        let result = builder.target_str("garbage");
        assert!(result.is_err(), "unknown scheme must error");
        match result.unwrap_err() {
            AudioError::InvalidParameter { param, .. } => {
                assert_eq!(param, "capture_target");
            }
            e => panic!("Expected InvalidParameter, got: {e:?}"),
        }

        // The builder was consumed by the failed call; a freshly created one
        // still carries the default target (no global mutation occurred).
        assert_eq!(
            AudioCaptureBuilder::new().target,
            CaptureTarget::SystemDefault
        );
    }

    /// target_str is chainable (returns AudioResult<Self>) and composes with the
    /// other setters.
    #[test]
    fn target_str_is_chainable() {
        let builder = AudioCaptureBuilder::new()
            .sample_rate(44100)
            .target_str("app:7")
            .expect("parses")
            .channels(1);
        assert_eq!(
            builder.target,
            CaptureTarget::Application(crate::core::config::ApplicationId("7".to_string()))
        );
        assert_eq!(builder.config.sample_rate, 44100);
        assert_eq!(builder.config.channels, 1);
    }

    /// The infallible try_target_str applies a valid string and silently keeps
    /// the prior target for an invalid one.
    #[test]
    fn try_target_str_applies_valid_keeps_prior_on_invalid() {
        // Valid → applied.
        let ok = AudioCaptureBuilder::new().try_target_str("name:Spotify");
        assert_eq!(
            ok.target,
            CaptureTarget::ApplicationByName("Spotify".to_string())
        );

        // Invalid → unchanged. Pre-set a non-default target, feed garbage, and
        // assert the prior target survives.
        let kept = AudioCaptureBuilder::new()
            .with_target(CaptureTarget::pid(99))
            .try_target_str("garbage");
        assert_eq!(
            kept.target,
            CaptureTarget::ProcessTree(crate::core::config::ProcessId(99))
        );
    }

    // ── preflight() tests (rsac-b65a) ─────────────────────────────────

    /// preflight() passes for a valid SystemDefault/48000/2ch config, without
    /// enumerating any device.
    #[test]
    fn preflight_ok_for_valid_default_config() {
        let builder = AudioCaptureBuilder::new()
            .with_target(CaptureTarget::SystemDefault)
            .sample_rate(48000)
            .channels(2);
        assert!(
            builder.preflight().is_ok(),
            "valid config must pass preflight"
        );
    }

    /// preflight() rejects an unsupported sample rate with
    /// InvalidParameter{param:"sample_rate"} and does NOT touch a device.
    #[test]
    fn preflight_rejects_unsupported_sample_rate() {
        let builder = AudioCaptureBuilder::new().sample_rate(11025);
        match builder.preflight().unwrap_err() {
            AudioError::InvalidParameter { param, reason } => {
                assert_eq!(param, "sample_rate");
                assert!(reason.contains("11025"));
            }
            e => panic!("Expected InvalidParameter, got: {e:?}"),
        }
    }

    /// preflight() rejects channels == 0 with ConfigurationError.
    #[test]
    fn preflight_rejects_zero_channels() {
        let builder = AudioCaptureBuilder::new().channels(0);
        match builder.preflight().unwrap_err() {
            AudioError::ConfigurationError { message } => {
                assert_eq!(message, "Channels must be greater than 0.");
            }
            e => panic!("Expected ConfigurationError, got: {e:?}"),
        }
    }

    /// preflight() rejects channels > 32 (MAX_CHANNELS) with ConfigurationError.
    #[test]
    fn preflight_rejects_channels_above_max() {
        let builder = AudioCaptureBuilder::new().channels(33);
        match builder.preflight().unwrap_err() {
            AudioError::ConfigurationError { message } => {
                assert!(
                    message.contains("33") && message.contains("32"),
                    "unexpected message: {message}"
                );
            }
            e => panic!("Expected ConfigurationError, got: {e:?}"),
        }
    }

    /// preflight() accepts the channel-count boundaries 1 and 32.
    #[test]
    fn preflight_accepts_channel_boundaries() {
        assert!(AudioCaptureBuilder::new().channels(1).preflight().is_ok());
        assert!(AudioCaptureBuilder::new().channels(32).preflight().is_ok());
    }

    /// preflight() accepts every rate in the supported whitelist.
    #[test]
    fn preflight_accepts_all_supported_sample_rates() {
        for rate in SUPPORTED_SAMPLE_RATES {
            assert!(
                AudioCaptureBuilder::new()
                    .sample_rate(rate)
                    .preflight()
                    .is_ok(),
                "rate {rate} should pass preflight"
            );
        }
    }

    /// On a platform whose backend does not support application capture,
    /// preflight() with an Application target returns PlatformNotSupported —
    /// the same error build() raises. We assert the capability-gated behavior
    /// matches PlatformCapabilities::query() on whatever platform runs the test.
    #[test]
    fn preflight_application_matches_capability_gate() {
        let caps = PlatformCapabilities::query();
        let builder = AudioCaptureBuilder::new()
            .with_target(CaptureTarget::Application(
                crate::core::config::ApplicationId("1234".to_string()),
            ))
            .sample_rate(48000)
            .channels(2);

        let result = builder.preflight();
        if caps.supports_application_capture {
            // The capability check must pass (any later failure would be from a
            // step preflight does not perform; preflight itself returns Ok).
            assert!(
                result.is_ok(),
                "preflight must pass app capability when supported"
            );
        } else {
            match result.unwrap_err() {
                AudioError::PlatformNotSupported { feature, platform } => {
                    assert_eq!(feature, "application capture");
                    assert_eq!(platform, caps.backend_name);
                }
                e => panic!("Expected PlatformNotSupported, got: {e:?}"),
            }
        }
    }

    /// The refactor is behavior-preserving: build() still rejects the same
    /// configs preflight() does, with the same error variants (proving build()
    /// routes through preflight()). Mirrors the existing builder_fails_* tests.
    #[test]
    fn build_routes_through_preflight_same_errors() {
        // Zero channels → ConfigurationError, before any device work.
        match AudioCaptureBuilder::new().channels(0).build().unwrap_err() {
            AudioError::ConfigurationError { .. } => {}
            e => panic!("Expected ConfigurationError from build(), got: {e:?}"),
        }
        // Unsupported rate → InvalidParameter{sample_rate}, before any device work.
        match AudioCaptureBuilder::new()
            .sample_rate(11025)
            .build()
            .unwrap_err()
        {
            AudioError::InvalidParameter { param, .. } => assert_eq!(param, "sample_rate"),
            e => panic!("Expected InvalidParameter from build(), got: {e:?}"),
        }
    }

    // ── RunningCapture / builder.start() tests (rsac-9175) ─────────────

    /// Build a RunningCapture directly from a mock-backed AudioCapture (the
    /// builder.start() path needs a device the mock layer bypasses, so we wrap
    /// the capture the same way builder.start() would).
    fn make_running_capture(mock: Arc<MockCapturingStream>) -> RunningCapture {
        RunningCapture(make_mock_capture(mock))
    }

    /// RunningCapture derefs to AudioCapture: read/stats/state methods are all
    /// reachable through Deref/DerefMut on the guard.
    #[test]
    fn running_capture_derefs_to_audio_capture() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.push_buffer(AudioBuffer::new(vec![0.7; 4], 2, 48000));
        let mut guard = make_running_capture(Arc::clone(&mock));

        // Through Deref: is_running() and stream_stats().
        assert!(guard.is_running());
        let stats = guard.stream_stats();
        assert!(stats.is_running);

        // read_buffer() now takes &self; it is reachable through Deref. A
        // DerefMut caller (a &mut guard) can still invoke a &self method, so the
        // narrowing is source-compatible.
        let buf = guard
            .read_buffer()
            .expect("read ok")
            .expect("a buffer is available");
        assert_eq!(buf.data()[0], 0.7);

        // Exercise a &mut-self method through DerefMut so `mut guard` is genuinely
        // required — proving the guard forwards both &self and &mut self methods.
        let _ = guard.stop();

        mock.signal_stop();
    }

    /// Dropping a RunningCapture stops the capture exactly once (the guard's
    /// Drop calls AudioCapture::stop, which stops the underlying stream once;
    /// the AudioCapture's own Drop then finds no stream and does not re-stop).
    #[test]
    fn running_capture_drop_stops_once() {
        let mock = Arc::new(MockCapturingStream::new());
        let guard = make_running_capture(Arc::clone(&mock));
        assert_eq!(mock.stop_calls(), 0, "not stopped before drop");

        drop(guard);
        assert_eq!(
            mock.stop_calls(),
            1,
            "guard Drop must stop the underlying stream exactly once"
        );
    }

    /// into_inner() returns the wrapped AudioCapture WITHOUT triggering the
    /// guard's stop. The returned capture is still running; stopping it later
    /// is the caller's responsibility.
    #[test]
    fn running_capture_into_inner_does_not_stop() {
        let mock = Arc::new(MockCapturingStream::new());
        let guard = make_running_capture(Arc::clone(&mock));

        let mut capture = guard.into_inner();
        assert_eq!(mock.stop_calls(), 0, "into_inner must not stop the capture");
        assert!(
            capture.is_running(),
            "capture still running after into_inner"
        );

        // The caller can now stop it explicitly.
        capture.stop().expect("explicit stop ok");
        assert_eq!(mock.stop_calls(), 1);
    }

    /// No double-stop: an explicit stop() followed by dropping the guard does
    /// not error and does not stop the underlying stream a second time (after
    /// the explicit stop, the AudioCapture has dropped its Arc, so the guard's
    /// Drop stop() is a no-op on the stream).
    #[test]
    fn running_capture_explicit_stop_then_drop_no_double_stop() {
        let mock = Arc::new(MockCapturingStream::new());
        let mut guard = make_running_capture(Arc::clone(&mock));

        guard.stop().expect("explicit stop ok");
        assert_eq!(mock.stop_calls(), 1, "explicit stop hit the stream once");

        // Dropping after an explicit stop must not panic, error, or re-stop the
        // underlying stream (the AudioCapture already released its Arc).
        drop(guard);
        assert_eq!(
            mock.stop_calls(),
            1,
            "drop after explicit stop must not double-stop the stream"
        );
    }

    /// The RAII guard ties teardown to scope: leaving a block drops the guard
    /// and stops the capture.
    #[test]
    fn running_capture_stops_at_scope_end() {
        let mock = Arc::new(MockCapturingStream::new());
        {
            let _guard = make_running_capture(Arc::clone(&mock));
            assert_eq!(mock.stop_calls(), 0);
        }
        assert_eq!(
            mock.stop_calls(),
            1,
            "leaving scope must stop the capture exactly once"
        );
    }

    // ── request_stop() tests (H2 / #28) ───────────────────────────────

    /// request_stop() signals the underlying stream (via its idempotent stop)
    /// so a parked read_buffer_blocking observes a terminal state. We assert it
    /// stops the mock stream exactly once.
    #[test]
    fn request_stop_signals_stream_once() {
        let mock = Arc::new(MockCapturingStream::new());
        let capture = make_mock_capture(Arc::clone(&mock));
        assert!(mock.is_running());
        assert_eq!(mock.stop_calls(), 0);

        // Takes &self — no &mut alias to the handle.
        capture.request_stop();
        assert_eq!(mock.stop_calls(), 1, "request_stop must signal stop once");
        assert!(
            !mock.is_running(),
            "stream is no longer running after request_stop"
        );
    }

    /// request_stop() is idempotent: a second call after the stream is already
    /// stopped still succeeds (no panic) — it just re-signals the idempotent
    /// stream stop.
    #[test]
    fn request_stop_is_idempotent() {
        let mock = Arc::new(MockCapturingStream::new());
        let capture = make_mock_capture(Arc::clone(&mock));
        capture.request_stop();
        capture.request_stop();
        // The mock counts each stop() call; both are accepted without panic.
        assert_eq!(mock.stop_calls(), 2);
        assert!(!mock.is_running());
    }

    /// request_stop() on a capture with no stream is a no-op (does not panic).
    #[test]
    fn request_stop_no_stream_is_noop() {
        let capture = make_capture_without_stream();
        // Must not panic when there is no stream to signal.
        capture.request_stop();
    }

    /// request_stop() unblocks a parked read_buffer_blocking(): a reader blocked
    /// on an empty-but-running mock returns the terminal StreamEnded once
    /// request_stop transitions the stream. Drives the real read path through
    /// &self (proving the narrowing lets a concurrent request_stop run while a
    /// read is in flight, with no &mut alias).
    #[test]
    fn request_stop_unblocks_parked_blocking_read() {
        use std::sync::atomic::AtomicBool;
        let mock = Arc::new(MockCapturingStream::new());
        // No buffers queued, but the mock reports running, so read_chunk() spins.
        let capture = Arc::new(make_mock_capture(Arc::clone(&mock)));

        // Barrier instead of a fixed sleep: the reader flips `entered` right
        // before it enters the blocking read, so the stop is signalled only once
        // the read is genuinely in flight — deterministic under parallel load.
        let entered = Arc::new(AtomicBool::new(false));
        let reader = {
            let capture = Arc::clone(&capture);
            let entered = Arc::clone(&entered);
            std::thread::spawn(move || {
                entered.store(true, Ordering::SeqCst);
                capture.read_buffer_blocking()
            })
        };

        // Wait (bounded) until the reader has entered the blocking read, so a
        // genuine hang still fails the test rather than spinning forever.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !entered.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }
        // Brief settle so the reader reaches read_chunk's spin-wait, then signal
        // a stop through &self — no &mut alias to the handle.
        std::thread::sleep(std::time::Duration::from_millis(5));
        capture.request_stop();

        let result = reader.join().expect("reader thread joins");
        match result {
            Err(AudioError::StreamEnded { .. }) => {}
            other => panic!("expected StreamEnded after request_stop, got: {other:?}"),
        }
    }

    // ── drain_to() tests (AEG-4 / FH-6) ───────────────────────────────

    /// A test sink that records, behind shared atomics, how many buffers it
    /// wrote and how many frames, and how many times flush()/close() were called.
    /// The atomics let the test inspect counts after the sink has been moved into
    /// (and dropped by) the drain thread. Optionally fails write() once with a
    /// FATAL error to exercise the early-exit-still-finalizes path.
    struct CountingSink {
        writes: Arc<AtomicU64>,
        frames: Arc<AtomicU64>,
        flushes: Arc<AtomicU64>,
        closes: Arc<AtomicU64>,
        /// When true, the FIRST write() returns a fatal error (and is counted as
        /// an attempt). Subsequent writes would succeed, but the drain loop should
        /// have already broken out.
        fail_first_write_fatal: bool,
        first_write_seen: bool,
    }

    impl CountingSink {
        fn shared() -> (
            Arc<AtomicU64>,
            Arc<AtomicU64>,
            Arc<AtomicU64>,
            Arc<AtomicU64>,
        ) {
            (
                Arc::new(AtomicU64::new(0)),
                Arc::new(AtomicU64::new(0)),
                Arc::new(AtomicU64::new(0)),
                Arc::new(AtomicU64::new(0)),
            )
        }
    }

    impl crate::sink::AudioSink for CountingSink {
        fn write(&mut self, buffer: &AudioBuffer) -> AudioResult<()> {
            if self.fail_first_write_fatal && !self.first_write_seen {
                self.first_write_seen = true;
                // A fatal write error: the drain loop must break but still
                // flush()/close() afterwards.
                return Err(AudioError::ConfigurationError {
                    message: "simulated fatal sink write failure".into(),
                });
            }
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.frames
                .fetch_add(buffer.num_frames() as u64, Ordering::SeqCst);
            Ok(())
        }

        fn flush(&mut self) -> AudioResult<()> {
            self.flushes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn close(&mut self) -> AudioResult<()> {
            self.closes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    /// drain_to() drives the sink: every buffered AudioBuffer is delivered via
    /// sink.write(), and on the terminal stream the thread flushes + closes the
    /// sink exactly once. This is the first path that drives AudioSink::write.
    #[test]
    fn drain_to_writes_all_buffers_then_flushes_and_closes() {
        let mock = Arc::new(MockCapturingStream::new());
        // Queue three buffers (2 frames each), then stop so the drain reaches a
        // fatal terminal after draining the tail.
        mock.push_buffer(AudioBuffer::new(vec![0.1; 4], 2, 48000));
        mock.push_buffer(AudioBuffer::new(vec![0.2; 4], 2, 48000));
        mock.push_buffer(AudioBuffer::new(vec![0.3; 4], 2, 48000));

        let (writes, frames, flushes, closes) = CountingSink::shared();
        let sink = CountingSink {
            writes: Arc::clone(&writes),
            frames: Arc::clone(&frames),
            flushes: Arc::clone(&flushes),
            closes: Arc::clone(&closes),
            fail_first_write_fatal: false,
            first_write_seen: false,
        };

        let capture = RunningCapture(make_mock_capture(Arc::clone(&mock)));
        let drain = capture.drain_to(sink).expect("drain_to should succeed");

        // POLL until the pump has drained all three buffered buffers, instead of
        // a fixed sleep that raced the pump-thread start under a loaded parallel
        // run. Bounded so a genuine hang still fails the test.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while writes.load(Ordering::SeqCst) < 3 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        // Now stop so the drain loop hits the fatal terminal and finalizes the
        // sink (flush + close exactly once).
        mock.signal_stop();

        // shutdown() joins the thread, so all writes + the single flush/close
        // have completed by the time it returns.
        drain.shutdown();

        assert_eq!(
            writes.load(Ordering::SeqCst),
            3,
            "all three buffers written"
        );
        assert_eq!(frames.load(Ordering::SeqCst), 6, "2 frames per buffer × 3");
        assert_eq!(
            flushes.load(Ordering::SeqCst),
            1,
            "flush called exactly once"
        );
        assert_eq!(
            closes.load(Ordering::SeqCst),
            1,
            "close called exactly once"
        );
    }

    /// rsac-7aa2(2): drain_to on a stream already at its fatal terminal is
    /// ACCEPTED (presence-only gate, drain-the-tail policy): the drain thread
    /// exits on its first read, writes nothing, and still finalizes the sink
    /// (flush + close) so e.g. a WAV header is written for an empty capture.
    #[test]
    fn drain_to_on_terminal_stream_finalizes_immediately() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.signal_stop(); // already terminal, no tail
        let capture = RunningCapture(make_mock_capture(mock));

        let (writes, frames, flushes, closes) = CountingSink::shared();
        let sink = CountingSink {
            writes: Arc::clone(&writes),
            frames,
            flushes: Arc::clone(&flushes),
            closes: Arc::clone(&closes),
            fail_first_write_fatal: false,
            first_write_seen: false,
        };
        let drain = capture
            .drain_to(sink)
            .expect("a terminal stream is accepted (drain-the-tail policy)");
        drain.shutdown(); // joins → the finalize has completed

        assert_eq!(writes.load(Ordering::SeqCst), 0, "nothing to drain");
        assert_eq!(flushes.load(Ordering::SeqCst), 1, "sink still flushed");
        assert_eq!(closes.load(Ordering::SeqCst), 1, "sink still closed");
    }

    /// rsac-7aa2(2): drain_to accepts the drainable Stopping window and drains
    /// the buffered tail into the sink before finalizing — the old is_running()
    /// gate rejected this call and stranded the tail.
    #[test]
    fn drain_to_accepts_stopping_window_and_drains_tail() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.push_buffer(AudioBuffer::new(vec![0.1; 4], 2, 48000));
        mock.push_buffer(AudioBuffer::new(vec![0.2; 4], 2, 48000));
        mock.signal_stop(); // not running, tail still drainable
        let capture = RunningCapture(make_mock_capture(Arc::clone(&mock)));

        let (writes, frames, flushes, closes) = CountingSink::shared();
        let sink = CountingSink {
            writes: Arc::clone(&writes),
            frames: Arc::clone(&frames),
            flushes: Arc::clone(&flushes),
            closes: Arc::clone(&closes),
            fail_first_write_fatal: false,
            first_write_seen: false,
        };
        let drain = capture
            .drain_to(sink)
            .expect("the Stopping window must be accepted");

        // Poll until the tail is drained (bounded), then join.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while writes.load(Ordering::SeqCst) < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        drain.shutdown();

        assert_eq!(writes.load(Ordering::SeqCst), 2, "the tail is drained");
        assert_eq!(frames.load(Ordering::SeqCst), 4, "2 frames per buffer × 2");
        assert_eq!(flushes.load(Ordering::SeqCst), 1);
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }

    /// A FATAL write() error ends the drain loop early, but flush() and close()
    /// still run so the sink is finalized (e.g. a WAV header is written).
    #[test]
    fn drain_to_finalizes_sink_after_fatal_write() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.push_buffer(AudioBuffer::new(vec![0.5; 4], 2, 48000));
        mock.push_buffer(AudioBuffer::new(vec![0.6; 4], 2, 48000));

        let (writes, frames, flushes, closes) = CountingSink::shared();
        let sink = CountingSink {
            writes: Arc::clone(&writes),
            frames: Arc::clone(&frames),
            flushes: Arc::clone(&flushes),
            closes: Arc::clone(&closes),
            fail_first_write_fatal: true,
            first_write_seen: false,
        };

        let capture = RunningCapture(make_mock_capture(Arc::clone(&mock)));
        let drain = capture.drain_to(sink).expect("drain_to should succeed");

        std::thread::sleep(std::time::Duration::from_millis(20));
        mock.signal_stop();
        drain.shutdown();

        // The first write failed fatally → loop broke immediately, so no
        // successful writes recorded — but flush + close still ran.
        assert_eq!(
            writes.load(Ordering::SeqCst),
            0,
            "no successful writes after the fatal first write"
        );
        assert_eq!(
            flushes.load(Ordering::SeqCst),
            1,
            "flush still runs on fatal exit"
        );
        assert_eq!(
            closes.load(Ordering::SeqCst),
            1,
            "close still runs on fatal exit"
        );
    }

    /// A recoverable read error ahead of buffered data must NOT end draining: the
    /// buffer behind the transient hiccup is still written.
    #[test]
    fn drain_to_continues_past_recoverable_read_error() {
        let mock = Arc::new(MockCapturingStream::new());
        mock.inject_recoverable_error(AudioError::StreamReadError {
            reason: "transient hiccup".into(),
        });
        mock.push_buffer(AudioBuffer::new(vec![0.9; 4], 2, 48000));

        let (writes, frames, flushes, closes) = CountingSink::shared();
        let sink = CountingSink {
            writes: Arc::clone(&writes),
            frames: Arc::clone(&frames),
            flushes: Arc::clone(&flushes),
            closes: Arc::clone(&closes),
            fail_first_write_fatal: false,
            first_write_seen: false,
        };

        let capture = RunningCapture(make_mock_capture(Arc::clone(&mock)));
        let drain = capture.drain_to(sink).expect("drain_to should succeed");

        // Poll until the pump has retried past the recoverable error and drained
        // the one buffer behind it, instead of a fixed sleep that raced the
        // pump-thread start under parallel load. Bounded so a hang still fails.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while writes.load(Ordering::SeqCst) < 1 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        mock.signal_stop();
        drain.shutdown();

        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "the buffer behind the recoverable error is still drained"
        );
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }

    /// drain_to() drains into the real bundled WavFileSink and produces a valid,
    /// readable WAV file (the end-to-end round-trip the example relies on).
    #[cfg(feature = "sink-wav")]
    #[test]
    fn drain_to_wav_round_trip_writes_valid_file() {
        use crate::core::config::AudioFormat;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drain_round_trip.wav");

        let mock = Arc::new(MockCapturingStream::new());
        // Two stereo buffers, 2 frames each at 48k.
        mock.push_buffer(AudioBuffer::new(vec![0.25, -0.25, 0.5, -0.5], 2, 48000));
        mock.push_buffer(AudioBuffer::new(vec![0.1, -0.1, 0.2, -0.2], 2, 48000));

        let format = AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
        };
        let sink = crate::sink::WavFileSink::new(&path, &format).expect("sink");

        let capture = RunningCapture(make_mock_capture(Arc::clone(&mock)));
        let drain = capture.drain_to(sink).expect("drain_to should succeed");

        std::thread::sleep(std::time::Duration::from_millis(20));
        mock.signal_stop();
        drain.shutdown(); // joins the thread → file flushed + finalized

        // The WAV must be valid and contain all the samples we pushed.
        let reader = hound::WavReader::open(&path).expect("valid WAV");
        let spec = reader.spec();
        assert_eq!(spec.channels, 2);
        assert_eq!(spec.sample_rate, 48000);
        let samples: Vec<f32> = reader.into_samples::<f32>().map(|s| s.unwrap()).collect();
        assert_eq!(samples.len(), 8, "two 4-sample buffers");
        assert!((samples[0] - 0.25).abs() < 1e-6);
        assert!((samples[7] - (-0.2)).abs() < 1e-6);
    }
}
