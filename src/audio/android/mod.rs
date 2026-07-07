//! Android audio capture backend — AAudio microphone slice (rsac-20cd) +
//! `AudioPlaybackCapture` playback tiers (rsac-77f1).
//!
//! # Current scope (honest status)
//!
//! Two data paths (docs/MOBILE_BACKEND_DESIGN.md § two data paths):
//!
//! | `CaptureTarget` | Android behaviour |
//! |---|---|
//! | `Device(DeviceId("default"))` | ✅ default-input (microphone) capture via AAudio — pure NDK, no Java |
//! | `SystemDefault` | 🟡 **playback capture** (`AudioPlaybackCapture`, ADR-0013 — all capturable playback, NOT the microphone) via the rsac AAR's Kotlin loop ([`playback`]); requires API 29+, `RECORD_AUDIO`, and a MediaProjection consent token ([`with_android_projection`]) — compiled, unverified on-device (rsac-e6d3) |
//! | `Application` / `ApplicationByName` / `ProcessTree` | 🟡 UID-filtered playback capture (tree ≡ app: all of an Android app's processes share one UID) — same requirements/status as `SystemDefault` |
//! | `Device(other id)` | ❌ real input-device ids (mic/USB/BT from `AudioManager.getDevices`) need the Java `AudioManager` list — rsac-ad8a |
//!
//! [`PlatformCapabilities::query`](crate::core::capabilities::PlatformCapabilities::query)
//! reports exactly this (`backend_name = "AAudio"`; the playback-capture
//! flags are `true` on API 29+ with `requires_user_consent = true`).
//!
//! # Host-app responsibilities (NOT this library's job)
//!
//! - **`RECORD_AUDIO` runtime permission** — required for the microphone
//!   *and* for playback capture; the host app must declare it and obtain
//!   the runtime grant (the Kotlin helpers in `mobile/android/` wrap the
//!   flow). Without it, stream creation fails actionably.
//! - **MediaProjection consent** (playback capture only) — obtain a token
//!   via `RsacProjection.request(activity)` and pass it to
//!   [`with_android_projection`]; playback builds without one fail the
//!   preflight with `UserConsentRequired`.
//! - **Foreground service** (playback capture, API 34+) — a
//!   `mediaProjection`-typed FGS must be running before capture starts;
//!   `RsacCaptureService.start(context)` provides one.
//!
//! [`with_android_projection`]: crate::api::AudioCaptureBuilder::with_android_projection
//!
//! # Architecture
//!
//! Same shape as every other rsac backend (no bespoke stream type):
//!
//! ```text
//! Mic (AAudio, may be RT)                Playback (Java loop, non-RT)
//! ───────────────────────                ────────────────────────────
//! extern "C" data_callback               Kotlin AudioRecord.read loop
//!   → BridgeProducer::push_…              → nativePush (JNI, jni.rs)
//!                                          → BridgeProducer::push_…
//!            └────────────┬────────────────────────┘
//!                         ▼
//!            BridgeStream<…>::read_chunk()   (consumer thread)
//! ```
//!
//! - [`aaudio`] holds the minimal in-tree `extern "C"` bindings for the
//!   stable AAudio NDK ABI (`libaaudio.so`) — no binding crates.
//! - [`thread`] provides `AndroidPlatformStream` (mic slice) and the
//!   panic-free/alloc-free AAudio callbacks (full ADR-0001 rules).
//! - [`jni`] is the JNI boundary for the playback path: `JNI_OnLoad`
//!   registration, the ingest-session registry, and the natives the AAR's
//!   `CaptureBridge`/`RsacProjection` call.
//! - [`playback`] orchestrates the AAR's Kotlin capture pipeline
//!   (`AndroidPlaybackDevice` + `AndroidPlaybackStream`) and owns the
//!   ADR-0013 target → UID mapping.

pub(crate) mod aaudio;
pub(crate) mod jni;
pub(crate) mod playback;
pub(crate) mod thread;

use std::sync::Arc;
use std::time::Duration;

