// src/core/capabilities.rs

//! Platform capability reporting.
//!
//! [`PlatformCapabilities`] provides honest reporting of what each platform's
//! audio backend supports — never pretend a platform can do something it cannot.
//!
//! On macOS, capabilities are determined at runtime based on the OS version
//! (Process Tap requires macOS 14.4+).

use super::config::SampleFormat;

/// Reports what the current platform's audio backend supports.
///
/// Used for honest capability reporting — never pretend a platform
/// can do something it cannot. Query capabilities at runtime via
/// [`PlatformCapabilities::query()`] and check before attempting
/// operations that may not be available on all platforms.
///
/// # Capability vs. readiness
///
/// A `PlatformCapabilities` value answers a **static** question: *can this
/// build, on this OS, do this kind of thing at all?* It is derived from the
/// target OS + enabled backend feature (and, on macOS, the runtime OS version
/// for Process-Tap gating). It is **not** a promise that a *specific* capture
/// will succeed right now. Three separate axes must all hold for a capture to
/// start:
///
/// 1. **Capability** (this type) — e.g. `supports_application_capture`. If
///    `false`, [`AudioCaptureBuilder::build`](crate::api::AudioCaptureBuilder::build)
///    fails its preflight with
///    [`PlatformNotSupported`](crate::core::error::AudioError::PlatformNotSupported)
///    before touching any device.
/// 2. **Permission** — a *runtime* grant, distinct from capability. On macOS
///    per-application capture needs the Audio-Capture TCC permission even when
///    `supports_application_capture == true`; query it with
///    [`check_audio_capture_permission`](crate::core::introspection::check_audio_capture_permission).
/// 3. **Readiness / resolution** — whether the specific
///    [`CaptureTarget`](crate::core::config::CaptureTarget) resolves *now* (the
///    device is present, the PID/app exists and is producing audio). A capable,
///    permitted platform can still fail here with
///    [`DeviceNotFound`](crate::core::error::AudioError::DeviceNotFound) /
///    [`ApplicationNotFound`](crate::core::error::AudioError::ApplicationNotFound)
///    at build/start time.
///
/// So `caps.supports_application_capture == true` means "this platform can
/// capture *some* application" — not "application `X` is capturable right now".
/// Treat capability as a gate you check first, then handle permission and
/// per-target resolution errors when they occur.
///
/// # Example
///
/// ```
/// use rsac::core::capabilities::PlatformCapabilities;
///
/// let caps = PlatformCapabilities::query();
/// if caps.supports_application_capture {
///     // Capability holds — but a specific CaptureTarget::Application(pid) can
///     // still fail with ApplicationNotFound (readiness) or need a permission
///     // grant, handled when build()/start() is called.
/// }
/// ```
#[derive(Debug, Clone)]
pub struct PlatformCapabilities {
    /// Whether system-wide audio capture is supported.
    pub supports_system_capture: bool,
    /// Whether per-application audio capture is supported.
    pub supports_application_capture: bool,
    /// Whether process-tree audio capture is supported.
    pub supports_process_tree_capture: bool,
    /// Whether device selection is supported.
    pub supports_device_selection: bool,
    /// Whether the backend can deliver device hot-plug / default-change
    /// notifications via [`DeviceEnumerator::watch`](crate::core::interface::DeviceEnumerator::watch).
    ///
    /// `false` means [`watch`](crate::core::interface::DeviceEnumerator::watch)
    /// returns [`AudioError::PlatformNotSupported`](crate::core::error::AudioError::PlatformNotSupported);
    /// honest reporting, never claim a notification source the backend has not
    /// wired up. Each platform arm flips this to `true` only once its OS listener
    /// is implemented.
    pub supports_device_change_notifications: bool,
    /// Whether starting a capture on this platform requires an explicit
    /// **user-consent artifact** to be supplied to the builder before
    /// `build()`.
    ///
    /// This is distinct from a runtime OS permission grant (axis 2 in
    /// "Capability vs. readiness" above): consent here is an artifact the
    /// *configuration* must carry — e.g. Android's `MediaProjection` token
    /// (obtained from a user dialog and passed via
    /// `AudioCaptureBuilder::with_android_projection`) or an iOS
    /// user-initiated broadcast session. When `true` and the artifact is
    /// missing for a target that needs it, the builder preflight fails with
    /// [`UserConsentRequired`](crate::core::error::AudioError::UserConsentRequired)
    /// before any OS resource is touched.
    ///
    /// `false` on all desktop backends (WASAPI / PipeWire / CoreAudio) and on
    /// the `unsupported` stub. `true` on iOS, where the App Group identifier
    /// for the ReplayKit broadcast path is the consent artifact (rsac-b3aa);
    /// Android flips to `true` when its playback-capture tiers land
    /// (rsac-77f1). ADR-0013, `docs/MOBILE_BACKEND_DESIGN.md`.
    pub requires_user_consent: bool,
    /// Supported sample formats.
    pub supported_sample_formats: Vec<SampleFormat>,
    /// Supported sample rate range (min, max) in Hz.
    pub sample_rate_range: (u32, u32),
    /// Maximum number of channels supported.
    pub max_channels: u16,
    /// Name of the audio backend (e.g., "WASAPI", "CoreAudio", "PipeWire").
    pub backend_name: &'static str,
}

