//! `IosPlatformStream` — the iOS backend's `PlatformStream` implementation.
//!
//! Mirrors the macOS `MacosPlatformStream` shape: an atomic active flag, the
//! live ObjC capture objects behind a `Mutex`, and a clone of the bridge's
//! shared state (`terminal`) so the stop/Drop choke point can drive the
//! bridge to its graceful ending state (producer terminal signal,
//! FH-1 / ADR-0010) — a parked reader then observes end-of-stream instead of
//! hanging on a stopped engine.
//!
//! # Threading model
//!
//! Unlike Linux (dedicated PipeWire thread) and Windows (rsac-owned capture
//! loop), AVFAudio manages its own threads: the engine drives the audio
//! hardware, and the input-node tap block fires on AVFAudio's internal tap
//! thread. There is **no rsac-owned capture thread** — the module name
//! follows the per-backend convention, not an actual `std::thread`.

#![cfg(all(target_os = "ios", feature = "feat_ios"))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::bridge::ring_buffer::{BridgeProducer, BridgeShared};
use crate::bridge::state::StreamState;
use crate::bridge::stream::PlatformStream;
use crate::core::config::{AudioFormat, CaptureTarget};
use crate::core::error::{AudioError, AudioResult};

use super::avaudio::{start_input_capture, AvAudioEngineCapture};
use super::DEFAULT_INPUT_DEVICE_ID;

// ── IosPlatformStream ────────────────────────────────────────────────────

/// Platform-specific stream handle for iOS (AVAudioEngine backend).
///
/// Wraps the live [`AvAudioEngineCapture`] (engine + input node, kept
/// retained for the stream's lifetime) and implements [`PlatformStream`] so
/// it can be used with `BridgeStream`.
///
/// # Shutdown
///
/// [`stop_capture`](PlatformStream::stop_capture) (and `Drop`, via the same
/// choke point) removes the tap, stops the engine, and then drives the
/// bridge `Running → Stopping` — the graceful producer terminal signal
/// (ADR-0010). The ordering matters: the terminal transition happens only
/// **after** the tap is removed, so no callback can push a buffer past the
/// declared end.
pub(crate) struct IosPlatformStream {
    /// Live AVAudioEngine objects, protected by a `Mutex` for `&self` access.
    /// Held for the stream's lifetime; dropping releases the ObjC strong
    /// references (thread-safe refcounting).
    capture: Mutex<AvAudioEngineCapture>,
    /// `true` while the engine/tap are running. `swap(false)` in the stop
    /// path makes teardown idempotent and race-free.
    is_active: AtomicBool,
    /// Producer-terminal-signal handle (FH-1 / ADR-0010): a clone of the
    /// bridge's shared state, used to drive `Running → Stopping` (+ reader
    /// wake) once the engine has stopped.
    terminal: Arc<BridgeShared>,
}

// SAFETY: `IosPlatformStream` carries `Retained<AVAudioEngine>` /
// `Retained<AVAudioInputNode>` (inside `AvAudioEngineCapture`), which are not
// `Send`/`Sync` by default — but `PlatformStream: Send` and
// `BridgeStream<S>: Sync` require both. This is sound because:
//
// - every access to the ObjC objects after construction goes through the
//   `Mutex<AvAudioEngineCapture>` (only `stop()` touches them), so no
//   unsynchronized concurrent ObjC calls can occur;
// - the calls made cross-thread (`removeTapOnBus:`, `AVAudioEngine stop`) are
//   documented safe off the main thread ("Taps may be safely installed and
//   removed while the engine is running"); AVAudioEngine has no
//   main-thread-only requirement for these lifecycle methods;
// - releasing the strong references from an arbitrary thread on drop is safe:
//   ObjC reference counting is thread-safe.
//
// Mirrors the `unsafe impl Send/Sync` discipline on `MacosPlatformStream`.
unsafe impl Send for IosPlatformStream {}
// SAFETY: see the `Send` justification above — all interior access is
// serialized by the `Mutex`; the remaining fields (`AtomicBool`,
// `Arc<BridgeShared>`) are `Send + Sync` already.
unsafe impl Sync for IosPlatformStream {}

