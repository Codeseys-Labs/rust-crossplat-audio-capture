//! macOS CoreAudio capture infrastructure using BridgeStream.
//!
//! This module provides `MacosPlatformStream` and `create_macos_capture()` for
//! wiring CoreAudio AUHAL capture into the lock-free ring buffer bridge.
//!
//! # Architecture
//!
//! Unlike Linux (PipeWire) and Windows (WASAPI), macOS CoreAudio manages its own
//! real-time audio thread. The AUHAL `set_input_callback` fires on CoreAudio's
//! internal thread. There is **no dedicated capture thread** — the OS callback
//! pushes audio directly into the `BridgeProducer`.
//!
//! ```text
//! CoreAudio RT Thread                   Consumer Thread
//! ──────────────────                    ───────────────
//! AUHAL input callback                  CapturingStream (BridgeConsumer)
//! BridgeProducer::push_or_drop()        BridgeStream::read_chunk()
//! ```
//!
//! The `MacosPlatformStream` wraps the `AudioUnit` handle and optional
//! `CoreAudioProcessTap` for lifecycle management.

#![cfg(target_os = "macos")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::bridge::ring_buffer::{BridgeProducer, BridgeShared};
use crate::bridge::state::StreamState;
use crate::bridge::stream::PlatformStream;
use crate::core::config::CaptureTarget;
use crate::core::error::{AudioError, AudioResult};

use super::coreaudio::map_ca_error;
use super::tap::CoreAudioProcessTap;

// Fix Group 4 & 5: Use AudioUnit::new(IOType) instead of AudioComponent/AudioComponentDescription.
// Fix Group 1: Import from coreaudio_sys (the -sys crate), not coreaudio::sys.
use coreaudio::audio_unit::{AudioUnit, Element, IOType, Scope};
use coreaudio_sys::{
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsPacked, kAudioFormatLinearPCM,
    kAudioOutputUnitProperty_CurrentDevice, kAudioOutputUnitProperty_EnableIO,
    kAudioUnitProperty_StreamFormat, AudioStreamBasicDescription,
};

/// AudioDeviceID type alias (Fix Group 3).
/// Same as CoreAudio's AudioObjectID = u32.
type AudioDeviceID = u32;

// ── MacosCaptureConfig ───────────────────────────────────────────────────

/// Resolved capture parameters passed to the CoreAudio capture setup.
///
/// This is a subset of [`AudioCaptureConfig`](crate::core::config::AudioCaptureConfig)
/// containing only the fields needed by the macOS backend to create a stream.
#[derive(Debug)]
pub(crate) struct MacosCaptureConfig {
    /// What to capture (system default, specific device, application, process tree, etc.).
    pub target: CaptureTarget,
    /// Desired sample rate in Hz (e.g., 48000).
    pub sample_rate: u32,
    /// Desired number of audio channels (e.g., 2 for stereo).
    pub channels: u16,
}

// ── MacosPlatformStream ──────────────────────────────────────────────────

