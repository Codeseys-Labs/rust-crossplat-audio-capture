// src/core/config.rs

//! Configuration types: capture target, stream config, audio format, IDs.
//!
//! This module defines what a capture *describes* (as opposed to the runtime
//! data flowing through it). The most important type is [`CaptureTarget`],
//! which selects what audio to capture: system default, a specific device,
//! a single application, or an entire process tree.
//!
//! [`StreamConfig`] carries the requested audio format ([`AudioFormat`]),
//! buffer sizing hints, and latency mode. [`DeviceId`], [`ApplicationId`],
//! and [`ProcessId`] are opaque newtypes that identify the capture subject.

// ── ID Newtypes ──────────────────────────────────────────────────────────

/// Opaque identifier for an audio device.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceId(pub String);

impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Opaque identifier for an application (audio session).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApplicationId(pub String);

impl std::fmt::Display for ApplicationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Opaque identifier for an OS process (PID).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProcessId(pub u32);

impl std::fmt::Display for ProcessId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ── CaptureTarget ────────────────────────────────────────────────────────

/// Unified capture target model covering all capture modes.
///
/// This enum specifies *what* audio should be captured. It replaces the old
/// combination of `DeviceSelector` + PID/session fields with a single,
/// explicit discriminated union.
///
/// # Stability
///
/// This enum is `#[non_exhaustive]`: new capture-target kinds may be added in a
/// minor release. **Out-of-crate** code matching on `CaptureTarget` must include a
/// trailing wildcard (`_ =>`) arm. The in-crate [`Display`](std::fmt::Display) and
/// [`FromStr`](std::str::FromStr) impls stay exhaustive on purpose so a new variant
/// forces its canonical string form to be defined.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CaptureTarget {
    /// Capture from the system default audio device / mix.
    #[default]
    SystemDefault,
    /// Capture from a specific device identified by [`DeviceId`].
    Device(DeviceId),
    /// Capture audio from a specific application session identified by [`ApplicationId`].
    Application(ApplicationId),
    /// Capture audio from the first application whose name matches the given string.
    ApplicationByName(String),
    /// Capture audio from a process and its child processes, identified by [`ProcessId`].
    ProcessTree(ProcessId),
}

impl std::fmt::Display for CaptureTarget {
    /// Formats a [`CaptureTarget`] into its canonical string form.
    ///
    /// The output is the exact inverse of the [`FromStr`](std::str::FromStr)
    /// impl: for any `target`,
    /// `target.to_string().parse::<CaptureTarget>() == Ok(target)`.
    ///
    /// Canonical forms:
    /// - [`SystemDefault`](CaptureTarget::SystemDefault) → `"system"`
    /// - [`Device`](CaptureTarget::Device) → `"device:<id>"`
    /// - [`Application`](CaptureTarget::Application) → `"app:<id>"`
    /// - [`ApplicationByName`](CaptureTarget::ApplicationByName) → `"name:<name>"`
    /// - [`ProcessTree`](CaptureTarget::ProcessTree) → `"tree:<pid>"`
    ///
    /// The `match` is intentionally exhaustive (no wildcard arm) so that adding
    /// a new [`CaptureTarget`] variant is a compile error until its canonical
    /// form is defined here.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CaptureTarget::SystemDefault => write!(f, "system"),
            CaptureTarget::Device(id) => write!(f, "device:{}", id.0),
            CaptureTarget::Application(id) => write!(f, "app:{}", id.0),
            CaptureTarget::ApplicationByName(name) => write!(f, "name:{}", name),
            CaptureTarget::ProcessTree(pid) => write!(f, "tree:{}", pid.0),
        }
    }
}

impl std::str::FromStr for CaptureTarget {
    type Err = crate::core::error::AudioError;