use crate::bridge::state::StreamState;
use crate::bridge::{calculate_capacity, create_bridge, BridgeStream};
use crate::core::config::{AudioFormat, DeviceId, SampleFormat, StreamConfig};
use crate::core::error::{AudioError, AudioResult};
use crate::core::interface::{AudioDevice, CapturingStream, DeviceEnumerator, DeviceKind};

pub use playback::AndroidPlaybackDevice;

/// The [`DeviceId`] string of the single logical Android input device.
///
/// Real input-device enumeration (built-in mic vs USB vs BT ids) requires
/// the Java `AudioManager.getDevices` list, which arrives with the rsac AAR
/// (rsac-c4b8). Until then rsac exposes exactly one logical device,
/// `"default"`, meaning "the default AAudio input route". The empty string
/// is also accepted as an alias when resolving a [`CaptureTarget::Device`]
/// (matching the Windows and iOS backends' default-endpoint convention).
///
/// [`CaptureTarget::Device`]: crate::core::config::CaptureTarget::Device
pub(crate) const DEFAULT_INPUT_DEVICE_ID: &str = "default";

// ── AndroidDeviceEnumerator ──────────────────────────────────────────────

/// [`DeviceEnumerator`] for Android (AAudio mic + `AudioPlaybackCapture`
/// playback).
///
/// Enumeration lists exactly **two logical devices** (mirroring the iOS
/// enumerator's mic + broadcast shape):
///
/// - `"default"` ([`AndroidAudioDevice`]): the default AAudio input route
///   (microphone / headset / BT / USB — whatever the OS routes).
/// - `"playback-capture"` ([`AndroidPlaybackDevice`]): the playback-capture
///   endpoint (`AudioPlaybackCapture` behind MediaProjection consent),
///   serving `SystemDefault` and the per-app tiers.
///
/// Real input-device ids (`AudioManager.getDevices`) need the Java side and
/// arrive with rsac-ad8a; they are deliberately not faked here.
#[derive(Debug, Clone, Copy)]
pub struct AndroidDeviceEnumerator;

impl AndroidDeviceEnumerator {
    /// Creates a new Android device enumerator.
    ///
    /// Non-fallible: no OS resources are touched until a stream is created
    /// (matching the factory contract in
    /// [`get_device_enumerator`](crate::audio::get_device_enumerator)).
    pub fn new() -> Self {
        Self
    }
}

impl Default for AndroidDeviceEnumerator {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceEnumerator for AndroidDeviceEnumerator {
    /// Lists the two logical Android devices: the default AAudio input
    /// (microphone) and the playback-capture endpoint.
    fn enumerate_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice>>> {
        Ok(vec![
            Box::new(AndroidAudioDevice::new()),
            Box::new(AndroidPlaybackDevice::new()),
        ])
    }

    /// Returns the playback-capture device — rsac's *default device* on
    /// Android.
    ///
    /// On every desktop backend `default_device()` returns the default
    /// **output** endpoint because rsac's headline capability is
    /// system-audio loopback; the Android equivalent of that endpoint is
    /// `AudioPlaybackCapture` (ADR-0013, rsac-77f1), so
    /// `CaptureTarget::SystemDefault` (and the per-app tiers) resolve here.
    /// The real preconditions — API 29+, `RECORD_AUDIO`, a MediaProjection
    /// consent token, the mediaProjection foreground service on API 34+ —
    /// surface as actionable errors at `create_stream` time (see
    /// [`AndroidPlaybackDevice`]). For the microphone, target
    /// `CaptureTarget::Device(DeviceId("default".into()))` explicitly.
    fn default_device(&self) -> AudioResult<Box<dyn AudioDevice>> {
        Ok(Box::new(AndroidPlaybackDevice::new()))
    }