/// Platform-specific stream handle for macOS (CoreAudio backend).
///
/// Wraps an `AudioUnit` (AUHAL) and optionally a `CoreAudioProcessTap`.
/// Implements [`PlatformStream`] so it can be used with
/// [`BridgeStream`](crate::bridge::stream::BridgeStream).
///
/// # Thread Safety
///
/// `MacosPlatformStream` is `Send` (required by `PlatformStream`). The inner
/// `AudioUnit` is protected by a `Mutex` for safe access from the consumer thread.
/// The `is_active` flag is atomic for lock-free status checks.
///
/// # Deterministic shutdown (M2)
///
/// Teardown order is **AudioUnit first, then ProcessTap** — the AUHAL unit reads
/// from the tap's aggregate device, so it must be fully stopped and disposed
/// before that device is destroyed. This ordering is guaranteed two ways:
///
/// 1. [`stop_capture`](Self::stop_capture) synchronously stops the AudioUnit
///    (`AudioOutputUnitStop` returns only after the IO proc has stopped) and is
///    idempotent.
/// 2. The explicit [`Drop`] impl stops the AudioUnit before any field is
///    dropped, then relies on field-declaration order (`audio_unit` →
///    `process_tap`) so the unit is disposed (via `AudioUnit`'s own `Drop`:
///    stop → uninitialize → dispose) before [`CoreAudioProcessTap`]'s `Drop`
///    destroys the aggregate device and then the tap.
pub(crate) struct MacosPlatformStream {
    /// The AUHAL AudioUnit, protected by Mutex for interior mutability.
    ///
    /// **Declared first** so it is dropped before `process_tap` (Rust drops
    /// fields in declaration order) — see the type-level "Deterministic
    /// shutdown" note.
    audio_unit: Mutex<AudioUnit>,
    /// Optional ProcessTap reference — kept alive for the lifetime of the stream.
    /// When dropped, the tap destroys the aggregate device first, then the tap.
    /// **Declared after `audio_unit`** so it outlives the AudioUnit.
    ///
    /// Held only for its `Drop` side effect (RAII teardown of the tap +
    /// aggregate device); never read directly after construction.
    #[allow(dead_code)]
    process_tap: Option<CoreAudioProcessTap>,
    /// Atomic flag: `true` while CoreAudio callbacks are active.
    is_active: AtomicBool,
    /// Producer-terminal-signal handle (FH-1 / ADR-0010): a clone of the bridge's
    /// shared state, used by [`stop_audio_unit`](Self::stop_audio_unit) to drive
    /// the bridge to a terminal/ending state once `AudioOutputUnitStop` has
    /// synchronously halted the IO proc — so a parked reader observes
    /// end-of-stream instead of hanging on a stopped/dropped CoreAudio stream.
    ///
    /// **Declared LAST** on purpose: the struct has a documented Drop-order
    /// contract (`audio_unit` before `process_tap`); an `Arc<BridgeShared>` has no
    /// teardown ordering requirement, so placing it after the ordered fields
    /// leaves that contract undisturbed.
    terminal: Arc<BridgeShared>,
    /// The captured device id (or process-tap aggregate id), kept so teardown can
    /// remove the device-is-alive listener (rsac-ead3).
    device_id: AudioDeviceID,
    /// Leaked [`DeviceAliveContext`] pointer for the `kAudioDevicePropertyDeviceIsAlive`
    /// listener (rsac-ead3 / ADR-0010), or `None` if registration failed. The
    /// listener drives the bridge terminal on spontaneous device death so a parked
    /// reader does not hang. Removed (best-effort, context intentionally leaked)
    /// at teardown; see [`super::coreaudio::register_device_alive_listener`].
    ///
    /// SAFETY (for the existing `unsafe impl Send/Sync` below): this raw pointer
    /// is only ever read by CoreAudio's listener thread via the FFI cookie and
    /// passed to `AudioObjectRemovePropertyListener` at teardown; rsac code never
    /// derefs it across threads. Its pointee's fields (`AudioObjectID`,
    /// `Arc<BridgeShared>`) are `Send`/`Sync`, so it does not change the existing
    /// thread-safety story.
    device_alive_ctx: Option<*mut super::coreaudio::DeviceAliveContext>,
}

impl MacosPlatformStream {
    /// Synchronously stops the AudioUnit, best-effort. Returns the `OSStatus`
    /// mapping error if the stop call failed. Idempotent: stopping an
    /// already-stopped unit is a no-op at the CoreAudio level.
    ///
    /// Marked private; `stop_capture` is the public (trait) entry point and
    /// `drop` reuses the same logic.
    fn stop_audio_unit(&self) -> AudioResult<()> {
        // `stop()` requires `&mut self` on the AudioUnit, so the lock guard must
        // be `mut`. `AudioOutputUnitStop` is synchronous — on return the IO proc
        // has stopped and no further input callbacks will fire.
        let mut au = self
            .audio_unit
            .lock()
            .map_err(|_| AudioError::InternalError {
                message: "AudioUnit mutex poisoned".to_string(),
                source: None,
            })?;
        au.stop().map_err(map_ca_error)?;
        self.is_active.store(false, Ordering::SeqCst);

        // Producer-terminal-signal (FH-1 / ADR-0010): now that the IO proc has
        // SYNCHRONOUSLY stopped (`AudioOutputUnitStop` returns only after no
        // further input callbacks can fire), drive the bridge to a graceful
        // ending state so a parked reader unblocks promptly instead of hanging
        // on a stopped stream. This MUST come after `au.stop()` — signaling
        // before it could let an in-flight callback push a buffer past the
        // declared end. Reached by both `stop_capture()` and `Drop` via this
        // single choke point, so dropping the handle (not just calling stop())
        // also lands the stream terminal.
        //
        // `Running → Stopping` is the graceful sibling (matches
        // `BridgeStream::stop` semantics: the consumer-side stop then completes
        // `Stopping → Stopped`); a buffered tail stays drainable. Idempotent and
        // lock-free: the CAS no-ops if the state already advanced past `Running`
        // (e.g. `BridgeStream::stop` already moved it to Stopping/Stopped), and
        // the call is a single atomic store + a waker wake — no alloc, no lock,
        // safe even though it shares the choke point with `Drop` (ADR-0001).
        let _ = self
            .terminal
            .state
            .transition(StreamState::Running, StreamState::Stopping);
        #[cfg(feature = "async-stream")]
        self.terminal.waker.wake();

        Ok(())
    }
}

impl PlatformStream for MacosPlatformStream {
    fn stop_capture(&self) -> AudioResult<()> {
        self.stop_audio_unit()
    }

