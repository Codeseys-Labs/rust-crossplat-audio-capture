// src/core/introspection.rs
//
//! System introspection helpers for audio source discovery.
//!
//! This module provides cross-platform utilities for discovering audio sources
//! (devices, applications, system default) without platform-specific `#[cfg]`
//! blocks in the calling code.
//!
//! # Separation of Concerns
//!
//! - **rsac core** (`api.rs`, `bridge/`, `audio/`) handles audio capture and stream delivery.
//! - **Introspection** (this module) handles system discovery — devices, processes, permissions.
//! - **Sinks** (`sink/`) handle downstream consumption of captured audio.
//!
//! These three concerns have clean interfaces between them.

use crate::core::config::{ApplicationId, CaptureTarget, DeviceId, ProcessId};
use crate::core::error::AudioResult;
use crate::core::interface::DeviceKind;
use std::time::Duration;

// ── AudioSource ─────────────────────────────────────────────────────────

/// Describes a capturable audio source discovered on the system.
///
/// This is a unified view combining devices, applications, and the system
/// default into a single enumerable type. Use [`list_audio_sources()`] to
/// discover all available sources.
#[derive(Debug, Clone)]
pub struct AudioSource {
    /// Unique identifier for this source (e.g., "system-default", "device:Built-in Output", "app:12345").
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// What kind of source this is.
    pub kind: AudioSourceKind,
}

/// Classification of an audio source.
///
/// # Stability
///
/// This enum is `#[non_exhaustive]`: new source classifications may be added in a
/// minor release. **Out-of-crate** code matching on `AudioSourceKind` must include
/// a trailing wildcard (`_ =>`) arm. The in-crate [`AudioSource::to_capture_target`]
/// match stays exhaustive so a new variant forces its capture-target mapping to be
/// defined.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AudioSourceKind {
    /// System default audio mix.
    SystemDefault,
    /// A specific audio device (input or output).
    ///
    /// `kind` is the device's endpoint direction when it could be resolved, or
    /// `None` when probing failed or is unsupported. It is `Option` because
    /// [`AudioDevice::kind`](crate::core::interface::AudioDevice::kind) is
    /// fallible (e.g. a backend that cannot determine the direction returns an
    /// error, which is mapped to `None` here via `.ok()`).
    Device {
        /// Platform device identifier, suitable for [`CaptureTarget::device`].
        device_id: String,
        /// Whether this is the platform's current default device.
        is_default: bool,
        /// Endpoint direction (input/output), or `None` when it could not be
        /// resolved (see the variant docs above).
        kind: Option<DeviceKind>,
    },
    /// An application producing audio.
    Application {
        /// OS process id of the application.
        pid: u32,
        /// Human-readable application name.
        app_name: String,
        /// Platform bundle/application identifier (e.g. a macOS bundle id),
        /// when the platform exposes one.
        bundle_id: Option<String>,
    },
}

impl AudioSource {
    /// Converts this source into a [`CaptureTarget`] suitable for `AudioCaptureBuilder`.
    ///
    /// A discovered [`AudioSourceKind::Application`] maps to
    /// [`CaptureTarget::Application`] — capturing **that single application's
    /// audio session**, not its descendants (L17). Selecting one discovered app
    /// from a source list and getting the whole process tree (children included)
    /// was surprising and over-broad; callers who specifically want the subtree
    /// can still construct [`CaptureTarget::pid`]/[`CaptureTarget::ProcessTree`]
    /// explicitly.
    ///
    /// The application's PID is carried as the [`ApplicationId`] string, which is
    /// how the platform backends parse it back into a numeric PID (see the
    /// `CaptureTarget::Application` arm of each backend's audio-client creation).
    pub fn to_capture_target(&self) -> CaptureTarget {
        match &self.kind {
            AudioSourceKind::SystemDefault => CaptureTarget::SystemDefault,
            AudioSourceKind::Device { device_id, .. } => {
                CaptureTarget::Device(DeviceId(device_id.clone()))
            }
            AudioSourceKind::Application { pid, .. } => {
                CaptureTarget::Application(ApplicationId(pid.to_string()))
            }
        }
    }
}

// ── Application enumeration scope (rsac-f547) ────────────────────────────

