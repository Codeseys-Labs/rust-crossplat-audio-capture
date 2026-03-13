//! Test helpers for CI audio integration tests.
//!
//! Provides infrastructure detection, test tone generation, audio playback,
//! and verification utilities used across all test modules.

use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::Duration;

use rsac::AudioBuffer;

// ---------------------------------------------------------------------------
// Audio infrastructure detection
// ---------------------------------------------------------------------------

/// Check if audio infrastructure is available for testing.
///
/// Priority:
/// 1. If `RSAC_CI_AUDIO_AVAILABLE=1`, return true (CI explicitly set this)
/// 2. If `RSAC_CI_AUDIO_AVAILABLE=0`, return false
/// 3. Fall back to runtime detection:
///    - Linux: check PipeWire socket exists AND `pw-cli` responds
///    - Other platforms: check device enumeration succeeds
pub fn audio_infrastructure_available() -> bool {
    // Check env var first
    if let Ok(val) = std::env::var("RSAC_CI_AUDIO_AVAILABLE") {
        return val == "1";
    }

    // Runtime detection
    runtime_detect_audio()
}

fn runtime_detect_audio() -> bool {
    // On Linux: check PipeWire
    #[cfg(target_os = "linux")]
    {
        // Check XDG_RUNTIME_DIR/pipewire-0 socket
        if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
            let socket = std::path::Path::new(&xdg).join("pipewire-0");
            if !socket.exists() {
                eprintln!("[ci_audio] PipeWire socket not found at {:?}", socket);
                return false;
            }
        }

        // Try pw-cli info 0
        match Command::new("pw-cli").args(["info", "0"]).output() {
            Ok(output) => {
                if output.status.success() {
                    eprintln!("[ci_audio] PipeWire detected and responding");
                    true
                } else {
                    eprintln!(
                        "[ci_audio] pw-cli failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    false
                }
            }
            Err(e) => {
                eprintln!("[ci_audio] pw-cli not found: {}", e);
                false
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        // Try device enumeration as a proxy
        match rsac::get_device_enumerator() {
            Ok(enumerator) => match enumerator.enumerate_devices() {
                Ok(devices) => !devices.is_empty(),
                Err(_) => false,
            },
            Err(_) => false,
        }
    }
}

// ---------------------------------------------------------------------------
// require_audio!() macro — skips a test when no audio infra is present
// ---------------------------------------------------------------------------

/// Macro that skips the current test if audio infrastructure is not available.
/// Prints diagnostic info about why the skip happened.
macro_rules! require_audio {
    () => {
        if !$crate::helpers::audio_infrastructure_available() {
            eprintln!(
                "\n╔══════════════════════════════════════════════════════════╗"
            );
            eprintln!(
                "║  SKIPPING: Audio infrastructure not available           ║"
            );
            eprintln!(
                "║  Set RSAC_CI_AUDIO_AVAILABLE=1 to force audio tests     ║"
            );
            eprintln!(
                "╚══════════════════════════════════════════════════════════╝\n"
            );
            return;
        }
    };
}

// ---------------------------------------------------------------------------
// WAV test-tone generation
// ---------------------------------------------------------------------------

/// Generate a test tone WAV file (440 Hz sine wave).
/// Returns the path to the generated temporary WAV file.
pub fn generate_test_wav(duration_secs: f32, sample_rate: u32, channels: u16) -> PathBuf {
    let dir = tempfile::tempdir().expect("Failed to create temp dir");
    let kept_path = dir.keep();
    let path = kept_path.join("test_tone.wav");

    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };

    let mut writer = hound::WavWriter::create(&path, spec).expect("Failed to create WAV writer");

    let num_samples = (duration_secs * sample_rate as f32) as usize;
    let frequency = 440.0f32;

    for i in 0..num_samples {
        let t = i as f32 / sample_rate as f32;
        let sample = (2.0 * std::f32::consts::PI * frequency * t).sin() * 0.8;
        for _ in 0..channels {
            writer.write_sample(sample).expect("Failed to write sample");
        }
    }

    writer.finalize().expect("Failed to finalize WAV");
    path
}

// ---------------------------------------------------------------------------
// Audio playback helpers
// ---------------------------------------------------------------------------

/// Spawn a platform-specific audio player for the given WAV file.
/// Returns the child process handle so it can be stopped later.
pub fn spawn_test_tone_player(wav_path: &std::path::Path) -> Option<Child> {
    #[cfg(target_os = "linux")]
    {
        // Try pw-play first (PipeWire native), fall back to paplay
        let child = Command::new("pw-play")
            .arg(wav_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn();

        match child {
            Ok(c) => {
                eprintln!("[ci_audio] Started pw-play for {:?}", wav_path);
                Some(c)
            }
            Err(_) => {
                // Fall back to paplay
                match Command::new("paplay")
                    .arg(wav_path)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                {
                    Ok(c) => {
                        eprintln!("[ci_audio] Started paplay for {:?}", wav_path);
                        Some(c)
                    }
                    Err(e) => {
                        eprintln!("[ci_audio] Failed to start audio player: {}", e);
                        None
                    }
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        let path_str = wav_path.to_string_lossy();
        let child = Command::new("powershell")
            .args([
                "-Command",
                &format!("(New-Object Media.SoundPlayer '{}').PlaySync()", path_str),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn();

        match child {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("[ci_audio] Failed to start Windows audio player: {}", e);
                None
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        let child = Command::new("afplay")
            .arg(wav_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn();

        match child {
            Ok(c) => Some(c),
            Err(e) => {
                eprintln!("[ci_audio] Failed to start macOS audio player: {}", e);
                None
            }
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        eprintln!("[ci_audio] No audio player available for this platform");
        None
    }
}

// ---------------------------------------------------------------------------
// Audio verification helpers
// ---------------------------------------------------------------------------

/// Verify that captured audio is not all silence (zeros).
/// Returns `true` if any sample has absolute value above the threshold.
pub fn verify_non_silence(buffer: &AudioBuffer, threshold: f32) -> bool {
    let max_amplitude = buffer.data().iter().map(|s| s.abs()).fold(0.0f32, f32::max);

    eprintln!(
        "[ci_audio] Max amplitude: {:.6} (threshold: {:.6})",
        max_amplitude, threshold
    );

    max_amplitude > threshold
}

/// Calculate and verify RMS energy of the captured audio.
/// Returns `(rms_value, passes_threshold)`.
pub fn verify_rms_energy(buffer: &AudioBuffer, min_rms: f32) -> (f32, bool) {
    let data = buffer.data();
    if data.is_empty() {
        return (0.0, false);
    }

    let sum_sq: f32 = data.iter().map(|s| s * s).sum();
    let rms = (sum_sq / data.len() as f32).sqrt();

    eprintln!(
        "[ci_audio] RMS energy: {:.6} (min threshold: {:.6})",
        rms, min_rms
    );

    (rms, rms >= min_rms)
}

/// Verify the audio format matches expectations.
pub fn verify_format(
    buffer: &AudioBuffer,
    expected_sample_rate: u32,
    expected_channels: u16,
) -> bool {
    let format = buffer.format();
    let sr_ok = format.sample_rate == expected_sample_rate;
    let ch_ok = format.channels == expected_channels;

    if !sr_ok {
        eprintln!(
            "[ci_audio] Sample rate mismatch: expected {}, got {}",
            expected_sample_rate, format.sample_rate
        );
    }
    if !ch_ok {
        eprintln!(
            "[ci_audio] Channel count mismatch: expected {}, got {}",
            expected_channels, format.channels
        );
    }

    sr_ok && ch_ok
}

// ---------------------------------------------------------------------------
// Timeout / cleanup helpers
// ---------------------------------------------------------------------------

/// Get the capture timeout duration from environment or default.
/// Reads `RSAC_TEST_CAPTURE_TIMEOUT_SECS` (default: 10).
pub fn capture_timeout() -> Duration {
    let secs: u64 = std::env::var("RSAC_TEST_CAPTURE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    Duration::from_secs(secs)
}

/// Helper to clean up a player child process.
pub fn stop_player(mut player: Child) {
    let _ = player.kill();
    let _ = player.wait();
}

// ---------------------------------------------------------------------------
// require_app_capture!() macro — skips when app capture is unsupported
// ---------------------------------------------------------------------------

/// Macro that skips the current test if application capture is not supported.
/// First checks audio infrastructure availability, then platform capabilities.
macro_rules! require_app_capture {
    () => {
        require_audio!();
        let caps = rsac::PlatformCapabilities::query();
        if !caps.supports_application_capture {
            eprintln!(
                "\n╔══════════════════════════════════════════════════════════╗"
            );
            eprintln!(
                "║  SKIPPING: App capture not supported on this platform   ║"
            );
            eprintln!(
                "╚══════════════════════════════════════════════════════════╝\n"
            );
            return;
        }
    };
}

// ---------------------------------------------------------------------------
// Application capture helpers
// ---------------------------------------------------------------------------

/// Spawn a platform-specific audio player and return the child process + its PID.
/// Unlike `spawn_test_tone_player`, this always returns the PID for use with
/// `CaptureTarget::ProcessTree` or PipeWire node discovery.
pub fn spawn_audio_player_get_pid(wav_path: &std::path::Path) -> Result<(Child, u32), String> {
    #[cfg(target_os = "linux")]
    {
        let child = Command::new("pw-play")
            .arg(wav_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .or_else(|_| {
                // Fall back to paplay
                Command::new("paplay")
                    .arg(wav_path)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
            })
            .map_err(|e| format!("Failed to spawn audio player: {e}"))?;

        let pid = child.id();
        eprintln!(
            "[ci_audio] Started audio player PID={pid} for {:?}",
            wav_path
        );
        Ok((child, pid))
    }

    #[cfg(target_os = "windows")]
    {
        let path_str = wav_path.to_string_lossy();
        let child = Command::new("powershell")
            .args([
                "-Command",
                &format!("(New-Object Media.SoundPlayer '{}').PlaySync()", path_str),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn Windows audio player: {e}"))?;

        let pid = child.id();
        eprintln!("[ci_audio] Started Windows audio player PID={pid}");
        Ok((child, pid))
    }

    #[cfg(target_os = "macos")]
    {
        let child = Command::new("afplay")
            .arg(wav_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn macOS audio player: {e}"))?;

        let pid = child.id();
        eprintln!("[ci_audio] Started macOS audio player PID={pid}");
        Ok((child, pid))
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        let _ = wav_path;
        Err("No audio player available for this platform".to_string())
    }
}

/// Discover the PipeWire node ID for a given process PID.
///
/// Runs `pw-dump` and parses the JSON output to find a node whose
/// `application.process.id` property matches the given PID.
/// Returns the node's `id` field as a `String`, or `None` if not found.
#[cfg(target_os = "linux")]
pub fn find_pipewire_node_for_pid(pid: u32) -> Option<String> {
    let output = Command::new("pw-dump").output().ok()?;

    if !output.status.success() {
        eprintln!(
            "[ci_audio] pw-dump failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse the JSON array from pw-dump
    let json: serde_json::Value = serde_json::from_str(&stdout).ok()?;
    let arr = json.as_array()?;

    let pid_str = pid.to_string();

    for obj in arr {
        // Check that this is a Node type
        let obj_type = obj.get("type")?.as_str()?;
        if obj_type != "PipeWire:Interface:Node" {
            continue;
        }

        // Look for application.process.id in info.props
        let props = obj.get("info").and_then(|i| i.get("props"));

        if let Some(props) = props {
            let app_pid = props.get("application.process.id").and_then(|v| v.as_str());

            if app_pid == Some(&pid_str) {
                // Return the node's id
                let node_id = obj.get("id")?.as_u64()?;
                eprintln!("[ci_audio] Found PipeWire node {} for PID {}", node_id, pid);
                return Some(node_id.to_string());
            }
        }
    }

    eprintln!("[ci_audio] No PipeWire node found for PID {}", pid);
    None
}
