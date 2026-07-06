//! iOS audio capture backend — AVAudioEngine **microphone slice** (rsac-9e02).
//!
//! # Current scope (honest status)
//!
//! This backend captures the **session's current audio input** (microphone,
//! or whatever input route `AVAudioSession` has active — wired headset, BT,
//! USB) via an `AVAudioEngine` input-node tap. That is the *entire* shipped
//! surface today:
//!
//! | `CaptureTarget` | iOS behaviour |
//! |---|---|
//! | `Device(DeviceId("default"))` | ✅ mic capture via AVAudioEngine input tap |
//! | `SystemDefault` | ❌ pending — the ReplayKit Broadcast Upload Extension path (rsac-b3aa) |
//! | `Application` / `ApplicationByName` / `ProcessTree` | ❌ **permanently unsupported** — Apple provides no API for capturing another app's audio (ADR-0013; never soften this) |
//!
//! [`PlatformCapabilities::query`](crate::core::capabilities::PlatformCapabilities::query)
//! reports exactly this (`backend_name = "AVAudioEngine"`, all per-app flags
//! `false`).
//!
//! # Host-app responsibilities (NOT this library's job)
//!
//! rsac deliberately does **not** touch the shared `AVAudioSession` — session
//! configuration is app-global policy that only the host application can own.
//! Before building a capture, the host app must:
//!
//! 1. declare `NSMicrophoneUsageDescription` in its `Info.plist`,
//! 2. obtain the microphone permission (the first mic access prompts), and
//! 3. configure + activate an `AVAudioSession` with a record-capable category
//!    (`.record` or `.playAndRecord`).
//!
//! The Swift helpers shipped in `mobile/ios/` (ADR-0012's SwiftPM package)
//! wrap this consent/session flow. If the session has no active input route,
//! stream creation fails with an actionable
//! [`AudioError::StreamCreationFailed`](crate::core::error::AudioError::StreamCreationFailed)
//! rather than delivering silence.
//!
//! # Architecture
//!
//! Same shape as every other rsac backend (no bespoke stream type):
//!
//! ```text
//! AVFAudio tap thread (non-RT)              Consumer thread
//! ───────────────────────────               ───────────────
//! AVAudioNodeTapBlock                        BridgeStream<IosPlatformStream>
//!   → interleave into scratch                  ::read_chunk()
//!   → BridgeProducer::push_samples_…
//! ```
//!
//! - `avaudio` owns the ObjC interop: engine/tap setup and the tap block
//!   that interleaves each `AVAudioPCMBuffer` into a pre-allocated scratch
//!   buffer and pushes it through the lock-free bridge (ADR-0001 adapted:
//!   no per-callback heap allocation; oversized buffers are dropped and
//!   counted, never allocated for).
//! - `thread` provides `IosPlatformStream` (the internal `PlatformStream`
//!   impl) whose stop path removes the tap, stops the engine, and drives the
//!   bridge to its graceful ending state (producer terminal signal,
//!   ADR-0010).

pub(crate) mod avaudio;
pub(crate) mod thread;

use std::sync::Arc;
use std::time::Duration;

use crate::bridge::state::StreamState;
use crate::bridge::{calculate_capacity, create_bridge, BridgeStream};
use crate::core::config::{AudioFormat, DeviceId, SampleFormat, StreamConfig};
use crate::core::error::{AudioError, AudioResult};
use crate::core::interface::{AudioDevice, CapturingStream, DeviceEnumerator, DeviceKind};

/// The [`DeviceId`] string of the single logical iOS input device.
///
/// iOS does not offer free device selection to apps — the active input is
/// chosen by the shared `AVAudioSession` route (mic / headset / BT / USB).
/// rsac therefore exposes exactly one logical device, `"default"`, meaning
/// "whatever input the session currently routes". The empty string is also
/// accepted as an alias when resolving a [`CaptureTarget::Device`]
/// (matching the Windows backend's default-endpoint convention).
///
/// [`CaptureTarget::Device`]: crate::core::config::CaptureTarget::Device
pub(crate) const DEFAULT_INPUT_DEVICE_ID: &str = "default";

// ── IosDeviceEnumerator ──────────────────────────────────────────────────

/// [`DeviceEnumerator`] for iOS (AVAudioEngine backend, mic slice).
///
/// Enumeration on iOS is intentionally minimal: the OS routes audio input at
/// the `AVAudioSession` level, so there is exactly **one logical input
/// device** — `"default"` — representing the session's current input route.
/// See [`DeviceEnumerator::default_device`] for why the *default device*
/// (rsac's loopback-oriented notion) is an error on iOS today.
#[derive(Debug, Clone, Copy)]
pub struct IosDeviceEnumerator;

impl IosDeviceEnumerator {
    /// Creates a new iOS device enumerator.
    ///
    /// Non-fallible: no OS resources are touched until a stream is created
    /// (matching the factory contract in
    /// [`get_device_enumerator`](crate::audio::get_device_enumerator)).
    pub fn new() -> Self {
        Self
    }
}

