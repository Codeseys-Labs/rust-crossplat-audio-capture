// Shared capture-session bookkeeping used by BOTH the desktop passthrough and
// the mobile bridge. The platform delegates (`desktop::Rsac` / `mobile::Rsac`)
// differ only in consent handling and Android token threading; everything else
// — the capture-id map, the build/start path, the derived-meter pump — is
// identical and lives here so it is written once.
//
// No capture policy: this module only orchestrates rsac's public API
// (`AudioCaptureBuilder` → `RunningCapture` → `subscribe`) and computes derived
// meters via `AudioBuffer`'s alloc-free level methods before dropping buffers.
// Raw samples cross IPC ONLY on the opt-in `subscribe_raw` path.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use tauri::ipc::Channel;

use rsac::{AudioBuffer, AudioCaptureBuilder, RunningCapture, SampleFormat};

use crate::models::*;
use crate::{Error, Result};

/// Map of live captures keyed by opaque id, plus a monotonic id counter.
///
/// `RunningCapture` is `Send + Sync` (it wraps the `Send + Sync`
/// `AudioCapture`), so a `Mutex<HashMap<..>>` is a sound Tauri managed-state
/// container. The lock is held only for map operations and the (cheap,
/// non-blocking) `subscribe()` call — never across the pump loop.
#[derive(Default)]
pub(crate) struct Sessions {
    map: Mutex<HashMap<String, RunningCapture>>,
    next_id: AtomicU64,
}

impl Sessions {
    /// Builds + starts a capture from the target string and config, stores it,
    /// and returns its id + negotiated format. `configure` lets the mobile
    /// delegate thread the Android projection token onto the builder before
    /// `start()`; desktop passes the identity closure.
    pub(crate) fn start<F>(
        &self,
        target: &str,
        config: CaptureConfig,
        configure: F,
    ) -> Result<StartCaptureResult>
    where
        F: FnOnce(AudioCaptureBuilder) -> Result<AudioCaptureBuilder>,
    {
        let mut builder = AudioCaptureBuilder::new().target_str(target)?;
        if let Some(rate) = config.sample_rate {
            builder = builder.sample_rate(rate);
        }
        if let Some(ch) = config.channels {
            builder = builder.channels(ch);
        }
        if let Some(ref fmt) = config.sample_format {
            builder = builder.sample_format(parse_sample_format(fmt)?);
        }
        if let Some(size) = config.buffer_size {
            builder = builder.buffer_size(Some(size));
        }

        // Platform hook (Android token threading; identity on desktop).
        let builder = configure(builder)?;

        let running = builder.start()?;
        let format = format_info(&running);

        let id = format!("capture-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        self.map
            .lock()
            .expect("sessions mutex poisoned")
            .insert(id.clone(), running);

        Ok(StartCaptureResult {
            capture_id: id,
            format,
        })
    }

    /// Stops + drops the capture. Idempotent — an unknown id is a no-op success
    /// (a JS caller that double-stops must not see an error).
    pub(crate) fn stop(&self, capture_id: &str) -> Result<()> {
        if let Some(mut running) = self
            .map
            .lock()
            .expect("sessions mutex poisoned")
            .remove(capture_id)
        {
            // Explicit stop makes teardown authoritative; Drop would also stop.
            running.stop()?;
        }
        Ok(())
    }

    /// Spawns the derived-meter pump for `capture_id`, forwarding
    /// `rsac://chunk-meta` events over `channel`. Buffers are metered
    /// (alloc-free) and dropped inside the pump — raw samples never cross IPC.
    pub(crate) fn subscribe_meta(
        &self,
        capture_id: &str,
        channel: Channel<ChunkMeta>,
    ) -> Result<()> {
        let rx = {
            let map = self.map.lock().expect("sessions mutex poisoned");
            let running = map
                .get(capture_id)
                .ok_or_else(|| Error::Plugin(format!("unknown capture id: {capture_id}")))?;
            running.subscribe()?
        };

        std::thread::Builder::new()
            .name("rsac-tauri-meta".into())
            .spawn(move || {
                // The bounded rsac subscribe channel already applies
                // drop-don't-block backpressure (ADR-0007); we only compute the
                // derived meters and forward. A dropped receiver or a stopped
                // capture disconnects `rx`, ending the loop.
                while let Ok(buffer) = rx.recv() {
                    let meta = chunk_meta(&buffer);
                    if channel.send(meta).is_err() {
                        break; // webview channel closed
                    }
                }
            })
            .map_err(|e| Error::Plugin(format!("failed to spawn meter pump: {e}")))?;

        Ok(())
    }

    /// Spawns the RAW pump — the opt-in slow path (`rsac://chunk-raw`). Only
    /// reachable when the host granted `allow-subscribe-raw` (§5).
    pub(crate) fn subscribe_raw(&self, capture_id: &str, channel: Channel<ChunkRaw>) -> Result<()> {
        let rx = {
            let map = self.map.lock().expect("sessions mutex poisoned");
            let running = map
                .get(capture_id)
                .ok_or_else(|| Error::Plugin(format!("unknown capture id: {capture_id}")))?;
            running.subscribe()?
        };

        std::thread::Builder::new()
            .name("rsac-tauri-raw".into())
            .spawn(move || {
                while let Ok(buffer) = rx.recv() {
                    let raw = ChunkRaw {
                        sample_rate: buffer.sample_rate(),
                        channels: buffer.channels(),
                        frames: buffer.num_frames(),
                        samples: buffer.data().to_vec(),
                    };
                    if channel.send(raw).is_err() {
                        break;
                    }
                }
            })
            .map_err(|e| Error::Plugin(format!("failed to spawn raw pump: {e}")))?;

        Ok(())
    }
}

// ── Conversions ──────────────────────────────────────────────────────────

/// Lowercase wire name for a `SampleFormat`.
pub(crate) fn sample_format_str(fmt: SampleFormat) -> &'static str {
    match fmt {
        SampleFormat::I16 => "i16",
        SampleFormat::I24 => "i24",
        SampleFormat::I32 => "i32",
        SampleFormat::F32 => "f32",
    }
}