impl IosPlatformStream {
    /// Stops the engine + tap (once) and signals the bridge terminal.
    ///
    /// Idempotent: the `swap(false)` ensures the ObjC teardown and the
    /// terminal transition run at most once; later calls are no-ops. Shared
    /// by [`stop_capture`](PlatformStream::stop_capture) and `Drop` so
    /// dropping the handle (without an explicit stop) also lands the stream
    /// terminal.
    fn stop_engine(&self) -> AudioResult<()> {
        if !self.is_active.swap(false, Ordering::SeqCst) {
            // Already stopped — idempotent no-op.
            return Ok(());
        }

        {
            let capture = self.capture.lock().map_err(|_| AudioError::InternalError {
                message: "AVAudioEngine capture mutex poisoned".to_string(),
                source: None,
            })?;
            // removeTapOnBus:0 first, then engine stop (no further tap
            // invocations are queued once this returns).
            capture.stop();
        }

        // Producer-terminal-signal (FH-1 / ADR-0010): the engine is stopped
        // and the tap removed, so no more pushes can occur — drive the bridge
        // to the graceful ending state (the `signal_done` semantics, applied
        // via the shared handle because the `BridgeProducer` itself lives
        // inside AVFAudio's copy of the tap block). `Running → Stopping`
        // keeps a buffered tail drainable; the CAS no-ops if the state
        // already advanced (e.g. `BridgeStream::stop` got there first).
        let _ = self
            .terminal
            .state
            .transition(StreamState::Running, StreamState::Stopping);
        // Wake a parked blocking reader so it observes the ending state
        // promptly (PU-5). Non-RT stop path — the notify is allowed here
        // (ADR-0001 forbids it only on the RT callback push path).
        self.terminal.notify_wake();
        #[cfg(feature = "async-stream")]
        self.terminal.waker.wake();

        Ok(())
    }
}

impl PlatformStream for IosPlatformStream {
    fn stop_capture(&self) -> AudioResult<()> {
        self.stop_engine()
    }

    fn is_active(&self) -> bool {
        self.is_active.load(Ordering::SeqCst)
    }
}

impl Drop for IosPlatformStream {
    /// Deterministic shutdown: stop the engine (and signal the bridge
    /// terminal) if the stream is still active, so dropping the handle never
    /// leaves a running tap pushing into a bridge nobody reads — nor a parked
    /// reader hanging (ADR-0010).
    fn drop(&mut self) {
        if self.is_active.load(Ordering::SeqCst) {
            if let Err(e) = self.stop_engine() {
                log::warn!("IosPlatformStream::drop: engine stop failed: {:?}", e);
            }
        }
    }
}

// ── Target resolution (mic-only slice) ───────────────────────────────────

/// Returns `true` if a [`CaptureTarget::Device`] id selects the single
/// logical iOS input device.
///
/// Accepts [`DEFAULT_INPUT_DEVICE_ID`] case-insensitively, plus the empty
/// string (the "default endpoint" convention shared with the Windows
/// backend).
fn is_default_input_id(id: &str) -> bool {
    id.is_empty() || id.eq_ignore_ascii_case(DEFAULT_INPUT_DEVICE_ID)
}

/// Validates a [`CaptureTarget`] against the iOS **mic slice** (ADR-0013).
///
/// | Target | Outcome |
/// |---|---|
/// | `Device("default")` (or `""`) | `Ok(())` — the microphone |
/// | `Device(other)` | [`AudioError::DeviceNotFound`] |
/// | `SystemDefault` | [`AudioError::PlatformNotSupported`] — **pending** rsac-b3aa (ReplayKit) |
/// | `Application` / `ApplicationByName` / `ProcessTree` | [`AudioError::PlatformNotSupported`] — **permanent** (no iOS API) |
///
/// The match is intentionally exhaustive (no wildcard): a new
/// `CaptureTarget` variant must be classified here before the crate
/// compiles for iOS.
fn ensure_mic_target(target: &CaptureTarget) -> AudioResult<()> {
    match target {
        CaptureTarget::Device(id) if is_default_input_id(&id.0) => Ok(()),
        CaptureTarget::Device(id) => Err(AudioError::DeviceNotFound {
            device_id: id.0.clone(),
        }),
        CaptureTarget::SystemDefault => Err(AudioError::PlatformNotSupported {
            feature: "system-audio capture on iOS: SystemDefault is the ReplayKit \
                      Broadcast Upload Extension path, which is not wired yet \
                      (rsac-b3aa). Supported today: the microphone via \
                      CaptureTarget::Device(DeviceId(\"default\".into())). \
                      Per-application capture does not exist on iOS, permanently"
                .to_string(),
            platform: "ios".to_string(),
        }),
        CaptureTarget::Application(_)
        | CaptureTarget::ApplicationByName(_)
        | CaptureTarget::ProcessTree(_) => Err(AudioError::PlatformNotSupported {
            feature: "per-application / process-tree capture on iOS: Apple \
                      provides no API for capturing another app's audio — this \
                      is permanent, not a pending feature (ADR-0013). Supported \
                      today: the microphone via \
                      CaptureTarget::Device(DeviceId(\"default\".into())); \
                      system audio arrives with the ReplayKit broadcast path \
                      (rsac-b3aa)"
                .to_string(),
            platform: "ios".to_string(),
        }),
    }
}