impl PlatformCapabilities {
    /// The whitelist of sample rates the [`AudioCaptureBuilder`] accepts at
    /// configuration time, as a single source of truth.
    ///
    /// This is the *config-time contract* — the exact set
    /// [`AudioCaptureBuilder::build`] / `preflight` validate the requested rate
    /// against — and is intentionally narrower than the per-platform
    /// [`sample_rate_range`](Self::sample_rate_range) a device may negotiate to.
    /// Callers can pre-validate a rate against this list (e.g. populating a UI
    /// drop-down) without constructing a builder, and the builder references the
    /// same const so the two cannot drift.
    ///
    /// [`AudioCaptureBuilder`]: crate::api::AudioCaptureBuilder
    /// [`AudioCaptureBuilder::build`]: crate::api::AudioCaptureBuilder::build
    pub const SUPPORTED_SAMPLE_RATES: [u32; 6] = [22050, 32000, 44100, 48000, 88200, 96000];

    /// Returns the builder's config-time sample-rate whitelist as a slice.
    ///
    /// A borrowed view of [`SUPPORTED_SAMPLE_RATES`](Self::SUPPORTED_SAMPLE_RATES)
    /// for callers that prefer a `&[u32]` (e.g. to `contains` / iterate without
    /// naming the array length). The contents are identical to the const.
    pub fn supported_sample_rates() -> &'static [u32] {
        &Self::SUPPORTED_SAMPLE_RATES
    }

    /// Human-readable rendering of [`SUPPORTED_SAMPLE_RATES`](Self::SUPPORTED_SAMPLE_RATES)
    /// (`"22050, 32000, 44100, 48000, 88200, 96000"`), single-sourced from the
    /// const so validation error messages (the capture builder's and the
    /// compose builder's) can never drift from the actual whitelist.
    pub(crate) fn supported_sample_rates_display() -> String {
        Self::SUPPORTED_SAMPLE_RATES
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Query the capabilities of the current platform's audio backend.
    ///
    /// Determined at compile time from BOTH the target OS *and* the matching
    /// platform feature flag (`feat_windows`/`feat_linux`/`feat_macos`). The
    /// backend modules are gated on `all(target_os = X, feature = feat_X)`
    /// (see `src/audio/mod.rs`), so when the OS matches but its feature is not
    /// enabled there is no backend to back the report — we must return
    /// the all-false `unsupported` capabilities rather than claim support a capture call would
    /// then refuse with `PlatformNotSupported`. Gating only on `target_os`
    /// (the previous behavior) made `--no-default-features --features
    /// feat_windows` on Linux falsely report full support.
    pub fn query() -> Self {
        #[cfg(all(target_os = "windows", feature = "feat_windows"))]
        {
            return Self::windows();
        }

        #[cfg(all(target_os = "macos", feature = "feat_macos"))]
        {
            return Self::macos();
        }

        #[cfg(all(target_os = "linux", feature = "feat_linux"))]
        {
            return Self::linux();
        }

        #[cfg(all(target_os = "android", feature = "feat_android"))]
        {
            return Self::android();
        }

        #[cfg(all(target_os = "ios", feature = "feat_ios"))]
        {
            return Self::ios();
        }

        // OS without its backend feature enabled, or an unsupported OS: no
        // backend is compiled in, so report nothing as supported.
        #[allow(unreachable_code)]
        Self::unsupported()
    }

    /// Check if a specific sample format is supported.
    pub fn supports_format(&self, format: SampleFormat) -> bool {
        self.supported_sample_formats.contains(&format)
    }

    /// Check if a specific sample rate is supported.
    ///
    /// A rate of 0 is never valid, and the `unsupported` backend reports a
    /// degenerate `(0, 0)` range meaning "nothing supported" — guard against
    /// both so the honest-capability contract holds (a rate of 0 must not slip
    /// through as "in range" for the empty stub).
    pub fn supports_sample_rate(&self, rate: u32) -> bool {
        rate > 0 && rate >= self.sample_rate_range.0 && rate <= self.sample_rate_range.1
    }

    /// Check if a specific channel count is supported.
    pub fn supports_channels(&self, channels: u16) -> bool {
        channels > 0 && channels <= self.max_channels
    }

    // ── Platform constructors (private) ──────────────────────────────────

    #[cfg(all(target_os = "windows", feature = "feat_windows"))]
    fn windows() -> Self {
        Self {
            supports_system_capture: true,
            supports_application_capture: true, // WASAPI session capture
            supports_process_tree_capture: true, // WASAPI include_tree=true
            supports_device_selection: true,
            // IMMNotificationClient watch() arm is implemented (rsac-e360).
            supports_device_change_notifications: true,
            // Desktop loopback needs no consent artifact (rsac-82d4).
            requires_user_consent: false,
            supported_sample_formats: vec![
                SampleFormat::I16,
                SampleFormat::I24,
                SampleFormat::I32,
                SampleFormat::F32,
            ],
            sample_rate_range: (8000, 384000),
            max_channels: 8,
            backend_name: "WASAPI",
        }
    }

    #[cfg(all(target_os = "macos", feature = "feat_macos"))]
    fn macos() -> Self {
        // Process Tap API requires macOS 14.4+. Detect at runtime.
        let (major, minor, _patch) = get_macos_version();
        let has_process_tap = major > 14 || (major == 14 && minor >= 4);

        Self {
            supports_system_capture: true,
            supports_application_capture: has_process_tap, // CoreAudio Process Tap (14.4+)
            supports_process_tree_capture: has_process_tap, // Multi-PID tap via sysinfo child discovery (14.4+)
            supports_device_selection: true,
            // CoreAudio AudioObjectPropertyListener watch() arm landed (rsac-3093):
            // device-list + default-output/input change notifications are wired up.
            supports_device_change_notifications: true,
            // TCC is a runtime permission, not a config-time consent artifact
            // (see the field docs) — so this stays false on macOS (rsac-82d4).
            requires_user_consent: false,
            supported_sample_formats: vec![SampleFormat::I16, SampleFormat::I32, SampleFormat::F32],
            sample_rate_range: (8000, 192000),
            max_channels: 8,
            backend_name: "CoreAudio",
        }
    }

    #[cfg(all(target_os = "linux", feature = "feat_linux"))]
    fn linux() -> Self {
        Self {
            supports_system_capture: true,
            supports_application_capture: true, // PipeWire node targeting
            supports_process_tree_capture: true, // /proc-based child PID discovery + pw-dump node lookup
            supports_device_selection: true,
            // PipeWire registry-listener watch() arm landed (rsac-b92e):
            // LinuxDeviceEnumerator::watch spawns a persistent registry +
            // `default` metadata listener thread that delivers DeviceAdded /
            // DeviceRemoved / DefaultChanged.
            supports_device_change_notifications: true,
            // Desktop loopback needs no consent artifact (rsac-82d4).
            requires_user_consent: false,
            supported_sample_formats: vec![SampleFormat::I16, SampleFormat::I32, SampleFormat::F32],
            sample_rate_range: (8000, 384000),
            max_channels: 32, // PipeWire supports many channels
            backend_name: "PipeWire",
        }
    }

    /// Android capabilities — AAudio microphone slice (rsac-20cd) +
    /// `AudioPlaybackCapture` playback tiers (rsac-77f1).
    ///
    /// Honest current state: the compiled backend captures the default audio
    /// input via AAudio (`CaptureTarget::Device`) on every supported API
    /// level, and the playback-capture tiers (`SystemDefault` /
    /// `Application*` / `ProcessTree` via `AudioPlaybackCapture` +
    /// MediaProjection consent, ADR-0013) on **API 29+** — reported via a
    /// runtime SDK check ([`get_android_sdk_version`]), the same
    /// version-probing pattern as macOS's Process Tap gate. On API < 29 the
    /// playback flags are honestly `false` (`AudioPlaybackCaptureConfiguration`
    /// does not exist there).
    ///
    /// `requires_user_consent: true` when the playback tiers are available:
    /// the [`AndroidProjectionToken`](crate::core::config::AndroidProjectionToken)
    /// is the config-time consent artifact (`with_android_projection`);
    /// `Device` (mic) targets never need it — the mic's RECORD_AUDIO
    /// *runtime permission* is axis-2 readiness, not a consent artifact
    /// (see the field docs).
    #[cfg(all(target_os = "android", feature = "feat_android"))]
    fn android() -> Self {
        // API 29 (Android 10) introduced AudioPlaybackCaptureConfiguration.
        let playback_capture = get_android_sdk_version() >= 29;
        Self {
            supports_system_capture: playback_capture, // AudioPlaybackCapture (rsac-77f1)
            supports_application_capture: playback_capture, // addMatchingUid
            supports_process_tree_capture: playback_capture, // PID→UID (tree ≡ app)
            // Only the default AAudio input + the logical playback endpoint
            // are reachable without the Java AudioManager device list
            // (arrives with rsac-ad8a).
            supports_device_selection: false,
            supports_device_change_notifications: false,
            // The MediaProjection token is the config-time consent artifact
            // for the playback tiers (ADR-0013); meaningless below API 29
            // where those tiers don't exist.
            requires_user_consent: playback_capture,
            // AAudio delivers PCM_I16 or PCM_FLOAT natively.
            supported_sample_formats: vec![SampleFormat::I16, SampleFormat::F32],
            sample_rate_range: (8000, 96000),
            max_channels: 2,
            backend_name: "AAudio",
        }
    }

    /// iOS capabilities — microphone (rsac-9e02) + ReplayKit broadcast system
    /// capture (rsac-b3aa).
    ///
    /// Honest current state: `CaptureTarget::Device` captures the session's
    /// audio input via `AVAudioEngine`; `CaptureTarget::SystemDefault` is
    /// served by the ReplayKit Broadcast Upload Extension transport — hence
    /// `supports_system_capture: true` **with** `requires_user_consent: true`:
    /// capture is user-initiated (broadcast picker) and the configuration
    /// must carry the App Group identifier
    /// (`AudioCaptureBuilder::with_ios_app_group`, the config-time consent
    /// artifact per ADR-0013; the app must also embed the RsacBroadcastKit
    /// extension). `Application*` / `ProcessTree` are **permanently** `false`
    /// on iOS — Apple provides no API for capturing another app's audio
    /// (ADR-0013; never soften this). The mic path itself needs only the
    /// `NSMicrophoneUsageDescription` *runtime* permission (axis-2), not the
    /// consent artifact.
    #[cfg(all(target_os = "ios", feature = "feat_ios"))]
    fn ios() -> Self {
        Self {
            // ReplayKit broadcast transport (rsac-b3aa): user-initiated,
            // App Group + embedded RsacBroadcastKit extension required.
            supports_system_capture: true,
            supports_application_capture: false, // permanent: no iOS API
            supports_process_tree_capture: false, // permanent: no iOS API
            // Input routing on iOS is session-driven (AVAudioSession routes),
            // not free device selection.
            supports_device_selection: false,
            supports_device_change_notifications: false,
            // The App Group id is the config-time consent artifact for
            // SystemDefault (ADR-0013); Device (mic) targets never need it.
            requires_user_consent: true,
            supported_sample_formats: vec![SampleFormat::F32],
            sample_rate_range: (8000, 96000),
            max_channels: 2,
            backend_name: "AVAudioEngine",
        }
    }

    /// Capabilities for a build with no compiled-in backend: an unsupported OS,
    /// or a supported OS whose platform feature flag is disabled. Always
    /// available so [`query()`](Self::query) has a fallback in every config.
    fn unsupported() -> Self {
        Self {
            supports_system_capture: false,
            supports_application_capture: false,
            supports_process_tree_capture: false,
            supports_device_selection: false,
            supports_device_change_notifications: false,
            requires_user_consent: false,
            supported_sample_formats: vec![],
            sample_rate_range: (0, 0),
            max_channels: 0,
            backend_name: "unsupported",
        }
    }
}