/// Whether an application enumeration is exactly the platform's audio producers,
/// or a superset because the audio-process filter was unavailable.
///
/// # Stability
/// `#[non_exhaustive]`: out-of-crate matches need a trailing `_ =>` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ApplicationScope {
    /// The list contains exactly the applications the platform reports as
    /// producing audio: Windows active sessions (`AudioSessionStateActive`),
    /// Linux PipeWire audio nodes, macOS 14.4+ CoreAudio audio-process objects.
    ExactAudioProducers,
    /// The audio-process filter was unavailable, so the list is the full set of
    /// running applications and may include silent ones. Currently reachable
    /// only on macOS < 14.4, or when the macOS CoreAudio process-object query is
    /// unavailable or reports no active PIDs (see the macOS backend fallback).
    AllRunningFallback,
    /// The backend enumeration itself failed (e.g. PipeWire unreachable, a
    /// WASAPI/CoreAudio query error), so the (empty) list is *incomplete* —
    /// not evidence that no applications are producing audio. Discovery stays
    /// best-effort (the error is swallowed), but scoped callers can distinguish
    /// "no producers found" from "could not look".
    EnumerationFailed,
}

/// Result of a scope-aware application enumeration: the sources plus whether the
/// list is exact or a fallback superset. Prefer this over
/// [`list_audio_applications`] when you must distinguish "these apps are
/// producing audio" from "here is every running app because filtering was
/// unavailable" (e.g. an app-picker UI, or a test discovering a capture target).
#[derive(Debug, Clone)]
pub struct ApplicationEnumeration {
    /// Discovered application sources (each `AudioSourceKind::Application`).
    pub applications: Vec<AudioSource>,
    /// Whether `applications` is exact or an unfiltered fallback superset.
    pub scope: ApplicationScope,
}

// ── Convenience constructors for CaptureTarget ──────────────────────────

impl CaptureTarget {
    /// Capture from an application by name (convenience constructor).
    ///
    /// Equivalent to `CaptureTarget::ApplicationByName(name.into())`.
    pub fn app(name: impl Into<String>) -> Self {
        CaptureTarget::ApplicationByName(name.into())
    }

    /// Capture from a process tree by PID (convenience constructor).
    ///
    /// Equivalent to `CaptureTarget::ProcessTree(ProcessId(pid))`.
    pub fn pid(pid: u32) -> Self {
        CaptureTarget::ProcessTree(ProcessId(pid))
    }

    /// Capture from a specific device by ID (convenience constructor).
    ///
    /// Equivalent to `CaptureTarget::Device(DeviceId(id.into()))`.
    pub fn device(id: impl Into<String>) -> Self {
        CaptureTarget::Device(DeviceId(id.into()))
    }
}

// ── Cross-platform audio source listing ─────────────────────────────────

/// Lists all capturable audio sources on the current platform.
///
/// Returns a unified list combining:
/// - System default audio
/// - All enumerated audio devices
/// - All applications currently producing audio (platform-specific)
///
/// This is the cross-platform equivalent of what `audio-graph` does with
/// per-platform `#[cfg]` blocks in its `list_sources()` method.
///
/// # Example
///
/// ```rust,no_run
/// use rsac::core::introspection::list_audio_sources;
///
/// let sources = list_audio_sources().unwrap();
/// for source in &sources {
///     println!("{}: {} ({:?})", source.id, source.name, source.kind);
/// }
/// ```
pub fn list_audio_sources() -> AudioResult<Vec<AudioSource>> {
    let mut sources = Vec::new();

    // 1. Always include system default
    sources.push(AudioSource {
        id: "system-default".to_string(),
        name: "System Default".to_string(),
        kind: AudioSourceKind::SystemDefault,
    });

    // 2. Enumerate devices
    match crate::audio::get_device_enumerator() {
        Ok(enumerator) => {
            if let Ok(devices) = enumerator.enumerate_devices() {
                for dev in &devices {
                    sources.push(AudioSource {
                        id: format!("device:{}", dev.id()),
                        name: dev.name(),
                        kind: AudioSourceKind::Device {
                            device_id: dev.id().to_string(),
                            is_default: dev.is_default(),
                            // Fallible probe → Option: None when the backend
                            // cannot resolve the endpoint direction (mx-a481).
                            kind: dev.kind().ok(),
                        },
                    });
                }
            }
        }
        Err(e) => {
            log::debug!("Device enumeration unavailable: {}", e);
        }
    }

    // 3. Enumerate applications (platform-specific). The system-source listing
    //    does not surface the enumeration scope, so discard it here.
    let _scope = list_audio_applications_scoped_into(&mut sources);

    Ok(sources)
}

