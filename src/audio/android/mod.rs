//! Android audio capture backend — AAudio **microphone slice** (rsac-20cd).
//!
//! # Current scope (honest status)
//!
//! This backend captures the **default audio input** (microphone, or
//! whatever input the OS currently routes — wired headset, BT, USB) via a
//! pure-NDK AAudio input stream. That is the *entire* shipped surface today:
//!
//! | `CaptureTarget` | Android behaviour |
//! |---|---|
//! | `Device(DeviceId("default"))` | ✅ default-input capture via AAudio |
//! | `SystemDefault` | ❌ pending — on Android this means **playback capture** (`AudioPlaybackCapture`, ADR-0013), NOT the microphone; arrives with rsac-77f1 |
//! | `Application` / `ApplicationByName` / `ProcessTree` | ❌ pending — UID-filtered playback capture + MediaProjection consent (rsac-77f1; on Android tree ≡ app, all of an app's processes share one UID) |
//! | `Device(other id)` | ❌ real input-device ids (mic/USB/BT from `AudioManager.getDevices`) need the Java `AudioManager` — arrives with the rsac AAR (rsac-c4b8) |
//!
//! [`PlatformCapabilities::query`](crate::core::capabilities::PlatformCapabilities::query)
//! reports exactly this (`backend_name = "AAudio"`, playback-capture flags
//! `false` until rsac-77f1).
//!
//! # Host-app responsibilities (NOT this library's job)
//!
//! Microphone capture requires the **`RECORD_AUDIO` runtime permission**:
//! the host app must declare it in the manifest and obtain the runtime
//! grant before building a capture (the Kotlin helpers shipped in
//! `mobile/android/` — ADR-0012's AAR — wrap this flow). Without the grant,
//! stream creation fails with an actionable
//! [`AudioError::StreamCreationFailed`](crate::core::error::AudioError::StreamCreationFailed)
//! rather than delivering silence.
//!
//! # Architecture
//!
//! Same shape as every other rsac backend (no bespoke stream type):
//!
//! ```text
//! AAudio data callback (may be RT)          Consumer thread
//! ────────────────────────────────          ───────────────
//! extern "C" data_callback                  BridgeStream<AndroidPlatformStream>
//!   → f32: push the slice directly            ::read_chunk()
//!   → i16: convert into pre-alloc scratch
//!   → BridgeProducer::push_samples_…
//! ```
//!
//! - `aaudio` holds the minimal in-tree `extern "C"` bindings for the
//!   stable AAudio NDK ABI (`libaaudio.so`) — no binding crates.
//! - `thread` provides `AndroidPlatformStream` (the internal
//!   [`PlatformStream`](crate::bridge::stream::PlatformStream) impl), the
//!   panic-free/alloc-free data callback (full ADR-0001 rules — AAudio may
//!   invoke it on a real-time thread), the error callback that drives the
//!   bridge terminal on device disconnect (ADR-0010), and the open/start
//!   factory.

pub(crate) mod aaudio;
pub(crate) mod thread;

use std::sync::Arc;
use std::time::Duration;

use crate::bridge::state::StreamState;
use crate::bridge::{calculate_capacity, create_bridge, BridgeStream};
use crate::core::config::{AudioFormat, DeviceId, SampleFormat, StreamConfig};
use crate::core::error::{AudioError, AudioResult};
use crate::core::interface::{AudioDevice, CapturingStream, DeviceEnumerator, DeviceKind};

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