    /// Parses a [`CaptureTarget`] from its canonical string form.
    ///
    /// Grammar (the `scheme` prefix is matched case-insensitively):
    /// - `system` | `default` → [`SystemDefault`](CaptureTarget::SystemDefault)
    /// - `device:<id>` → [`Device`](CaptureTarget::Device). The body is taken
    ///   verbatim after the **first** colon, so device ids that themselves
    ///   contain colons (e.g. `hw:0,0`) are preserved.
    /// - `app:<id>` → [`Application`](CaptureTarget::Application)
    /// - `name:<name>` → [`ApplicationByName`](CaptureTarget::ApplicationByName)
    /// - `tree:<pid>` | `pid:<pid>` → [`ProcessTree`](CaptureTarget::ProcessTree),
    ///   where `<pid>` must be a `u32`.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::InvalidParameter`] with `param == "capture_target"`
    /// for an unknown scheme or a non-numeric / out-of-range pid. Never panics.
    ///
    /// [`AudioError::InvalidParameter`]: crate::core::error::AudioError::InvalidParameter
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use crate::core::error::AudioError;

        let invalid = |reason: String| AudioError::InvalidParameter {
            param: "capture_target".to_string(),
            reason,
        };

        // Schemes without a body: bare `system` / `default`.
        // Compare case-insensitively against the (whole) trimmed input.
        if s.eq_ignore_ascii_case("system") || s.eq_ignore_ascii_case("default") {
            return Ok(CaptureTarget::SystemDefault);
        }

        // Schemes of the form `<scheme>:<body>`. Split on the FIRST colon only,
        // so a body that contains further colons (device ids) is preserved.
        let (scheme, body) = match s.split_once(':') {
            Some((scheme, body)) => (scheme, body),
            None => {
                return Err(invalid(format!(
                    "unknown capture target '{}': expected one of \
                     'system', 'default', 'device:<id>', 'app:<id>', \
                     'name:<name>', 'tree:<pid>', or 'pid:<pid>'",
                    s
                )));
            }
        };

        // Case-insensitive scheme matching: lowercase only the short scheme
        // token (never the body, which is preserved verbatim).
        let scheme_lc = scheme.to_ascii_lowercase();

        // Parses a pid body as a u32, mapping failures to InvalidParameter.
        let parse_pid = |kind: &str| -> Result<u32, AudioError> {
            body.parse::<u32>()
                .map_err(|e| invalid(format!("invalid {} pid '{}': {}", kind, body, e)))
        };

        match scheme_lc.as_str() {
            "device" => Ok(CaptureTarget::Device(DeviceId(body.to_string()))),
            "app" => Ok(CaptureTarget::Application(ApplicationId(body.to_string()))),
            "name" => Ok(CaptureTarget::ApplicationByName(body.to_string())),
            "tree" => Ok(CaptureTarget::ProcessTree(ProcessId(parse_pid("tree")?))),
            "pid" => Ok(CaptureTarget::ProcessTree(ProcessId(parse_pid("pid")?))),
            other => Err(invalid(format!(
                "unknown capture target scheme '{}': expected one of \
                 'device', 'app', 'name', 'tree', or 'pid' \
                 (or bare 'system'/'default')",
                other
            ))),
        }
    }
}

impl TryFrom<&str> for CaptureTarget {
    type Error = crate::core::error::AudioError;

    /// Parses a [`CaptureTarget`] from a string slice.
    ///
    /// Delegates to the [`FromStr`](std::str::FromStr) implementation, so it
    /// follows exactly the same grammar and error rules.
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        s.parse()
    }
}

// ── SampleFormat (new canonical 4-variant) ───────────────────────────────

/// Specifies the format of audio samples.
///
/// All audio data is standardized to `f32` internally, but this enum
/// describes the wire/storage format for configuration and capability
/// negotiation.
///
/// # Stability
///
/// This enum is **deliberately not** `#[non_exhaustive]`: the four PCM sample
/// formats are a fixed, intentional set callers match exhaustively (e.g. to size
/// per-sample buffers). Keeping it closed is a stability guarantee — the set will
/// not grow in a way that silently breaks exhaustive matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleFormat {
    /// Signed 16-bit integer.
    I16,
    /// Signed 24-bit integer (packed in 32-bit container).
    I24,
    /// Signed 32-bit integer.
    I32,
    /// 32-bit IEEE 754 floating-point.
    F32,
}

