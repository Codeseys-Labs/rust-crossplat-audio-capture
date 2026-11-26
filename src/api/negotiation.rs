//! Device-format negotiation shared by
//! [`AudioCaptureBuilder::build`](crate::api::AudioCaptureBuilder::build) and
//! [`AudioCaptureBuilder::negotiated_format`](crate::api::AudioCaptureBuilder::negotiated_format).
//!
//! Non-Linux only: WASAPI and CoreAudio advertise a fixed format set that can be
//! negotiated at config time, whereas PipeWire negotiates at stream-open.

use crate::core::config::{AudioFormat, SampleFormat};
use crate::core::error::{AudioError, AudioResult};

/// Resolve the delivery [`AudioFormat`] a device will produce for `requested`,
/// applying the same closest-match negotiation [`build`](crate::api::AudioCaptureBuilder::build)
/// uses.
///
/// Single source of truth shared by [`build`](crate::api::AudioCaptureBuilder::build) and
/// [`negotiated_format`](crate::api::AudioCaptureBuilder::negotiated_format) so a pre-build
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
pub(super) fn negotiate_device_format(
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
/// Used by [`AudioCaptureBuilder::build()`](crate::api::AudioCaptureBuilder::build) to negotiate when a device does
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
    use crate::core::config::StreamConfig;

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