// ── macOS version detection ──────────────────────────────────────────────

/// Returns the Android API level (SDK version) of the running device, or
/// `0` when it cannot be determined.
///
/// Reads the `ro.build.version.sdk` system property via
/// `__system_property_get` (a stable libc export on every Android version)
/// — no JNI, no subprocess; the same in-process version-probing pattern as
/// [`get_macos_version`]'s sysctl. A read/parse failure returns `0`, so
/// version-gated capabilities honestly report `false` rather than claiming
/// an API that may not exist.
///
/// Used by [`PlatformCapabilities`] to gate the playback-capture tiers
/// (`AudioPlaybackCaptureConfiguration` requires API 29+).
#[cfg(target_os = "android")]
pub fn get_android_sdk_version() -> u32 {
    use std::os::raw::c_char;

    // PROP_VALUE_MAX from <sys/system_properties.h>.
    const PROP_VALUE_MAX: usize = 92;

    extern "C" {
        /// Stable Android libc API: copies the property value (NUL-terminated,
        /// at most PROP_VALUE_MAX bytes including the NUL) into `value` and
        /// returns its length, or 0 when the property does not exist.
        fn __system_property_get(name: *const c_char, value: *mut c_char) -> i32;
    }

    let mut buf = [0u8; PROP_VALUE_MAX];
    // SAFETY: the name is a NUL-terminated literal and `buf` is at least
    // PROP_VALUE_MAX bytes, which is the documented maximum the call writes.
    let len = unsafe {
        __system_property_get(
            c"ro.build.version.sdk".as_ptr(),
            buf.as_mut_ptr().cast::<c_char>(),
        )
    };
    if len <= 0 {
        log::warn!(
            "Could not read ro.build.version.sdk; reporting API level 0 \
             (version-gated capabilities will be false)"
        );
        return 0;
    }
    let text = std::str::from_utf8(&buf[..len as usize]).unwrap_or("");
    text.trim().parse().unwrap_or_else(|_| {
        log::warn!(
            "Unparseable ro.build.version.sdk value {:?}; reporting API \
             level 0 (version-gated capabilities will be false)",
            text
        );
        0
    })
}