/// Lists running applications that may be producing audio.
///
/// Returns application info in a cross-platform format. On platforms where
/// application enumeration is not supported, returns an empty list.
///
/// # Filtered vs. fallback
///
/// On macOS the returned list is normally *exactly* the applications CoreAudio
/// reports as producing audio (14.4+), but on macOS < 14.4 — or when the
/// CoreAudio process-object query is unavailable / reports no active PIDs — it
/// silently falls back to the full set of running applications (a superset that
/// may include silent ones). This function cannot distinguish the two modes;
/// use [`list_audio_applications_scoped`] when you must (e.g. an app-picker UI,
/// or a test discovering a real capture target). Windows and Linux always return
/// the exact audio-producer set.
pub fn list_audio_applications() -> AudioResult<Vec<AudioSource>> {
    Ok(list_audio_applications_scoped()?.applications)
}

/// Like [`list_audio_applications`], but also reports the enumeration
/// [`ApplicationScope`] so callers can tell an exact audio-producer list from an
/// unfiltered fallback superset.
///
/// On Windows and Linux the scope is always
/// [`ApplicationScope::ExactAudioProducers`]. On macOS it is
/// [`ApplicationScope::AllRunningFallback`] when the CoreAudio audio-process
/// filter was unavailable (macOS < 14.4, or the process-object query was
/// unavailable / found no active PIDs), and
/// [`ApplicationScope::ExactAudioProducers`] otherwise.
pub fn list_audio_applications_scoped() -> AudioResult<ApplicationEnumeration> {
    let mut applications = Vec::new();
    let scope = list_audio_applications_scoped_into(&mut applications);
    Ok(ApplicationEnumeration {
        applications,
        scope,
    })
}

/// Internal: appends audio applications to the provided vec and returns the
/// enumeration [`ApplicationScope`].
#[cfg(all(target_os = "macos", feature = "feat_macos"))]
fn list_audio_applications_scoped_into(sources: &mut Vec<AudioSource>) -> ApplicationScope {
    match crate::audio::macos::enumerate_audio_applications_scoped() {
        Ok((apps, is_fallback)) => {
            for app in apps {
                sources.push(AudioSource {
                    id: format!("app:{}", app.process_id),
                    name: app.name.clone(),
                    kind: AudioSourceKind::Application {
                        pid: app.process_id,
                        app_name: app.name,
                        bundle_id: app.bundle_id,
                    },
                });
            }
            if is_fallback {
                ApplicationScope::AllRunningFallback
            } else {
                ApplicationScope::ExactAudioProducers
            }
        }
        // Enumeration failed: push nothing and say so — an empty list from a
        // failed query is INCOMPLETE, not an exact "no producers" answer
        // (PR #59 review). Errors are swallowed to keep discovery best-effort.
        Err(_) => ApplicationScope::EnumerationFailed,
    }
}

#[cfg(all(target_os = "windows", feature = "feat_windows"))]
fn list_audio_applications_scoped_into(sources: &mut Vec<AudioSource>) -> ApplicationScope {
    // Windows filters to `AudioSessionStateActive`, so a successful query is
    // always the exact audio-producer set; a failed query is incomplete.
    match crate::audio::windows::enumerate_application_audio_sessions() {
        Ok(sessions) => {
            for session in sessions {
                sources.push(AudioSource {
                    id: format!("app:{}", session.process_id),
                    name: session.display_name.clone(),
                    kind: AudioSourceKind::Application {
                        pid: session.process_id,
                        app_name: session.display_name,
                        bundle_id: None,
                    },
                });
            }
            ApplicationScope::ExactAudioProducers
        }
        Err(_) => ApplicationScope::EnumerationFailed,
    }
}