/// Parse a wire sample-format string (case-insensitive) into a `SampleFormat`.
fn parse_sample_format(s: &str) -> Result<SampleFormat> {
    match s.to_ascii_lowercase().as_str() {
        "i16" => Ok(SampleFormat::I16),
        "i24" => Ok(SampleFormat::I24),
        "i32" => Ok(SampleFormat::I32),
        "f32" => Ok(SampleFormat::F32),
        other => Err(Error::Plugin(format!(
            "unknown sample format {other:?} (expected one of: i16, i24, i32, f32)"
        ))),
    }
}

/// Negotiated `FormatInfo` for a running capture; falls back to the requested
/// config when the backend does not report one yet.
fn format_info(running: &RunningCapture) -> FormatInfo {
    if let Some(fmt) = running.format() {
        FormatInfo {
            sample_rate: fmt.sample_rate,
            channels: fmt.channels,
            sample_format: sample_format_str(fmt.sample_format).to_string(),
        }
    } else {
        let cfg = &running.config().stream_config;
        FormatInfo {
            sample_rate: cfg.sample_rate,
            channels: cfg.channels,
            sample_format: sample_format_str(cfg.sample_format).to_string(),
        }
    }
}

/// Compute the derived `ChunkMeta` from an `AudioBuffer` (alloc-free meters,
/// NaN-safe) BEFORE the buffer is dropped — the napi `ChunkMeta` precedent.
fn chunk_meta(buffer: &AudioBuffer) -> ChunkMeta {
    let channels = buffer.channels();
    let sample_rate = buffer.sample_rate();
    let frames = buffer.num_frames();
    let duration_secs = if sample_rate > 0 {
        frames as f64 / sample_rate as f64
    } else {
        0.0
    };

    let mut channel_rms = Vec::with_capacity(channels as usize);
    let mut channel_peak = Vec::with_capacity(channels as usize);
    for ch in 0..channels {
        channel_rms.push(buffer.channel_rms(ch).unwrap_or(0.0));
        channel_peak.push(buffer.channel_peak(ch).unwrap_or(0.0));
    }

    ChunkMeta {
        sample_rate,
        channels,
        frames,
        duration_secs,
        rms: buffer.rms(),
        peak: buffer.peak(),
        rms_dbfs: buffer.rms_dbfs(),
        peak_dbfs: buffer.peak_dbfs(),
        channel_rms,
        channel_peak,
        format: FormatInfo {
            sample_rate,
            channels,
            sample_format: "f32".to_string(), // rsac's internal buffer format
        },
    }
}