impl Default for IosDeviceEnumerator {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceEnumerator for IosDeviceEnumerator {
    /// Lists the single logical iOS audio input device.
    ///
    /// Returns exactly one device — `DeviceId("default")`, the session's
    /// current audio input. Per-route enumeration
    /// (`AVAudioSession.availableInputs`) is session state owned by the host
    /// app and is deliberately not duplicated here.
    fn enumerate_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice>>> {
        Ok(vec![Box::new(IosAudioDevice::new())])
    }

    /// The rsac *default device* is not available on iOS yet — errors with
    /// guidance.
    ///
    /// On desktop backends `default_device()` returns the default **output**
    /// endpoint because rsac's headline capability there is system-audio
    /// loopback. On iOS, system audio (`CaptureTarget::SystemDefault`) is the
    /// ReplayKit Broadcast Upload Extension path, which is **not wired yet**
    /// (rsac-b3aa). Pretending the microphone is "the default device" would
    /// silently deliver different audio than the desktop contract promises
    /// (the dishonest-fallback option ADR-0013 explicitly rejected), so this
    /// returns [`AudioError::PlatformNotSupported`] with the honest state:
    ///
    /// - **Supported now:** microphone capture via
    ///   `CaptureTarget::Device(DeviceId("default".into()))`.
    /// - **Pending:** system audio via the ReplayKit broadcast path
    ///   (rsac-b3aa).
    /// - **Permanent:** per-app / process-tree capture does not exist on iOS
    ///   — Apple provides no API.
    fn default_device(&self) -> AudioResult<Box<dyn AudioDevice>> {
        Err(AudioError::PlatformNotSupported {
            feature: "default-device (system audio loopback) capture on iOS: \
                      SystemDefault is the ReplayKit Broadcast Upload Extension \
                      path, which is not wired yet (rsac-b3aa). Use \
                      CaptureTarget::Device(DeviceId(\"default\".into())) to \
                      capture the microphone (the session's current audio \
                      input). Per-application capture does not exist on iOS, \
                      permanently — Apple provides no API for it"
                .to_string(),
            platform: "ios".to_string(),
        })
    }

    // watch(): inherits the trait default (PlatformNotSupported) — consistent
    // with `supports_device_change_notifications: false` in
    // PlatformCapabilities::ios(). Route changes are AVAudioSession
    // notifications, which belong to the host app / mobile/ios helpers.
}

// ── IosAudioDevice ───────────────────────────────────────────────────────

/// The single logical iOS audio input device (the session's current input).
///
/// A metadata-only handle: constructing it touches no OS resources. The
/// AVAudioEngine machinery is created lazily in
/// [`create_stream`](AudioDevice::create_stream).
#[derive(Debug, Clone, Copy)]
pub struct IosAudioDevice;

impl IosAudioDevice {
    /// Creates the logical default-input device handle.
    pub fn new() -> Self {
        Self
    }
}

impl Default for IosAudioDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioDevice for IosAudioDevice {
    fn id(&self) -> DeviceId {
        DeviceId(DEFAULT_INPUT_DEVICE_ID.to_string())
    }

    fn name(&self) -> String {
        "Default audio input (AVAudioEngine)".to_string()
    }

    fn is_default(&self) -> bool {
        true
    }

    /// Advisory format list: f32 at common iOS session rates, mono/stereo.
    ///
    /// iOS negotiates the *actual* rate/channels at the `AVAudioSession`
    /// level (e.g. `setPreferredSampleRate`), not per-stream — so this list
    /// is a hint, not a contract. The **delivered** format is read from the
    /// live input node at stream creation and reported authoritatively via
    /// [`CapturingStream::format`]; requested rate/channels that differ from
    /// the session's native input format are not converted by this backend.
    fn supported_formats(&self) -> Vec<AudioFormat> {
        const RATES_STEREO: [u32; 2] = [48_000, 44_100];
        const RATES_MONO: [u32; 4] = [48_000, 44_100, 16_000, 8_000];
        let mut formats = Vec::with_capacity(RATES_STEREO.len() + RATES_MONO.len());
        for rate in RATES_STEREO {
            formats.push(AudioFormat {
                sample_rate: rate,
                channels: 2,
                sample_format: SampleFormat::F32,
            });
        }
        for rate in RATES_MONO {
            formats.push(AudioFormat {
                sample_rate: rate,
                channels: 1,
                sample_format: SampleFormat::F32,
            });
        }
        formats
    }

    fn kind(&self) -> AudioResult<DeviceKind> {
        Ok(DeviceKind::Input)
    }

