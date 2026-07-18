//! Android **playback capture** — the `AudioPlaybackCapture` tiers
//! (rsac-77f1, ADR-0013).
//!
//! Serves `CaptureTarget::SystemDefault`, `Application`,
//! `ApplicationByName`, and `ProcessTree` by orchestrating the rsac AAR's
//! Kotlin capture loop through JNI (see [`super::jni`] for the boundary and
//! `mobile/android/` for the Java side):
//!
//! ```text
//! create_stream()                     Kotlin (AAR)
//! ───────────────                     ────────────
//! resolve target → matchUid
//! register ingest session   ────────► CaptureBridge(projection, session,
//! create_and_start_bridge()             rate, ch, matchUid, framesPerRead)
//!                                      RsacCaptureService.registerBridge
//!                                      bridge.start() → read thread:
//!                                        AudioRecord.read → nativePush ──► ring
//! stop_capture()/Drop       ────────► bridge.stop() (join, release)
//!   unregister session first            └ thread exit → nativeSessionEnded
//!   projection.stop() + DeleteGlobalRef        (no-op: already unregistered)
//! ```
//!
//! # Target → UID mapping (normative, ADR-0013)
//!
//! | `CaptureTarget` | `matchUid` |
//! |---|---|
//! | `SystemDefault` | none (`-1`) — usage filters only ("all capturable playback") |
//! | `Application(ApplicationId)` | the id parsed as a numeric **app UID** |
//! | `ApplicationByName(String)` | package name → UID via the AAR's `PackageResolver` (`PackageManager`) |
//! | `ProcessTree(ProcessId)` | PID → UID from `/proc/<pid>/status` (pure Rust). **Tree ≡ app**: all of an Android app's processes share one UID — a documented equivalence, not a limitation |
//!
//! The usage-filter set (`MEDIA`/`GAME`/`UNKNOWN`) is fixed on the Kotlin
//! side (the transport mapping from ADR-0013); the UID is the only policy
//! input crossing the boundary.
//!
//! # Consent-token lifecycle
//!
//! The [`AndroidProjectionToken`](crate::core::config::AndroidProjectionToken)
//! (a `GlobalRef` to the user-consented `MediaProjection`, minted by
//! `RsacProjection.request` → `nativeRetainProjection`) is consumed from
//! [`StreamConfig::android_projection`] and **owned by the stream**: the
//! teardown choke point stops the projection (`MediaProjection.stop()`) and
//! deletes the ref — one token = one projection session, released on
//! capture drop exactly as `RsacProjection`'s contract documents.
//!
//! # Terminal semantics (ADR-0010 / ADR-0003)
//!
//! - **Graceful stop** (`stop_capture` / `Drop`): the session is
//!   unregistered *first* (so trailing `nativePush`/`nativeSessionEnded`
//!   calls are provably-safe no-ops), Kotlin `stop()` joins the read
//!   thread, and the bridge is driven `Running → Stopping` (drainable
//!   tail).
//! - **Spontaneous death** (projection revoked, foreground service
//!   destroyed, `AudioRecord` death): the Kotlin read loop exits and calls
//!   `nativeSessionEnded`, which finds the session still registered and
//!   forces the sticky terminal `Error` — a parked reader observes the
//!   Fatal `StreamEnded` instead of hanging.
//!
//! [`StreamConfig::android_projection`]: crate::core::config::StreamConfig::android_projection

#![cfg(all(target_os = "android", feature = "feat_android"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jni_sys::jobject;

use crate::bridge::ring_buffer::{BridgeProducer, BridgeShared};
use crate::bridge::state::StreamState;
use crate::bridge::stream::PlatformStream;
use crate::bridge::{calculate_capacity, create_bridge, BridgeStream};
use crate::core::config::{AudioFormat, CaptureTarget, DeviceId, SampleFormat, StreamConfig};
use crate::core::error::{AudioError, AudioResult};
use crate::core::interface::{AudioDevice, CapturingStream, DeviceKind};

use super::jni;
use super::thread::scratch_capacity_samples;

/// The [`DeviceId`] string of the logical Android playback-capture
/// endpoint.
pub(crate) const PLAYBACK_DEVICE_ID: &str = "playback-capture";

/// Frames per Java-side read/push period — lockstep with the Kotlin
/// `CaptureBridge.DEFAULT_FRAMES_PER_READ` (480 frames = 10 ms at 48 kHz);
/// passed explicitly so the two sides cannot drift silently.
const FRAMES_PER_READ: i32 = 480;