/// Returns the macOS version as `(major, minor, patch)`.
///
/// Uses `sysctl kern.osproductversion` for reliable detection without
/// spawning a subprocess. Falls back to parsing `sw_vers -productVersion`
/// output, then to `(0, 0, 0)` if both fail.
///
/// # Examples
///
/// - macOS 14.4.1 → `(14, 4, 1)`
/// - macOS 15.0   → `(15, 0, 0)`
///
/// Used by [`PlatformCapabilities`] to determine Process Tap
/// availability (requires macOS 14.4+).
#[cfg(target_os = "macos")]
pub fn get_macos_version() -> (u32, u32, u32) {
    // Try sysctl first (no subprocess needed)
    if let Some(version) = get_macos_version_sysctl() {
        return version;
    }

    // Fallback: parse sw_vers output
    if let Some(version) = get_macos_version_sw_vers() {
        return version;
    }

    // Last resort: unknown version (capabilities will report false for version-gated features)
    log::warn!("Could not determine macOS version; defaulting to (0, 0, 0)");
    (0, 0, 0)
}

/// Try to get macOS version via sysctl `kern.osproductversion`.
#[cfg(target_os = "macos")]
fn get_macos_version_sysctl() -> Option<(u32, u32, u32)> {
    use std::ffi::CStr;

    // Safety: sysctl with a well-known name and null-terminated output buffer is safe.
    unsafe {
        let name = b"kern.osproductversion\0";
        let mut buf = [0u8; 32];
        let mut buf_len = buf.len();
        let ret = libc::sysctlbyname(
            name.as_ptr() as *const libc::c_char,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut buf_len,
            std::ptr::null_mut(),
            0,
        );
        if ret != 0 {
            return None;
        }

        let version_str = CStr::from_ptr(buf.as_ptr() as *const libc::c_char)
            .to_str()
            .ok()?;
        parse_version_string(version_str)
    }
}