#[cfg(all(target_os = "linux", feature = "feat_linux"))]
fn list_audio_applications_scoped_into(sources: &mut Vec<AudioSource>) -> ApplicationScope {
    // Linux PipeWire application discovery via the **native** in-process registry
    // (H4 part 2 / rsac-8ebb), reached through the `crate::audio` facade exactly
    // like the macOS / Windows arms above. This replaces the former `pw-dump`
    // subprocess + `serde_json` JSON parse on this `core` library path (rsac-3ff4);
    // the subprocess `pw-dump` JSON fallback now lives only in the platform
    // backend (`audio/linux/thread.rs`, for node-serial resolution), not here.
    //
    // Errors are swallowed (empty result) to match macOS/Windows: discovery is
    // best-effort and must never fail `list_audio_sources`. PID dedup is handled
    // natively (`PwAppSnapshot` is keyed by PID), but we still guard against an
    // id already present in `sources` for parity with the other backends.
    match crate::audio::linux::enumerate_audio_applications() {
        Ok(apps) => {
            for app in apps {
                let id = format!("app:{}", app.process_id);
                if sources.iter().any(|s| s.id == id) {
                    continue;
                }
                sources.push(AudioSource {
                    id,
                    name: app.name.clone(),
                    kind: AudioSourceKind::Application {
                        pid: app.process_id,
                        app_name: app.name,
                        bundle_id: None,
                    },
                });
            }
            // A successful native PipeWire audio-node snapshot is always exact.
            ApplicationScope::ExactAudioProducers
        }
        // PipeWire unreachable (no daemon/socket): the empty list is
        // incomplete, not a "no producers" answer (PR #59 review).
        Err(_) => ApplicationScope::EnumerationFailed,
    }
}

#[cfg(not(any(
    all(target_os = "macos", feature = "feat_macos"),
    all(target_os = "windows", feature = "feat_windows"),
    all(target_os = "linux", feature = "feat_linux"),
)))]
fn list_audio_applications_scoped_into(_sources: &mut Vec<AudioSource>) -> ApplicationScope {
    // Unsupported platform: the empty list is an exact (if trivial) audio-producer set.
    ApplicationScope::ExactAudioProducers
}

// ── Permission helpers ──────────────────────────────────────────────────

/// Describes the permission status for audio capture features.
///
/// # Stability
///
/// This enum is `#[non_exhaustive]`: new permission states may be added in a minor
/// release (e.g. a platform that distinguishes "restricted by policy" from
/// "denied"). **Out-of-crate** code matching on `PermissionStatus` must include a
/// trailing wildcard (`_ =>`) arm.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PermissionStatus {
    /// Permission is granted — the feature can be used.
    Granted,
    /// Permission has not been requested yet.
    NotDetermined,
    /// Permission was denied by the user or system policy.
    Denied,
    /// Permission is not applicable on this platform (always allowed).
    NotRequired,
}

/// Checks whether the current process has permission to capture audio.
///
/// On macOS 14.4+, Process Tap is gated by the **Audio Capture** TCC service
/// (`kTCCServiceAudioCapture`), which is a distinct, stricter TCC service from
/// Screen Recording (`kTCCServiceScreenCapture`). Apps declare the dependency
/// via `NSAudioCaptureUsageDescription` in Info.plist, and the OS prompts the
/// user on first Process Tap attempt.
///
/// On other platforms, audio capture typically doesn't require special
/// permissions, so this returns `PermissionStatus::NotRequired`.
///
/// ## macOS: real answer requires the `macos-tcc-spi` feature
///
/// There is **no public API** to query the `kTCCServiceAudioCapture` status
/// (`AVCaptureDevice.authorizationStatus(for: .audio)` reports the *microphone*
/// service, not the Process Tap). With the opt-in `macos-tcc-spi` feature
/// (ADR-0015), this preflights the private `TCCAccessPreflight` SPI and returns
/// a real `Granted`/`Denied`/`NotDetermined`. With the feature **off** (the
/// default), it honestly returns `NotDetermined` — matching `insidegui/AudioCap`'s
/// no-SPI build. Note the preflight is *advisory*: a `Granted` result does not
/// guarantee non-silent capture (a terminal-launched or unbundled process is
/// denied at runtime regardless of the TCC DB); the runtime silent-zeros guard
/// (ADR-0016) is the authoritative backstop.
pub fn check_audio_capture_permission() -> PermissionStatus {
    #[cfg(all(target_os = "macos", feature = "macos-tcc-spi"))]
    {
        // DAG note: this is the same documented core→audio deviation as the
        // discovery calls elsewhere in this file (see the allowlist in
        // scripts/check-module-dag.sh). ADR-0015.
        crate::audio::macos::permission::audio_capture_permission()
    }

    #[cfg(all(target_os = "macos", not(feature = "macos-tcc-spi")))]
    {
        // Process Tap / per-application capture requires the Audio Capture TCC
        // service (kTCCServiceAudioCapture), distinct from Screen Recording
        // (kTCCServiceScreenCapture). Without the `macos-tcc-spi` feature we
        // have no public way to query it, so we honestly report NotDetermined
        // (matching `insidegui/AudioCap`'s no-SPI build). Enable `macos-tcc-spi`
        // for a real preflight (ADR-0015).
        PermissionStatus::NotDetermined
    }

    #[cfg(not(target_os = "macos"))]
    {
        PermissionStatus::NotRequired
    }
}

