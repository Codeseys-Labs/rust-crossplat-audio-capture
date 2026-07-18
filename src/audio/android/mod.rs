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
//! | `Device(other id)` | 🟡 real input-device ids via the AAR `AudioManager.getDevices` list + `AAudioStreamBuilder_setDeviceId` (rsac-ad8a) — compiled, unverified on-device (rsac-e6d3) |
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
use crate::core::interface::{
    AudioDevice, CapturingStream, DeviceEnumerator, DeviceEvent, DeviceEventHandler, DeviceKind,
    DeviceWatcher,
};

pub use playback::AndroidPlaybackDevice;

/// The [`DeviceId`] string of the single logical Android input device.
///
/// This is the default-route sentinel: `"default"` means "the default AAudio
/// input route" (whatever the OS routes — mic / headset / BT / USB). The
/// empty string is accepted as an alias when resolving a
/// [`CaptureTarget::Device`] (matching the Windows and iOS backends'
/// default-endpoint convention). Real input-device ids (built-in mic vs USB
/// vs BT) are the numeric `AudioDeviceInfo.getId()` values enumerated via the
/// AAR's `AudioManager.getDevices` list (rsac-ad8a); a numeric id routes
/// through [`AAudioStreamBuilder_setDeviceId`](super::aaudio::AAudioStreamBuilder_setDeviceId)
/// and only a non-numeric / non-positive id yields
/// [`AudioError::DeviceNotFound`].
///
/// [`CaptureTarget::Device`]: crate::core::config::CaptureTarget::Device
pub(crate) const DEFAULT_INPUT_DEVICE_ID: &str = "default";

// ── Real input-device records (rsac-ad8a) ────────────────────────────────

/// Field separator (US, U+001F) in the `RsacDevices.inputDevices` wire
/// format — separates `id␟type␟name` within one record.
const FIELD_SEP: char = '\u{001f}';

/// Record separator (RS, U+001E) — joins per-device records.
const RECORD_SEP: char = '\u{001e}';

/// One real input device parsed from the AAR's `AudioManager.getDevices`
/// list (rsac-ad8a). Pure data — no FFI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AndroidInputDevice {
    /// The `AudioDeviceInfo.getId()` value (always positive on-device).
    pub id: i32,
    /// The `AudioDeviceInfo.getType()` value (an `AudioDeviceInfo.TYPE_*`).
    pub type_code: i32,
    /// The device label (`getProductName()`), or a synthesized name.
    pub name: String,
}

/// A human-readable name for a device whose `getProductName()` was empty,
/// derived from the common `AudioDeviceInfo.TYPE_*` codes.
///
/// Only a small, stable subset is mapped by name; anything else gets a
/// generic label carrying the id, so a name is always non-empty.
fn synthesize_input_name(type_code: i32, id: i32) -> String {
    // AudioDeviceInfo.TYPE_* constants (stable framework values).
    let kind = match type_code {
        15 => "Built-in mic",     // TYPE_BUILTIN_MIC
        3 => "Wired headset mic", // TYPE_WIRED_HEADSET
        4 => "Wired headphones",  // TYPE_WIRED_HEADPHONES
        7 => "Bluetooth SCO mic", // TYPE_BLUETOOTH_SCO
        11 => "USB audio device", // TYPE_USB_DEVICE
        12 => "USB accessory",    // TYPE_USB_ACCESSORY
        22 => "USB headset",      // TYPE_USB_HEADSET
        18 => "Telephony",        // TYPE_TELEPHONY
        _ => return format!("Audio input {id}"),
    };
    kind.to_string()
}