    // watch(): inherits the trait default (PlatformNotSupported) —
    // consistent with `supports_device_change_notifications: false` in
    // PlatformCapabilities::android(). Input-route change notifications are
    // AudioManager/AudioDeviceCallback territory, which belongs to the Java
    // side (rsac-ad8a).
}

// ── AndroidAudioDevice ───────────────────────────────────────────────────

/// The single logical Android audio input device (the default AAudio
/// input).
///
/// A metadata-only handle: constructing it touches no OS resources. The
/// AAudio stream is created lazily in
/// [`create_stream`](AudioDevice::create_stream).
#[derive(Debug, Clone, Copy)]
pub struct AndroidAudioDevice;

impl AndroidAudioDevice {
    /// Creates the logical default-input device handle.
    pub fn new() -> Self {
        Self
    }
}

impl Default for AndroidAudioDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioDevice for AndroidAudioDevice {
    fn id(&self) -> DeviceId {
        DeviceId(DEFAULT_INPUT_DEVICE_ID.to_string())
    }

    fn name(&self) -> String {
        "Default audio input (AAudio)".to_string()
    }

    fn is_default(&self) -> bool {
        true
    }

    /// Advisory format list: F32 and I16 at common Android rates,
    /// mono/stereo.
    ///
    /// AAudio negotiates the *actual* rate/channels/wire-format at
    /// stream-open time (the backend requests the configured shape and
    /// falls back to the device-native one), so this list is a hint, not a
    /// contract. The **delivered** format is read back from the open stream
    /// and reported authoritatively via [`CapturingStream::format`]; the
    /// bridge payload is always interleaved f32 (an I16 wire format is
    /// converted in the callback), so consumers see `F32` regardless of the
    /// entry that matched.
    fn supported_formats(&self) -> Vec<AudioFormat> {
        const RATES_STEREO: [u32; 2] = [48_000, 44_100];
        const RATES_MONO: [u32; 4] = [48_000, 44_100, 16_000, 8_000];
        let mut formats = Vec::with_capacity((RATES_STEREO.len() + RATES_MONO.len()) * 2);
        // F32 first: the first entry seeds DeviceInfo::default_format, and
        // f32 is rsac's canonical delivery.
        for sample_format in [SampleFormat::F32, SampleFormat::I16] {
            for rate in RATES_STEREO {
                formats.push(AudioFormat {
                    sample_rate: rate,
                    channels: 2,
                    sample_format,
                });
            }
            for rate in RATES_MONO {
                formats.push(AudioFormat {
                    sample_rate: rate,
                    channels: 1,
                    sample_format,
                });
            }
        }
        formats
    }

    fn kind(&self) -> AudioResult<DeviceKind> {
        Ok(DeviceKind::Input)
    }

    /// Creates a live microphone capture stream through the ring-buffer
    /// bridge.
    ///
    /// Wiring (identical shape to the desktop and iOS backends): create the
    /// bridge (ring depth honours `config.buffer_size`, ADR-0007 pattern),
    /// transition it to `Running`, open + start the AAudio input stream
    /// ([`thread::create_android_capture`]), and wrap everything in a
    /// `BridgeStream`. The stream's [`format`](CapturingStream::format)
    /// reports the **delivered** format, which may differ from the requested
    /// one — see [`AudioDevice::supported_formats`].
    ///
    /// # Errors
    ///
    /// - [`AudioError::PlatformNotSupported`] for `SystemDefault` and
    ///   `Application*` / `ProcessTree` (playback-capture tiers, served by
    ///   [`AndroidPlaybackDevice`], not this mic device).
    /// - [`AudioError::DeviceNotFound`] for a `Device` id other than
    ///   `"default"` (real ids arrive with rsac-ad8a).
    /// - [`AudioError::StreamCreationFailed`] /
    ///   [`AudioError::StreamStartFailed`] from the AAudio open/start path
    ///   (typically: the `RECORD_AUDIO` runtime permission is missing — a
    ///   host-app responsibility; see the module docs).
    fn create_stream(&self, config: &StreamConfig) -> AudioResult<Box<dyn CapturingStream>> {
        let requested = config.to_audio_format();

        // Ring sizing: honour the requested slot count like Windows/Linux
        // do (ADR-0007 direction), defaulting to
        // calculate_capacity(None, 4) = 64.
        let capacity = calculate_capacity(config.buffer_size, 4);
        let (producer, consumer) = create_bridge(capacity, requested.clone());

        // Transition bridge state Created → Running before the callback
        // starts pushing, so the first period's buffers are readable.
        consumer
            .shared()
            .state
            .transition(StreamState::Created, StreamState::Running)
            .map_err(|actual| AudioError::InternalError {
                message: format!(
                    "Failed to transition bridge state to Running (was {:?})",
                    actual
                ),
                source: None,
            })?;

        // Producer-terminal-signal handle (ADR-0010): the platform stream's
        // stop/Drop choke point (and the AAudio error callback, for
        // disconnects) drives the bridge to its ending state so a parked
        // reader observes end-of-stream instead of hanging.
        let terminal = Arc::clone(consumer.shared());

        // Resolve the target, open + start the AAudio stream. `delivered`
        // is the REAL negotiated format (also published on the bridge as
        // the negotiated format before the first push).
        let (platform_stream, delivered) =
            thread::create_android_capture(&config.capture_target, &requested, producer, terminal)?;

        let bridge_stream =
            BridgeStream::new(consumer, platform_stream, delivered, Duration::from_secs(1));

        Ok(Box::new(bridge_stream))
    }
}

// ══════════════════════════════════════════════════════════════════════════
// Tests — metadata-only (no FFI): they compile for the Android target under
// `--tests` and will run on a future emulator job. They never touch AAudio.
// ══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerate_devices_lists_mic_and_playback() {
        let enumerator = AndroidDeviceEnumerator::new();
        let devices = enumerator
            .enumerate_devices()
            .expect("enumeration is infallible metadata");
        assert_eq!(devices.len(), 2, "mic + playback-capture endpoint");