// ── Factory ──────────────────────────────────────────────────────────────

/// Creates and starts an AVAudioEngine microphone capture, returning the
/// [`IosPlatformStream`] handle plus the **delivered** (session-native)
/// [`AudioFormat`].
///
/// Steps:
///
/// 1. Validate `target` against the mic slice ([`ensure_mic_target`]).
/// 2. Start the engine + input tap
///    ([`start_input_capture`]) — this also publishes the delivered format
///    on the bridge before the first push.
/// 3. Wrap the live objects in an [`IosPlatformStream`] carrying the
///    `terminal` handle (ADR-0010).
///
/// # Errors
///
/// Target errors per [`ensure_mic_target`];
/// [`AudioError::StreamCreationFailed`] from the engine start path (no
/// active input route / denied mic permission — host-app `AVAudioSession`
/// responsibilities, see the module docs of [`super`]).
pub(crate) fn create_ios_capture(
    target: &CaptureTarget,
    producer: BridgeProducer,
    terminal: Arc<BridgeShared>,
) -> AudioResult<(IosPlatformStream, AudioFormat)> {
    ensure_mic_target(target)?;

    let (capture, delivered) = start_input_capture(producer)?;

    log::debug!(
        "AVAudioEngine: capture started (target={:?}, delivered {} Hz, {} ch)",
        target,
        delivered.sample_rate,
        delivered.channels
    );

    Ok((
        IosPlatformStream {
            capture: Mutex::new(capture),
            is_active: AtomicBool::new(true),
            terminal,
        },
        delivered,
    ))
}

// ── Compile-time assertions ──────────────────────────────────────────────

/// Assert that `IosPlatformStream` is `Send + Sync` (required by
/// `PlatformStream` and `BridgeStream<S>`).
fn _assert_ios_platform_stream_send_sync() {
    fn _assert<T: Send + Sync>() {}
    _assert::<IosPlatformStream>();
}

// ══════════════════════════════════════════════════════════════════════════
// Tests — pure logic only (no ObjC): target classification. Compile for the
// iOS target under `--tests`, run on-device.
// ══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{ApplicationId, DeviceId, ProcessId};
    use crate::core::error::ErrorKind;

    #[test]
    fn default_device_ids_are_accepted() {
        for id in ["default", "DEFAULT", "Default", ""] {
            let target = CaptureTarget::Device(DeviceId(id.to_string()));
            assert!(
                ensure_mic_target(&target).is_ok(),
                "id {id:?} must select the mic"
            );
        }
    }

    #[test]
    fn unknown_device_id_is_device_not_found() {
        let target = CaptureTarget::Device(DeviceId("42".to_string()));
        match ensure_mic_target(&target).unwrap_err() {
            AudioError::DeviceNotFound { device_id } => assert_eq!(device_id, "42"),
            other => panic!("expected DeviceNotFound, got {other:?}"),
        }
    }

    #[test]
    fn system_default_is_pending_not_permanent() {
        let err = ensure_mic_target(&CaptureTarget::SystemDefault).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Platform);
        match err {
            AudioError::PlatformNotSupported { feature, platform } => {
                assert_eq!(platform, "ios");
                assert!(feature.contains("rsac-b3aa"), "pending seed: {feature}");
                assert!(feature.contains("ReplayKit"), "the real path: {feature}");
                assert!(
                    feature.contains("Device(DeviceId(\"default\""),
                    "mic guidance: {feature}"
                );
            }
            other => panic!("expected PlatformNotSupported, got {other:?}"),
        }
    }

    #[test]
    fn per_app_targets_are_permanently_unsupported() {
        let targets = [
            CaptureTarget::Application(ApplicationId("1234".to_string())),
            CaptureTarget::ApplicationByName("Safari".to_string()),
            CaptureTarget::ProcessTree(ProcessId(1234)),
        ];
        for target in targets {
            match ensure_mic_target(&target).unwrap_err() {
                AudioError::PlatformNotSupported { feature, platform } => {
                    assert_eq!(platform, "ios");
                    assert!(
                        feature.contains("permanent"),
                        "must state permanence for {target:?}: {feature}"
                    );
                    assert!(
                        feature.contains("no API"),
                        "must state the reason for {target:?}: {feature}"
                    );
                }
                other => panic!("expected PlatformNotSupported for {target:?}, got {other:?}"),
            }
        }
    }
}