impl Default for SampleFormat {
    /// Returns `F32` — the library's internal standard format.
    fn default() -> Self {
        SampleFormat::F32
    }
}

impl SampleFormat {
    /// Returns the number of bits per sample for this format.
    pub fn bits_per_sample(&self) -> u16 {
        match self {
            SampleFormat::I16 => 16,
            SampleFormat::I24 => 24,
            SampleFormat::I32 | SampleFormat::F32 => 32,
        }
    }
}

// ── AudioFormat ──────────────────────────────────────────────────────────

/// Represents a concrete audio format describing sample rate, channels,
/// and sample type.
///
/// Used by [`AudioBuffer`](super::buffer::AudioBuffer) and the
/// [`AudioDevice`](super::interface::AudioDevice) trait to describe
/// the actual format of audio data.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AudioFormat {
    /// The number of samples per second (e.g., 44100, 48000).
    pub sample_rate: u32,
    /// The number of audio channels (e.g., 1 for mono, 2 for stereo).
    pub channels: u16,
    /// The specific sample format.
    pub sample_format: SampleFormat,
}

impl Default for AudioFormat {
    /// Provides a default `AudioFormat`: 48 kHz, stereo, F32.
    fn default() -> Self {
        AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
        }
    }
}

// ── StreamConfig ─────────────────────────────────────────────────────────

/// Configuration for an audio stream.
///
/// Specifies the desired audio format and buffer size when opening a stream.
/// This is a simplified, flat representation — no nested `AudioFormat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamConfig {
    /// The desired sample rate in Hz (e.g., 48000).
    pub sample_rate: u32,
    /// The desired number of audio channels (e.g., 2 for stereo).
    pub channels: u16,
    /// The desired sample format.
    pub sample_format: SampleFormat,
    /// Optional desired buffer size in frames.
    /// If `None`, the backend will choose a suitable default.
    pub buffer_size: Option<usize>,
    /// The capture target for this stream.
    /// Propagated from [`AudioCaptureBuilder`](crate::api::AudioCaptureBuilder) so backends know
    /// whether to do system, application, or process-tree capture.
    pub capture_target: CaptureTarget,
}

impl Default for StreamConfig {
    /// Provides a default `StreamConfig`: 48 kHz, 2 channels, F32, no buffer size preference.
    fn default() -> Self {
        StreamConfig {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32,
            buffer_size: None,
            capture_target: CaptureTarget::default(), // SystemDefault
        }
    }
}

impl StreamConfig {
    /// Converts this `StreamConfig` into a corresponding [`AudioFormat`].
    pub fn to_audio_format(&self) -> AudioFormat {
        AudioFormat {
            sample_rate: self.sample_rate,
            channels: self.channels,
            sample_format: self.sample_format,
        }
    }
}

// ── AudioCaptureConfig ───────────────────────────────────────────────────

/// Full configuration for an audio capture session.
///
/// Created by [`AudioCaptureBuilder`](crate::api::AudioCaptureBuilder),
/// this struct stores the validated capture parameters.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioCaptureConfig {
    /// What to capture — system default, specific device, application, etc.
    pub target: CaptureTarget,
    /// Stream format and buffer configuration.
    pub stream_config: StreamConfig,
}

// ── Legacy / Compatibility Types ─────────────────────────────────────────

/// Defines preferred latency modes for an audio stream.
///
/// Backends will attempt to honor this preference, but actual latency
/// may vary based on system capabilities and load.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum LatencyMode {
    /// Prioritizes the lowest possible latency.
    LowLatency,
    /// Aims for a balance (default).
    #[default]
    Balanced,
    /// Prioritizes lower power consumption.
    PowerSaving,
}

/// Specifies criteria for selecting an audio device.
///
/// **Deprecated** — prefer [`CaptureTarget`] for new code.
/// Retained for backward compatibility during the API transition.
#[allow(deprecated)]
#[deprecated(note = "Use CaptureTarget instead")]
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum DeviceSelector {
    /// Selects the system's default input device.
    #[default]
    DefaultInput,
    /// Selects the system's default output device.
    DefaultOutput,
    /// Selects a device by its platform-specific identifier.
    ById(String),
    /// Selects a device by name (first match).
    ByName(String),
}