// ── Stream statistics ───────────────────────────────────────────────────

/// Captures real-time statistics about a running audio stream.
///
/// Obtained via [`AudioCapture::stream_stats()`](crate::api::AudioCapture::stream_stats).
///
/// This is a cheap, point-in-time snapshot of the bridge's diagnostic counters
/// plus the running state, uptime, and negotiated format. The counters are read
/// with `Relaxed` loads on the (non-real-time) query path; reading them never
/// allocates on or blocks the OS audio callback thread.
///
/// This struct is `#[non_exhaustive]`: new diagnostic fields may be added in a
/// minor release, so construct it only via [`StreamStats::default()`] (it
/// implements [`Default`]) plus field assignment, and match it with `..`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct StreamStats {
    /// Number of audio buffers dropped due to ring buffer overflow.
    ///
    /// Equivalent to [`buffers_dropped`](Self::buffers_dropped); retained as a
    /// distinct field for backward compatibility with the original surface.
    pub overruns: u64,
    /// Cumulative number of buffers **delivered to the consumer** (popped off the
    /// ring buffer) since the stream started.
    pub buffers_captured: u64,
    /// Cumulative number of buffers **dropped due to ring buffer overflow** since
    /// the stream started (alias of [`overruns`](Self::overruns)).
    pub buffers_dropped: u64,
    /// Cumulative number of buffers **enqueued by the producer** (the OS audio
    /// callback) since the stream started.
    pub buffers_pushed: u64,
    /// How long the stream has been running. [`Duration::ZERO`] when the stream
    /// has not been started (or has been stopped).
    pub uptime: Duration,
    /// Whether the stream is currently capturing.
    pub is_running: bool,
    /// The audio format being captured.
    pub format_description: String,
}

impl StreamStats {
    /// Fraction of buffers lost to ring-buffer overflow, in `0.0..=1.0`.
    ///
    /// Computed as `buffers_dropped / (buffers_captured + buffers_dropped)`,
    /// i.e. the share of *accounted-for* buffers (delivered or dropped) that were
    /// lost. Returns `0.0` when the denominator is zero (no buffers yet), so it is
    /// safe to call on a freshly created or never-started capture.
    ///
    /// For example, three captured and one dropped yields `0.25`.
    pub fn dropped_ratio(&self) -> f64 {
        let denom = self.buffers_captured + self.buffers_dropped;
        if denom == 0 {
            return 0.0;
        }
        self.buffers_dropped as f64 / denom as f64
    }
}

// ── Backpressure reporting ───────────────────────────────────────────────

/// A windowed view of recent producer→consumer backpressure.
///
/// Obtained via
/// [`AudioCapture::backpressure_report()`](crate::api::AudioCapture::backpressure_report).
///
/// The legacy [`is_under_backpressure`](Self::is_under_backpressure) signal is an
/// all-or-nothing flag that trips only on a run of *consecutive* drops and resets
/// on any successful push, so it misses sustained partial loss (e.g. a steady
/// 1-in-3 drop pattern). This report carries the legacy bool unchanged **and**
/// adds a [`drop_rate`](Self::drop_rate) computed over a window of recent push
/// activity, which surfaces partial-loss patterns the bool cannot.
///
/// This struct is `#[non_exhaustive]`: prefer [`Default`] plus field assignment
/// and match with `..`.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct BackpressureReport {
    /// The wall-clock span the `pushed`/`dropped` tallies cover. [`Duration::ZERO`]
    /// when the implementation reports lifetime totals rather than a bounded
    /// window (the current bridge fallback — see the type-level note).
    pub window: Duration,
    /// Buffers successfully pushed by the producer within the window.
    pub pushed: u64,
    /// Buffers dropped due to ring-buffer overflow within the window.
    pub dropped: u64,
    /// Fraction of buffers lost within the window, in `0.0..=1.0`. Computed
    /// read-side as `dropped / (pushed + dropped)` with a zero-division guard
    /// (`0.0` when nothing has been pushed or dropped).
    pub drop_rate: f64,
    /// The legacy consecutive-drop backpressure flag, carried unchanged so callers
    /// that relied on it keep working. See
    /// `CapturingStream::is_under_backpressure`.
    pub is_under_backpressure: bool,
}

