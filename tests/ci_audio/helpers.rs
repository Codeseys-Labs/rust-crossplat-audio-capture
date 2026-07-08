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
    // Every test funnels through here (the require_* macros), so it doubles
    // as the logging chokepoint: install the env_logger backend once so
    // RUST_LOG=rsac=debug (set job-wide in CI) actually emits the library's
    // debug lines into the --nocapture output. Without a backend the CI env
    // var was a silent no-op, which made backend-timing regressions (e.g.
    // the rsac-b106 evidence loop's StopCapture-latency question)
    // undebuggable from CI logs.
    init_test_logging();

    // Check env var first
    if let Ok(val) = std::env::var("RSAC_CI_AUDIO_AVAILABLE") {
        return val == "1";
    }

    // Runtime detection
    runtime_detect_audio()
}

/// Installs the test-side `env_logger` backend exactly once (idempotent and
/// race-free across the harness's test threads). Timestamped with
/// microseconds so CI log timelines can be correlated with the workflow's
/// own timestamps.
pub fn init_test_logging() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let _ = env_logger::Builder::from_default_env()
            .format_timestamp_micros()
            .try_init();
    });
}
/// Whether the test environment provides a *deterministic* audio source.
///
/// Set by CI (`RSAC_CI_AUDIO_DETERMINISTIC=1`) only for capture tiers where the
/// audio path is fully reproducible. Today that is Windows system capture, where
/// VB-CABLE is verified as the active default playback endpoint before tests
/// run. When this returns `true`, capture tests upgrade their soft non-silence
/// checks into HARD ASSERTS — a deterministic source that yields silence is a
/// real regression, not flakiness.
///
/// When unset (Linux Firecracker PipeWire routing, Windows device/process tiers,
/// or macOS BlackHole/TCC), tests keep the soft-warn behavior so they do not
/// flake on non-reproducible hosts.
pub fn deterministic_audio_env() -> bool {
    matches!(
        std::env::var("RSAC_CI_AUDIO_DETERMINISTIC").as_deref(),
        Ok("1")
    )
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

/// Generate a test tone WAV file (440 Hz sine wave) in 32-bit float PCM.
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

/// Generate a 16-bit PCM sibling WAV next to the given float WAV.
///
/// Windows' built-in `PlaySound` / `System.Media.SoundPlayer` only reliably
/// plays WAVE_FORMAT_PCM (integer) files. 32-bit IEEE-float PCM
/// (WAVE_FORMAT_IEEE_FLOAT) frequently fails to play through the default
/// endpoint on the `windows-latest` runner, which is the root cause of
/// rsac#24: the test tone never reaches VB-CABLE's loopback, so system
/// capture sees 0 buffers.
///
/// Writes a `<stem>_pcm16.wav` sibling in the same directory as `float_wav`.
/// Duration, sample rate, and channel count match the float WAV's shape
/// (440 Hz sine tone).
#[cfg(target_os = "windows")]
fn generate_pcm16_sibling(
    float_wav: &std::path::Path,
    duration_secs: f32,
    sample_rate: u32,
    channels: u16,
) -> PathBuf {
    let parent = float_wav
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let stem = float_wav
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("test_tone");
    let path = parent.join(format!("{stem}_pcm16.wav"));

    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = hound::WavWriter::create(&path, spec).expect("Failed to create WAV writer");

    let num_samples = (duration_secs * sample_rate as f32) as usize;
    let frequency = 440.0f32;
    // Scale 0.8 amplitude into i16 range to match the float version's energy.
    let peak = 0.8 * i16::MAX as f32;

    for i in 0..num_samples {
        let t = i as f32 / sample_rate as f32;
        let sample_f = (2.0 * std::f32::consts::PI * frequency * t).sin();
        let sample_i = (sample_f * peak) as i16;
        for _ in 0..channels {
            writer
                .write_sample(sample_i)
                .expect("Failed to write sample");
        }
    }

    writer.finalize().expect("Failed to finalize WAV");
    path
}

// ---------------------------------------------------------------------------
// Audio playback helpers
// ---------------------------------------------------------------------------

/// Warm-up + early-exit guard for a freshly-spawned audio player.
///
/// Sleeps briefly to let the player begin streaming, then calls
/// `child.try_wait()`. If the player has ALREADY exited with a non-success
/// status, that means the source died before producing audio — capturing
/// from a dead source would yield silence and make "capture is broken" look
/// identical to "the test tone never played". To prevent that false signal,
/// we drain the child's stderr and HARD PANIC with the diagnostic.
///
/// If the child is still running (the normal case) we return immediately,
/// leaving the handle untouched for the caller to manage. A successful early
/// exit (status 0) is tolerated — some one-shot players legitimately finish a
/// short clip — and is left for the caller's read loop / timeout to handle.
fn warmup_and_guard_player(child: &mut Child, label: &str) {
    // Brief warm-up: long enough for a failing player to surface an error,
    // short relative to capture timeouts so we don't eat the test budget.
    std::thread::sleep(Duration::from_millis(300));

    match child.try_wait() {
        Ok(Some(status)) if !status.success() => {
            // Player already died with an error — capture would see silence.
            // Surface the real cause rather than masquerading as "capture broken".
            let mut stderr_text = String::new();
            if let Some(mut err) = child.stderr.take() {
                use std::io::Read;
                let _ = err.read_to_string(&mut stderr_text);
            }
            panic!(
                "[ci_audio] {label} audio player exited early with {status} before \
                 producing audio. This is a SOURCE failure, not a capture failure. \
                 stderr:\n{stderr_text}"
            );
        }
        Ok(Some(status)) => {
            // Exited cleanly during warm-up (short clip). Unusual for our
            // 5s tones, but not an error — let the caller's loop handle it.
            eprintln!(
                "[ci_audio] {label} audio player exited during warm-up with {status} \
                 (clean) — continuing"
            );
        }
        Ok(None) => {
            // Still running — the expected healthy path.
        }
        Err(e) => {
            eprintln!("[ci_audio] {label} try_wait() failed (non-fatal): {e}");
        }
    }
}

/// Spawn a platform-specific audio player for the given WAV file.
/// Returns the child process handle so it can be stopped later.
pub fn spawn_test_tone_player(wav_path: &std::path::Path) -> Option<Child> {
    #[cfg(target_os = "linux")]
    {
        // Player preference (rsac-b106): when the CI routing gate has pinned
        // an explicit sink via PULSE_SINK, prefer paplay — the Pulse layer
        // honors PULSE_SINK, giving a deterministic route regardless of the
        // PipeWire default-metadata state (which is not settable on the
        // dbus-less Firecracker runners). Without PULSE_SINK, prefer pw-play
        // (PipeWire-native default routing) and fall back to paplay.
        let pulse_sink_pinned = std::env::var_os("PULSE_SINK").is_some();
        let order: [&str; 2] = if pulse_sink_pinned {
            ["paplay", "pw-play"]
        } else {
            ["pw-play", "paplay"]
        };
        for player in order {
            match Command::new(player)
                .arg(wav_path)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
            {
                Ok(mut c) => {
                    eprintln!(
                        "[ci_audio] Started {player} for {:?} (PULSE_SINK pinned: {pulse_sink_pinned})",
                        wav_path
                    );
                    warmup_and_guard_player(&mut c, player);
                    return Some(c);
                }
                Err(e) => {
                    eprintln!("[ci_audio] Failed to start {player}: {e}; trying next player");
                }
            }
        }
        eprintln!("[ci_audio] Failed to start any audio player");
        None
    }

    #[cfg(target_os = "windows")]
    {
        // SoundPlayer.PlaySync on a 32-bit float WAV is unreliable on
        // windows-latest runners: the PlaySound-backed winmm path often
        // silently drops WAVE_FORMAT_IEEE_FLOAT frames, so the tone never
        // reaches the default endpoint (VB-CABLE) and loopback capture
        // sees 0 buffers (rsac#24). Work around by generating a 16-bit
        // PCM sibling WAV and using PlayLooping — WAVE_FORMAT_PCM is the
        // format PlaySound reliably handles on every Windows build.
        let pcm16_path = generate_pcm16_sibling(wav_path, 5.0, 48000, 2);
        let path_str = pcm16_path.to_string_lossy();
        // PlayLooping runs asynchronously; keep the PowerShell host alive
        // for 30s (longer than any single capture test's timeout) so the
        // tone is always audible for the duration of the test.
        let script = format!(
            "$p = New-Object System.Media.SoundPlayer '{}'; $p.PlayLooping(); Start-Sleep -Seconds 30; $p.Stop()",
            path_str
        );
        let child = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn();

        match child {
            Ok(mut c) => {
                eprintln!(
                    "[ci_audio] Started Windows PlayLooping for {:?}",
                    pcm16_path
                );
                warmup_and_guard_player(&mut c, "Windows PlayLooping");
                Some(c)
            }
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
            Ok(mut c) => {
                warmup_and_guard_player(&mut c, "afplay");
                Some(c)
            }
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

/// Verify that a tone at `target_hz` dominates the captured audio.
///
/// Uses the Goertzel algorithm — an efficient single-bin DFT — to measure the
/// energy at `target_hz` relative to the *average* energy across a spread of
/// reference frequencies. The deterministic CI source is a 440 Hz sine tone
/// (see [`generate_test_wav`]); if capture is working, the 440 Hz bin should
/// stand far above the noise floor of unrelated bins.
///
/// Algorithm:
/// 1. Deinterleave the first channel (samples are interleaved by
///    `buffer.channels()`).
/// 2. Run Goertzel at the target frequency and at a set of off-target
///    reference frequencies (sub-harmonics / unrelated bins).
/// 3. Return `true` iff the target-bin power exceeds the dominance threshold
///    times the mean off-target power.
///
/// Threshold: `DOMINANCE = 8.0`. A clean sine tone produces a target-bin
/// power orders of magnitude above unrelated bins, so 8× is comfortably
/// above ambient numerical leakage while still tolerating real-world spectral
/// smearing from short capture windows and resampling. Returns `false` for
/// empty buffers or a zero sample rate (cannot compute a frequency bin).
pub fn verify_tone_present(buffer: &AudioBuffer, target_hz: f32) -> bool {
    /// Target-bin power must exceed this multiple of the mean off-target
    /// (reference) bin power for the tone to count as "present".
    const DOMINANCE: f32 = 8.0;

    let channels = buffer.channels().max(1) as usize;
    let sample_rate = buffer.sample_rate() as f32;
    let interleaved = buffer.data();

    if interleaved.is_empty() || sample_rate <= 0.0 {
        eprintln!("[ci_audio] verify_tone_present: empty buffer or zero sample rate");
        return false;
    }

    // Deinterleave channel 0.
    let mono: Vec<f32> = interleaved.iter().step_by(channels).copied().collect();
    let n = mono.len();
    if n == 0 {
        return false;
    }

    // Goertzel single-bin power for an arbitrary (non-integer-bin) frequency.
    let goertzel_power = |freq: f32| -> f32 {
        let omega = 2.0 * std::f32::consts::PI * freq / sample_rate;
        let coeff = 2.0 * omega.cos();
        let mut s_prev = 0.0f32;
        let mut s_prev2 = 0.0f32;
        for &x in &mono {
            let s = x + coeff * s_prev - s_prev2;
            s_prev2 = s_prev;
            s_prev = s;
        }
        // Power = |X(freq)|^2, normalized by sample count so it is comparable
        // across buffers of different lengths.
        let power = s_prev2 * s_prev2 + s_prev * s_prev - coeff * s_prev * s_prev2;
        power / n as f32
    };

    let target_power = goertzel_power(target_hz);

    // Off-target reference bins, kept clear of the target and its first
    // harmonic to avoid spectral-leakage contamination. All within Nyquist
    // for the deterministic 48 kHz fixture.
    let nyquist = sample_rate / 2.0;
    let refs: [f32; 4] = [
        target_hz * 0.5,  // 220 Hz
        target_hz * 1.5,  // 660 Hz
        target_hz * 2.5,  // 1100 Hz
        target_hz * 3.25, // 1430 Hz
    ];
    let mut ref_sum = 0.0f32;
    let mut ref_count = 0.0f32;
    for &f in &refs {
        if f > 0.0 && f < nyquist {
            ref_sum += goertzel_power(f);
            ref_count += 1.0;
        }
    }
    let ref_mean = if ref_count > 0.0 {
        ref_sum / ref_count
    } else {
        0.0
    };

    let dominates = target_power > DOMINANCE * ref_mean && target_power > 0.0;

    eprintln!(
        "[ci_audio] verify_tone_present: target {:.0}Hz power={:.6}, ref_mean={:.6}, \
         ratio={:.2} (threshold {:.1}x) → {}",
        target_hz,
        target_power,
        ref_mean,
        if ref_mean > 0.0 {
            target_power / ref_mean
        } else {
            f32::INFINITY
        },
        DOMINANCE,
        dominates
    );

    dominates
}

/// Assert the invariants a delivered `AudioBuffer` must satisfy, choosing the
/// right strength for the host.
///
/// rsac is **capture-only with no resampling** (see `VISION.md`): the builder's
/// `sample_rate`/`channels` are a *request*, but a shared-mode backend (WASAPI
/// shared, PipeWire, the CoreAudio process tap) delivers the device's
/// **negotiated mix format**, which may differ (e.g. a 96 kHz / 8-channel HDMI
/// or pro interface). So two tiers of invariant apply:
///
/// * **Always** (every host): the buffer is *self-consistent* — positive rate
///   and channel count, and interleaved `data.len() == num_frames * channels`.
///   This catches genuine silent-wrong-output regressions (bogus-but-well-formed
///   buffers) on any hardware.
/// * **Deterministic source only** (`RSAC_CI_AUDIO_DETERMINISTIC=1`, the Linux
///   null sink / Windows VB-CABLE runner, both pinned to the requested format):
///   the delivered format must *equal* the requested `expected_*`. On an
///   arbitrary developer host the device picks the format, so equality is not a
///   valid invariant — asserting it there is the bug this helper replaces.
///
/// Note we intentionally do **not** assert the buffer equals `capture.format()`:
/// the negotiated format is not always recorded on the consumer side yet (the
/// `set_negotiated_format` limitation noted in `docs/CROSS_LANGUAGE_BINDINGS.md`
/// / `PERFORMANCE.md`), so `format()` can still echo the request while the buffer
/// carries the real delivered rate.
pub fn assert_buffer_format(
    buffer: &AudioBuffer,
    expected_sample_rate: u32,
    expected_channels: u16,
) {
    // Tier 1 — self-consistency, enforced on every host.
    assert!(
        buffer.sample_rate() > 0,
        "delivered buffer must have a positive sample rate, got {}",
        buffer.sample_rate()
    );
    assert!(
        buffer.channels() > 0,
        "delivered buffer must have a positive channel count, got {}",
        buffer.channels()
    );
    assert_eq!(
        buffer.num_frames() * buffer.channels() as usize,
        buffer.data().len(),
        "interleaved data length must equal num_frames * channels \
         (rate={}, channels={}, frames={}, data.len={})",
        buffer.sample_rate(),
        buffer.channels(),
        buffer.num_frames(),
        buffer.data().len()
    );

    // Tier 2 — exact match, only where the source format is controlled.
    if deterministic_audio_env() {
        assert_eq!(
            buffer.sample_rate(),
            expected_sample_rate,
            "deterministic source: delivered sample_rate must equal the configured \
             request (the null sink / VB-CABLE is pinned to it)"
        );
        assert_eq!(
            buffer.channels(),
            expected_channels,
            "deterministic source: delivered channels must equal the configured request"
        );
    } else if buffer.sample_rate() != expected_sample_rate || buffer.channels() != expected_channels
    {
        // Non-deterministic host: divergence is expected (no resampling), just
        // record it so a surprising format is still visible in the test log.
        eprintln!(
            "[ci_audio] note: delivered format {}Hz/{}ch differs from requested \
             {}Hz/{}ch — expected on a host whose device negotiates its own mix \
             format (rsac does not resample)",
            buffer.sample_rate(),
            buffer.channels(),
            expected_sample_rate,
            expected_channels
        );
    }
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
// macOS TCC gate — Process Tap / Application capture require Audio Capture
// permission (TCC, kTCCServiceAudioCapture) that cannot be granted
// non-interactively on headless managed runners (Blacksmith, GH-hosted).
// Note: this is NOT the same as Screen Recording (kTCCServiceScreenCapture),
// which GH-hosted runners DO pre-grant to /bin/bash. Audio Capture is a
// separate, stricter TCC service that is NOT pre-granted anywhere.
// Without TCC, CoreAudio's AudioHardwareCreateProcessTap can block for 10+
// minutes before returning an error, eating the full job timeout. Tests that
// drive Process Tap must gate on this env var on macOS.
// Reference: insidegui/AudioCap uses NSAudioCaptureUsageDescription Info.plist
// key + requests kTCCServiceAudioCapture via the standard system prompt.
// ---------------------------------------------------------------------------

/// Check whether the macOS TCC Audio Capture permission is granted for the
/// test runner. On non-macOS platforms this is always true (no TCC gate).
///
/// On macOS, returns true iff `RSAC_CI_MACOS_TCC_GRANTED=1` is set. CI
/// environments that cannot grant TCC (Blacksmith, GH-hosted) must leave this
/// unset so Process Tap tests skip cleanly instead of hanging.
pub fn macos_tcc_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        matches!(
            std::env::var("RSAC_CI_MACOS_TCC_GRANTED").as_deref(),
            Ok("1")
        )
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

// ---------------------------------------------------------------------------
// require_system_capture!() macro — skips when SystemDefault needs TCC
// ---------------------------------------------------------------------------

/// Macro that skips the current test if `CaptureTarget::SystemDefault` cannot
/// be exercised on this host. On macOS 14.4+, `SystemDefault` is implemented
/// via `AudioHardwareCreateProcessTap` + a system-wide `CATapDescription`,
/// which is gated by `kTCCServiceAudioCapture`. On headless managed runners
/// without a pre-granted TCC grant, that call hangs 10–18 minutes before
/// erroring — identical symptom to Process Tap. Non-macOS platforms take
/// a non-TCC path (WASAPI loopback / PipeWire monitor) and always proceed.
macro_rules! require_system_capture {
    () => {
        require_audio!();
        if !$crate::helpers::macos_tcc_available() {
            eprintln!(
                "\n╔══════════════════════════════════════════════════════════╗"
            );
            eprintln!(
                "║  SKIPPING: macOS TCC Audio Capture not granted          ║"
            );
            eprintln!(
                "║  CaptureTarget::SystemDefault uses Process Tap on macOS ║"
            );
            eprintln!(
                "║  — same TCC gate as Application/ProcessTree. Use the    ║"
            );
            eprintln!(
                "║  BlackHole-as-input pattern for CI system capture.      ║"
            );
            eprintln!(
                "╚══════════════════════════════════════════════════════════╝\n"
            );
            return;
        }
    };
}

// ---------------------------------------------------------------------------
// require_app_capture!() macro — skips when app capture is unsupported
// ---------------------------------------------------------------------------

/// Macro that skips the current test if application capture is not supported.
/// First checks audio infrastructure availability, then platform capabilities,
/// then (on macOS) the TCC Audio Capture gate (kTCCServiceAudioCapture — NOT
/// Screen Recording; those are distinct TCC services).
macro_rules! require_app_capture {
    () => {
        require_audio!();
        if !$crate::helpers::macos_tcc_available() {
            eprintln!(
                "\n╔══════════════════════════════════════════════════════════╗"
            );
            eprintln!(
                "║  SKIPPING: macOS TCC Audio Capture not granted          ║"
            );
            eprintln!(
                "║  Set RSAC_CI_MACOS_TCC_GRANTED=1 if TCC is pre-granted. ║"
            );
            eprintln!(
                "╚══════════════════════════════════════════════════════════╝\n"
            );
            return;
        }
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
// require_device_selection!() macro — skips when device selection unsupported
// ---------------------------------------------------------------------------

/// Macro that skips the current test if device selection is not supported.
/// First checks audio infrastructure availability, then platform capabilities.
macro_rules! require_device_selection {
    () => {
        require_audio!();
        let caps = rsac::PlatformCapabilities::query();
        if !caps.supports_device_selection {
            eprintln!(
                "\n╔══════════════════════════════════════════════════════════╗"
            );
            eprintln!(
                "║  SKIPPING: Device selection not supported on platform   ║"
            );
            eprintln!(
                "╚══════════════════════════════════════════════════════════╝\n"
            );
            return;
        }
    };
}

// ---------------------------------------------------------------------------
// require_process_capture!() macro — skips when process tree capture unsupported
// ---------------------------------------------------------------------------

/// Macro that skips the current test if process tree capture is not supported.
/// First checks audio infrastructure availability, then (on macOS) the TCC
/// Audio Capture gate (kTCCServiceAudioCapture — NOT Screen Recording; those
/// are distinct TCC services), then platform capabilities.
macro_rules! require_process_capture {
    () => {
        require_audio!();
        if !$crate::helpers::macos_tcc_available() {
            eprintln!(
                "\n╔══════════════════════════════════════════════════════════╗"
            );
            eprintln!(
                "║  SKIPPING: macOS TCC Audio Capture not granted          ║"
            );
            eprintln!(
                "║  Set RSAC_CI_MACOS_TCC_GRANTED=1 if TCC is pre-granted. ║"
            );
            eprintln!(
                "╚══════════════════════════════════════════════════════════╝\n"
            );
            return;
        }
        let caps = rsac::PlatformCapabilities::query();
        if !caps.supports_process_tree_capture {
            eprintln!(
                "\n╔══════════════════════════════════════════════════════════╗"
            );
            eprintln!(
                "║  SKIPPING: Process tree capture not supported           ║"
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
/// `CaptureTarget::Application(ApplicationId(pid))` or `CaptureTarget::ProcessTree`.
pub fn spawn_audio_player_get_pid(wav_path: &std::path::Path) -> Result<(Child, u32), String> {
    #[cfg(target_os = "linux")]
    {
        // Same PULSE_SINK-aware player preference as spawn_test_tone_player
        // (rsac-b106): a pinned Pulse sink makes paplay the deterministic
        // route on runners where PipeWire default metadata is not settable.
        let pulse_sink_pinned = std::env::var_os("PULSE_SINK").is_some();
        let order: [&str; 2] = if pulse_sink_pinned {
            ["paplay", "pw-play"]
        } else {
            ["pw-play", "paplay"]
        };
        let child = Command::new(order[0])
            .arg(wav_path)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .or_else(|_| {
                Command::new(order[1])
                    .arg(wav_path)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
            })
            .map_err(|e| format!("Failed to spawn audio player: {e}"))?;

        let mut child = child;
        let pid = child.id();
        eprintln!(
            "[ci_audio] Started audio player PID={pid} for {:?} (PULSE_SINK pinned: {pulse_sink_pinned})",
            wav_path
        );
        warmup_and_guard_player(&mut child, "pw-play/paplay");
        Ok((child, pid))
    }

    #[cfg(target_os = "windows")]
    {
        // See `spawn_test_tone_player` for the rsac#24 rationale: we
        // must feed SoundPlayer a 16-bit PCM WAV and use PlayLooping so
        // the tone keeps hitting VB-CABLE's default-endpoint loopback
        // for the full test window.
        let pcm16_path = generate_pcm16_sibling(wav_path, 5.0, 48000, 2);
        let path_str = pcm16_path.to_string_lossy();
        let script = format!(
            "$p = New-Object System.Media.SoundPlayer '{}'; $p.PlayLooping(); Start-Sleep -Seconds 30; $p.Stop()",
            path_str
        );
        let child = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn Windows audio player: {e}"))?;

        let mut child = child;
        let pid = child.id();
        eprintln!(
            "[ci_audio] Started Windows PlayLooping PID={pid} for {:?}",
            pcm16_path
        );
        warmup_and_guard_player(&mut child, "Windows PlayLooping");
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

        let mut child = child;
        let pid = child.id();
        eprintln!("[ci_audio] Started macOS audio player PID={pid}");
        warmup_and_guard_player(&mut child, "afplay");
        Ok((child, pid))
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        let _ = wav_path;
        Err("No audio player available for this platform".to_string())
    }
}

/// Best-effort lookup of the PipeWire node id registered for a client PID,
/// via `pw-dump` (matching `info.props["application.process.id"]`).
///
/// Used by the Linux app-capture tests purely as a SKIP gate: if PipeWire
/// never registered a node for the spawned player, per-application capture
/// cannot possibly route audio in that environment, so the test skips instead
/// of failing on a CI routing limitation. Any failure mode (no `pw-dump`
/// binary, non-zero exit, unparseable output, no matching node) returns
/// `None` — this helper must never panic a test.
#[cfg(target_os = "linux")]
pub fn find_pipewire_node_for_pid(pid: u32) -> Option<u32> {
    let output = Command::new("pw-dump").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let objects = parsed.as_array()?;
    for obj in objects {
        // Only PipeWire node objects can be capture targets.
        let is_node = obj
            .get("type")
            .and_then(|t| t.as_str())
            .is_some_and(|t| t.ends_with("Node"));
        if !is_node {
            continue;
        }
        let Some(props) = obj.pointer("/info/props") else {
            continue;
        };
        // pw-dump emits application.process.id as a number on current
        // versions but has emitted strings historically — accept both.
        let matches_pid = match props.get("application.process.id") {
            Some(v) => {
                v.as_u64() == Some(u64::from(pid))
                    || v.as_str().is_some_and(|s| s.trim() == pid.to_string())
            }
            None => false,
        };
        if !matches_pid {
            continue;
        }
        if let Some(id) = obj.get("id").and_then(|i| i.as_u64()) {
            return Some(id as u32);
        }
    }
    None
}