/// Parses the flat delimited record string produced by
/// `RsacDevices.inputDevices` (see `RsacDevices.kt`) into typed records.
///
/// Wire format: `id␟typeInt␟name` records, joined by `␞`. All parsing is
/// **defensive** — a malformed record is skipped (never a panic, never an
/// error), and valid neighbours are kept:
///
/// - split on `␞`; skip blank records;
/// - split each record on `␟`, require ≥ 3 fields;
/// - `id` and `type_code` parse as `i32` (skip the record if either fails);
/// - `id` must be `> 0` (0 is the AAudio `AAUDIO_UNSPECIFIED` sentinel, and
///   real `AudioDeviceInfo` ids are positive);
/// - an empty name is synthesized from the type code
///   ([`synthesize_input_name`]).
///
/// Trailing / blank separators are ignored.
fn parse_input_device_records(raw: &str) -> Vec<AndroidInputDevice> {
    let mut devices = Vec::new();
    for record in raw.split(RECORD_SEP) {
        if record.is_empty() {
            continue;
        }
        let mut fields = record.split(FIELD_SEP);
        let (Some(id_str), Some(type_str), Some(name)) =
            (fields.next(), fields.next(), fields.next())
        else {
            // Fewer than 3 fields — malformed, skip.
            continue;
        };
        let Ok(id) = id_str.parse::<i32>() else {
            continue;
        };
        let Ok(type_code) = type_str.parse::<i32>() else {
            continue;
        };
        if id <= 0 {
            // 0 = UNSPECIFIED sentinel; negatives are never valid ids.
            continue;
        }
        let name = if name.is_empty() {
            synthesize_input_name(type_code, id)
        } else {
            name.to_string()
        };
        devices.push(AndroidInputDevice {
            id,
            type_code,
            name,
        });
    }
    devices
}

/// Diffs a previous input-device id-set against the currently enumerated
/// devices, producing the [`DeviceEvent::DeviceAdded`] /
/// [`DeviceEvent::DeviceRemoved`] events and the next id-set (rsac-d3e2).
///
/// Pure data — cfg-independent of any FFI, unit-tested on the host. Adds are
/// emitted first (in `current` order, carrying the device name and
/// `DeviceKind::Input` — only input devices are enumerated), then removals.
/// The `"default"` sentinel and `"playback-capture"` endpoint never appear
/// here: only real numeric `AudioDeviceInfo.getId()` ids are diffed.
pub(crate) fn diff_device_events(
    previous: &std::collections::HashSet<i32>,
    current: &[AndroidInputDevice],
) -> (Vec<DeviceEvent>, std::collections::HashSet<i32>) {
    let current_ids: std::collections::HashSet<i32> = current.iter().map(|d| d.id).collect();
    let mut events = Vec::new();
    for dev in current.iter().filter(|d| !previous.contains(&d.id)) {
        events.push(DeviceEvent::DeviceAdded {
            id: DeviceId(dev.id.to_string()),
            name: dev.name.clone(),
            kind: DeviceKind::Input,
        });
    }
    for id in previous.difference(&current_ids) {
        events.push(DeviceEvent::DeviceRemoved {
            id: DeviceId(id.to_string()),
        });
    }
    (events, current_ids)
}

// ── AndroidDeviceEnumerator ──────────────────────────────────────────────