    fn is_active(&self) -> bool {
        self.is_active.load(Ordering::SeqCst)
    }
}

impl Drop for MacosPlatformStream {
    /// Deterministic shutdown (M2): stop the AudioUnit synchronously *before*
    /// the struct's fields are dropped, guaranteeing the IO proc has stopped
    /// before the aggregate device / tap are destroyed. Field-declaration order
    /// (`audio_unit` then `process_tap`) then disposes the unit before the tap's
    /// own `Drop` tears down the aggregate device and the tap.
    fn drop(&mut self) {
        // rsac-ead3: remove the device-is-alive listener FIRST, so an app-driven
        // teardown does not race the death-watch proc. Best-effort + the context
        // is intentionally NOT freed (CoreAudio gives no in-flight-proc barrier);
        // see `remove_device_alive_listener`. A normal stop signals the terminal
        // via `stop_audio_unit` below — the death watch is only for the
        // no-stop/no-Drop spontaneous-death case.
        if let Some(ctx) = self.device_alive_ctx.take() {
            // SAFETY: `ctx` came from `register_device_alive_listener` for
            // `self.device_id` and has not been removed before (we `take()` it).
            unsafe {
                super::coreaudio::remove_device_alive_listener(self.device_id, ctx);
            }
        }
        if self.is_active.load(Ordering::SeqCst) {
            if let Err(e) = self.stop_audio_unit() {
                log::warn!("MacosPlatformStream::drop: AudioUnit stop failed: {:?}", e);
            }
        }
        // `audio_unit` (Mutex<AudioUnit>) drops next → AudioUnit::Drop performs
        // stop → uninitialize → free callbacks → dispose. Then `process_tap`
        // drops → CoreAudioProcessTap::Drop destroys the aggregate device first,
        // then the tap.
    }
}

// ── Factory Function ─────────────────────────────────────────────────────

