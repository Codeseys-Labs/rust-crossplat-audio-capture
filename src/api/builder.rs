//! [`AudioCaptureBuilder`]: configuration, device-independent validation
//! ([`preflight`](AudioCaptureBuilder::preflight)), device resolution, and
//! format negotiation ahead of [`AudioCapture`] construction.

use super::capture::AudioCapture;
#[cfg(not(target_os = "linux"))]
use super::negotiation::negotiate_device_format;
use super::running::RunningCapture;

use crate::audio::get_device_enumerator;
use crate::core::capabilities::PlatformCapabilities;
#[cfg(target_os = "android")]
use crate::core::config::AndroidProjectionToken;
use crate::core::config::{
    AudioCaptureConfig, AudioFormat, CaptureTarget, SampleFormat, StreamConfig,
};
use crate::core::error::{AudioError, AudioResult};
#[cfg(feature = "hotplug-recovery")]
use crate::core::recovery::AutoRecoveryConfig;
use std::str::FromStr;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

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
pub(super) const SUPPORTED_SAMPLE_RATES: [u32; 6] = PlatformCapabilities::SUPPORTED_SAMPLE_RATES;

/// The maximum channel count the builder accepts at config time (the valid
/// range is `1..=MAX_CHANNELS`). Mirrors the most permissive backend's ceiling;
/// a narrower per-platform limit is enforced later by `PlatformCapabilities`.
const MAX_CHANNELS: u16 = 32;

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
    pub(super) target: CaptureTarget,
    pub(super) config: StreamConfig,
    /// Android `MediaProjection` consent token (rsac-82d4 / ADR-0013).
    /// Present only on Android builds; playback-capture targets need it once
    /// an Android backend advertises the capability.
    #[cfg(target_os = "android")]
    android_projection: Option<AndroidProjectionToken>,
    /// Auto-recovery policy set by
    /// [`with_auto_recovery`](Self::with_auto_recovery); `None` keeps the
    /// stream unsupervised (the default).
    #[cfg(feature = "hotplug-recovery")]
    auto_recovery: Option<AutoRecoveryConfig>,
}

impl Default for AudioCaptureBuilder {
    fn default() -> Self {
        Self {
            target: CaptureTarget::SystemDefault,
            config: StreamConfig::default(),
            #[cfg(target_os = "android")]
            android_projection: None,
            #[cfg(feature = "hotplug-recovery")]
            auto_recovery: None,
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

    /// Enables automatic recovery when the system default device changes or the
    /// running stream is invalidated *(feature `hotplug-recovery`)*.
    ///
    /// The started stream is supervised: a default-device change (coalesced
    /// over [`settle_delay`](AutoRecoveryConfig::settle_delay)) or a terminal
    /// backend error closes the old stream **before** opening a replacement on
    /// the current default device, retrying with capped exponential backoff per
    /// `config`. Delivered buffers keep a monotonic timeline across reopens and
    /// carry the [`stream_generation`](crate::core::buffer::AudioBuffer::stream_generation)
    /// that produced them; observe progress via
    /// [`AudioCapture::recovery_snapshot`] and
    /// [`AudioCapture::subscribe_recovery_events`]. A replacement whose delivery
    /// format differs from the original ends the stream with a fatal
    /// [`AudioError::StreamEnded`] before any sample of the new format is
    /// delivered.
    ///
    /// Only [`CaptureTarget::SystemDefault`] on desktop platforms is supported;
    /// [`preflight`](Self::preflight) (and therefore [`build`](Self::build) /
    /// [`start`](Self::start)) rejects every other combination.
    #[cfg(feature = "hotplug-recovery")]
    pub fn with_auto_recovery(mut self, config: AutoRecoveryConfig) -> Self {
        self.auto_recovery = Some(config);
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
    /// - *(feature `hotplug-recovery`)* [`AudioError::PlatformNotSupported`]
    ///   when `with_auto_recovery` is set on Android or iOS, and
    ///   [`AudioError::ConfigurationError`] when it is set with any target other
    ///   than [`CaptureTarget::SystemDefault`].
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
        // ── Auto-recovery policy (hotplug-recovery) ──────────────────
        // Checked first: the supported scope (desktop SystemDefault) is a
        // config-time contract independent of what the platform can capture.
        #[cfg(feature = "hotplug-recovery")]
        {
            if self.auto_recovery.is_some() {
                super::recovery::preflight_auto_recovery(&self.target)?;
            }
        }

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

        // ── Validate sample rate ────────────────────────────────────
        // Parameter validation runs BEFORE the consent preflights below: an
        // invalid config is an invalid config regardless of consent state,
        // and reporting UserConsentRequired for a channels=0 build would
        // send the caller chasing a consent dialog that cannot help. (Found
        // by the first-ever run of the cfg(ios) unit tests on the simulator
        // leg, rsac-97c8 — 15 param-validation tests misreported consent.)
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
            #[cfg(feature = "hotplug-recovery")]
            recovery: self
                .auto_recovery
                .map(|config| Arc::new(super::recovery::RecoveryHub::new(config))),
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
    pub fn negotiated_format(&self) -> AudioResult<AudioFormat> {
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
    /// [`Deref`](std::ops::Deref)s/[`DerefMut`](std::ops::DerefMut)s to the
    /// wrapped [`AudioCapture`], so every read /
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