/// [`DeviceEnumerator`] for Android (AAudio mic + `AudioPlaybackCapture`
/// playback).
///
/// Enumeration always lists, in order:
///
/// - `"default"` ([`AndroidAudioDevice::new`]): the default AAudio input
///   route (microphone / headset / BT / USB — whatever the OS routes),
///   `is_default`. Always first, so `devices[0]` is a stable "let the OS
///   route the default input" handle, still targetable via `Device("default")`.
/// - Zero or more **real input devices** ([`AndroidAudioDevice::from_real`]),
///   one per `AudioDeviceInfo` in the AAR's
///   `AudioManager.getDevices(GET_DEVICES_INPUTS)` list (rsac-ad8a), each
///   pinnable via `Device(<numeric id>)` → `AAudioStreamBuilder_setDeviceId`.
/// - `"playback-capture"` ([`AndroidPlaybackDevice`]): the playback-capture
///   endpoint (`AudioPlaybackCapture` behind MediaProjection consent),
///   serving `SystemDefault` and the per-app tiers.
///
/// **JNI-absent fallback:** when the Java list cannot be obtained (host
/// tests / pure-NDK consumers with no `JavaVM`, the AAR classes absent, or
/// the Java call threw), the real-device middle section is empty and
/// enumeration yields exactly `[default sentinel, playback]` — honest (no
/// fabricated devices) and unchanged from the pre-rsac-ad8a behaviour.
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
    /// Lists the Android devices: the default AAudio input sentinel, any real
    /// input devices from the AAR's `AudioManager.getDevices` list, and the
    /// playback-capture endpoint.
    ///
    /// When the Java list is unavailable (no `JavaVM` on host tests / pure-NDK
    /// consumers, the AAR classes absent, or the Java call threw), the
    /// real-device section is empty and the result is exactly
    /// `[default sentinel, playback]` — the pre-rsac-ad8a behaviour, kept
    /// honest and host-stable.
    fn enumerate_devices(&self) -> AudioResult<Vec<Box<dyn AudioDevice>>> {
        let mut devices: Vec<Box<dyn AudioDevice>> = Vec::new();
        // devices[0]: the stable default-route sentinel (is_default).
        devices.push(Box::new(AndroidAudioDevice::new()));
        // Real input devices, when the JNI path can reach the AAR list. Any
        // Err (host / NDK-only / AAR-absent / Java threw) falls back silently.
        if let Ok(raw) = jni::enumerate_input_device_records() {
            for rec in parse_input_device_records(&raw) {
                devices.push(Box::new(AndroidAudioDevice::from_real(&rec)));
            }
        }
        // The playback-capture endpoint stays last.
        devices.push(Box::new(AndroidPlaybackDevice::new()));
        Ok(devices)
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

    /// Subscribes to input-device add/remove notifications via the AAR's
    /// `AudioManager.registerAudioDeviceCallback` (rsac-d3e2).
    ///
    /// Emits [`DeviceEvent::DeviceAdded`] / [`DeviceEvent::DeviceRemoved`]
    /// (always `DeviceKind::Input`, numeric `AudioDeviceInfo.getId()` ids)
    /// by re-enumerating + diffing the AAR input-device list on each
    /// callback fire — the events are therefore always consistent with what
    /// [`enumerate_devices`](Self::enumerate_devices) returns. It does
    /// **not** emit `DefaultChanged` / `StateChanged`: `AudioDeviceCallback`
    /// exposes no default-route or state signal, and claiming them would
    /// violate the honest-capabilities rule.
    ///
    /// Events are delivered on the AAR's dedicated `HandlerThread` (never
    /// the main looper, never an RT audio thread). Errors with
    /// [`AudioError::PlatformNotSupported`] when the AAR callback path is
    /// unavailable (pure-NDK / older AAR) — consistent with
    /// `supports_device_change_notifications` in
    /// [`PlatformCapabilities::query`](crate::core::capabilities::PlatformCapabilities::query).
    fn watch(&self, on_event: DeviceEventHandler) -> AudioResult<DeviceWatcher> {
        jni::watch_input_devices(on_event)
    }
}

// ── AndroidAudioDevice ───────────────────────────────────────────────────

/// An Android audio **input** device.
///
/// A metadata-only handle: constructing it touches no OS resources. The
/// AAudio stream is created lazily in
/// [`create_stream`](AudioDevice::create_stream), routed by
/// `config.capture_target` (the caller sets `Device(self.id())`).
///
/// Two flavours:
///
/// - [`new`](Self::new): the **default-route sentinel** (`DeviceId("default")`,
///   `is_default`), meaning "let the OS route the default input".
/// - [`from_real`](Self::from_real): a **real** device from the AAR's
///   `AudioManager.getDevices` list, carrying its numeric
///   `AudioDeviceInfo.getId()` and product name (rsac-ad8a).
#[derive(Debug, Clone)]
pub struct AndroidAudioDevice {
    id: DeviceId,
    name: String,
    is_default: bool,
}

impl AndroidAudioDevice {
    /// Creates the default-route input sentinel (`DeviceId("default")`,
    /// `is_default`) — the stable `devices[0]` handle.
    pub fn new() -> Self {
        Self {
            id: DeviceId(DEFAULT_INPUT_DEVICE_ID.to_string()),
            name: "Default audio input (AAudio)".to_string(),
            is_default: true,
        }
    }