/// Creates and starts a CoreAudio capture session, returning a `MacosPlatformStream`.
///
/// This is the primary entry point for the macOS backend. It:
/// 1. Matches on the `CaptureTarget` variant to determine the device/tap to use
/// 2. Creates and configures an AUHAL `AudioUnit`
/// 3. Registers an input callback that pushes audio into the `BridgeProducer` (lock-free)
/// 4. Starts the AudioUnit
/// 5. Returns the `MacosPlatformStream` handle
///
/// # Arguments
///
/// * `config` — Resolved capture parameters.
/// * `producer` — The `BridgeProducer` to push captured audio into.
/// * `terminal` — A clone of the bridge's `BridgeShared` (producer-terminal-signal,
///   FH-1 / ADR-0010). Stored on the returned [`MacosPlatformStream`] so its
///   `stop_capture`/`Drop` choke point can drive the bridge to a terminal/ending
///   state once the IO proc has stopped, preventing a parked reader from hanging.
///
/// # Errors
///
/// Returns `AudioError` if any CoreAudio operation fails (component lookup,
/// AudioUnit creation, property setting, initialization, or start).
pub(crate) fn create_macos_capture(
    config: MacosCaptureConfig,
    mut producer: BridgeProducer,
    terminal: Arc<BridgeShared>,
) -> AudioResult<MacosPlatformStream> {
    // ── Step 1: Resolve target to device ID and optional ProcessTap ──

    let (device_id, process_tap) = resolve_capture_target(&config)?;

    // ── Step 2: Create AUHAL AudioUnit (Fix Group 4) ──
    // Use AudioUnit::new(IOType::HalOutput) instead of manual AudioComponent lookup.
    // IOType::HalOutput handles the component description internally.

    let mut audio_unit = AudioUnit::new(IOType::HalOutput).map_err(map_ca_error)?;

    // ── Step 3: Configure the AudioUnit ──

    // Set current device
    audio_unit
        .set_property(
            kAudioOutputUnitProperty_CurrentDevice,
            Scope::Global,
            Element::Output,
            Some(&device_id),
        )
        .map_err(map_ca_error)?;

    // Enable IO for input (capture) on input bus
    let enable_io: u32 = 1;
    audio_unit
        .set_property(
            kAudioOutputUnitProperty_EnableIO,
            Scope::Input,
            Element::Input,
            Some(&enable_io),
        )
        .map_err(map_ca_error)?;

    // Disable IO for output on output bus
    let disable_io: u32 = 0;
    audio_unit
        .set_property(
            kAudioOutputUnitProperty_EnableIO,
            Scope::Output,
            Element::Output,
            Some(&disable_io),
        )
        .map_err(map_ca_error)?;

    // Build ASBD for interleaved F32
    let asbd = build_f32_asbd(config.sample_rate, config.channels);

    // Set stream format on OUTPUT scope of INPUT bus (what CoreAudio delivers to us)
    audio_unit
        .set_property(
            kAudioUnitProperty_StreamFormat,
            Scope::Output,
            Element::Input,
            Some(&asbd),
        )
        .map_err(map_ca_error)?;

    // Set stream format on INPUT scope of OUTPUT bus (matching format)
    audio_unit
        .set_property(
            kAudioUnitProperty_StreamFormat,
            Scope::Input,
            Element::Output,
            Some(&asbd),
        )
        .map_err(map_ca_error)?;

    // Initialize the AudioUnit
    audio_unit.initialize().map_err(map_ca_error)?;

    // ── Step 4: Register input callback that pushes to BridgeProducer ──
    // Fix Group 6: Use the high-level coreaudio-rs callback API instead of
    // manually allocating AudioBufferList and calling AudioUnitRender.
    // The `set_input_callback` handles buffer management and render internally,
    // providing audio data directly via `args.data.buffer`.

    let channels = config.channels;
    let sample_rate = config.sample_rate;

    audio_unit
        .set_input_callback(
            move |args: coreaudio::audio_unit::render_callback::Args<
                coreaudio::audio_unit::render_callback::data::Interleaved<f32>,
            >| {
                // REAL-TIME SAFETY:
                // - BridgeProducer::push_samples_guarded() is lock-free (rtrb)
                // - Uses internal scratch buffer to avoid heap allocation when
                //   ring buffer is full (back-pressure). On successful push,
                //   one copy from the callback slice into a Vec is unavoidable
                //   since AudioBuffer owns its data.
                // - No locks, no blocking I/O
                //
                // FFI-BOUNDARY PANIC GUARD (PS-4 / rsac-5a48): this closure runs
                // on CoreAudio's own real-time IO proc — a *foreign* C callback
                // context. A panic unwinding out of here would cross the FFI
                // boundary back into the CoreAudio engine, which is undefined
                // behavior (it can corrupt or abort the process). So we use the
                // panic-GUARDED push: `push_samples_guarded` wraps the push in
                // `catch_unwind`, so a panic can never escape into the C frame —
                // on a caught panic it logs once, counts a drop, and poisons the
                // stream to Error (a parked reader then observes end-of-stream).
                // The guard is alloc-free on the happy path (the closure only
                // borrows `&mut producer` and `data`), so ADR-0001 is preserved.
                // Windows runs the equivalent push on rsac's *own* Rust thread,
                // where an unwind is well-defined, so it stays unguarded.

                let data: &[f32] = args.data.buffer;

                if !data.is_empty() {
                    producer.push_samples_guarded(data, channels, sample_rate);
                }

                Ok(())
            },
        )
        .map_err(map_ca_error)?;

    // ── Step 5: Start the AudioUnit ──

    audio_unit.start().map_err(map_ca_error)?;

    log::debug!(
        "CoreAudio: capture started (target={:?}, {}Hz, {}ch)",
        config.target,
        config.sample_rate,
        config.channels
    );

    // ── Step 6: Register the device-is-alive listener (rsac-ead3 / ADR-0010) ──
    // So a spontaneous device/tap death with NO stop()/Drop drives the bridge
    // terminal and unblocks a parked reader instead of hanging. Best-effort: on
    // failure the stream still works (it just won't auto-catch a death), so we
    // do NOT propagate the error. Uses a clone of `terminal` before it is moved
    // into the struct below.
    let device_alive_ctx =
        super::coreaudio::register_device_alive_listener(device_id, terminal.clone());

    // ── Step 7: Return the platform stream handle ──

    Ok(MacosPlatformStream {
        audio_unit: Mutex::new(audio_unit),
        process_tap,
        is_active: AtomicBool::new(true),
        terminal,
        device_id,
        device_alive_ctx,
    })
}

// ── Helper: Resolve CaptureTarget ────────────────────────────────────────