        let mic = &devices[0];
        assert_eq!(mic.id(), DeviceId(DEFAULT_INPUT_DEVICE_ID.to_string()));
        assert_eq!(mic.name(), "Default audio input (AAudio)");
        assert!(mic.is_default(), "mic stays the default Input device");
        assert_eq!(mic.kind().unwrap(), DeviceKind::Input);

        let playback = &devices[1];
        assert_eq!(
            playback.id(),
            DeviceId(playback::PLAYBACK_DEVICE_ID.to_string())
        );
        assert_eq!(playback.kind().unwrap(), DeviceKind::Output);
        assert!(playback.is_default(), "default of its (Output) kind");
    }

    #[test]
    fn default_device_is_the_playback_capture_endpoint() {
        // rsac's default device is the system-audio endpoint on every
        // platform; on Android that is the AudioPlaybackCapture device
        // (ADR-0013), NOT the microphone — the dishonest-fallback option
        // ADR-0013 explicitly rejected.
        let enumerator = AndroidDeviceEnumerator::new();
        let device = enumerator
            .default_device()
            .expect("the playback endpoint is metadata-only until create_stream");
        assert_eq!(
            device.id(),
            DeviceId(playback::PLAYBACK_DEVICE_ID.to_string())
        );
        assert_eq!(device.kind().unwrap(), DeviceKind::Output);
    }

    #[test]
    fn supported_formats_are_advisory_f32_and_i16() {
        let device = AndroidAudioDevice::new();
        let formats = device.supported_formats();
        assert!(!formats.is_empty());
        for fmt in &formats {
            assert!(
                fmt.sample_format == SampleFormat::F32 || fmt.sample_format == SampleFormat::I16,
                "AAudio delivers PCM_FLOAT or PCM_I16 only, got {:?}",
                fmt.sample_format
            );
            assert!(fmt.sample_rate > 0);
            assert!(fmt.channels == 1 || fmt.channels == 2);
        }
        // First entry (the DeviceInfo::default_format seed) is 48 kHz
        // stereo F32.
        assert_eq!(formats[0].sample_rate, 48_000);
        assert_eq!(formats[0].channels, 2);
        assert_eq!(formats[0].sample_format, SampleFormat::F32);
        // Both wire formats are represented.
        assert!(formats.iter().any(|f| f.sample_format == SampleFormat::I16));
    }

    #[test]
    fn describe_snapshot_is_consistent() {
        let info = AndroidAudioDevice::new().describe();
        assert_eq!(info.id, DeviceId(DEFAULT_INPUT_DEVICE_ID.to_string()));
        assert_eq!(info.kind, DeviceKind::Input);
        assert!(info.is_default);
        assert!(info.default_format.is_some());
    }
}