    /// Creates a live microphone capture stream through the ring-buffer
    /// bridge.
    ///
    /// Wiring (identical shape to the desktop backends): create the bridge
    /// (ring depth honours `config.buffer_size`, ADR-0007 pattern), transition
    /// it to `Running`, start the AVAudioEngine input tap
    /// (`thread::create_ios_capture`), and wrap everything in a
    /// `BridgeStream`. The stream's [`format`](CapturingStream::format)
    /// reports the **delivered** (session-native) format, which may differ
    /// from the requested one — see [`AudioDevice::supported_formats`].
    ///
    /// # Errors
    ///
    /// - [`AudioError::PlatformNotSupported`] for `SystemDefault`
    ///   (ReplayKit path pending, rsac-b3aa) and for `Application*` /
    ///   `ProcessTree` (permanently impossible on iOS).
    /// - [`AudioError::DeviceNotFound`] for a `Device` id other than
    ///   `"default"`.
    /// - [`AudioError::StreamCreationFailed`] when the engine cannot start
    ///   (typically: no active input route / missing mic permission — a
    ///   host-app `AVAudioSession` responsibility; see the module docs).
    fn create_stream(&self, config: &StreamConfig) -> AudioResult<Box<dyn CapturingStream>> {
        let requested = config.to_audio_format();

        // Ring sizing: honour the requested slot count like Windows/Linux do
        // (ADR-0007 direction), defaulting to calculate_capacity(None, 4) = 64.
        let capacity = calculate_capacity(config.buffer_size, 4);
        let (producer, consumer) = create_bridge(capacity, requested);

        // Transition bridge state Created → Running before the tap starts
        // pushing, so the first callback's buffers are readable.
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
        // stop/Drop choke point drives the bridge to its graceful ending
        // state so a parked reader observes end-of-stream instead of hanging.
        let terminal = Arc::clone(consumer.shared());

        // Resolve the target, start the engine + tap. `delivered` is the REAL
        // session-native format (also published on the bridge as the
        // negotiated format before the tap is installed).
        let (platform_stream, delivered) =
            thread::create_ios_capture(&config.capture_target, producer, terminal)?;

        let bridge_stream =
            BridgeStream::new(consumer, platform_stream, delivered, Duration::from_secs(1));

        Ok(Box::new(bridge_stream))
    }
}

// ══════════════════════════════════════════════════════════════════════════
// Tests — metadata-only (no ObjC): compile for the iOS target under --tests,
// run on-device. They never touch AVAudioEngine.
// ══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::error::ErrorKind;

    #[test]
    fn enumerate_devices_returns_single_default_input() {
        let enumerator = IosDeviceEnumerator::new();
        let devices = enumerator
            .enumerate_devices()
            .expect("enumeration is infallible metadata");
        assert_eq!(devices.len(), 1, "exactly one logical iOS input device");

        let device = &devices[0];
        assert_eq!(device.id(), DeviceId(DEFAULT_INPUT_DEVICE_ID.to_string()));
        assert_eq!(device.name(), "Default audio input (AVAudioEngine)");
        assert!(device.is_default());
        assert_eq!(device.kind().unwrap(), DeviceKind::Input);
    }

    #[test]
    fn default_device_is_honest_platform_not_supported() {
        let enumerator = IosDeviceEnumerator::new();
        let err = match enumerator.default_device() {
            Ok(_) => panic!("default_device must error until rsac-b3aa"),
            Err(err) => err,
        };
        assert_eq!(err.kind(), ErrorKind::Platform);
        match err {
            AudioError::PlatformNotSupported { feature, platform } => {
                assert_eq!(platform, "ios");
                // The three honesty pillars: what works, what's pending, what
                // is permanent.
                assert!(feature.contains("default"), "mic guidance: {feature}");
                assert!(feature.contains("rsac-b3aa"), "pending seed: {feature}");
                assert!(feature.contains("permanently"), "permanence: {feature}");
                assert!(feature.contains("ReplayKit"), "system path: {feature}");
            }
            other => panic!("expected PlatformNotSupported, got {other:?}"),
        }
    }

    #[test]
    fn supported_formats_are_all_f32_and_non_empty() {
        let device = IosAudioDevice::new();
        let formats = device.supported_formats();
        assert!(!formats.is_empty());
        for fmt in &formats {
            assert_eq!(fmt.sample_format, SampleFormat::F32);
            assert!(fmt.sample_rate > 0);
            assert!(fmt.channels == 1 || fmt.channels == 2);
        }
        // First entry (the DeviceInfo::default_format seed) is 48 kHz stereo.
        assert_eq!(formats[0].sample_rate, 48_000);
        assert_eq!(formats[0].channels, 2);
    }

    #[test]
    fn describe_snapshot_is_consistent() {
        let info = IosAudioDevice::new().describe();
        assert_eq!(info.id, DeviceId(DEFAULT_INPUT_DEVICE_ID.to_string()));
        assert_eq!(info.kind, DeviceKind::Input);
        assert!(info.is_default);
        assert!(info.default_format.is_some());
    }
}