// ── Shared read-only commands (platform-independent) ──────────────────────

/// `list_targets` implementation: `rsac::list_audio_sources()` mapped to
/// `TargetInfo`.
pub(crate) fn list_targets() -> Result<Vec<TargetInfo>> {
    let sources = rsac::list_audio_sources()?;
    Ok(sources
        .into_iter()
        .map(|s| {
            let kind = match &s.kind {
                rsac::AudioSourceKind::SystemDefault => "systemDefault",
                rsac::AudioSourceKind::Device { .. } => "device",
                rsac::AudioSourceKind::Application { .. } => "application",
                // AudioSourceKind is #[non_exhaustive]; classify unknowns
                // honestly rather than mislabel them.
                _ => "unknown",
            };
            TargetInfo {
                id: s.id,
                name: s.name,
                kind: kind.to_string(),
            }
        })
        .collect())
}

/// `capabilities` implementation: `PlatformCapabilities::query()` serialized
/// verbatim (honesty gate — never claim a feature `query()` reports false).
pub(crate) fn capabilities() -> Capabilities {
    let caps = rsac::PlatformCapabilities::query();
    Capabilities {
        supports_system_capture: caps.supports_system_capture,
        supports_application_capture: caps.supports_application_capture,
        supports_process_tree_capture: caps.supports_process_tree_capture,
        supports_device_selection: caps.supports_device_selection,
        supports_device_change_notifications: caps.supports_device_change_notifications,
        requires_user_consent: caps.requires_user_consent,
        supported_sample_formats: caps
            .supported_sample_formats
            .iter()
            .map(|f| sample_format_str(*f).to_string())
            .collect(),
        sample_rate_range: caps.sample_rate_range,
        max_channels: caps.max_channels,
        backend_name: caps.backend_name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_format_roundtrip() {
        for fmt in [
            SampleFormat::I16,
            SampleFormat::I24,
            SampleFormat::I32,
            SampleFormat::F32,
        ] {
            let s = sample_format_str(fmt);
            assert_eq!(parse_sample_format(s).unwrap(), fmt);
        }
    }

    #[test]
    fn parse_sample_format_is_case_insensitive() {
        assert_eq!(parse_sample_format("F32").unwrap(), SampleFormat::F32);
        assert_eq!(parse_sample_format("I16").unwrap(), SampleFormat::I16);
    }

    #[test]
    fn parse_sample_format_rejects_unknown() {
        let err = parse_sample_format("f64").unwrap_err();
        // An unknown format is a plugin-misuse error, classified fatal so a JS
        // caller does not blind-retry.
        match err {
            Error::Plugin(msg) => assert!(msg.contains("f64")),
            other => panic!("expected Error::Plugin, got {other:?}"),
        }
    }

    #[test]
    fn capabilities_mirror_platform_query() {
        // The `capabilities` command must surface PlatformCapabilities::query()
        // verbatim (honesty gate) — never claim a feature query() reports false.
        let caps = capabilities();
        let raw = rsac::PlatformCapabilities::query();
        assert_eq!(caps.supports_system_capture, raw.supports_system_capture);
        assert_eq!(
            caps.supports_application_capture,
            raw.supports_application_capture
        );
        assert_eq!(caps.requires_user_consent, raw.requires_user_consent);
        assert_eq!(caps.max_channels, raw.max_channels);
        assert_eq!(caps.sample_rate_range, raw.sample_rate_range);
        assert_eq!(caps.backend_name, raw.backend_name);
        assert_eq!(
            caps.supported_sample_formats.len(),
            raw.supported_sample_formats.len()
        );
    }
}