/// `matchUid` sentinel for "no UID filter" — lockstep with the Kotlin
/// `CaptureBridge.NO_UID_FILTER`.
const NO_UID_FILTER: i32 = -1;

// ── UID resolution ───────────────────────────────────────────────────────

/// Extracts the real UID from the text of `/proc/<pid>/status` (the first
/// field of the `Uid:` line). Pure parsing — unit-tested on every host.
fn parse_uid_from_status(status_text: &str) -> Option<u32> {
    status_text
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Resolves a PID to its UID via `/proc/<pid>/status` — the ADR-0013
/// `ProcessTree` mapping, done in pure Rust (no JNI needed).
///
/// Honest limitation (documented in the AAR's `PackageResolver` too):
/// Android mounts `/proc` with `hidepid=2`, so other apps' processes are
/// generally not readable — this works for the caller's own UID's processes
/// (which covers the tree ≡ app equivalence) and PIDs the platform exposes.
fn uid_for_pid(pid: u32) -> AudioResult<u32> {
    let path = format!("/proc/{}/status", pid);
    let text = std::fs::read_to_string(&path).map_err(|e| AudioError::ApplicationNotFound {
        identifier: format!(
            "PID {} (cannot read {}: {}; the process does not exist or is \
             not visible to this app — Android hides other apps' /proc \
             entries)",
            pid, path, e
        ),
    })?;
    parse_uid_from_status(&text).ok_or_else(|| AudioError::ApplicationNotFound {
        identifier: format!("PID {} ({} has no parseable Uid: line)", pid, path),
    })
}

/// Resolves a playback [`CaptureTarget`] to its `matchUid` argument
/// (ADR-0013 mapping — see the module docs table).
fn resolve_match_uid(target: &CaptureTarget) -> AudioResult<i32> {
    match target {
        CaptureTarget::SystemDefault => Ok(NO_UID_FILTER),
        CaptureTarget::Application(app_id) => {
            app_id.0.parse::<u32>().map(|uid| uid as i32).map_err(|_| {
                AudioError::InvalidParameter {
                    param: "application id".to_string(),
                    reason: format!(
                        "on Android, CaptureTarget::Application carries the \
                         numeric app UID (ADR-0013); {:?} is not a number. \
                         Use CaptureTarget::ApplicationByName(package) for a \
                         package name",
                        app_id.0
                    ),
                }
            })
        }
        CaptureTarget::ApplicationByName(package) => jni::resolve_uid_for_package(package),
        CaptureTarget::ProcessTree(pid) => uid_for_pid(pid.0).map(|uid| uid as i32),
        CaptureTarget::Device(_) => Err(AudioError::PlatformNotSupported {
            feature: "device capture through the Android playback-capture \
                      endpoint: this endpoint serves the playback tiers \
                      (SystemDefault / Application / ApplicationByName / \
                      ProcessTree). Use \
                      CaptureTarget::Device(DeviceId(\"default\".into())) for \
                      the microphone"
                .to_string(),
            platform: "android".to_string(),
        }),
    }
}

// ── AndroidPlaybackDevice ────────────────────────────────────────────────

/// The logical Android playback-capture endpoint (`AudioPlaybackCapture`
/// behind MediaProjection consent).
///
/// A metadata-only handle: constructing it touches no OS or JVM resources.
/// The Kotlin capture pipeline is created lazily in
/// [`create_stream`](AudioDevice::create_stream). This is rsac's *default
/// device* on Android — `CaptureTarget::SystemDefault` resolves here, the
/// same way every desktop backend's default device is the system-audio
/// loopback endpoint (and the same shape as iOS's `BroadcastAudioDevice`).
#[derive(Debug, Clone, Copy)]
pub struct AndroidPlaybackDevice;

impl AndroidPlaybackDevice {
    /// Creates the logical playback-capture device handle.
    pub fn new() -> Self {
        Self
    }
}

impl Default for AndroidPlaybackDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioDevice for AndroidPlaybackDevice {
    fn id(&self) -> DeviceId {
        DeviceId(PLAYBACK_DEVICE_ID.to_string())
    }

    fn name(&self) -> String {
        "Playback capture (AudioPlaybackCapture)".to_string()
    }

    /// `true` — the default of its ([`DeviceKind::Output`]) kind; the mic
    /// device stays the default [`DeviceKind::Input`].
    fn is_default(&self) -> bool {
        true
    }

    /// The shapes the Kotlin `CaptureBridge` constructs `AudioRecord` with:
    /// `PCM_FLOAT` mono/stereo at the common Android rates. Unlike AAudio,
    /// there is no renegotiation — the record either honors the requested
    /// shape or construction fails — so the delivered format equals the
    /// (negotiated) requested one.
    fn supported_formats(&self) -> Vec<AudioFormat> {
        const RATES: [u32; 2] = [48_000, 44_100];
        let mut formats = Vec::with_capacity(RATES.len() * 2);
        for rate in RATES {
            for channels in [2u16, 1] {
                formats.push(AudioFormat {
                    sample_rate: rate,
                    channels,
                    sample_format: SampleFormat::F32,
                });
            }
        }
        formats
    }

    /// [`DeviceKind::Output`]: playback capture is a loopback of the
    /// system's (possibly UID-filtered) output mix — same convention as the
    /// desktop loopback endpoints and iOS's broadcast device.
    fn kind(&self) -> AudioResult<DeviceKind> {
        Ok(DeviceKind::Output)
    }

    /// Creates a live playback-capture stream through the ring-buffer
    /// bridge.
    ///
    /// Wiring (identical shape to every other rsac backend): create the
    /// bridge (ring depth honours `config.buffer_size`, ADR-0007 pattern),
    /// transition it to `Running`, start the Kotlin capture pipeline
    /// ([`create_playback_capture`]), and wrap everything in a
    /// [`BridgeStream`].
    ///
    /// # Errors
    ///
    /// - [`AudioError::UserConsentRequired`] when no projection token is in
    ///   the config (normally caught earlier by the `build()` preflight).
    /// - [`AudioError::ApplicationNotFound`] /
    ///   [`AudioError::InvalidParameter`] from target → UID resolution.
    /// - [`AudioError::StreamCreationFailed`] /
    ///   [`AudioError::StreamStartFailed`] from the JNI/Kotlin pipeline
    ///   (missing AAR classes, RECORD_AUDIO not granted, consumed/revoked
    ///   projection, no mediaProjection foreground service on API 34+).
    fn create_stream(&self, config: &StreamConfig) -> AudioResult<Box<dyn CapturingStream>> {
        let requested = config.to_audio_format();

        let capacity = calculate_capacity(config.buffer_size, 4);
        let (producer, consumer) = create_bridge(capacity, requested.clone());

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

        let terminal = Arc::clone(consumer.shared());

        let (platform_stream, delivered) =
            create_playback_capture(config, &requested, producer, terminal)?;

        let bridge_stream =
            BridgeStream::new(consumer, platform_stream, delivered, Duration::from_secs(1));

        Ok(Box::new(bridge_stream))
    }
}

// ── AndroidPlaybackStream ────────────────────────────────────────────────

/// JNI handles for one playback capture, serialized by the `Mutex` in
/// [`AndroidPlaybackStream`]. Both are nulled/zeroed as they are released —
/// the idempotency guard for teardown.
struct PlaybackHandles {
    /// GlobalRef to the Kotlin `CaptureBridge`; null once released.
    bridge: jobject,
    /// The owned projection token (GlobalRef to the `MediaProjection`);
    /// `0` once released.
    projection_token: i64,
}

/// Platform-specific stream handle for Android playback capture.
///
/// Implements [`PlatformStream`] so it can be wrapped by
/// [`BridgeStream`](crate::bridge::stream::BridgeStream) like every other
/// rsac backend.
pub(crate) struct AndroidPlaybackStream {
    /// JNI handles, serialized by the `Mutex` for `&self` access.
    handles: Mutex<PlaybackHandles>,
    /// The ingest-session registry id (see [`super::jni`]'s module docs).
    session_id: i64,
    /// `true` while the Kotlin pipeline is delivering. Cleared by the
    /// teardown choke point and by `nativeSessionEnded` (spontaneous
    /// death), so [`is_active`](PlatformStream::is_active) reflects both.
    is_active: Arc<AtomicBool>,
    /// Producer-terminal-signal handle (ADR-0010).
    terminal: Arc<BridgeShared>,
}

// SAFETY: the raw `jobject`s inside the Mutex are JNI **GlobalRefs**, which
// the JNI spec defines as valid on any thread; every use goes through the
// Mutex (only the teardown choke point touches them), so no unsynchronized
// concurrent use can occur. The remaining fields are Send + Sync already.
// Mirrors the discipline on `AndroidPlatformStream` / `IosPlatformStream`.
unsafe impl Send for AndroidPlaybackStream {}
// SAFETY: see the `Send` justification — all interior pointer access is
// serialized by the `Mutex`.
unsafe impl Sync for AndroidPlaybackStream {}

impl AndroidPlaybackStream {
    /// Stops the Kotlin pipeline (once), releases the projection, and
    /// signals the bridge terminal.
    ///
    /// Ordering matters (see the module docs' terminal-semantics section):
    ///
    /// 1. **Unregister the ingest session** — before Kotlin `stop()`, so a
    ///    `nativePush` still in flight after the bounded join is a provable
    ///    no-op (the registry design exists for exactly this hazard) and
    ///    the read thread's final `nativeSessionEnded` finds nothing.
    /// 2. Kotlin `bridge.stop()` + `unregisterBridge` + drop the GlobalRef.
    /// 3. `MediaProjection.stop()` + drop its GlobalRef (one token = one
    ///    projection session).
    /// 4. Drive the bridge `Running → Stopping` (graceful producer
    ///    terminal, ADR-0010) and wake parked readers.
    ///
    /// Idempotent: the nulled handles (under the `Mutex`) make later calls
    /// no-ops. JNI failures are logged, never propagated — teardown always
    /// runs to completion.
    fn stop_and_close(&self) -> AudioResult<()> {
        let mut handles = self.handles.lock().map_err(|_| AudioError::InternalError {
            message: "Android playback stream handles mutex poisoned".to_string(),
            source: None,
        })?;

        if handles.bridge.is_null() && handles.projection_token == 0 {
            return Ok(());
        }

        // 1. Make trailing Java-entered calls no-ops.
        jni::unregister_session(self.session_id);

        // 2. Stop + detach + release the Kotlin bridge.
        let bridge = std::mem::replace(&mut handles.bridge, std::ptr::null_mut());
        jni::stop_and_release_bridge(bridge);

        // 3. Release the projection (consent-token lifecycle contract).
        let token = std::mem::replace(&mut handles.projection_token, 0);
        jni::stop_and_release_projection(token);

        self.is_active.store(false, Ordering::SeqCst);

        // 4. Graceful producer terminal (ADR-0010): `Running → Stopping`
        // keeps a buffered tail drainable; the CAS no-ops if the state
        // already advanced (e.g. nativeSessionEnded's sticky Error).
        let _ = self
            .terminal
            .state
            .transition(StreamState::Running, StreamState::Stopping);
        self.terminal.notify_wake();
        #[cfg(feature = "async-stream")]
        self.terminal.waker.wake();

        Ok(())
    }
}

impl PlatformStream for AndroidPlaybackStream {
    fn stop_capture(&self) -> AudioResult<()> {
        self.stop_and_close()
    }

    fn is_active(&self) -> bool {
        self.is_active.load(Ordering::SeqCst)
    }
}

impl Drop for AndroidPlaybackStream {
    /// Deterministic shutdown: dropping the handle never leaves the Kotlin
    /// read thread pushing into a bridge nobody reads, a parked reader
    /// hanging (ADR-0010), or the projection GlobalRef leaked.
    fn drop(&mut self) {
        if let Err(e) = self.stop_and_close() {
            log::warn!("AndroidPlaybackStream::drop: teardown failed: {:?}", e);
        }
    }
}

// ── Factory ──────────────────────────────────────────────────────────────

/// Resolves the target, registers the ingest session, and starts the
/// Kotlin capture pipeline, returning the [`AndroidPlaybackStream`] handle
/// plus the delivered [`AudioFormat`].
///
/// The delivered format **is** the requested one: the Kotlin
/// `CaptureBridge` constructs its `AudioRecord` with exactly the requested
/// rate/channels in `PCM_FLOAT` (no renegotiation — construction fails
/// instead), and the format is published on the bridge before the read
/// loop starts so `CapturingStream::format()` is authoritative from the
/// first buffer.
fn create_playback_capture(
    config: &StreamConfig,
    requested: &AudioFormat,
    producer: BridgeProducer,
    terminal: Arc<BridgeShared>,
) -> AudioResult<(AndroidPlaybackStream, AudioFormat)> {
    // ── Consent token (normally enforced by the build() preflight) ──
    let token =
        config
            .android_projection
            .as_ref()
            .ok_or_else(|| AudioError::UserConsentRequired {
                feature: "Android playback capture".to_string(),
                missing: "MediaProjection token — obtain one via \
                      RsacProjection.request() and pass it to \
                      AudioCaptureBuilder::with_android_projection()"
                    .to_string(),
            })?;
    if token.as_raw() == 0 {
        return Err(AudioError::StreamCreationFailed {
            reason: "the Android projection token is 0 — the consent flow \
                     failed to retain the MediaProjection (see \
                     RsacProjection.nativeRetainProjection). Request consent \
                     again and pass the fresh token"
                .to_string(),
            context: None,
        });
    }

    // ── Kotlin-side constraints, checked before any JNI work ────────
    if requested.channels != 1 && requested.channels != 2 {
        return Err(AudioError::UnsupportedFormat {
            format: format!(
                "{} channels (Android playback capture supports mono or \
                 stereo)",
                requested.channels
            ),
            context: None,
        });
    }

    // ── Target → UID (ADR-0013) ──────────────────────────────────────
    let match_uid = resolve_match_uid(&config.capture_target)?;

    // ── Claim sole deletion ownership (single-owner latch, rsac-3407) ──
    // The token is `Clone` (so `StreamConfig`/builder stay `Clone`) but not
    // `Copy`; the shared consume-latch guarantees that at most one stream in a
    // token's clone lineage ever holds a deletable raw handle, so exactly one
    // `DeleteGlobalRef` runs. A second `build()` from a cloned config/builder
    // is refused here rather than double-releasing the JNI `GlobalRef` (UB).
    //
    // Claimed *after* every fallible preflight (channel/UID validation) so a
    // post-consume validation error can't strand the claim: once we consume,
    // the only remaining failure path is `create_and_start_bridge`, whose error
    // arm calls `release_claim()` to re-arm the token for retry. Consuming
    // earlier would leave the shared latch stuck `true` (and the `GlobalRef`
    // undeleted) on a channel/UID error, refusing every retry with the token.
    let raw = token
        .try_consume()
        .ok_or_else(|| AudioError::StreamCreationFailed {
            reason: "this MediaProjection consent token has already been handed to \
                 another capture stream; each retained projection handle owns \
                 exactly one capture session — re-run the consent flow rather \
                 than cloning the token or its StreamConfig"
                .to_string(),
            context: None,
        })?;

    let delivered = AudioFormat {
        sample_rate: requested.sample_rate,
        channels: requested.channels,
        sample_format: SampleFormat::F32,
    };
    // Publish before any push so readers never see the requested-format
    // fallback once data flows (M1 pattern, same as the mic slice).
    producer.set_negotiated_format(&delivered);

    // ── Ingest session (see jni.rs for the registry design) ─────────
    let is_active = Arc::new(AtomicBool::new(true));
    let session_id = jni::register_session(
        producer,
        Arc::clone(&terminal),
        Arc::clone(&is_active),
        scratch_capacity_samples(delivered.sample_rate, delivered.channels),
    );

    // ── Kotlin pipeline: construct → register → start ────────────────
    let bridge = match jni::create_and_start_bridge(
        raw as jobject,
        session_id,
        delivered.sample_rate,
        delivered.channels,
        match_uid,
        FRAMES_PER_READ,
    ) {
        Ok(bridge) => bridge,
        Err(e) => {
            // Nothing Java-side is running; reclaim the session so the
            // producer (and its ring) is dropped rather than leaked. The
            // projection token stays with the caller (they may retry), so
            // release the deletion claim we took above.
            jni::unregister_session(session_id);
            token.release_claim();
            return Err(e);
        }
    };

    log::debug!(
        "Android playback capture started (target={:?}, matchUid={}, {} Hz, \
         {} ch, {} frames/read)",
        config.capture_target,
        match_uid,
        delivered.sample_rate,
        delivered.channels,
        FRAMES_PER_READ
    );

    Ok((
        AndroidPlaybackStream {
            handles: Mutex::new(PlaybackHandles {
                bridge,
                projection_token: raw,
            }),
            session_id,
            is_active,
            terminal,
        },
        delivered,
    ))
}

// ══════════════════════════════════════════════════════════════════════════
// Tests — pure logic only (no JNI): UID parsing and target mapping edges.
// They compile for the Android target under `--tests` and will run on the
// emulator leg (rsac-e6d3).
// ══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::ApplicationId;

    // ── /proc status parsing ─────────────────────────────────────────

    #[test]
    fn parse_uid_takes_the_first_field_of_the_uid_line() {
        let status =
            "Name:\tcom.example.app\nPid:\t4242\nUid:\t10123\t10123\t10123\t10123\nGid:\t10123\n";
        assert_eq!(parse_uid_from_status(status), Some(10_123));
    }

    #[test]
    fn parse_uid_handles_missing_or_malformed_lines() {
        assert_eq!(parse_uid_from_status(""), None);
        assert_eq!(parse_uid_from_status("Name:\tfoo\nGid:\t1000\n"), None);
        assert_eq!(parse_uid_from_status("Uid:\tnot-a-number\n"), None);
        assert_eq!(parse_uid_from_status("Uid:\n"), None);
    }

    #[test]
    fn parse_uid_ignores_lines_that_merely_contain_uid() {
        // Only a line *starting* with "Uid:" is the real field.
        let status = "SigCgt:\t0000000000000000\nNoUid: 99\nUid:\t10007\t10007\n";
        assert_eq!(parse_uid_from_status(status), Some(10_007));
    }

    // ── Target mapping (ADR-0013) ────────────────────────────────────

    #[test]
    fn system_default_maps_to_no_uid_filter() {
        assert_eq!(
            resolve_match_uid(&CaptureTarget::SystemDefault).unwrap(),
            NO_UID_FILTER
        );
    }

    #[test]
    fn application_id_carries_the_numeric_uid() {
        let target = CaptureTarget::Application(ApplicationId("10123".to_string()));
        assert_eq!(resolve_match_uid(&target).unwrap(), 10_123);
    }

    #[test]
    fn non_numeric_application_id_is_actionable() {
        let target = CaptureTarget::Application(ApplicationId("com.example.app".to_string()));
        match resolve_match_uid(&target).unwrap_err() {
            AudioError::InvalidParameter { reason, .. } => {
                assert!(reason.contains("numeric app UID"), "{reason}");
                assert!(reason.contains("ApplicationByName"), "{reason}");
            }
            other => panic!("expected InvalidParameter, got {other:?}"),
        }
    }

    // ── Single-owner token ownership (rsac-3407) ─────────────────────
    #[test]
    fn cloned_token_second_consume_is_refused() {
        // Pure-latch mirror of what a duplicate create_playback_capture would
        // hit: the second stream built from a cloned token/StreamConfig fails
        // its try_consume, so it never obtains a deletable raw handle (no
        // double DeleteGlobalRef). No JNI involved.
        use crate::core::config::AndroidProjectionToken;
        // SAFETY: fabricated handle; never reaches JNI in this latch-only test.
        let token = unsafe { AndroidProjectionToken::from_raw(123) };
        let cloned = token.clone();
        assert_eq!(token.try_consume(), Some(123));
        assert_eq!(cloned.try_consume(), None);
    }

    #[test]
    fn device_target_is_refused_with_mic_guidance() {
        let target = CaptureTarget::Device(DeviceId(PLAYBACK_DEVICE_ID.to_string()));
        match resolve_match_uid(&target).unwrap_err() {
            AudioError::PlatformNotSupported { feature, platform } => {
                assert_eq!(platform, "android");
                assert!(feature.contains("Device(DeviceId(\"default\""), "{feature}");
            }
            other => panic!("expected PlatformNotSupported, got {other:?}"),
        }
    }

    // ── Device metadata ──────────────────────────────────────────────

    #[test]
    fn playback_device_metadata_is_consistent() {
        let device = AndroidPlaybackDevice::new();
        assert_eq!(device.id(), DeviceId(PLAYBACK_DEVICE_ID.to_string()));
        assert!(device.is_default());
        assert_eq!(device.kind().unwrap(), DeviceKind::Output);
        let formats = device.supported_formats();
        assert!(!formats.is_empty());
        for fmt in &formats {
            assert_eq!(fmt.sample_format, SampleFormat::F32);
            assert!(fmt.channels == 1 || fmt.channels == 2);
        }
        // First entry (the DeviceInfo::default_format seed) is 48 kHz
        // stereo F32.
        assert_eq!(formats[0].sample_rate, 48_000);
        assert_eq!(formats[0].channels, 2);
    }

    #[test]
    fn frames_per_read_matches_the_kotlin_default() {
        // Lockstep with CaptureBridge.DEFAULT_FRAMES_PER_READ (480 = 10 ms
        // at 48 kHz). The jni_lockstep tests guard the symbol names; this
        // guards the period constant.
        assert_eq!(FRAMES_PER_READ, 480);
    }
}