/// [`DeviceEnumerator`] for Android (AAudio backend, mic slice).
///
/// Enumeration is intentionally minimal: without the Java `AudioManager`
/// (AAR, rsac-c4b8) the NDK cannot list input devices, so exactly **one
/// logical input device** — `"default"` — is reported, representing the
/// default AAudio input route. See
/// [`DeviceEnumerator::default_device`] for why the *default device*
/// (rsac's loopback-oriented notion) is an error on Android today.
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
    /// Lists the single logical Android audio input device.
    ///
    /// Returns exactly one device — `DeviceId("default")`, the default
    /// AAudio input. A real device list (built-in mic / USB / BT ids via
    /// `AudioManager.getDevices`) needs the Java side and arrives with the
    /// rsac AAR (rsac-c4b8); it is deliberately not faked here.
    fn enumerate_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice>>> {
        Ok(vec![Box::new(AndroidAudioDevice::new())])
    }

    /// The rsac *default device* is not available on Android yet — errors
    /// with guidance.
    ///
    /// On desktop backends `default_device()` returns the default **output**
    /// endpoint because rsac's headline capability there is system-audio
    /// loopback. On Android, system audio (`CaptureTarget::SystemDefault`)
    /// means `AudioPlaybackCapture` behind the MediaProjection consent flow
    /// (ADR-0013), which is **not wired yet** (rsac-77f1). Pretending the
    /// microphone is "the default device" would silently deliver different
    /// audio than the desktop contract promises (the dishonest-fallback
    /// option ADR-0013 explicitly rejected) — and `api.rs` routes
    /// `SystemDefault` / `Application*` / `ProcessTree` through this method,
    /// so this is the honest refusal point. It returns
    /// [`AudioError::PlatformNotSupported`] with the honest state:
    ///
    /// - **Supported now:** microphone capture via
    ///   `CaptureTarget::Device(DeviceId("default".into()))`.
    /// - **Pending:** playback capture (`SystemDefault` and the per-app /
    ///   process-tree targets) via `AudioPlaybackCapture` + MediaProjection
    ///   consent (rsac-77f1).
    fn default_device(&self) -> AudioResult<Box<dyn AudioDevice>> {
        Err(AudioError::PlatformNotSupported {
            feature: "default-device (system audio) capture on Android: \
                      SystemDefault maps to AudioPlaybackCapture — all \
                      capturable playback, NOT the microphone (ADR-0013) — \
                      which is not wired yet (rsac-77f1, MediaProjection \
                      consent flow; per-app and process-tree capture arrive \
                      with the same seed). Use \
                      CaptureTarget::Device(DeviceId(\"default\".into())) to \
                      capture the microphone (the default AAudio input)"
                .to_string(),
            platform: "android".to_string(),
        })
    }

    // watch(): inherits the trait default (PlatformNotSupported) —
    // consistent with `supports_device_change_notifications: false` in
    // PlatformCapabilities::android(). Input-route change notifications are
    // AudioManager/AudioDeviceCallback territory, which belongs to the Java
    // side (AAR, rsac-c4b8).
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
    ///   `Application*` / `ProcessTree` (playback-capture tiers, pending
    ///   rsac-77f1 — see the module docs).
    /// - [`AudioError::DeviceNotFound`] for a `Device` id other than
    ///   `"default"` (real ids arrive with the AAR, rsac-c4b8).
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
    use crate::core::error::ErrorKind;

    #[test]
    fn enumerate_devices_returns_single_default_input() {
        let enumerator = AndroidDeviceEnumerator::new();
        let devices = enumerator
            .enumerate_devices()
            .expect("enumeration is infallible metadata");
        assert_eq!(devices.len(), 1, "exactly one logical Android input device");

        let device = &devices[0];
        assert_eq!(device.id(), DeviceId(DEFAULT_INPUT_DEVICE_ID.to_string()));
        assert_eq!(device.name(), "Default audio input (AAudio)");
        assert!(device.is_default());
        assert_eq!(device.kind().unwrap(), DeviceKind::Input);
    }

    #[test]
    fn default_device_is_honest_platform_not_supported() {
        let enumerator = AndroidDeviceEnumerator::new();
        let err = match enumerator.default_device() {
            Ok(_) => panic!("default_device must error until rsac-77f1"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::Platform);
        match err {
            AudioError::PlatformNotSupported { feature, platform } => {
                assert_eq!(platform, "android");
                // The honesty pillars: what SystemDefault really means on
                // Android, which seed delivers it, and what works today.
                assert!(
                    feature.contains("AudioPlaybackCapture"),
                    "real meaning: {feature}"
                );
                assert!(feature.contains("NOT the"), "not-the-mic: {feature}");
                assert!(feature.contains("rsac-77f1"), "pending seed: {feature}");
                assert!(
                    feature.contains("Device(DeviceId(\"default\""),
                    "mic guidance: {feature}"
                );
            }
            other => panic!("expected PlatformNotSupported, got {other:?}"),
        }
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