/// Resolves a [`CaptureTarget`] to a CoreAudio `AudioDeviceID` and optional
/// `CoreAudioProcessTap`.
///
/// | Target                 | Strategy                                                    |
/// |------------------------|-------------------------------------------------------------|
/// | `SystemDefault`        | `CoreAudioProcessTap::new_system()` → global tap + agg dev |
/// | `Device(id)`           | Parse `DeviceId.0` as `u32`; require INPUT streams (M8)     |
/// | `Application(pid)`     | `CoreAudioProcessTap::new(pid)` → tap's AudioObjectID      |
/// | `ApplicationByName(n)` | `enumerate_audio_applications()` → find PID → tap           |
/// | `ProcessTree(pid)`     | `CoreAudioProcessTap::new_tree(pid)` → multi-PID tap       |
fn resolve_capture_target(
    config: &MacosCaptureConfig,
) -> AudioResult<(AudioDeviceID, Option<CoreAudioProcessTap>)> {
    match &config.target {
        CaptureTarget::SystemDefault => {
            // System-wide capture via Process Tap + Aggregate Device.
            // Direct AUHAL input capture from the default output device does NOT work
            // (the output device's AUHAL callback never fires). The Process Tap pattern
            // is required even for system-wide capture on macOS 14.4+.
            let tap = CoreAudioProcessTap::new_system()?;
            let tap_device_id = tap.id();
            log::debug!(
                "CoreAudio: SystemDefault → tap aggregate device_id={}",
                tap_device_id
            );
            Ok((tap_device_id, Some(tap)))
        }

        CaptureTarget::Device(device_id) => {
            let id: u32 = device_id
                .0
                .parse()
                .map_err(|_| AudioError::DeviceNotFound {
                    device_id: device_id.0.clone(),
                })?;

            // M8: AUHAL's input callback only fires when the configured device
            // actually has INPUT streams. Feeding an output-only device (the
            // common case — `MacosDeviceEnumerator` enumerates the system's
            // output devices for loopback) straight to AUHAL produces a
            // "running" stream whose callback never fires — a silently-dead
            // capture. CoreAudio offers no way to loop back an arbitrary output
            // device through AUHAL directly; that requires a Process Tap +
            // aggregate device, which on this backend is keyed to the *default*
            // output device (see `CoreAudioProcessTap::new_system`). So rather
            // than return a dead stream, we verify the device has input streams
            // and reject output-only devices with an actionable error.
            match device_has_input_streams(id) {
                Ok(true) => {
                    log::debug!("CoreAudio: Device target → input device_id={}", id);
                    Ok((id, None))
                }
                // This is a platform-capability limitation, not a format
                // mismatch — use PlatformNotSupported (ErrorKind::Platform) so
                // callers branching on kind()/recoverability() classify it
                // correctly, rather than UnsupportedFormat (Configuration).
                Ok(false) => Err(AudioError::PlatformNotSupported {
                    feature: format!(
                        "capturing device {} directly: it has no input streams, and direct \
                         AUHAL capture from an output-only device is not supported on macOS \
                         (the input callback never fires). Use CaptureTarget::SystemDefault \
                         for system-audio loopback (routed through a CoreAudio Process Tap), \
                         or select a device that exposes input streams",
                        id
                    ),
                    platform: "CoreAudio".to_string(),
                }),
                Err(e) => {
                    // Could not probe the device's stream configuration. Surface
                    // the backend error rather than returning a possibly-dead
                    // stream.
                    log::warn!(
                        "CoreAudio: could not probe input streams for device {}: {:?}",
                        id,
                        e
                    );
                    Err(e)
                }
            }
        }

        CaptureTarget::Application(app_id) => {
            let pid: u32 = app_id
                .0
                .parse()
                .map_err(|_| AudioError::ApplicationNotFound {
                    identifier: format!(
                        "Cannot parse PID from ApplicationId '{}': expected numeric PID",
                        app_id.0
                    ),
                })?;

            let tap = CoreAudioProcessTap::new(pid, &format!("rsac-tap-{}", pid))?;
            let tap_device_id = tap.id();
            log::debug!(
                "CoreAudio: Application target (PID={}) → tap_id={}",
                pid,
                tap_device_id
            );
            Ok((tap_device_id, Some(tap)))
        }

        CaptureTarget::ApplicationByName(name) => {
            // Enumerate running applications and find the first match.
            //
            // L3: Use EXACT case-insensitive matching (algorithm now unified
            // across all three platforms — substring `.contains` matching has
            // been removed). Windows matches the OS process name (e.g.
            // "firefox.exe"), Linux matches the PipeWire `application.name` /
            // binary, and macOS matches the localized app name reported by
            // `NSRunningApplication.localizedName` (e.g. "Safari", "Music").
            // The matched FIELD necessarily differs per platform; only the
            // matching algorithm (exact, case-insensitive) is shared.
            let apps = super::coreaudio::enumerate_audio_applications()?;
            let app = apps
                .iter()
                .find(|a| app_name_matches(&a.name, name))
                .ok_or_else(|| AudioError::ApplicationNotFound {
                    identifier: format!("No running application matching name '{}'", name),
                })?;

            let pid = app.process_id;
            let tap = CoreAudioProcessTap::new(pid, &format!("rsac-tap-{}", pid))?;
            let tap_device_id = tap.id();
            log::debug!(
                "CoreAudio: ApplicationByName('{}') → PID={}, tap_id={}",
                name,
                pid,
                tap_device_id
            );
            Ok((tap_device_id, Some(tap)))
        }

        CaptureTarget::ProcessTree(pid) => {
            // Multi-PID tap: captures parent process + all direct child processes
            let tap = CoreAudioProcessTap::new_tree(pid.0)?;
            let tap_device_id = tap.id();
            log::debug!(
                "CoreAudio: ProcessTree (parent PID={}) → tap_id={}",
                pid.0,
                tap_device_id
            );
            Ok((tap_device_id, Some(tap)))
        }
    }
}