    /// Creates a handle for a **real** enumerated input device: its numeric
    /// `AudioDeviceInfo.getId()` becomes the [`DeviceId`] string (which
    /// [`resolve_mic_target`](thread::resolve_mic_target) parses back to the
    /// AAudio device id), carrying the device name; never `is_default`.
    pub(crate) fn from_real(rec: &AndroidInputDevice) -> Self {
        Self {
            id: DeviceId(rec.id.to_string()),
            name: rec.name.clone(),
            is_default: false,
        }
    }
}

impl Default for AndroidAudioDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioDevice for AndroidAudioDevice {
    fn id(&self) -> DeviceId {
        self.id.clone()
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn is_default(&self) -> bool {
        self.is_default
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
    /// - [`AudioError::DeviceNotFound`] only for an **invalid** `Device` id:
    ///   non-numeric or non-positive. `"default"` / `""` route the default
    ///   input; a positive numeric id routes through
    ///   `AAudioStreamBuilder_setDeviceId` (rsac-ad8a). A syntactically valid
    ///   id the OS later rejects surfaces as
    ///   [`AudioError::StreamCreationFailed`] from the open path, not here.
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

    // ── Real input-device record parsing (rsac-ad8a) ─────────────────

    /// Builds a wire record string from `(id, type, name)` tuples using the
    /// real US/RS separators (so the test also pins the delimiter bytes).
    fn wire(records: &[(&str, &str, &str)]) -> String {
        records
            .iter()
            .map(|(id, ty, name)| format!("{id}{FIELD_SEP}{ty}{FIELD_SEP}{name}"))
            .collect::<Vec<_>>()
            .join(&RECORD_SEP.to_string())
    }

    #[test]
    fn parse_happy_path_multi_record_preserves_order() {
        let raw = wire(&[
            ("5", "15", "Built-in Mic"),
            ("11", "3", "USB-C Headset"),
            ("42", "7", "BT Earbuds"),
        ]);
        let devices = parse_input_device_records(&raw);
        assert_eq!(devices.len(), 3);
        assert_eq!(
            devices[0],
            AndroidInputDevice {
                id: 5,
                type_code: 15,
                name: "Built-in Mic".to_string()
            }
        );
        assert_eq!(devices[1].id, 11);
        assert_eq!(devices[1].name, "USB-C Headset");
        assert_eq!(devices[2].id, 42);
    }

    #[test]
    fn parse_empty_string_is_empty_vec() {
        assert!(parse_input_device_records("").is_empty());
    }

    #[test]
    fn parse_skips_malformed_records_keeps_valid_neighbours() {
        // Records: missing field, non-numeric id, id==0, id<0, blank, then a
        // valid one and a valid one before them. Interleave valid + invalid.
        let raw = wire(&[
            ("7", "15", "Good One"),  // valid
            ("notint", "15", "Bad"),  // non-numeric id → skip
            ("9", "notint", "BadTy"), // non-numeric type → skip
            ("0", "15", "Zero"),      // id == 0 (UNSPECIFIED) → skip
            ("-3", "15", "Neg"),      // id < 0 → skip
            ("13", "11", "Good Two"), // valid
        ]);
        // Splice in a record with too few fields and a blank record.
        let raw = format!("{raw}{RECORD_SEP}justonefield{RECORD_SEP}{RECORD_SEP}");
        let devices = parse_input_device_records(&raw);
        let names: Vec<&str> = devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["Good One", "Good Two"]);
        assert_eq!(devices[0].id, 7);
        assert_eq!(devices[1].id, 13);
    }

    #[test]
    fn parse_trailing_separator_is_ignored() {
        let raw = format!("{}{RECORD_SEP}", wire(&[("5", "15", "Mic")]));
        let devices = parse_input_device_records(&raw);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, 5);
    }

    #[test]
    fn parse_empty_name_is_synthesized_from_type_code() {
        let raw = wire(&[("5", "15", ""), ("11", "11", ""), ("9", "999", "")]);
        let devices = parse_input_device_records(&raw);
        // TYPE_BUILTIN_MIC, TYPE_USB_DEVICE, then an unknown type → generic
        // label carrying the id (never empty).
        assert_eq!(devices[0].name, "Built-in mic");
        assert_eq!(devices[1].name, "USB audio device");
        assert_eq!(devices[2].name, "Audio input 9");
        assert!(devices.iter().all(|d| !d.name.is_empty()));
    }