/// Try to get macOS version via `sw_vers -productVersion`.
#[cfg(target_os = "macos")]
fn get_macos_version_sw_vers() -> Option<(u32, u32, u32)> {
    let output = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let version_str = String::from_utf8(output.stdout).ok()?;
    parse_version_string(version_str.trim())
}

/// Parse a version string like "14.4.1" or "15.0" into `(major, minor, patch)`.
#[cfg(target_os = "macos")]
fn parse_version_string(s: &str) -> Option<(u32, u32, u32)> {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.is_empty() {
        return None;
    }
    let major = parts[0].parse::<u32>().ok()?;
    let minor = parts
        .get(1)
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(0);
    let patch = parts
        .get(2)
        .and_then(|p| p.parse::<u32>().ok())
        .unwrap_or(0);
    Some((major, minor, patch))
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// True when a real backend is compiled in — i.e. the current target_os
    /// matches an enabled `feat_*` feature. When false (e.g. building with
    /// `feat_linux` on Windows), `query()` returns the all-false `unsupported`
    /// stub, so the cross-platform "a real backend supports X" assertions below
    /// must be skipped. Mirrors the gating in `PlatformCapabilities::query()`
    /// (audit H4).
    const HAS_BACKEND: bool = cfg!(any(
        all(target_os = "windows", feature = "feat_windows"),
        all(target_os = "macos", feature = "feat_macos"),
        all(target_os = "linux", feature = "feat_linux"),
    ));

    #[test]
    fn query_returns_valid_capabilities() {
        // `caps` is used in cfg-gated blocks below. On targets where no
        // block matches (or when compiling without the corresponding
        // platform feature), the binding appears unused — silence the
        // lint with a blanket allow. (clippy --fix previously renamed
        // this to _caps, which broke the Linux cfg block at runtime —
        // see CI run 24905086492.)
        #[allow(unused_variables)]
        let caps = PlatformCapabilities::query();

        #[cfg(all(target_os = "linux", feature = "feat_linux"))]
        {
            assert_eq!(caps.backend_name, "PipeWire");
            assert!(caps.supports_system_capture);
            assert!(caps.supports_application_capture);
            assert!(caps.supports_process_tree_capture);
            assert!(caps.supports_device_selection);
            // watch() arm landed for Linux (rsac-b92e).
            assert!(caps.supports_device_change_notifications);
            assert_eq!(caps.max_channels, 32);
            assert_eq!(caps.sample_rate_range, (8000, 384000));
            assert!(!caps.supported_sample_formats.is_empty());
        }

        #[cfg(all(target_os = "windows", feature = "feat_windows"))]
        {
            assert_eq!(caps.backend_name, "WASAPI");
            assert!(caps.supports_system_capture);
        }

        #[cfg(all(target_os = "macos", feature = "feat_macos"))]
        {
            assert_eq!(caps.backend_name, "CoreAudio");
            assert!(caps.supports_system_capture);
        }
    }

    // Regression (audit H4): query() must be gated on BOTH target_os AND the
    // matching feature. When the current OS's backend feature is NOT enabled,
    // query() must report the all-false `unsupported` backend rather than
    // claiming support that every capture call would then refuse.
    #[test]
    fn query_reports_unsupported_without_matching_feature() {
        // This test only makes a claim in the configuration where no
        // (target_os, feature) pair is active. In the normal default-feature
        // build the matching feature IS enabled, so we assert the positive case
        // there and the negative case otherwise.
        let caps = PlatformCapabilities::query();

        #[cfg(any(
            all(target_os = "windows", feature = "feat_windows"),
            all(target_os = "macos", feature = "feat_macos"),
            all(target_os = "linux", feature = "feat_linux"),
        ))]
        {
            // A backend is compiled in → must NOT be the unsupported stub.
            assert_ne!(
                caps.backend_name, "unsupported",
                "with the matching feature enabled, query() must report a real backend"
            );
        }

        #[cfg(not(any(
            all(target_os = "windows", feature = "feat_windows"),
            all(target_os = "macos", feature = "feat_macos"),
            all(target_os = "linux", feature = "feat_linux"),
        )))]
        {
            // No backend compiled in (e.g. feat_linux on Windows) → honest stub.
            assert_eq!(caps.backend_name, "unsupported");
            assert!(!caps.supports_system_capture);
            assert!(!caps.supports_application_capture);
            assert!(!caps.supports_process_tree_capture);
        }
    }

    #[test]
    fn supports_format_f32() {
        let caps = PlatformCapabilities::query();
        // The unsupported stub advertises no formats; only assert F32 support
        // when a real backend is compiled in.
        #[cfg(any(
            all(target_os = "windows", feature = "feat_windows"),
            all(target_os = "macos", feature = "feat_macos"),
            all(target_os = "linux", feature = "feat_linux"),
        ))]
        assert!(caps.supports_format(SampleFormat::F32));
        #[cfg(not(any(
            all(target_os = "windows", feature = "feat_windows"),
            all(target_os = "macos", feature = "feat_macos"),
            all(target_os = "linux", feature = "feat_linux"),
        )))]
        assert!(!caps.supports_format(SampleFormat::F32));
    }

    #[test]
    fn supports_format_missing() {
        let caps = PlatformCapabilities {
            supports_system_capture: false,
            supports_application_capture: false,
            supports_process_tree_capture: false,
            supports_device_selection: false,
            supports_device_change_notifications: false,
            requires_user_consent: false,
            supported_sample_formats: vec![SampleFormat::I16],
            sample_rate_range: (8000, 48000),
            max_channels: 2,
            backend_name: "test",
        };
        assert!(!caps.supports_format(SampleFormat::F32));
    }

    #[test]
    fn supports_sample_rate_48000() {
        if !HAS_BACKEND {
            return; // unsupported stub has rate range (0,0)
        }
        let caps = PlatformCapabilities::query();
        assert!(caps.supports_sample_rate(48000));
    }

    #[test]
    fn supports_sample_rate_zero_is_false() {
        let caps = PlatformCapabilities::query();
        // Rate 0 is never valid — true on every backend, including the
        // `unsupported` stub whose (0,0) range is now explicitly rejected by
        // the `rate > 0` guard in supports_sample_rate (review R2-#3).
        assert!(!caps.supports_sample_rate(0));
    }

    #[test]
    fn supports_channels_stereo() {
        if !HAS_BACKEND {
            return; // stub max_channels is 0
        }
        let caps = PlatformCapabilities::query();
        assert!(caps.supports_channels(2));
    }

    #[test]
    fn supports_channels_zero_is_false() {
        let caps = PlatformCapabilities::query();
        assert!(!caps.supports_channels(0));
    }

    // ── Consent requirement (rsac-82d4) ─────────────────────────────

    /// Every desktop backend — and the unsupported stub — must report that no
    /// config-time consent artifact is required.
    ///
    /// Gated off the mobile OSes: iOS honestly reports
    /// `requires_user_consent: true` (rsac-b3aa — the App Group id is the
    /// consent artifact for the ReplayKit broadcast path), and Android does
    /// on API 29+ (rsac-77f1 — the MediaProjection token). Mobile
    /// consent-reporting is covered by the per-OS capability tests below;
    /// asserting `false` here on those targets would make the test lie.
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    #[test]
    fn desktop_and_stub_require_no_user_consent() {
        let caps = PlatformCapabilities::query();
        assert!(
            !caps.requires_user_consent,
            "no desktop backend (or the unsupported stub) requires a consent artifact"
        );
    }

    /// rsac-20cd / rsac-77f1: Android must report its honest state — the
    /// playback-capture tiers are SDK-gated (`AudioPlaybackCapture` needs
    /// API 29+) and, exactly when available, carry the consent requirement
    /// (the MediaProjection token, ADR-0013). The three tiers and the
    /// consent flag always move together: on Android tree ≡ app ≡ system
    /// transport-wise (one Kotlin pipeline, UID filter optional).
    #[cfg(all(target_os = "android", feature = "feat_android"))]
    #[test]
    fn android_reports_sdk_gated_playback_capture_with_consent() {
        let caps = PlatformCapabilities::query();
        assert_eq!(caps.backend_name, "AAudio");
        let expect_playback = get_android_sdk_version() >= 29;
        assert_eq!(caps.supports_system_capture, expect_playback);
        assert_eq!(caps.supports_application_capture, expect_playback);
        assert_eq!(caps.supports_process_tree_capture, expect_playback);
        assert_eq!(
            caps.requires_user_consent, expect_playback,
            "the MediaProjection token is required exactly when the playback \
             tiers exist (ADR-0013)"
        );
        assert!(!caps.supports_device_selection, "rsac-ad8a pending");
        assert!(caps.supports_format(SampleFormat::F32));
        assert!(caps.supports_sample_rate(48000));
    }

    /// rsac-9e02 / rsac-b3aa: iOS must report its honest state — system
    /// capture available (ReplayKit broadcast) **with** the consent
    /// requirement, `Application*`/`ProcessTree` PERMANENTLY false (ADR-0013).
    #[cfg(all(target_os = "ios", feature = "feat_ios"))]
    #[test]
    fn ios_reports_broadcast_system_capture_with_consent() {
        let caps = PlatformCapabilities::query();
        assert_eq!(caps.backend_name, "AVAudioEngine");
        assert!(
            caps.supports_system_capture,
            "SystemDefault is served by the ReplayKit broadcast path (rsac-b3aa)"
        );
        assert!(
            caps.requires_user_consent,
            "the App Group id is the config-time consent artifact (ADR-0013)"
        );
        assert!(!caps.supports_application_capture, "permanent: no iOS API");
        assert!(!caps.supports_process_tree_capture, "permanent: no iOS API");
        assert!(caps.supports_format(SampleFormat::F32));
        assert!(caps.supports_sample_rate(48000));
    }

    // ── Additional tests ────────────────────────────────────────────

    // ── Backend name (platform-specific) ────────────────────────────

    #[test]
    #[cfg(all(target_os = "linux", feature = "feat_linux"))]
    fn backend_name_is_pipewire_on_linux() {
        let caps = PlatformCapabilities::query();
        assert_eq!(caps.backend_name, "PipeWire");
    }

    #[test]
    #[cfg(all(target_os = "windows", feature = "feat_windows"))]
    fn backend_name_is_wasapi_on_windows() {
        let caps = PlatformCapabilities::query();
        assert_eq!(caps.backend_name, "WASAPI");
    }

    #[test]
    #[cfg(all(target_os = "macos", feature = "feat_macos"))]
    fn backend_name_is_coreaudio_on_macos() {
        let caps = PlatformCapabilities::query();
        assert_eq!(caps.backend_name, "CoreAudio");
    }

    // ── Sample format support (cross-platform) ──────────────────────

    #[test]
    fn supports_i16_format() {
        if !HAS_BACKEND {
            return;
        }
        let caps = PlatformCapabilities::query();
        assert!(caps.supports_format(SampleFormat::I16));
    }

    #[test]
    fn supports_i32_format() {
        if !HAS_BACKEND {
            return;
        }
        let caps = PlatformCapabilities::query();
        assert!(caps.supports_format(SampleFormat::I32));
    }

    // ── I24 support (platform-specific) ─────────────────────────────

    #[test]
    #[cfg(all(target_os = "linux", feature = "feat_linux"))]
    fn does_not_support_i24_on_linux() {
        let caps = PlatformCapabilities::query();
        assert!(!caps.supports_format(SampleFormat::I24));
    }

    #[test]
    #[cfg(all(target_os = "windows", feature = "feat_windows"))]
    fn supports_i24_on_windows() {
        let caps = PlatformCapabilities::query();
        assert!(caps.supports_format(SampleFormat::I24));
    }

    #[test]
    #[cfg(all(target_os = "macos", feature = "feat_macos"))]
    fn does_not_support_i24_on_macos() {
        let caps = PlatformCapabilities::query();
        assert!(!caps.supports_format(SampleFormat::I24));
    }

    // ── Sample rate boundaries (platform-specific) ──────────────────

    #[test]
    fn supports_sample_rate_min_boundary() {
        if !HAS_BACKEND {
            return;
        }
        let caps = PlatformCapabilities::query();
        assert!(
            caps.supports_sample_rate(8000),
            "min boundary 8000 should be supported on all platforms"
        );
    }

    #[test]
    #[cfg(any(
        all(target_os = "linux", feature = "feat_linux"),
        all(target_os = "windows", feature = "feat_windows")
    ))]
    fn supports_sample_rate_max_boundary_384000() {
        let caps = PlatformCapabilities::query();
        assert!(
            caps.supports_sample_rate(384000),
            "max boundary 384000 should be supported on Linux/Windows"
        );
    }

    #[test]
    #[cfg(all(target_os = "macos", feature = "feat_macos"))]
    fn supports_sample_rate_max_boundary_192000() {
        let caps = PlatformCapabilities::query();
        assert!(
            caps.supports_sample_rate(192000),
            "max boundary 192000 should be supported on macOS"
        );
        assert!(
            !caps.supports_sample_rate(192001),
            "above max boundary 192000 should not be supported on macOS"
        );
    }

    #[test]
    fn does_not_support_sample_rate_above_max() {
        let caps = PlatformCapabilities::query();
        // 384001 is above the maximum for all platforms (Linux/Windows: 384000, macOS: 192000)
        assert!(!caps.supports_sample_rate(384001));
    }

    // ── Channel count boundaries (platform-specific) ────────────────

    #[test]
    #[cfg(all(target_os = "linux", feature = "feat_linux"))]
    fn supports_channels_max_boundary_linux() {
        let caps = PlatformCapabilities::query();
        assert!(caps.supports_channels(32)); // Linux max is 32
        assert!(!caps.supports_channels(33));
    }

    #[test]
    #[cfg(all(target_os = "windows", feature = "feat_windows"))]
    fn supports_channels_max_boundary_windows() {
        let caps = PlatformCapabilities::query();
        assert!(caps.supports_channels(8)); // Windows max is 8
        assert!(!caps.supports_channels(9));
    }

    #[test]
    #[cfg(all(target_os = "macos", feature = "feat_macos"))]
    fn supports_channels_max_boundary_macos() {
        let caps = PlatformCapabilities::query();
        assert!(caps.supports_channels(8)); // macOS max is 8
        assert!(!caps.supports_channels(9));
    }

    #[test]
    fn does_not_support_channels_above_max() {
        let caps = PlatformCapabilities::query();
        // 33 is above the maximum for all platforms (Linux: 32, Windows/macOS: 8)
        assert!(!caps.supports_channels(33));
    }

    // ── Capture capability tests ────────────────────────────────────

    #[test]
    fn query_system_capture_supported() {
        if !HAS_BACKEND {
            return;
        }
        let caps = PlatformCapabilities::query();
        assert!(caps.supports_system_capture);
    }

    #[test]
    #[cfg(any(
        all(target_os = "windows", feature = "feat_windows"),
        all(target_os = "linux", feature = "feat_linux")
    ))]
    fn query_application_capture_supported() {
        let caps = PlatformCapabilities::query();
        assert!(caps.supports_application_capture);
    }

    #[test]
    #[cfg(all(target_os = "macos", feature = "feat_macos"))]
    fn query_application_capture_reflects_version() {
        let caps = PlatformCapabilities::query();
        let (major, minor, _) = get_macos_version();
        let expected = major > 14 || (major == 14 && minor >= 4);
        assert_eq!(
            caps.supports_application_capture, expected,
            "supports_application_capture should match macOS version ({}.{})",
            major, minor
        );
    }

    #[test]
    #[cfg(all(target_os = "linux", feature = "feat_linux"))]
    fn query_process_tree_supported_on_linux() {
        let caps = PlatformCapabilities::query();
        assert!(caps.supports_process_tree_capture);
    }

    #[test]
    #[cfg(all(target_os = "windows", feature = "feat_windows"))]
    fn query_process_tree_supported_on_windows() {
        let caps = PlatformCapabilities::query();
        assert!(caps.supports_process_tree_capture);
    }

    #[test]
    #[cfg(all(target_os = "macos", feature = "feat_macos"))]
    fn macos_reports_device_change_notifications_supported() {
        // rsac-3093: the CoreAudio AudioObjectPropertyListener watch() arm is
        // implemented, so macOS must now honestly advertise device-change
        // notification support (no longer the false stub).
        let caps = PlatformCapabilities::query();
        assert!(
            caps.supports_device_change_notifications,
            "macOS should report device-change notification support once watch() is wired up"
        );
    }

    #[test]
    #[cfg(all(target_os = "macos", feature = "feat_macos"))]
    fn query_process_tree_reflects_version_on_macos() {
        let caps = PlatformCapabilities::query();
        let (major, minor, _) = get_macos_version();
        let expected = major > 14 || (major == 14 && minor >= 4);
        assert_eq!(
            caps.supports_process_tree_capture, expected,
            "supports_process_tree_capture should match macOS version ({}.{})",
            major, minor
        );
    }

    #[test]
    fn clone_capabilities() {
        let caps = PlatformCapabilities::query();
        let cloned = caps.clone();
        assert_eq!(caps.backend_name, cloned.backend_name);
        assert_eq!(caps.supports_system_capture, cloned.supports_system_capture);
        assert_eq!(
            caps.supports_application_capture,
            cloned.supports_application_capture
        );
        assert_eq!(
            caps.supports_process_tree_capture,
            cloned.supports_process_tree_capture
        );
        assert_eq!(
            caps.supports_device_selection,
            cloned.supports_device_selection
        );
        assert_eq!(
            caps.supported_sample_formats,
            cloned.supported_sample_formats
        );
        assert_eq!(caps.sample_rate_range, cloned.sample_rate_range);
        assert_eq!(caps.max_channels, cloned.max_channels);
    }

    // ── macOS version detection tests ────────────────────────────────

    #[test]
    #[cfg(all(target_os = "macos", feature = "feat_macos"))]
    fn get_macos_version_returns_nonzero() {
        let (major, _minor, _patch) = get_macos_version();
        // We should always be able to detect the version on a real macOS system
        assert!(
            major >= 10,
            "macOS major version should be >= 10, got {}",
            major
        );
    }

    #[test]
    #[cfg(all(target_os = "macos", feature = "feat_macos"))]
    fn parse_version_string_typical() {
        assert_eq!(parse_version_string("14.4.1"), Some((14, 4, 1)));
        assert_eq!(parse_version_string("15.0"), Some((15, 0, 0)));
        assert_eq!(parse_version_string("12.6.3"), Some((12, 6, 3)));
    }

    #[test]
    #[cfg(all(target_os = "macos", feature = "feat_macos"))]
    fn parse_version_string_edge_cases() {
        assert_eq!(parse_version_string("14"), Some((14, 0, 0)));
        assert_eq!(parse_version_string(""), None);
        assert_eq!(parse_version_string("abc"), None);
    }

    // ── SUPPORTED_SAMPLE_RATES const / supported_sample_rates() (rsac-c957) ──

    /// The promoted public const is the canonical config-time whitelist and must
    /// equal the documented six rates exactly.
    #[test]
    fn supported_sample_rates_const_is_canonical() {
        assert_eq!(
            PlatformCapabilities::SUPPORTED_SAMPLE_RATES,
            [22050, 32000, 44100, 48000, 88200, 96000]
        );
    }

    /// `supported_sample_rates()` returns a slice over the same const: 48000 is
    /// present, 11025 (a valid audio rate that is *not* whitelisted) is absent.
    #[test]
    fn supported_sample_rates_slice_membership() {
        let rates = PlatformCapabilities::supported_sample_rates();
        assert!(rates.contains(&48000), "48000 must be in the whitelist");
        assert!(
            !rates.contains(&11025),
            "11025 is not a whitelisted config-time rate"
        );
        // The slice is a borrowed view of the const — identical contents/length.
        assert_eq!(rates, &PlatformCapabilities::SUPPORTED_SAMPLE_RATES);
    }
}