// ── Helper: ApplicationByName matching (L3) ──────────────────────────────

/// Matches a candidate application's localized name against a user-supplied
/// `ApplicationByName` query.
///
/// L3: This is an **exact, case-insensitive** comparison, consistent with the
/// Windows backend (exact process-name match) and the Linux backend
/// (`eq_ignore_ascii_case` on `application.name`). It deliberately does NOT do
/// substring matching, which previously diverged from the other platforms and
/// could resolve "Music" to "Apple Music".
fn app_name_matches(candidate: &str, query: &str) -> bool {
    candidate.eq_ignore_ascii_case(query)
}

// ── Helper: Probe device input streams (M8) ──────────────────────────────

/// Returns `true` if the CoreAudio device with the given `AudioDeviceID`
/// exposes INPUT streams (i.e. AUHAL's input callback can fire for it).
///
/// AUHAL only delivers audio to its input callback when the configured device
/// has input streams. An output-only device (the typical loopback target) has
/// none, so configuring AUHAL against it yields a "running" stream whose
/// callback never fires. We use this probe to reject such devices with a clear
/// error rather than returning a silently-dead stream (M8).
///
/// This delegates to `coreaudio::audio_unit::macos_helpers::get_audio_device_supports_scope`,
/// which queries `kAudioDevicePropertyStreamConfiguration` on the input scope
/// and checks whether any buffer reports `mNumberChannels > 0`. Reusing the
/// crate's helper avoids hand-rolling the variable-length `AudioBufferList` FFI.
fn device_has_input_streams(device_id: AudioDeviceID) -> AudioResult<bool> {
    use coreaudio::audio_unit::macos_helpers::get_audio_device_supports_scope;
    get_audio_device_supports_scope(device_id, Scope::Input).map_err(map_ca_error)
}

// ── Helper: Build F32 ASBD ───────────────────────────────────────────────

/// Builds an `AudioStreamBasicDescription` for interleaved F32 PCM.
fn build_f32_asbd(sample_rate: u32, channels: u16) -> AudioStreamBasicDescription {
    let bytes_per_sample: u32 = 4; // f32
    let bytes_per_frame = bytes_per_sample * channels as u32;

    AudioStreamBasicDescription {
        mSampleRate: sample_rate as f64,
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
        mBytesPerPacket: bytes_per_frame,
        mFramesPerPacket: 1, // Uncompressed PCM
        mBytesPerFrame: bytes_per_frame,
        mChannelsPerFrame: channels as u32,
        mBitsPerChannel: 32,
        mReserved: 0,
    }
}

// ── Compile-time assertions ──────────────────────────────────────────────

/// Assert that `MacosPlatformStream` is `Send` (required by `PlatformStream`).
fn _assert_macos_platform_stream_send() {
    fn _assert<T: Send>() {}
    _assert::<MacosPlatformStream>();
}