    #[test]
    fn parse_name_with_spaces_and_unicode_round_trips() {
        let raw = wire(&[("5", "15", "Bosch Mikrofon 日本語 🎙")]);
        let devices = parse_input_device_records(&raw);
        assert_eq!(devices[0].name, "Bosch Mikrofon 日本語 🎙");
    }

    // ── Device-change diffing (rsac-d3e2) ────────────────────────────

    fn dev(id: i32, name: &str) -> AndroidInputDevice {
        AndroidInputDevice {
            id,
            type_code: 15,
            name: name.to_string(),
        }
    }

    #[test]
    fn diff_empty_previous_emits_adds() {
        let previous = std::collections::HashSet::new();
        let current = [dev(5, "Built-in Mic"), dev(11, "USB Headset")];
        let (events, next) = diff_device_events(&previous, &current);
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0],
            DeviceEvent::DeviceAdded {
                id: DeviceId("5".to_string()),
                name: "Built-in Mic".to_string(),
                kind: DeviceKind::Input,
            }
        );
        assert_eq!(
            events[1],
            DeviceEvent::DeviceAdded {
                id: DeviceId("11".to_string()),
                name: "USB Headset".to_string(),
                kind: DeviceKind::Input,
            }
        );
        assert_eq!(next, [5, 11].into_iter().collect());
    }

    #[test]
    fn diff_detects_added_and_removed() {
        let previous: std::collections::HashSet<i32> = [5, 11].into_iter().collect();
        let current = [dev(11, "USB Headset"), dev(42, "BT Earbuds")];
        let (events, next) = diff_device_events(&previous, &current);
        assert_eq!(events.len(), 2, "one add + one remove");
        assert_eq!(
            events[0],
            DeviceEvent::DeviceAdded {
                id: DeviceId("42".to_string()),
                name: "BT Earbuds".to_string(),
                kind: DeviceKind::Input,
            }
        );
        assert_eq!(
            events[1],
            DeviceEvent::DeviceRemoved {
                id: DeviceId("5".to_string()),
            }
        );
        assert_eq!(next, [11, 42].into_iter().collect());
    }

    #[test]
    fn diff_no_change_is_empty() {
        let previous: std::collections::HashSet<i32> = [5, 11].into_iter().collect();
        let current = [dev(5, "Built-in Mic"), dev(11, "USB Headset")];
        let (events, next) = diff_device_events(&previous, &current);
        assert!(events.is_empty(), "identical topology emits nothing");
        assert_eq!(next, previous);
    }

    #[test]
    fn diff_added_carries_name_and_input_kind() {
        let previous = std::collections::HashSet::new();
        let current = [dev(7, "Bosch Mikrofon 日本語 🎙")];
        let (events, _) = diff_device_events(&previous, &current);
        let DeviceEvent::DeviceAdded { id, name, kind } = &events[0] else {
            panic!("expected DeviceAdded, got {:?}", events[0]);
        };
        assert_eq!(id, &DeviceId("7".to_string()));
        assert_eq!(name, "Bosch Mikrofon 日本語 🎙");
        assert_eq!(*kind, DeviceKind::Input);
    }

    // ── AndroidAudioDevice identity (rsac-ad8a) ──────────────────────

    #[test]
    fn from_real_carries_numeric_id_and_is_not_default() {
        let rec = AndroidInputDevice {
            id: 11,
            type_code: 22,
            name: "USB Headset".to_string(),
        };
        let device = AndroidAudioDevice::from_real(&rec);
        assert_eq!(device.id(), DeviceId("11".to_string()));
        assert_eq!(device.name(), "USB Headset");
        assert!(!device.is_default());
        assert_eq!(device.kind().unwrap(), DeviceKind::Input);
    }

    #[test]
    fn new_preserves_default_sentinel_invariants() {
        let device = AndroidAudioDevice::new();
        assert_eq!(device.id(), DeviceId(DEFAULT_INPUT_DEVICE_ID.to_string()));
        assert_eq!(device.name(), "Default audio input (AAudio)");
        assert!(device.is_default());
        assert_eq!(device.kind().unwrap(), DeviceKind::Input);
    }
}
