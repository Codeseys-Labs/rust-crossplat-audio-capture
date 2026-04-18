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

use crate::core::config::{CaptureTarget, DeviceId, ProcessId};
use crate::core::error::AudioResult;

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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioSourceKind {
    /// System default audio mix.
    SystemDefault,
    /// A specific audio device (input or output).
    Device { device_id: String, is_default: bool },
    /// An application producing audio.
    Application {
        pid: u32,
        app_name: String,
        bundle_id: Option<String>,
    },
}

impl AudioSource {
    /// Converts this source into a [`CaptureTarget`] suitable for `AudioCaptureBuilder`.
    pub fn to_capture_target(&self) -> CaptureTarget {
        match &self.kind {
            AudioSourceKind::SystemDefault => CaptureTarget::SystemDefault,
            AudioSourceKind::Device { device_id, .. } => {
                CaptureTarget::Device(DeviceId(device_id.clone()))
            }
            AudioSourceKind::Application { pid, .. } => CaptureTarget::ProcessTree(ProcessId(*pid)),
        }
    }
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
                        },
                    });
                }
            }
        }
        Err(e) => {
            log::debug!("Device enumeration unavailable: {}", e);
        }
    }

    // 3. Enumerate applications (platform-specific)
    list_audio_applications_into(&mut sources);

    Ok(sources)
}

/// Lists running applications that may be producing audio.
///
/// Returns application info in a cross-platform format. On platforms where
/// application enumeration is not supported, returns an empty list.
pub fn list_audio_applications() -> AudioResult<Vec<AudioSource>> {
    let mut sources = Vec::new();
    list_audio_applications_into(&mut sources);
    Ok(sources)
}

/// Internal: appends audio applications to the provided vec.
#[cfg(all(target_os = "macos", feature = "feat_macos"))]
fn list_audio_applications_into(sources: &mut Vec<AudioSource>) {
    if let Ok(apps) = crate::audio::macos::enumerate_audio_applications() {
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
    }
}

#[cfg(all(target_os = "windows", feature = "feat_windows"))]
fn list_audio_applications_into(sources: &mut Vec<AudioSource>) {
    if let Ok(sessions) = crate::audio::windows::enumerate_application_audio_sessions() {
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
    }
}

#[cfg(all(target_os = "linux", feature = "feat_linux"))]
fn list_audio_applications_into(sources: &mut Vec<AudioSource>) {
    // Linux PipeWire application discovery via pw-dump
    if let Ok(output) = std::process::Command::new("pw-dump").output() {
        if let Ok(json_str) = String::from_utf8(output.stdout) {
            if let Ok(nodes) = serde_json::from_str::<Vec<serde_json::Value>>(&json_str) {
                for node in &nodes {
                    if node.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node")
                    {
                        continue;
                    }
                    let info = match node.get("info") {
                        Some(i) => i,
                        None => continue,
                    };
                    let props = match info.get("props") {
                        Some(p) => p,
                        None => continue,
                    };
                    // Only include audio stream nodes (not device nodes)
                    let media_class = props
                        .get("media.class")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if !media_class.contains("Stream") {
                        continue;
                    }
                    let app_name = props
                        .get("application.name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Unknown");
                    let pid = props
                        .get("application.process.id")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<u32>().ok())
                        .unwrap_or(0);

                    if pid == 0 {
                        continue;
                    }

                    // Deduplicate by PID
                    let id = format!("app:{}", pid);
                    if sources.iter().any(|s| s.id == id) {
                        continue;
                    }

                    sources.push(AudioSource {
                        id,
                        name: app_name.to_string(),
                        kind: AudioSourceKind::Application {
                            pid,
                            app_name: app_name.to_string(),
                            bundle_id: None,
                        },
                    });
                }
            }
        }
    }
}

#[cfg(not(any(
    all(target_os = "macos", feature = "feat_macos"),
    all(target_os = "windows", feature = "feat_windows"),
    all(target_os = "linux", feature = "feat_linux"),
)))]
fn list_audio_applications_into(_sources: &mut Vec<AudioSource>) {}

// ── Permission helpers ──────────────────────────────────────────────────

/// Describes the permission status for audio capture features.
#[derive(Debug, Clone, PartialEq, Eq)]
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
/// On macOS, this checks the Screen Recording TCC permission (required for Process Tap).
/// On other platforms, audio capture typically doesn't require special permissions,
/// so this returns `PermissionStatus::NotRequired`.
pub fn check_audio_capture_permission() -> PermissionStatus {
    #[cfg(target_os = "macos")]
    {
        // On macOS, system audio capture via Process Tap requires Screen Recording permission.
        // We can check this by attempting to access CGWindowListCopyWindowInfo.
        // For simplicity, we report NotDetermined — the OS will prompt on first use.
        // A more sophisticated check would use the CGPreflightScreenCaptureAccess API (macOS 15+).
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
/// Obtained via `AudioCapture::stream_stats()`.
#[derive(Debug, Clone, Default)]
pub struct StreamStats {
    /// Number of audio buffers dropped due to ring buffer overflow.
    pub overruns: u64,
    /// Whether the stream is currently capturing.
    pub is_running: bool,
    /// The audio format being captured.
    pub format_description: String,
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
            CaptureTarget::ProcessTree(ProcessId(1234))
        );
    }

    #[test]
    fn test_list_audio_sources_includes_system_default() {
        let sources = list_audio_sources().unwrap();
        assert!(sources
            .iter()
            .any(|s| s.kind == AudioSourceKind::SystemDefault));
        assert!(sources.iter().any(|s| s.id == "system-default"));
    }
}