#[allow(deprecated)]
impl std::fmt::Display for DeviceSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceSelector::DefaultInput => write!(f, "DefaultInput"),
            DeviceSelector::DefaultOutput => write!(f, "DefaultOutput"),
            DeviceSelector::ById(id) => write!(f, "ById({})", id),
            DeviceSelector::ByName(name) => write!(f, "ByName({})", name),
        }
    }
}

/// Specifies the audio file format for recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioFileFormat {
    /// Waveform Audio File Format.
    #[default]
    Wav,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::error::AudioError;

    // ── DeviceId ─────────────────────────────────────────────────────

    #[test]
    fn device_id_construction_and_display() {
        let id = DeviceId("hw:0,0".to_string());
        assert_eq!(id.to_string(), "hw:0,0");
    }

    #[test]
    fn device_id_clone_and_eq() {
        let id = DeviceId("device-1".to_string());
        let cloned = id.clone();
        assert_eq!(id, cloned);
    }

    #[test]
    fn device_id_inequality() {
        let a = DeviceId("a".to_string());
        let b = DeviceId("b".to_string());
        assert_ne!(a, b);
    }

    #[test]
    fn device_id_debug() {
        let id = DeviceId("test".to_string());
        let dbg = format!("{:?}", id);
        assert!(dbg.contains("DeviceId"));
        assert!(dbg.contains("test"));
    }

    // ── ApplicationId ────────────────────────────────────────────────

    #[test]
    fn application_id_construction_and_display() {
        let id = ApplicationId("com.example.app".to_string());
        assert_eq!(id.to_string(), "com.example.app");
    }

    #[test]
    fn application_id_clone_and_eq() {
        let id = ApplicationId("session-42".to_string());
        let cloned = id.clone();
        assert_eq!(id, cloned);
    }

    #[test]
    fn application_id_debug() {
        let id = ApplicationId("app".to_string());
        let dbg = format!("{:?}", id);
        assert!(dbg.contains("ApplicationId"));
    }

    // ── ProcessId ────────────────────────────────────────────────────

    #[test]
    fn process_id_construction_and_display() {
        let pid = ProcessId(12345);
        assert_eq!(pid.to_string(), "12345");
    }

    #[test]
    fn process_id_clone_and_eq() {
        let pid = ProcessId(42);
        let cloned = pid.clone();
        assert_eq!(pid, cloned);
    }

    #[test]
    fn process_id_debug() {
        let pid = ProcessId(1);
        let dbg = format!("{:?}", pid);
        assert!(dbg.contains("ProcessId"));
        assert!(dbg.contains("1"));
    }

    // ── CaptureTarget ────────────────────────────────────────────────

    #[test]
    fn capture_target_default_is_system_default() {
        assert_eq!(CaptureTarget::default(), CaptureTarget::SystemDefault);
    }

    #[test]
    fn capture_target_all_variants_constructible() {
        let _ = CaptureTarget::SystemDefault;
        let _ = CaptureTarget::Device(DeviceId("d".to_string()));
        let _ = CaptureTarget::Application(ApplicationId("a".to_string()));
        let _ = CaptureTarget::ApplicationByName("Firefox".to_string());
        let _ = CaptureTarget::ProcessTree(ProcessId(1));
    }

    #[test]
    fn capture_target_clone() {
        let target = CaptureTarget::Device(DeviceId("hw:1".to_string()));
        let cloned = target.clone();
        assert_eq!(target, cloned);
    }

    #[test]
    fn capture_target_eq_same_variant() {
        let a = CaptureTarget::ApplicationByName("Spotify".to_string());
        let b = CaptureTarget::ApplicationByName("Spotify".to_string());
        assert_eq!(a, b);
    }

    #[test]
    fn capture_target_ne_different_variant() {
        assert_ne!(
            CaptureTarget::SystemDefault,
            CaptureTarget::ProcessTree(ProcessId(1))
        );
    }

    #[test]
    fn capture_target_debug() {
        let dbg = format!("{:?}", CaptureTarget::SystemDefault);
        assert!(dbg.contains("SystemDefault"));
    }

    // ── CaptureTarget: Display ───────────────────────────────────────

    #[test]
    fn capture_target_display_system_default() {
        assert_eq!(CaptureTarget::SystemDefault.to_string(), "system");
    }

    #[test]
    fn capture_target_display_device() {
        assert_eq!(
            CaptureTarget::Device(DeviceId("hw:0,0".to_string())).to_string(),
            "device:hw:0,0"
        );
    }

    #[test]
    fn capture_target_display_application() {
        assert_eq!(
            CaptureTarget::Application(ApplicationId("Spotify".to_string())).to_string(),
            "app:Spotify"
        );
    }

    #[test]
    fn capture_target_display_application_by_name() {
        assert_eq!(
            CaptureTarget::ApplicationByName("Firefox".to_string()).to_string(),
            "name:Firefox"
        );
    }

    #[test]
    fn capture_target_display_process_tree() {
        assert_eq!(
            CaptureTarget::ProcessTree(ProcessId(7)).to_string(),
            "tree:7"
        );
    }

    // ── CaptureTarget: FromStr (happy paths) ─────────────────────────

    #[test]
    fn capture_target_parse_system() {
        assert_eq!(
            "system".parse::<CaptureTarget>().unwrap(),
            CaptureTarget::SystemDefault
        );
    }

    #[test]
    fn capture_target_parse_default_alias() {
        assert_eq!(
            "default".parse::<CaptureTarget>().unwrap(),
            CaptureTarget::SystemDefault
        );
    }

    #[test]
    fn capture_target_parse_scheme_is_case_insensitive() {
        assert_eq!(
            "SYSTEM".parse::<CaptureTarget>().unwrap(),
            CaptureTarget::SystemDefault
        );
        assert_eq!(
            "Default".parse::<CaptureTarget>().unwrap(),
            CaptureTarget::SystemDefault
        );
        assert_eq!(
            "DEVICE:hw:0,0".parse::<CaptureTarget>().unwrap(),
            CaptureTarget::Device(DeviceId("hw:0,0".to_string()))
        );
        assert_eq!(
            "App:Spotify".parse::<CaptureTarget>().unwrap(),
            CaptureTarget::Application(ApplicationId("Spotify".to_string()))
        );
        assert_eq!(
            "Tree:42".parse::<CaptureTarget>().unwrap(),
            CaptureTarget::ProcessTree(ProcessId(42))
        );
    }

    #[test]
    fn capture_target_parse_device_preserves_colons_in_id() {
        // Split on the FIRST colon only — the id may itself contain colons.
        assert_eq!(
            "device:hw:0,0".parse::<CaptureTarget>().unwrap(),
            CaptureTarget::Device(DeviceId("hw:0,0".to_string()))
        );
    }

    #[test]
    fn capture_target_parse_application() {
        assert_eq!(
            "app:Spotify".parse::<CaptureTarget>().unwrap(),
            CaptureTarget::Application(ApplicationId("Spotify".to_string()))
        );
    }

    #[test]
    fn capture_target_parse_application_body_preserves_colons() {
        // The body after `app:` is taken verbatim, colons and all.
        assert_eq!(
            "app:com.example:session:1"
                .parse::<CaptureTarget>()
                .unwrap(),
            CaptureTarget::Application(ApplicationId("com.example:session:1".to_string()))
        );
    }

    #[test]
    fn capture_target_parse_application_by_name() {
        assert_eq!(
            "name:VLC".parse::<CaptureTarget>().unwrap(),
            CaptureTarget::ApplicationByName("VLC".to_string())
        );
    }

    #[test]
    fn capture_target_parse_name_body_is_case_preserving() {
        // Only the scheme is case-insensitive; the body keeps its case.
        assert_eq!(
            "NAME:MixedCaseApp".parse::<CaptureTarget>().unwrap(),
            CaptureTarget::ApplicationByName("MixedCaseApp".to_string())
        );
    }

    #[test]
    fn capture_target_parse_tree() {
        assert_eq!(
            "tree:99".parse::<CaptureTarget>().unwrap(),
            CaptureTarget::ProcessTree(ProcessId(99))
        );
    }

    #[test]
    fn capture_target_parse_pid_alias_equals_tree() {
        // Both `pid:<n>` and `tree:<n>` map to ProcessTree.
        assert_eq!(
            "pid:99".parse::<CaptureTarget>().unwrap(),
            CaptureTarget::ProcessTree(ProcessId(99))
        );
        assert_eq!(
            "pid:99".parse::<CaptureTarget>().unwrap(),
            "tree:99".parse::<CaptureTarget>().unwrap()
        );
    }

    #[test]
    fn capture_target_parse_pid_zero_and_max() {
        assert_eq!(
            "tree:0".parse::<CaptureTarget>().unwrap(),
            CaptureTarget::ProcessTree(ProcessId(0))
        );
        let max = u32::MAX;
        assert_eq!(
            format!("tree:{}", max).parse::<CaptureTarget>().unwrap(),
            CaptureTarget::ProcessTree(ProcessId(max))
        );
    }

    // ── CaptureTarget: FromStr (error paths, never panic) ────────────

    #[test]
    fn capture_target_parse_unknown_scheme_errors() {
        let err = "bogus:x".parse::<CaptureTarget>().unwrap_err();
        match err {
            AudioError::InvalidParameter { param, .. } => {
                assert_eq!(param, "capture_target");
            }
            other => panic!("expected InvalidParameter, got {:?}", other),
        }
    }

    #[test]
    fn capture_target_parse_no_colon_unknown_errors() {
        let err = "bogus".parse::<CaptureTarget>().unwrap_err();
        match err {
            AudioError::InvalidParameter { param, .. } => {
                assert_eq!(param, "capture_target");
            }
            other => panic!("expected InvalidParameter, got {:?}", other),
        }
    }

    #[test]
    fn capture_target_parse_non_numeric_pid_errors() {
        for input in ["pid:abc", "tree:abc", "pid:", "tree:1.5", "pid:-1"] {
            let err = input.parse::<CaptureTarget>().unwrap_err();
            match err {
                AudioError::InvalidParameter { param, .. } => {
                    assert_eq!(param, "capture_target", "for input {:?}", input);
                }
                other => panic!("expected InvalidParameter for {:?}, got {:?}", input, other),
            }
        }
    }

    #[test]
    fn capture_target_parse_overflow_pid_errors() {
        // One past u32::MAX must error, not panic or wrap.
        let too_big = (u32::MAX as u64 + 1).to_string();
        let err = format!("tree:{}", too_big)
            .parse::<CaptureTarget>()
            .unwrap_err();
        match err {
            AudioError::InvalidParameter { param, .. } => {
                assert_eq!(param, "capture_target");
            }
            other => panic!("expected InvalidParameter, got {:?}", other),
        }
    }

    #[test]
    fn capture_target_parse_empty_string_errors() {
        let err = "".parse::<CaptureTarget>().unwrap_err();
        assert!(matches!(err, AudioError::InvalidParameter { .. }));
    }

    // ── CaptureTarget: TryFrom<&str> matches FromStr ─────────────────

    #[test]
    fn capture_target_try_from_matches_from_str() {
        let inputs = [
            "system",
            "default",
            "device:hw:0,0",
            "app:Spotify",
            "name:VLC",
            "tree:99",
            "pid:99",
            "bogus:x",
            "pid:abc",
            "",
        ];
        for input in inputs {
            let via_from_str = input.parse::<CaptureTarget>();
            let via_try_from = CaptureTarget::try_from(input);
            match (via_from_str, via_try_from) {
                (Ok(a), Ok(b)) => assert_eq!(a, b, "Ok mismatch for {:?}", input),
                (Err(a), Err(b)) => {
                    // Compare on the structured fields (AudioError isn't PartialEq).
                    match (a, b) {
                        (
                            AudioError::InvalidParameter {
                                param: pa,
                                reason: ra,
                            },
                            AudioError::InvalidParameter {
                                param: pb,
                                reason: rb,
                            },
                        ) => {
                            assert_eq!(pa, pb, "param mismatch for {:?}", input);
                            assert_eq!(ra, rb, "reason mismatch for {:?}", input);
                        }
                        (other_a, other_b) => {
                            panic!(
                                "non-InvalidParameter errors for {:?}: {:?} / {:?}",
                                input, other_a, other_b
                            )
                        }
                    }
                }
                (a, b) => panic!("Ok/Err disagreement for {:?}: {:?} / {:?}", input, a, b),
            }
        }
    }

    // ── CaptureTarget: round-trip (Display ∘ FromStr == identity) ────

    #[test]
    fn capture_target_round_trip_all_variants() {
        let targets = [
            CaptureTarget::SystemDefault,
            CaptureTarget::Device(DeviceId("hw:0,0".to_string())),
            CaptureTarget::Device(DeviceId("simple-id".to_string())),
            CaptureTarget::Device(DeviceId(String::new())),
            CaptureTarget::Application(ApplicationId("Spotify".to_string())),
            CaptureTarget::Application(ApplicationId("com.example:session:1".to_string())),
            CaptureTarget::ApplicationByName("Firefox".to_string()),
            CaptureTarget::ApplicationByName("Name With Spaces".to_string()),
            CaptureTarget::ProcessTree(ProcessId(0)),
            CaptureTarget::ProcessTree(ProcessId(7)),
            CaptureTarget::ProcessTree(ProcessId(u32::MAX)),
        ];
        for t in targets {
            let rendered = t.to_string();
            let parsed = rendered.parse::<CaptureTarget>().unwrap_or_else(|e| {
                panic!(
                    "round-trip parse failed for {:?} -> {:?}: {}",
                    t, rendered, e
                )
            });
            assert_eq!(parsed, t, "round-trip mismatch via {:?}", rendered);
        }
    }

    #[test]
    fn capture_target_round_trip_canonical_forms() {
        // Exact canonical strings cited in the spec acceptance criteria.
        assert_eq!(
            CaptureTarget::ProcessTree(ProcessId(7)).to_string(),
            "tree:7"
        );
        assert_eq!(
            CaptureTarget::ApplicationByName("Firefox".to_string()).to_string(),
            "name:Firefox"
        );
        assert_eq!(
            "tree:7".parse::<CaptureTarget>().unwrap(),
            CaptureTarget::ProcessTree(ProcessId(7))
        );
        assert_eq!(
            "name:Firefox".parse::<CaptureTarget>().unwrap(),
            CaptureTarget::ApplicationByName("Firefox".to_string())
        );
    }

    // ── SampleFormat ─────────────────────────────────────────────────

    #[test]
    fn sample_format_default_is_f32() {
        assert_eq!(SampleFormat::default(), SampleFormat::F32);
    }

    #[test]
    fn sample_format_bits_per_sample() {
        assert_eq!(SampleFormat::I16.bits_per_sample(), 16);
        assert_eq!(SampleFormat::I24.bits_per_sample(), 24);
        assert_eq!(SampleFormat::I32.bits_per_sample(), 32);
        assert_eq!(SampleFormat::F32.bits_per_sample(), 32);
    }

    #[test]
    fn sample_format_copy() {
        let fmt = SampleFormat::I16;
        let copied = fmt; // Copy — not move
        assert_eq!(fmt, copied);
    }

    #[test]
    fn sample_format_all_variants_eq() {
        assert_eq!(SampleFormat::I16, SampleFormat::I16);
        assert_eq!(SampleFormat::I24, SampleFormat::I24);
        assert_eq!(SampleFormat::I32, SampleFormat::I32);
        assert_eq!(SampleFormat::F32, SampleFormat::F32);
        assert_ne!(SampleFormat::I16, SampleFormat::F32);
    }

    // ── AudioFormat ──────────────────────────────────────────────────

    #[test]
    fn audio_format_default() {
        let fmt = AudioFormat::default();
        assert_eq!(fmt.sample_rate, 48000);
        assert_eq!(fmt.channels, 2);
        assert_eq!(fmt.sample_format, SampleFormat::F32);
    }

    #[test]
    fn audio_format_custom_construction() {
        let fmt = AudioFormat {
            sample_rate: 44100,
            channels: 1,
            sample_format: SampleFormat::I16,
        };
        assert_eq!(fmt.sample_rate, 44100);
        assert_eq!(fmt.channels, 1);
        assert_eq!(fmt.sample_format, SampleFormat::I16);
    }

    #[test]
    fn audio_format_clone_and_eq() {
        let fmt = AudioFormat {
            sample_rate: 96000,
            channels: 8,
            sample_format: SampleFormat::I32,
        };
        let cloned = fmt.clone();
        assert_eq!(fmt, cloned);
    }

    // ── StreamConfig ─────────────────────────────────────────────────

    #[test]
    fn stream_config_default() {
        let cfg = StreamConfig::default();
        assert_eq!(cfg.sample_rate, 48000);
        assert_eq!(cfg.channels, 2);
        assert_eq!(cfg.sample_format, SampleFormat::F32);
        assert_eq!(cfg.buffer_size, None);
    }

    #[test]
    fn stream_config_to_audio_format() {
        let cfg = StreamConfig {
            sample_rate: 44100,
            channels: 6,
            sample_format: SampleFormat::I24,
            buffer_size: Some(1024),
            capture_target: CaptureTarget::SystemDefault,
        };
        let fmt = cfg.to_audio_format();
        assert_eq!(fmt.sample_rate, 44100);
        assert_eq!(fmt.channels, 6);
        assert_eq!(fmt.sample_format, SampleFormat::I24);
    }

    #[test]
    fn stream_config_to_audio_format_default_roundtrip() {
        let cfg = StreamConfig::default();
        let fmt = cfg.to_audio_format();
        assert_eq!(fmt, AudioFormat::default());
    }

    #[test]
    fn stream_config_custom_with_buffer_size() {
        let cfg = StreamConfig {
            sample_rate: 22050,
            channels: 1,
            sample_format: SampleFormat::I16,
            buffer_size: Some(512),
            capture_target: CaptureTarget::SystemDefault,
        };
        assert_eq!(cfg.buffer_size, Some(512));
    }

    // ── AudioCaptureConfig ───────────────────────────────────────────

    #[test]
    fn audio_capture_config_default() {
        let cfg = AudioCaptureConfig::default();
        assert_eq!(cfg.target, CaptureTarget::SystemDefault);
        assert_eq!(cfg.stream_config, StreamConfig::default());
    }

    #[test]
    fn audio_capture_config_custom() {
        let cfg = AudioCaptureConfig {
            target: CaptureTarget::ProcessTree(ProcessId(999)),
            stream_config: StreamConfig {
                sample_rate: 96000,
                channels: 2,
                sample_format: SampleFormat::F32,
                buffer_size: Some(2048),
                capture_target: CaptureTarget::ProcessTree(ProcessId(999)),
            },
        };
        assert_eq!(cfg.target, CaptureTarget::ProcessTree(ProcessId(999)));
        assert_eq!(cfg.stream_config.sample_rate, 96000);
        assert_eq!(cfg.stream_config.buffer_size, Some(2048));
    }

    #[test]
    fn audio_capture_config_clone_and_eq() {
        let cfg = AudioCaptureConfig {
            target: CaptureTarget::ApplicationByName("VLC".to_string()),
            stream_config: StreamConfig::default(),
        };
        let cloned = cfg.clone();
        assert_eq!(cfg, cloned);
    }

    // ── Edge cases ───────────────────────────────────────────────────

    #[test]
    fn device_id_empty_string() {
        let id = DeviceId(String::new());
        assert_eq!(id.to_string(), "");
    }

    #[test]
    fn process_id_zero() {
        let pid = ProcessId(0);
        assert_eq!(pid.to_string(), "0");
    }
}