// ══════════════════════════════════════════════════════════════════════════
// Tests
// ══════════════════════════════════════════════════════════════════════════

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use crate::core::config::{ApplicationId, CaptureTarget, DeviceId};
    use coreaudio_sys::kAudioFormatFlagIsNonInterleaved;

    // ── build_f32_asbd tests ─────────────────────────────────────────

    #[test]
    fn build_f32_asbd_48k_stereo() {
        let asbd = build_f32_asbd(48000, 2);

        assert_eq!(asbd.mSampleRate, 48000.0);
        assert_eq!(asbd.mFormatID, kAudioFormatLinearPCM);
        assert_ne!(
            asbd.mFormatFlags & kAudioFormatFlagIsFloat,
            0,
            "should have Float flag"
        );
        assert_ne!(
            asbd.mFormatFlags & kAudioFormatFlagIsPacked,
            0,
            "should have Packed flag"
        );
        assert_eq!(asbd.mChannelsPerFrame, 2);
        assert_eq!(asbd.mBitsPerChannel, 32);
        assert_eq!(asbd.mBytesPerFrame, 8); // 4 bytes * 2 channels
        assert_eq!(asbd.mBytesPerPacket, 8);
        assert_eq!(asbd.mFramesPerPacket, 1); // uncompressed PCM
        assert_eq!(asbd.mReserved, 0);
    }

    #[test]
    fn build_f32_asbd_44100_mono() {
        let asbd = build_f32_asbd(44100, 1);

        assert_eq!(asbd.mSampleRate, 44100.0);
        assert_eq!(asbd.mChannelsPerFrame, 1);
        assert_eq!(asbd.mBytesPerFrame, 4); // 4 bytes * 1 channel
        assert_eq!(asbd.mBytesPerPacket, 4);
        assert_eq!(asbd.mBitsPerChannel, 32);
    }

    #[test]
    fn build_f32_asbd_96k_8ch() {
        let asbd = build_f32_asbd(96000, 8);

        assert_eq!(asbd.mSampleRate, 96000.0);
        assert_eq!(asbd.mChannelsPerFrame, 8);
        assert_eq!(asbd.mBytesPerFrame, 32); // 4 bytes * 8 channels
        assert_eq!(asbd.mBytesPerPacket, 32);
    }

    #[test]
    fn build_f32_asbd_does_not_set_non_interleaved() {
        let asbd = build_f32_asbd(48000, 2);
        assert_eq!(
            asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved,
            0,
            "should NOT have NonInterleaved flag (we use interleaved)"
        );
    }

    // ── app_name_matches (L3) ────────────────────────────────────────

    #[test]
    fn app_name_matches_is_exact_case_insensitive() {
        // Exact match, any case → true
        assert!(app_name_matches("Safari", "safari"));
        assert!(app_name_matches("Music", "MUSIC"));
        assert!(app_name_matches("Music", "Music"));
    }

    #[test]
    fn app_name_matches_rejects_substrings() {
        // L3: substring matches must NOT succeed (this was the old `.contains`
        // bug — "Music" would match "Apple Music").
        assert!(!app_name_matches("Apple Music", "Music"));
        assert!(!app_name_matches("Safari Technology Preview", "Safari"));
        assert!(!app_name_matches("Music", "Apple Music"));
    }

    // ── MacosCaptureConfig construction ──────────────────────────────

    #[test]
    fn capture_config_debug_format() {
        let config = MacosCaptureConfig {
            target: CaptureTarget::SystemDefault,
            sample_rate: 48000,
            channels: 2,
        };
        let debug = format!("{:?}", config);
        assert!(debug.contains("SystemDefault"));
        assert!(debug.contains("48000"));
        assert!(debug.contains("2"));
    }

    // ── resolve_capture_target tests (require audio hardware) ────────

    #[test]
    #[ignore = "requires macOS 14.4+ audio hardware"]
    fn resolve_system_default_returns_valid_device_id() {
        let config = MacosCaptureConfig {
            target: CaptureTarget::SystemDefault,
            sample_rate: 48000,
            channels: 2,
        };

        let result = resolve_capture_target(&config);
        assert!(
            result.is_ok(),
            "resolve SystemDefault should succeed: {:?}",
            result.err()
        );

        let (device_id, process_tap) = result.unwrap();
        assert!(device_id > 0, "device_id should be > 0, got {}", device_id);
        assert!(
            process_tap.is_some(),
            "SystemDefault should create a system-wide ProcessTap"
        );
    }

    #[test]
    #[ignore = "requires macOS audio hardware with an input device"]
    fn resolve_device_by_id_succeeds_for_default_input() {
        // M8: A Device target only works when the device exposes input streams.
        // Use the default INPUT device (get_default_device_id(true)) — it has
        // input streams, so resolve should succeed with no ProcessTap.
        use coreaudio::audio_unit::macos_helpers::get_default_device_id;
        let input_id = match get_default_device_id(true) {
            Some(id) => id,
            None => {
                eprintln!("no default input device on this host; skipping");
                return;
            }
        };

        let config = MacosCaptureConfig {
            target: CaptureTarget::Device(DeviceId(input_id.to_string())),
            sample_rate: 48000,
            channels: 2,
        };

        let result = resolve_capture_target(&config);
        assert!(
            result.is_ok(),
            "resolve Device(input) should succeed: {:?}",
            result.err()
        );

        let (device_id, process_tap) = result.unwrap();
        assert_eq!(
            device_id, input_id,
            "resolved device_id should match requested"
        );
        assert!(
            process_tap.is_none(),
            "Device target should not create a ProcessTap"
        );
    }

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn resolve_output_only_device_is_rejected() {
        // M8: Feeding an output-only device (the default OUTPUT device) to AUHAL
        // input does not work — the callback never fires. resolve_capture_target
        // must reject it with a clear error rather than returning a dead stream.
        use coreaudio::audio_unit::macos_helpers::{
            get_audio_device_supports_scope, get_default_device_id,
        };
        use coreaudio::audio_unit::Scope;

        let output_id = get_default_device_id(false).expect("should get default output device");

        // Only meaningful if this output device genuinely lacks input streams
        // (true for typical built-in speakers / external DACs).
        if get_audio_device_supports_scope(output_id, Scope::Input).unwrap_or(false) {
            eprintln!(
                "default output device {} also has input streams; skipping",
                output_id
            );
            return;
        }

        let config = MacosCaptureConfig {
            target: CaptureTarget::Device(DeviceId(output_id.to_string())),
            sample_rate: 48000,
            channels: 2,
        };

        let result = resolve_capture_target(&config);
        match result {
            Err(AudioError::PlatformNotSupported { feature, .. }) => {
                assert!(
                    feature.contains("no input streams"),
                    "error should explain the output-only device problem, got: {}",
                    feature
                );
            }
            other => panic!(
                "Expected PlatformNotSupported for output-only device, got: {:?}",
                other
            ),
        }
    }

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn resolve_device_invalid_id_returns_error() {
        let config = MacosCaptureConfig {
            target: CaptureTarget::Device(DeviceId("not-a-number".to_string())),
            sample_rate: 48000,
            channels: 2,
        };

        let result = resolve_capture_target(&config);
        assert!(result.is_err(), "invalid device ID should return error");
        match result.unwrap_err() {
            AudioError::DeviceNotFound { device_id } => {
                assert_eq!(device_id, "not-a-number");
            }
            other => panic!("Expected DeviceNotFound, got: {:?}", other),
        }
    }

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn resolve_application_by_name_nonexistent_returns_error() {
        let config = MacosCaptureConfig {
            target: CaptureTarget::ApplicationByName(
                "ThisApplicationDefinitelyDoesNotExist_12345".to_string(),
            ),
            sample_rate: 48000,
            channels: 2,
        };

        let result = resolve_capture_target(&config);
        assert!(result.is_err(), "nonexistent app name should return error");
        match result.unwrap_err() {
            AudioError::ApplicationNotFound { identifier } => {
                assert!(
                    identifier.contains("ThisApplicationDefinitelyDoesNotExist"),
                    "error should contain the app name, got: {}",
                    identifier
                );
            }
            other => panic!("Expected ApplicationNotFound, got: {:?}", other),
        }
    }

    #[test]
    #[ignore = "requires macOS 14.4+ audio hardware"]
    fn resolve_application_by_pid_smoke_test() {
        // Use the current process PID — it won't necessarily produce audio,
        // but tests the tap creation path. Expect either success or a specific
        // error (e.g., the process isn't an audio source).
        let current_pid = std::process::id();
        let config = MacosCaptureConfig {
            target: CaptureTarget::Application(ApplicationId(current_pid.to_string())),
            sample_rate: 48000,
            channels: 2,
        };

        let result = resolve_capture_target(&config);
        // Either succeeds (tap created) or fails with a backend error
        // (process isn't an audio source). Both are valid outcomes.
        match &result {
            Ok((device_id, tap)) => {
                assert!(*device_id > 0, "tap device_id should be > 0");
                assert!(
                    tap.is_some(),
                    "Application target should create a ProcessTap"
                );
            }
            Err(AudioError::BackendError { .. }) => {
                // Expected: the current process may not be a valid audio source
            }
            Err(AudioError::InternalError { .. }) => {
                // Expected: CATapDescription or process tap API might not be available
            }
            Err(other) => {
                panic!("Unexpected error type for Application target: {:?}", other);
            }
        }
    }

    // ── Full stream creation tests (require audio hardware) ──────────

    #[test]
    #[ignore = "requires macOS audio hardware"]
    fn create_macos_capture_system_default() {
        use crate::bridge::calculate_capacity;
        use crate::bridge::ring_buffer::create_bridge;
        use crate::core::config::AudioFormat;

        let format = AudioFormat::default();
        let capacity = calculate_capacity(None, 4);
        let (producer, consumer) = create_bridge(capacity, format);

        // Producer-terminal-signal (FH-1 / ADR-0010): the platform stream needs a
        // clone of the bridge's shared state. Bring the session to Running so the
        // graceful `stop_capture` transition (Running → Stopping) below applies.
        let terminal = Arc::clone(consumer.shared());
        terminal.state.force_set(StreamState::Running);

        let config = MacosCaptureConfig {
            target: CaptureTarget::SystemDefault,
            sample_rate: 48000,
            channels: 2,
        };

        let result = create_macos_capture(config, producer, terminal);
        assert!(
            result.is_ok(),
            "create_macos_capture should succeed: {:?}",
            result.err()
        );

        let stream = result.unwrap();
        assert!(stream.is_active(), "stream should be active after creation");

        // Clean up: stop the stream
        let stop_result = stream.stop_capture();
        assert!(stop_result.is_ok(), "stop should succeed");
        assert!(
            !stream.is_active(),
            "stream should not be active after stop"
        );
        // Producer-terminal-signal: stop_capture drove the bridge to a terminal/
        // ending state (Running → Stopping) so a parked reader unblocks.
        assert_eq!(
            consumer.shared().state.get(),
            StreamState::Stopping,
            "stop_capture must drive the bridge to the graceful Stopping state (FH-1)"
        );
    }

    // ── Compile-time trait assertions ────────────────────────────────

    #[test]
    fn macos_platform_stream_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<MacosPlatformStream>();
    }
}