impl BackpressureReport {
    /// Builds a report from raw push/drop tallies and the legacy bool, computing
    /// [`drop_rate`](Self::drop_rate) with a zero-division guard.
    ///
    /// `window` is the span the tallies cover; pass [`Duration::ZERO`] when the
    /// counts are lifetime totals rather than a bounded window.
    ///
    /// `AudioCapture::backpressure_report` builds this from the bridge's
    /// fixed-size ring of per-window `(pushed, dropped)` snapshots (rsac-cfe4's
    /// alloc-free RT-side counters in `bridge/ring_buffer.rs`, read via
    /// `CapturingStream::drop_window_snapshot`), so the counts are a bounded
    /// recent window rather than lifetime totals. The `window` span is estimated
    /// read-side from the buffer size and negotiated rate, falling back to
    /// [`Duration::ZERO`] only when the span cannot be attributed.
    pub(crate) fn from_counts(
        window: Duration,
        pushed: u64,
        dropped: u64,
        is_under_backpressure: bool,
    ) -> Self {
        let denom = pushed + dropped;
        let drop_rate = if denom == 0 {
            0.0
        } else {
            dropped as f64 / denom as f64
        };
        Self {
            window,
            pushed,
            dropped,
            drop_rate,
            is_under_backpressure,
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capture_target_convenience_constructors() {
        let t = CaptureTarget::app("Firefox");
        assert_eq!(t, CaptureTarget::ApplicationByName("Firefox".to_string()));

        let t = CaptureTarget::pid(12345);
        assert_eq!(t, CaptureTarget::ProcessTree(ProcessId(12345)));

        let t = CaptureTarget::device("built-in-output");
        assert_eq!(
            t,
            CaptureTarget::Device(DeviceId("built-in-output".to_string()))
        );
    }

    #[test]
    fn test_audio_source_to_capture_target() {
        let source = AudioSource {
            id: "system-default".to_string(),
            name: "System Default".to_string(),
            kind: AudioSourceKind::SystemDefault,
        };
        assert_eq!(source.to_capture_target(), CaptureTarget::SystemDefault);

        // L17: a discovered single application maps to Application(pid),
        // capturing that app's session only — NOT ProcessTree (the whole
        // subtree). The PID is carried as the ApplicationId string.
        let source = AudioSource {
            id: "app:1234".to_string(),
            name: "Firefox".to_string(),
            kind: AudioSourceKind::Application {
                pid: 1234,
                app_name: "Firefox".to_string(),
                bundle_id: Some("org.mozilla.firefox".to_string()),
            },
        };
        assert_eq!(
            source.to_capture_target(),
            CaptureTarget::Application(ApplicationId("1234".to_string()))
        );
        // And explicitly NOT the over-broad process-tree capture.
        assert_ne!(
            source.to_capture_target(),
            CaptureTarget::ProcessTree(ProcessId(1234))
        );
    }

    /// The additive `kind` field on `AudioSourceKind::Device` does not affect
    /// `to_capture_target()` — the arm ignores it via `..` — and a `Device`
    /// source still maps to `CaptureTarget::Device`.
    #[test]
    fn device_source_with_kind_maps_to_capture_target() {
        let source = AudioSource {
            id: "device:speakers".to_string(),
            name: "Speakers".to_string(),
            kind: AudioSourceKind::Device {
                device_id: "speakers".to_string(),
                is_default: true,
                kind: Some(DeviceKind::Output),
            },
        };
        assert_eq!(
            source.to_capture_target(),
            CaptureTarget::Device(DeviceId("speakers".to_string()))
        );

        // A None kind (probe failed / unsupported) maps identically — the new
        // field is purely informational for to_capture_target().
        let unknown = AudioSource {
            id: "device:mic".to_string(),
            name: "Mic".to_string(),
            kind: AudioSourceKind::Device {
                device_id: "mic".to_string(),
                is_default: false,
                kind: None,
            },
        };
        assert_eq!(
            unknown.to_capture_target(),
            CaptureTarget::Device(DeviceId("mic".to_string()))
        );
    }

    /// The `kind` field round-trips through pattern matching, including the
    /// `None` (indeterminate) case.
    #[test]
    fn device_source_kind_field_round_trips() {
        let with_kind = AudioSourceKind::Device {
            device_id: "d0".to_string(),
            is_default: false,
            kind: Some(DeviceKind::Input),
        };
        match with_kind {
            AudioSourceKind::Device { kind, .. } => {
                assert_eq!(kind, Some(DeviceKind::Input));
            }
            other => panic!("expected Device, got {other:?}"),
        }
    }

    #[test]
    fn test_list_audio_sources_includes_system_default() {
        let sources = list_audio_sources().unwrap();
        assert!(sources
            .iter()
            .any(|s| s.kind == AudioSourceKind::SystemDefault));
        assert!(sources.iter().any(|s| s.id == "system-default"));
    }

    /// `list_audio_applications()` is best-effort: on every platform it must
    /// return `Ok` (an empty list when discovery is unsupported or the audio
    /// daemon is unavailable) and never panic. This exercises the Linux native
    /// PipeWire arm — which now reaches `crate::audio::linux::enumerate_audio_applications`
    /// instead of the former `pw-dump` + `serde_json` subprocess parse (rsac-3ff4)
    /// — in a headless / device-free CI run (it degrades to an empty list when
    /// PipeWire cannot connect), and the macOS / Windows / unsupported arms too.
    #[test]
    fn list_audio_applications_is_best_effort_and_well_formed() {
        let apps = list_audio_applications().expect("list_audio_applications is infallible");

        // Every discovered application source must be shaped as an
        // `app:<pid>`-prefixed `Application`, must carry the same PID in its id
        // and kind, and must be PID-deduplicated across the returned list.
        let mut seen_ids = std::collections::HashSet::new();
        for source in &apps {
            assert!(
                source.id.starts_with("app:"),
                "application source id must be `app:<pid>`-prefixed, got {:?}",
                source.id
            );
            match &source.kind {
                AudioSourceKind::Application { pid, .. } => {
                    assert_eq!(
                        source.id,
                        format!("app:{pid}"),
                        "id must encode the same PID as the kind"
                    );
                    assert!(
                        seen_ids.insert(source.id.clone()),
                        "application sources must be PID-deduplicated; saw {:?} twice",
                        source.id
                    );
                }
                other => {
                    panic!("list_audio_applications yielded a non-Application source: {other:?}")
                }
            }
        }
    }

    /// `list_audio_applications_scoped()` is best-effort and well-formed: it
    /// returns `Ok`, every source is an `Application`, and on the always-exact
    /// backends (Windows / Linux CI) the reported scope is
    /// `ExactAudioProducers`. Mirrors
    /// `list_audio_applications_is_best_effort_and_well_formed` (rsac-f547).
    #[test]
    fn list_audio_applications_scoped_is_well_formed() {
        let enumeration =
            list_audio_applications_scoped().expect("list_audio_applications_scoped is infallible");

        for source in &enumeration.applications {
            assert!(
                source.id.starts_with("app:"),
                "application source id must be `app:<pid>`-prefixed, got {:?}",
                source.id
            );
            match &source.kind {
                AudioSourceKind::Application { pid, .. } => {
                    assert_eq!(
                        source.id,
                        format!("app:{pid}"),
                        "id must encode the same PID as the kind"
                    );
                }
                other => panic!(
                    "list_audio_applications_scoped yielded a non-Application source: {other:?}"
                ),
            }
        }

        // Windows and Linux report the exact audio-producer set on success
        // (Windows filters to AudioSessionStateActive; Linux returns the native
        // PipeWire audio-node snapshot) — or EnumerationFailed when the backend
        // query itself fails (headless CI: no PipeWire daemon in the unit-test
        // job, no WASAPI session access). They can never report the macOS-only
        // AllRunningFallback superset.
        #[cfg(any(
            all(target_os = "windows", feature = "feat_windows"),
            all(target_os = "linux", feature = "feat_linux"),
        ))]
        assert_ne!(
            enumeration.scope,
            ApplicationScope::AllRunningFallback,
            "Windows/Linux enumeration is exact-on-success or EnumerationFailed — \
             never the unfiltered fallback superset"
        );
    }

    /// The unscoped `list_audio_applications()` is a loss-free projection of the
    /// scoped variant's `applications` — proves the delegation drops only the
    /// scope, never any source (rsac-f547).
    #[test]
    fn list_audio_applications_matches_scoped_applications() {
        let plain = list_audio_applications().expect("infallible");
        let scoped = list_audio_applications_scoped().expect("infallible");
        assert_eq!(plain.len(), scoped.applications.len());
        for (a, b) in plain.iter().zip(scoped.applications.iter()) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.name, b.name);
            assert_eq!(a.kind, b.kind);
        }
    }

    // ── StreamStats (rsac-4c07) ───────────────────────────────────────

    /// Default StreamStats has zeroed counters, ZERO uptime, not running, and a
    /// `dropped_ratio()` of 0.0 (divide-by-zero guard).
    #[test]
    fn stream_stats_default_is_zeroed_and_safe() {
        let s = StreamStats::default();
        assert_eq!(s.overruns, 0);
        assert_eq!(s.buffers_captured, 0);
        assert_eq!(s.buffers_dropped, 0);
        assert_eq!(s.buffers_pushed, 0);
        assert_eq!(s.uptime, std::time::Duration::ZERO);
        assert!(!s.is_running);
        assert!(s.format_description.is_empty());
        // No buffers accounted for → guard returns 0.0, no panic.
        assert_eq!(s.dropped_ratio(), 0.0);
    }

    /// dropped_ratio() = dropped / (captured + dropped); 3 captured + 1 dropped → 0.25.
    #[test]
    fn stream_stats_dropped_ratio_computes_quarter() {
        let s = StreamStats {
            buffers_captured: 3,
            buffers_dropped: 1,
            ..StreamStats::default()
        };
        assert_eq!(s.dropped_ratio(), 0.25);
    }

    /// dropped_ratio() guards a zero denominator even when other fields are set.
    #[test]
    fn stream_stats_dropped_ratio_zero_denominator_guard() {
        let s = StreamStats {
            buffers_pushed: 100, // pushed does not enter the denominator
            ..StreamStats::default()
        };
        assert_eq!(s.dropped_ratio(), 0.0);
    }

    // ── BackpressureReport (rsac-cfe4) ────────────────────────────────

    /// drop_rate is 0.0 when nothing has been pushed or dropped (guard).
    #[test]
    fn backpressure_report_default_drop_rate_is_zero() {
        let r = BackpressureReport::default();
        assert_eq!(r.pushed, 0);
        assert_eq!(r.dropped, 0);
        assert_eq!(r.drop_rate, 0.0);
        assert!(!r.is_under_backpressure);
        assert_eq!(r.window, std::time::Duration::ZERO);
    }

    /// from_counts() computes drop_rate = dropped / (pushed + dropped). A
    /// half-and-half loss pattern (which never trips the consecutive-drop bool)
    /// reports drop_rate ~0.5.
    #[test]
    fn backpressure_report_from_counts_half_loss() {
        // is_under_backpressure stays false: this models the interleaved
        // drop,push,drop,push pattern that resets consecutive_drops each push.
        let r = BackpressureReport::from_counts(std::time::Duration::ZERO, 2, 2, false);
        assert_eq!(r.pushed, 2);
        assert_eq!(r.dropped, 2);
        assert!(
            (r.drop_rate - 0.5).abs() < f64::EPSILON,
            "drop_rate should be ~0.5, got {}",
            r.drop_rate
        );
        assert!(
            !r.is_under_backpressure,
            "windowed report must surface partial loss the legacy bool misses"
        );
    }

    /// from_counts() guards a zero denominator.
    #[test]
    fn backpressure_report_from_counts_zero_denominator() {
        let r = BackpressureReport::from_counts(std::time::Duration::ZERO, 0, 0, false);
        assert_eq!(r.drop_rate, 0.0);
    }
}
