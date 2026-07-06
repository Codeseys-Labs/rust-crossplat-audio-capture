//! PipeWire dedicated thread infrastructure.
//!
//! This module provides the thread + channel infrastructure for running PipeWire
//! objects (`Rc`/`!Send`) on a dedicated thread, communicating with the caller
//! via `std::sync::mpsc` channels.
//!
//! # Architecture
//!
//! ```text
//! User Thread                          PipeWire Thread (dedicated)
//! ────────────                         ──────────────────────────
//! AudioCapture / CapturingStream       MainLoop, Context, Core, Registry
//! BridgeConsumer                       Stream, StreamListener
//! command_tx ─────mpsc::channel────►  command_rx
//!                                      BridgeProducer (writes to ring buffer)
//! ◄──────mpsc::Sender──────────────   response_tx
//! ```
//!
//! All PipeWire `Rc`-based objects live exclusively on the dedicated thread.
//! The `PipeWireThread` handle is `Send + Sync` and safe to use from any thread.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::bridge::ring_buffer::{BridgeProducer, BridgeShared};
use crate::bridge::state::StreamState;
use crate::bridge::stream::PlatformStream;
use crate::core::config::CaptureTarget;
use crate::core::error::{AudioError, AudioResult};

/// Upper bound on how long a caller will block waiting for the PipeWire thread
/// to complete a registry/metadata *snapshot* (device enumeration / default
/// resolution).
///
/// Unlike the capture handshake, a snapshot requires a `core.sync()`/`done`
/// roundtrip with the daemon so the initial registry dump can settle before we
/// read it. The roundtrip is normally a few event-loop iterations (≪1 s); the
/// bounded wait turns a wedged daemon into [`AudioError::Timeout`] rather than
/// an unbounded hang (mirrors `HANDSHAKE_TIMEOUT`).
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);

/// Upper bound on how long a caller will block waiting for the PipeWire thread
/// to acknowledge a `StartCapture` / `StopCapture` command.
///
/// The handshake reply normally arrives within one event-loop iteration
/// (≤50 ms), but `StartCapture` also creates and connects a PipeWire stream.
/// A bounded wait (audit findings M2/M3) ensures a wedged or dead PipeWire
/// thread surfaces as [`AudioError::Timeout`] instead of hanging the caller
/// on an unbounded `recv()`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

// ── CaptureConfig ────────────────────────────────────────────────────────

/// Resolved capture parameters passed to the PipeWire thread.
///
/// This is a subset of [`AudioCaptureConfig`](crate::core::config::AudioCaptureConfig)
/// containing only the fields needed by the PipeWire backend to create a stream.
#[derive(Debug)]
pub(crate) struct CaptureConfig {
    /// What to capture (system default, specific app, process tree, etc.).
    pub target: CaptureTarget,
    /// Desired sample rate in Hz (e.g., 48000).
    pub sample_rate: u32,
    /// Desired number of audio channels (e.g., 2 for stereo).
    pub channels: u16,
}

/// A [`CaptureTarget`] whose PipeWire `TARGET_OBJECT` has already been resolved.
///
/// Resolution (running `pw-dump` and walking `/proc`) is performed on the
/// **caller thread** inside [`PipeWireThread::start_capture`], *before* the
/// `StartCapture` command is sent to the PipeWire event-loop thread. This keeps
/// the event loop responsive: it never blocks on a subprocess or filesystem
/// walk while audio buffers are being pumped (audit findings M2/M3).
///
/// The event-loop handler only has to translate this into stream properties —
/// a pure, non-blocking operation.
#[derive(Debug)]
pub(crate) enum ResolvedTarget {
    /// Capture the default sink monitor — no `TARGET_OBJECT`.
    SystemDefault,
    /// Attach to a node identified by the given `object.serial` string.
    Serial(String),
}

// ── Snapshot types ─────────────────────────────────────────────────────────

/// A single audio node discovered via the native PipeWire registry.
///
/// Produced on the PipeWire thread by walking the registry's `global`
/// callbacks (audit finding H4 / `rsac-bfd8`), this replaces the per-line
/// scrape of `pw-cli list-objects` / `pw-dump`.
///
/// # Identity contract
///
/// `id` is the node's `object.serial` (falling back to the registry global
/// `id` when a node advertises no serial). This is the *same* identifier the
/// capture path keys `TARGET_OBJECT` on, so a `DeviceId` built from it
/// round-trips through [`CaptureTarget::Device`] → `PwNodeLookup::Device`
/// without a second lookup (acceptance criterion: device ids are numeric
/// `object.serial`).
#[derive(Debug, Clone)]
pub(crate) struct PwDeviceSnapshot {
    /// Node `object.serial` (or registry global `id` if no serial) as a string.
    pub id: String,
    /// Human-readable name: `node.description`, then `node.nick`, then
    /// `node.name`, then a generic placeholder.
    pub name: String,
    /// The raw `node.name` (e.g. `alsa_output.pci-...`), retained verbatim so
    /// default-device resolution can match the *name* stored in `default`
    /// metadata (which keys on `node.name`, not the friendlier description).
    /// Empty when the node advertised no `node.name`.
    pub node_name: String,
    /// The node's `media.class` (e.g. `"Audio/Sink"`, `"Audio/Source"`).
    pub media_class: String,
}

/// A single audio-producing **application** discovered via the native PipeWire
/// registry.
///
/// Produced on the PipeWire thread by walking the registry's `global` callbacks
/// (audit finding H4 part 2 / `rsac-8ebb`), this replaces the `pw-dump`
/// subprocess scrape that `list_audio_applications` previously performed.
///
/// # Predicate (parity with the old subprocess parser)
///
/// A node becomes an application source when its `media.class` contains
/// `"Stream"` (i.e. a per-application `Stream/Output/Audio` /
/// `Stream/Input/Audio` node, not a device sink/source) **and** it advertises a
/// parseable numeric `application.process.id`. Nodes without a usable PID are
/// skipped — exactly the `pid == 0` filter the subprocess parser applied.
///
/// # Identity contract
///
/// `pid` is the application's `application.process.id`; it is the same numeric
/// PID the capture path keys on, so an [`ApplicationId`] built from it
/// round-trips through [`CaptureTarget::Application`] →
/// `PwNodeLookup::ByPid` without a second lookup. `node_serial` carries the
/// node's `object.serial` (falling back to the registry global `id` when a node
/// advertises no serial) for callers that want to attach directly to the
/// stream node.
///
/// [`ApplicationId`]: crate::core::config::ApplicationId
/// [`CaptureTarget::Application`]: crate::core::config::CaptureTarget::Application
#[derive(Debug, Clone)]
pub(crate) struct PwAppSnapshot {
    /// The application's `application.process.id`.
    pub pid: u32,
    /// Human-readable application name: `application.name`, then
    /// `application.process.binary`, then a generic placeholder.
    pub app_name: String,
    /// Node `object.serial` (or registry global `id` if no serial) as a string.
    pub node_serial: String,
}

/// The default sink/source node *names* reported by the PipeWire `default`
/// metadata object.
///
/// These are node **names** (e.g. `alsa_output.pci-...`), not numeric ids —
/// the caller resolves them against the [`PwDeviceSnapshot`] list to recover a
/// round-trippable `object.serial` (same contract the old
/// `pw-metadata`-based path had).
#[derive(Debug, Clone, Default)]
pub(crate) struct PwDefaultSnapshot {
    /// `default.audio.sink` node name, if the daemon reported one.
    pub sink_name: Option<String>,
    /// `default.audio.source` node name, if the daemon reported one.
    pub source_name: Option<String>,
}

/// Mutable state populated by the registry + metadata listeners on the
/// PipeWire thread. Wrapped in `Rc<RefCell<…>>` and shared (cloned) into the
/// `Fn + 'static` global/property callbacks — the wiremix idiom. It never
/// crosses a thread boundary: only *owned* `Vec`/`Option` copies are sent back
/// over the mpsc reply channel.
#[derive(Default)]
struct RegistrySnapshot {
    /// Audio nodes discovered so far, keyed by registry global id so a single
    /// node is recorded once even if its `global` event arrives more than once.
    devices: std::collections::BTreeMap<u32, PwDeviceSnapshot>,
    /// Per-application audio stream nodes discovered so far, keyed by registry
    /// global id so re-announcement of the same node is idempotent. PID-level
    /// deduplication (one entry per process even when an app has several stream
    /// nodes) is applied when the owned snapshot is built — see
    /// [`PipeWireThread::snapshot_applications`].
    applications: std::collections::BTreeMap<u32, PwAppSnapshot>,
    /// Default sink/source names from the `default` metadata object.
    default: PwDefaultSnapshot,
}

/// Extract the node *name* from a `default.audio.sink` / `default.audio.source`
/// metadata value.
///
/// PipeWire stores these as a JSON object `{"name":"alsa_output.pci-..."}`. We
/// pull out the `name` field; if the value is not JSON (older daemons may store
/// a bare quoted string) we fall back to the de-quoted raw value. `None` input
/// (property removed) maps to `None`.
fn parse_default_metadata_name(value: Option<&str>) -> Option<String> {
    let v = value?;
    serde_json::from_str::<serde_json::Value>(v)
        .ok()
        .and_then(|j| j.get("name").and_then(|n| n.as_str()).map(str::to_owned))
        .or_else(|| Some(v.trim_matches('"').to_owned()))
}

// ── PipeWireCommand ──────────────────────────────────────────────────────

/// Commands sent from the caller thread to the dedicated PipeWire thread.
///
/// Each command that expects a response includes a `response_tx` oneshot sender
/// so the PipeWire thread can reply with the result.
pub(crate) enum PipeWireCommand {
    /// Begin capturing audio with the given configuration.
    ///
    /// The [`BridgeProducer`] is moved to the PipeWire thread — it is `Send`
    /// and will be used by the PipeWire `process` callback to push audio data
    /// into the ring buffer.
    StartCapture {
        config: CaptureConfig,
        /// `TARGET_OBJECT` resolved on the caller thread (M2/M3): the PipeWire
        /// event loop must not run `pw-dump`/`/proc` resolution itself.
        resolved: ResolvedTarget,
        producer: BridgeProducer,
        response_tx: std_mpsc::Sender<AudioResult<()>>,
    },

    /// Stop the current capture session and clean up PipeWire stream objects.
    StopCapture {
        response_tx: std_mpsc::Sender<AudioResult<()>>,
    },

    /// Snapshot the current set of audio nodes from the native registry.
    ///
    /// The handler waits for a `core.sync()`/`done` roundtrip so the initial
    /// registry dump has settled before replying — otherwise it would race an
    /// empty registry and report "no devices" on a healthy system.
    SnapshotDevices {
        response_tx: std_mpsc::Sender<AudioResult<Vec<PwDeviceSnapshot>>>,
    },

    /// Snapshot the default sink/source node names from the `default` metadata.
    ///
    /// Like [`SnapshotDevices`](PipeWireCommand::SnapshotDevices), the handler
    /// waits for a sync/done roundtrip so the metadata listener has fired
    /// before replying.
    SnapshotDefault {
        response_tx: std_mpsc::Sender<AudioResult<PwDefaultSnapshot>>,
    },

    /// Snapshot the set of audio-producing applications from the native
    /// registry (H4 part 2 / `rsac-8ebb`).
    ///
    /// Like [`SnapshotDevices`](PipeWireCommand::SnapshotDevices), the handler
    /// waits for a `core.sync()`/`done` roundtrip so the registry's initial
    /// dump has settled before replying — otherwise it would race an empty
    /// registry and report "no applications" on a host that is actively playing
    /// audio. The returned list is PID-deduplicated.
    SnapshotApplications {
        response_tx: std_mpsc::Sender<AudioResult<Vec<PwAppSnapshot>>>,
    },

    /// Resolve a capture [`TargetQuery`] against the native registry snapshot,
    /// returning the matched node's `object.serial` (H4 part 3 / `rsac-nat1`).
    ///
    /// Like [`SnapshotDevices`](PipeWireCommand::SnapshotDevices), the handler
    /// waits for a `core.sync()`/`done` roundtrip so the registry's initial dump
    /// has settled before matching — otherwise it would race an empty registry
    /// and spuriously report "no match" on a healthy system. `Ok(None)` means
    /// the registry settled but held no matching node (the caller may then try
    /// the `pw-dump` fallback); an `Err` is a spawn/roundtrip failure.
    ResolveTarget {
        query: TargetQuery,
        response_tx: std_mpsc::Sender<AudioResult<Option<String>>>,
    },

    /// Enumerate the `SPA_PARAM_EnumFormat` parameters a node advertises and map
    /// each to a [`crate::core::config::AudioFormat`] (PR-5 / `rsac-7469`).
    ///
    /// `serial` is a node's `object.serial` (the same identifier
    /// [`PwDeviceSnapshot::id`] carries). The handler resolves it to the node's
    /// registry global id, binds a `Node` proxy on the loop thread, registers a
    /// `param` listener, fires `enum_params(EnumFormat)`, and — like
    /// [`SnapshotDevices`](PipeWireCommand::SnapshotDevices) — waits for a
    /// `core.sync()`/`done` roundtrip so every emitted `param` event has settled
    /// before replying.
    ///
    /// This is **advisory discovery only** (L2 / EF-3): it never alters the
    /// authoritative connect-time format that `param_changed` negotiates and
    /// stamps onto each delivered [`AudioBuffer`](crate::core::buffer::AudioBuffer).
    /// A node that advertises no `EnumFormat` (or that cannot be resolved/bound)
    /// yields `Ok(vec![])` — never a fabricated guess.
    EnumNodeFormats {
        serial: String,
        response_tx: std_mpsc::Sender<AudioResult<Vec<crate::core::config::AudioFormat>>>,
    },

    /// Shut down the PipeWire thread entirely. No response needed — the thread exits.
    Shutdown,
}

// ── SPA → rsac format mapping ────────────────────────────────────────────

/// Map a negotiated/advertised SPA audio format to the rsac
/// [`SampleFormat`](crate::core::config::SampleFormat), or `None` for formats
/// rsac does not model (compressed, planar, unsigned, exotic widths, …).
///
/// Only the interleaved signed-integer and 32-bit float PCM families rsac
/// understands are mapped (the brief's `S16 / S24 / S32 / F32` set, including
/// each family's little-endian and host-native spellings). The `S24_32`
/// variants are 24-bit samples carried in a 32-bit container, which is exactly
/// how rsac documents [`SampleFormat::I24`](crate::core::config::SampleFormat::I24)
/// ("packed in 32-bit container"), so they map there too. Anything else returns
/// `None` so the caller simply omits it from the advisory list rather than
/// guessing — keeping the documented-empty contract honest.
#[cfg(feature = "feat_linux")]
fn spa_audio_format_to_sample_format(
    fmt: libspa::param::audio::AudioFormat,
) -> Option<crate::core::config::SampleFormat> {
    use crate::core::config::SampleFormat;
    use libspa::param::audio::AudioFormat as Spa;

    // `AudioFormat` derives `PartialEq`, so compare against the associated
    // constants directly. `S16`/`S24`/`S32` are the host-native aliases (LE on
    // the little-endian hosts PipeWire runs on); the explicit `*LE` spellings
    // cover daemons that report the canonical little-endian variant. There is
    // no plain `F32` constant in libspa, so only `F32LE` is matched for float.
    if fmt == Spa::S16 || fmt == Spa::S16LE {
        Some(SampleFormat::I16)
    } else if fmt == Spa::S24 || fmt == Spa::S24LE || fmt == Spa::S24_32LE {
        Some(SampleFormat::I24)
    } else if fmt == Spa::S32 || fmt == Spa::S32LE {
        Some(SampleFormat::I32)
    } else if fmt == Spa::F32LE {
        Some(SampleFormat::F32)
    } else {
        None
    }
}

/// Decode every **complete** little-endian `f32` from a byte slice whose start
/// is not 4-byte-aligned, into the caller-supplied reusable `scratch` (#30).
///
/// This is the fallback path for the rare case where the SPA chunk's valid
/// region (`[offset, offset + size)`) is *not* word-aligned — i.e.
/// `bytes.align_to::<f32>()` yields a non-empty head/tail. The zero-copy
/// `align_to` fast path would silently skip those edge bytes (dropping up to a
/// whole sample), so this routine instead reads the bytes in 4-byte groups via
/// `from_le_bytes` (which has no alignment requirement) so **no whole sample is
/// lost** regardless of the start offset.
///
/// # Real-time safety (ADR-0001)
///
/// `scratch` is owned by [`CaptureStreamData`] and pre-sized off the RT thread,
/// so in steady state this only `clear()`s and re-fills it — **no allocation**.
/// `clear()` retains the existing capacity, so the per-sample `push`es below
/// reuse it; the `Vec` can only grow if a period larger than every
/// previously-seen period arrives (a bounded, transient warm-up cost, identical
/// in spirit to the bridge's scratch grow). It performs no locking and no I/O.
///
/// # Return value
///
/// Returns the number of **trailing bytes** that did not form a complete `f32`
/// (`bytes.len() % 4`) and were therefore not decodable into a whole sample.
/// For a correctly-framed f32 stream this is `0`; a non-zero value means the
/// chunk's byte length was not a multiple of 4 (a genuinely truncated final
/// sample, which no realignment can recover). Callers can surface this for
/// visibility.
#[cfg(feature = "feat_linux")]
fn decode_unaligned_f32_le(bytes: &[u8], scratch: &mut Vec<f32>) -> usize {
    scratch.clear();
    let mut chunks = bytes.chunks_exact(4);
    // `chunks_exact` yields only complete 4-byte groups; each is a valid f32 bit
    // pattern. `from_le_bytes` reads unaligned bytes safely (no `align_to`).
    for c in chunks.by_ref() {
        // `try_into` on a 4-byte `chunks_exact` slice cannot fail.
        let arr: [u8; 4] = match c.try_into() {
            Ok(a) => a,
            // Unreachable: `chunks_exact(4)` always yields 4-byte slices. Degrade
            // (skip the group) rather than panic, per the no-panics rule.
            Err(_) => {
                debug_assert!(false, "chunks_exact(4) yielded a non-4-byte slice");
                continue;
            }
        };
        scratch.push(f32::from_le_bytes(arr));
    }
    // The remainder is a partial (truncated) final sample with no whole f32 to
    // recover — report its byte length so the caller can flag the truncation.
    chunks.remainder().len()
}

// ── CaptureStreamData ────────────────────────────────────────────────────

/// User data stored inside the PipeWire stream listener.
///
/// Passed to `Stream::add_local_listener_with_user_data()` and accessible
/// from the `param_changed` and `process` callbacks as `&mut CaptureStreamData`.
///
/// # Real-time safety
///
/// The `producer` field uses `rtrb` lock-free push — safe for the PipeWire
/// process callback thread. The `Vec<f32>` allocation in the process callback
/// is acceptable for the initial implementation but should be optimized with
/// a pre-allocated scratch buffer in future iterations.
struct CaptureStreamData {
    /// Negotiated audio format — updated by the `param_changed` callback
    /// when PipeWire negotiates the actual stream format.
    format: libspa::param::audio::AudioInfoRaw,
    /// Ring buffer producer — pushes `AudioBuffer`s to the consumer thread.
    producer: BridgeProducer,
    /// Number of audio channels (updated from negotiated format, falls back to requested).
    channels: u16,
    /// Sample rate in Hz (updated from negotiated format, falls back to requested).
    sample_rate: u32,
    /// Reusable scratch for the **misaligned-edge** fallback in the `process`
    /// callback (#30). PipeWire buffers are normally word-aligned, so
    /// `align_to::<f32>()` consumes the whole valid region with no head/tail and
    /// this stays untouched. If a chunk's valid region ever starts on a
    /// non-4-byte boundary, [`decode_unaligned_f32_le`] re-decodes it into this
    /// pre-sized `Vec` so no whole sample is silently dropped — reusing the
    /// existing capacity keeps the RT push allocation-free in steady state
    /// (ADR-0001).
    realign_scratch: Vec<f32>,
    /// Number of chunks whose valid region was **not** word-aligned and took
    /// the [`decode_unaligned_f32_le`] realignment fallback (#30 / rsac-9096).
    ///
    /// Bumped in the `.process` RT callback with a plain saturating add — the
    /// callback must NOT log (formatting allocates; typical logger backends
    /// lock + syscall), so the diagnostic is deferred: the one-shot summary is
    /// emitted from the non-RT [`Drop`] impl below at session teardown. Plain
    /// `u64`, no atomic: the listener callbacks hold exclusive `&mut` access,
    /// and `Drop` runs with ownership after the listener hook is removed, so
    /// no concurrent reader exists.
    misaligned_chunks: u64,
    /// Cumulative count of trailing bytes that formed a truncated partial
    /// sample (unrecoverable by realignment) across all misaligned chunks
    /// (rsac-9096). Same write/read discipline as
    /// [`misaligned_chunks`](Self::misaligned_chunks).
    misaligned_truncated_bytes: u64,
}

/// rsac-9096: emit the misaligned-chunk diagnostic from a **non-RT** point.
///
/// The `.process` callback (where misalignment is detected) is a C callback
/// invoked from inside `main_loop.iterate()` on the real-time data path,
/// outside any panic guard — `log::warn!` there would format (allocate) and
/// let a typical logger backend lock and syscall, violating the callback's own
/// "no logging" contract (ADR-0001). The callback therefore only bumps the
/// plain counters above; this `Drop` — which runs on the loop thread during
/// session teardown (`capture_listener = None` / the StopCapture, Shutdown,
/// and channel-disconnect arms), strictly after the listener hook is removed
/// so no further callback can fire — surfaces the one-shot summary.
impl Drop for CaptureStreamData {
    fn drop(&mut self) {
        if self.misaligned_chunks > 0 {
            log::warn!(
                "PipeWire delivered {} non-word-aligned audio chunk(s) during this \
                 capture session (realigned via copy in the process callback); {} \
                 trailing byte(s) formed truncated partial samples and were not \
                 recoverable.",
                self.misaligned_chunks,
                self.misaligned_truncated_bytes
            );
        }
    }
}

// ── PipeWireThread ───────────────────────────────────────────────────────

/// Handle to the dedicated PipeWire thread.
///
/// All PipeWire `Rc`-based objects (MainLoop, Context, Core, Registry, Stream)
/// live on the spawned thread. The caller communicates via [`PipeWireCommand`]s
/// sent through the command channel, and receives responses via per-command
/// response senders.
///
/// # Lifecycle
///
/// 1. [`PipeWireThread::spawn()`] creates the thread and waits for PipeWire init.
/// 2. [`start_capture()`](PipeWireThread::start_capture) / [`stop_capture()`](PipeWireThread::stop_capture)
///    send commands and block for the response.
/// 3. On [`Drop`], a `Shutdown` command is sent and the thread is joined.
pub(crate) struct PipeWireThread {
    /// Channel to send commands to the PipeWire thread.
    command_tx: std_mpsc::Sender<PipeWireCommand>,
    /// Join handle for the dedicated thread (taken on drop).
    thread_handle: Option<std::thread::JoinHandle<()>>,
    /// Shared flag: `true` while the PipeWire thread's event loop is running.
    /// Read by `is_alive()`, which is called from `LinuxPlatformStream::is_active()`.
    #[allow(dead_code)]
    is_running: Arc<AtomicBool>,
}

impl PipeWireThread {
    /// Spawn the dedicated PipeWire thread.
    ///
    /// This creates a new OS thread named `"rsac-pipewire"` that:
    /// 1. Initializes PipeWire (`pipewire::init()`)
    /// 2. Creates `MainLoop`, `Context`, `Core`, and `Registry`
    /// 3. Enters the event loop, pumping PipeWire events and processing commands
    ///
    /// The call blocks until PipeWire initialization completes on the new thread.
    /// Returns an error if any PipeWire initialization step fails.
    ///
    /// # Errors
    ///
    /// - [`AudioError::BackendInitializationFailed`] if the thread cannot be spawned
    ///   or if PipeWire initialization fails (MainLoop, Context, Core, or Registry).
    pub fn spawn() -> AudioResult<Self> {
        let (command_tx, command_rx) = std_mpsc::channel();
        let (init_tx, init_rx) = std_mpsc::channel();
        let is_running = Arc::new(AtomicBool::new(true));
        let is_running_thread = Arc::clone(&is_running);

        let thread_handle = std::thread::Builder::new()
            .name("rsac-pipewire".to_string())
            .spawn(move || {
                pw_thread_main(command_rx, init_tx, is_running_thread);
            })
            .map_err(|e| AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: format!("Failed to spawn PipeWire thread: {}", e),
            })?;

        // Block until the PipeWire thread reports init success or failure.
        let init_result = init_rx
            .recv()
            .map_err(|_| AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: "PipeWire thread exited before reporting init status".to_string(),
            })?;

        // If PipeWire init failed, the thread has already exited. Propagate the error.
        init_result?;

        Ok(PipeWireThread {
            command_tx,
            thread_handle: Some(thread_handle),
            is_running,
        })
    }

    /// Send a `StartCapture` command to the PipeWire thread and wait for the response.
    ///
    /// The `BridgeProducer` is moved to the PipeWire thread where it will be used
    /// by the PipeWire `process` callback to push captured audio into the ring buffer.
    ///
    /// This creates a PipeWire stream, registers listener callbacks (param_changed
    /// for format negotiation, process for audio data), and connects the stream.
    ///
    /// The capture target is resolved to an `object.serial` on the calling
    /// thread (M2/M3) before the command is dispatched, so `pw-dump`/`/proc`
    /// work never runs on the PipeWire event loop.
    ///
    /// # Errors
    ///
    /// - [`AudioError::ApplicationNotFound`] / [`AudioError::DeviceNotFound`] if
    ///   target resolution fails (no matching node / unparseable PID).
    /// - [`AudioError::BackendError`] if the PipeWire thread is not running or
    ///   exits without replying, or if stream creation/connection fails.
    /// - [`AudioError::Timeout`] if the PipeWire thread does not acknowledge the
    ///   command within [`HANDSHAKE_TIMEOUT`].
    pub fn start_capture(
        &self,
        config: CaptureConfig,
        producer: BridgeProducer,
    ) -> AudioResult<()> {
        // Resolve the capture target on THIS (caller) thread — running pw-dump
        // and walking /proc must not happen on the PipeWire event loop, which
        // would block audio buffer delivery (audit findings M2/M3). The event
        // loop only ever receives a fully-resolved TARGET_OBJECT.
        let resolved = resolve_capture_target(&config.target)?;

        let (response_tx, response_rx) = std_mpsc::channel();

        self.command_tx
            .send(PipeWireCommand::StartCapture {
                config,
                resolved,
                producer,
                response_tx,
            })
            .map_err(|_| AudioError::BackendError {
                backend: "PipeWire".to_string(),
                operation: "start_capture".to_string(),
                message: "PipeWire thread is not running (command channel closed)".to_string(),
                context: None,
            })?;

        match response_rx.recv_timeout(HANDSHAKE_TIMEOUT) {
            Ok(result) => result,
            Err(std_mpsc::RecvTimeoutError::Timeout) => Err(AudioError::Timeout {
                operation: "PipeWire StartCapture handshake".to_string(),
                duration: HANDSHAKE_TIMEOUT,
            }),
            Err(std_mpsc::RecvTimeoutError::Disconnected) => Err(AudioError::BackendError {
                backend: "PipeWire".to_string(),
                operation: "start_capture".to_string(),
                message: "PipeWire thread exited before responding to StartCapture".to_string(),
                context: None,
            }),
        }
    }

    /// Send a `StopCapture` command to the PipeWire thread and wait for the response.
    ///
    /// Tells the PipeWire thread to tear down the current capture stream and
    /// release the `BridgeProducer`.
    ///
    /// # Errors
    ///
    /// - [`AudioError::BackendError`] if the PipeWire thread is not running or
    ///   exits without replying.
    /// - [`AudioError::Timeout`] if the PipeWire thread does not acknowledge the
    ///   command within [`HANDSHAKE_TIMEOUT`].
    pub fn stop_capture(&self) -> AudioResult<()> {
        let (response_tx, response_rx) = std_mpsc::channel();

        self.command_tx
            .send(PipeWireCommand::StopCapture { response_tx })
            .map_err(|_| AudioError::BackendError {
                backend: "PipeWire".to_string(),
                operation: "stop_capture".to_string(),
                message: "PipeWire thread is not running (command channel closed)".to_string(),
                context: None,
            })?;

        match response_rx.recv_timeout(HANDSHAKE_TIMEOUT) {
            Ok(result) => result,
            Err(std_mpsc::RecvTimeoutError::Timeout) => Err(AudioError::Timeout {
                operation: "PipeWire StopCapture handshake".to_string(),
                duration: HANDSHAKE_TIMEOUT,
            }),
            Err(std_mpsc::RecvTimeoutError::Disconnected) => Err(AudioError::BackendError {
                backend: "PipeWire".to_string(),
                operation: "stop_capture".to_string(),
                message: "PipeWire thread exited before responding to StopCapture".to_string(),
                context: None,
            }),
        }
    }

    /// Snapshot the current audio nodes from the native PipeWire registry.
    ///
    /// Sends a [`SnapshotDevices`](PipeWireCommand::SnapshotDevices) command and
    /// blocks (bounded by [`SNAPSHOT_TIMEOUT`]) for the reply. The PipeWire
    /// thread waits for a `core.sync()`/`done` roundtrip before replying so the
    /// initial registry dump has settled — the returned list is therefore the
    /// real device set, never a racy empty snapshot.
    ///
    /// Only owned [`PwDeviceSnapshot`] values cross the channel; the registry
    /// callbacks themselves run on the PipeWire loop thread.
    ///
    /// # Errors
    ///
    /// - [`AudioError::BackendError`] if the PipeWire thread is not running or
    ///   exits without replying.
    /// - [`AudioError::Timeout`] if the snapshot does not complete within
    ///   [`SNAPSHOT_TIMEOUT`].
    pub fn snapshot_devices(&self) -> AudioResult<Vec<PwDeviceSnapshot>> {
        let (response_tx, response_rx) = std_mpsc::channel();

        self.command_tx
            .send(PipeWireCommand::SnapshotDevices { response_tx })
            .map_err(|_| AudioError::BackendError {
                backend: "PipeWire".to_string(),
                operation: "snapshot_devices".to_string(),
                message: "PipeWire thread is not running (command channel closed)".to_string(),
                context: None,
            })?;

        match response_rx.recv_timeout(SNAPSHOT_TIMEOUT) {
            Ok(result) => result,
            Err(std_mpsc::RecvTimeoutError::Timeout) => Err(AudioError::Timeout {
                operation: "PipeWire SnapshotDevices roundtrip".to_string(),
                duration: SNAPSHOT_TIMEOUT,
            }),
            Err(std_mpsc::RecvTimeoutError::Disconnected) => Err(AudioError::BackendError {
                backend: "PipeWire".to_string(),
                operation: "snapshot_devices".to_string(),
                message: "PipeWire thread exited before responding to SnapshotDevices".to_string(),
                context: None,
            }),
        }
    }

    /// Snapshot the default sink/source node names from PipeWire `default`
    /// metadata.
    ///
    /// Sends a [`SnapshotDefault`](PipeWireCommand::SnapshotDefault) command and
    /// blocks (bounded by [`SNAPSHOT_TIMEOUT`]) for the reply, which the
    /// PipeWire thread only sends after a sync/done roundtrip. The returned
    /// names are node *names*; the caller resolves them to a round-trippable
    /// `object.serial` against the device snapshot.
    ///
    /// # Errors
    ///
    /// - [`AudioError::BackendError`] if the PipeWire thread is not running or
    ///   exits without replying.
    /// - [`AudioError::Timeout`] if the snapshot does not complete within
    ///   [`SNAPSHOT_TIMEOUT`].
    pub fn snapshot_default(&self) -> AudioResult<PwDefaultSnapshot> {
        let (response_tx, response_rx) = std_mpsc::channel();

        self.command_tx
            .send(PipeWireCommand::SnapshotDefault { response_tx })
            .map_err(|_| AudioError::BackendError {
                backend: "PipeWire".to_string(),
                operation: "snapshot_default".to_string(),
                message: "PipeWire thread is not running (command channel closed)".to_string(),
                context: None,
            })?;

        match response_rx.recv_timeout(SNAPSHOT_TIMEOUT) {
            Ok(result) => result,
            Err(std_mpsc::RecvTimeoutError::Timeout) => Err(AudioError::Timeout {
                operation: "PipeWire SnapshotDefault roundtrip".to_string(),
                duration: SNAPSHOT_TIMEOUT,
            }),
            Err(std_mpsc::RecvTimeoutError::Disconnected) => Err(AudioError::BackendError {
                backend: "PipeWire".to_string(),
                operation: "snapshot_default".to_string(),
                message: "PipeWire thread exited before responding to SnapshotDefault".to_string(),
                context: None,
            }),
        }
    }

    /// Snapshot the audio-producing applications from the native PipeWire
    /// registry (H4 part 2 / `rsac-8ebb`).
    ///
    /// Sends a [`SnapshotApplications`](PipeWireCommand::SnapshotApplications)
    /// command and blocks (bounded by [`SNAPSHOT_TIMEOUT`]) for the reply. The
    /// PipeWire thread waits for a `core.sync()`/`done` roundtrip before
    /// replying so the initial registry dump has settled — the returned list is
    /// therefore the real application set, never a racy empty snapshot, and it
    /// is PID-deduplicated (one entry per process).
    ///
    /// Only owned [`PwAppSnapshot`] values cross the channel; the registry
    /// callbacks themselves run on the PipeWire loop thread.
    ///
    /// # Errors
    ///
    /// - [`AudioError::BackendError`] if the PipeWire thread is not running or
    ///   exits without replying.
    /// - [`AudioError::Timeout`] if the snapshot does not complete within
    ///   [`SNAPSHOT_TIMEOUT`].
    pub fn snapshot_applications(&self) -> AudioResult<Vec<PwAppSnapshot>> {
        let (response_tx, response_rx) = std_mpsc::channel();

        self.command_tx
            .send(PipeWireCommand::SnapshotApplications { response_tx })
            .map_err(|_| AudioError::BackendError {
                backend: "PipeWire".to_string(),
                operation: "snapshot_applications".to_string(),
                message: "PipeWire thread is not running (command channel closed)".to_string(),
                context: None,
            })?;

        match response_rx.recv_timeout(SNAPSHOT_TIMEOUT) {
            Ok(result) => result,
            Err(std_mpsc::RecvTimeoutError::Timeout) => Err(AudioError::Timeout {
                operation: "PipeWire SnapshotApplications roundtrip".to_string(),
                duration: SNAPSHOT_TIMEOUT,
            }),
            Err(std_mpsc::RecvTimeoutError::Disconnected) => Err(AudioError::BackendError {
                backend: "PipeWire".to_string(),
                operation: "snapshot_applications".to_string(),
                message: "PipeWire thread exited before responding to SnapshotApplications"
                    .to_string(),
                context: None,
            }),
        }
    }

    /// Resolve a capture [`TargetQuery`] to a node `object.serial` using the
    /// **native** registry snapshot (H4 part 3 / `rsac-nat1`) — no `pw-dump`.
    ///
    /// Sends a [`ResolveTarget`](PipeWireCommand::ResolveTarget) command and
    /// blocks (bounded by [`SNAPSHOT_TIMEOUT`]) for the reply. The PipeWire
    /// thread waits for a `core.sync()`/`done` roundtrip so the registry has
    /// settled before matching. `Ok(None)` means the registry settled but held
    /// no matching node (the caller may then fall back to the `pw-dump` path).
    ///
    /// # Errors
    ///
    /// - [`AudioError::BackendError`] if the PipeWire thread is not running or
    ///   exits without replying.
    /// - [`AudioError::Timeout`] if resolution does not complete within
    ///   [`SNAPSHOT_TIMEOUT`].
    pub fn resolve_target(&self, query: TargetQuery) -> AudioResult<Option<String>> {
        let (response_tx, response_rx) = std_mpsc::channel();

        self.command_tx
            .send(PipeWireCommand::ResolveTarget { query, response_tx })
            .map_err(|_| AudioError::BackendError {
                backend: "PipeWire".to_string(),
                operation: "resolve_target".to_string(),
                message: "PipeWire thread is not running (command channel closed)".to_string(),
                context: None,
            })?;

        match response_rx.recv_timeout(SNAPSHOT_TIMEOUT) {
            Ok(result) => result,
            Err(std_mpsc::RecvTimeoutError::Timeout) => Err(AudioError::Timeout {
                operation: "PipeWire ResolveTarget roundtrip".to_string(),
                duration: SNAPSHOT_TIMEOUT,
            }),
            Err(std_mpsc::RecvTimeoutError::Disconnected) => Err(AudioError::BackendError {
                backend: "PipeWire".to_string(),
                operation: "resolve_target".to_string(),
                message: "PipeWire thread exited before responding to ResolveTarget".to_string(),
                context: None,
            }),
        }
    }

    /// Enumerate the audio formats a node advertises via its
    /// `SPA_PARAM_EnumFormat` parameters (PR-5 / `rsac-7469`).
    ///
    /// Sends an [`EnumNodeFormats`](PipeWireCommand::EnumNodeFormats) command for
    /// the node whose `object.serial` is `serial` and blocks (bounded by
    /// [`SNAPSHOT_TIMEOUT`]) for the reply. The PipeWire thread binds the node,
    /// fires `enum_params`, and waits for a `core.sync()`/`done` roundtrip so the
    /// emitted `param` events have settled before replying.
    ///
    /// This is **advisory discovery only** (L2 / EF-3): the returned list does
    /// not change the connect-time format the backend actually negotiates and
    /// delivers (that remains whatever `param_changed` reports). A node with no
    /// `EnumFormat` — or one that cannot be resolved/bound — yields `Ok(vec![])`
    /// rather than a guess.
    ///
    /// Only owned [`crate::core::config::AudioFormat`] values cross the channel;
    /// the node proxy + its `param` listener live on the PipeWire loop thread.
    ///
    /// # Errors
    ///
    /// - [`AudioError::BackendError`] if the PipeWire thread is not running or
    ///   exits without replying.
    /// - [`AudioError::Timeout`] if the enumeration does not complete within
    ///   [`SNAPSHOT_TIMEOUT`].
    pub fn enum_node_formats(
        &self,
        serial: &str,
    ) -> AudioResult<Vec<crate::core::config::AudioFormat>> {
        let (response_tx, response_rx) = std_mpsc::channel();

        self.command_tx
            .send(PipeWireCommand::EnumNodeFormats {
                serial: serial.to_string(),
                response_tx,
            })
            .map_err(|_| AudioError::BackendError {
                backend: "PipeWire".to_string(),
                operation: "enum_node_formats".to_string(),
                message: "PipeWire thread is not running (command channel closed)".to_string(),
                context: None,
            })?;

        match response_rx.recv_timeout(SNAPSHOT_TIMEOUT) {
            Ok(result) => result,
            Err(std_mpsc::RecvTimeoutError::Timeout) => Err(AudioError::Timeout {
                operation: "PipeWire EnumNodeFormats roundtrip".to_string(),
                duration: SNAPSHOT_TIMEOUT,
            }),
            Err(std_mpsc::RecvTimeoutError::Disconnected) => Err(AudioError::BackendError {
                backend: "PipeWire".to_string(),
                operation: "enum_node_formats".to_string(),
                message: "PipeWire thread exited before responding to EnumNodeFormats".to_string(),
                context: None,
            }),
        }
    }

    /// Returns `true` if the PipeWire thread is still alive.
    ///
    /// This checks the shared atomic flag, which is set to `false` when the
    /// thread's event loop exits (either due to `Shutdown` or an error).
    /// Called by `LinuxPlatformStream::is_active()` (PlatformStream trait contract).
    pub fn is_alive(&self) -> bool {
        self.is_running.load(Ordering::SeqCst)
    }
}

impl Drop for PipeWireThread {
    fn drop(&mut self) {
        // Send Shutdown command — ignore errors (thread may already be dead).
        let _ = self.command_tx.send(PipeWireCommand::Shutdown);

        // Join the thread to ensure clean shutdown.
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

// ── LinuxPlatformStream ──────────────────────────────────────────────────

/// Platform-specific stream handle for Linux (PipeWire backend).
///
/// Wraps a shared [`PipeWireThread`] handle and implements [`PlatformStream`]
/// so it can be used with [`BridgeStream`](crate::bridge::stream::BridgeStream).
///
/// # Thread Safety
///
/// `LinuxPlatformStream` is `Send` (required by `PlatformStream`). The inner
/// `Arc<Mutex<PipeWireThread>>` provides shared ownership and interior mutability.
pub(crate) struct LinuxPlatformStream {
    pw_thread: Arc<Mutex<PipeWireThread>>,
}

impl LinuxPlatformStream {
    /// Create a new `LinuxPlatformStream` wrapping the given PipeWire thread.
    pub fn new(pw_thread: Arc<Mutex<PipeWireThread>>) -> Self {
        Self { pw_thread }
    }
}

impl PlatformStream for LinuxPlatformStream {
    fn stop_capture(&self) -> AudioResult<()> {
        self.pw_thread
            .lock()
            .map_err(|_| AudioError::InternalError {
                message: "PipeWire thread mutex poisoned".to_string(),
                source: None,
            })?
            .stop_capture()
    }

    fn is_active(&self) -> bool {
        self.pw_thread.lock().map(|t| t.is_alive()).unwrap_or(false)
    }
}

// ── Process Tree Discovery ───────────────────────────────────────────────

/// Discovers all PIDs in a process tree rooted at `parent_pid`.
///
/// Walks the Linux `/proc` filesystem to find all descendant processes
/// (children, grandchildren, etc.) of the given parent PID. Returns a
/// deduplicated, sorted `Vec<u32>` containing the parent PID and all
/// discovered descendants.
///
/// # Algorithm
///
/// For each process in `/proc`, reads `/proc/{pid}/stat` to extract the
/// parent PID (field 4). Builds a parent→children map, then performs a
/// breadth-first traversal from `parent_pid` to collect all descendants.
///
/// If `/proc` cannot be read (e.g., in a containerized environment without
/// procfs), returns a single-element vector containing just `parent_pid`
/// (graceful degradation to single-process capture).
///
/// # Example
///
/// If process 1000 has children 1001 and 1002, and 1001 has child 1003:
/// ```text
/// discover_process_tree_pids(1000) → [1000, 1001, 1002, 1003]
/// ```
fn discover_process_tree_pids(parent_pid: u32) -> Vec<u32> {
    use std::collections::{HashMap, VecDeque};
    use std::fs;

    // Build a map of pid → parent_pid by reading /proc/*/stat
    let mut parent_map: HashMap<u32, u32> = HashMap::new();

    let proc_entries = match fs::read_dir("/proc") {
        Ok(entries) => entries,
        Err(e) => {
            log::warn!(
                "ProcessTree: cannot read /proc: {}. Falling back to single PID {}",
                e,
                parent_pid
            );
            return vec![parent_pid];
        }
    };

    for entry in proc_entries.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();

        // Only process numeric directory names (PIDs)
        let pid: u32 = match name.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };

        // Read /proc/{pid}/stat to extract PPID (field 4)
        let stat_path = format!("/proc/{}/stat", pid);
        if let Ok(stat_contents) = fs::read_to_string(&stat_path) {
            if let Some(ppid) = parse_ppid_from_stat(&stat_contents) {
                parent_map.insert(pid, ppid);
            }
        }
    }

    // BFS from parent_pid to find all descendants
    let mut all_pids: Vec<u32> = vec![parent_pid];
    let mut queue: VecDeque<u32> = VecDeque::new();
    queue.push_back(parent_pid);

    // Build children map for efficient lookup
    let mut children_map: HashMap<u32, Vec<u32>> = HashMap::new();
    for (&child, &parent) in &parent_map {
        children_map.entry(parent).or_default().push(child);
    }

    while let Some(current_pid) = queue.pop_front() {
        if let Some(children) = children_map.get(&current_pid) {
            for &child_pid in children {
                if !all_pids.contains(&child_pid) {
                    all_pids.push(child_pid);
                    queue.push_back(child_pid);
                }
            }
        }
    }

    all_pids.sort();
    all_pids.dedup();

    log::info!(
        "ProcessTree: parent_pid={}, discovered {} total PIDs: {:?}",
        parent_pid,
        all_pids.len(),
        all_pids
    );

    all_pids
}

/// Parses the parent PID (PPID) from the contents of `/proc/{pid}/stat`.
///
/// The `/proc/{pid}/stat` format is:
/// ```text
/// pid (comm) state ppid pgid sid ...
/// ```
///
/// The process name (`comm`) may contain spaces and parentheses, so we
/// find the last `)` to locate the end of the comm field, then parse
/// the fourth field (PPID) from the remaining fields.
fn parse_ppid_from_stat(stat_contents: &str) -> Option<u32> {
    // Find the end of the comm field (last ')' in the line)
    let after_comm = stat_contents.rfind(')')? + 1;
    let remainder = &stat_contents[after_comm..];

    // Fields after comm: state ppid pgid ...
    // Split by whitespace and get the 2nd field (ppid, 0-indexed: state=0, ppid=1)
    let mut fields = remainder.split_whitespace();
    fields.next()?; // skip state
    let ppid_str = fields.next()?;
    ppid_str.parse::<u32>().ok()
}

// ── Native registry target resolution (H4 part 3 / rsac-nat1) ────────────
//
// The capture path historically resolved every non-default `CaptureTarget` to a
// PipeWire node `object.serial` by shelling out to `pw-dump` and parsing its
// JSON (`find_pipewire_node_serial`). Device/Application discovery already has a
// *native* in-process path (the registry `global` snapshot — `PwDeviceSnapshot`
// / `PwAppSnapshot`), so the capture path can reuse that same snapshot instead
// of a subprocess. These pure helpers do the matching against an already-taken
// snapshot; `resolve_capture_target` drives the snapshot + these helpers, and
// falls back to `pw-dump` only when the native path can't resolve (e.g. a
// permission-restricted registry that hides stream nodes).

/// How to match a capture target against a native registry snapshot.
///
/// This mirrors the [`PwNodeLookup`] strategies but operates on the owned
/// [`PwDeviceSnapshot`] / [`PwAppSnapshot`] lists produced by the registry
/// `global` callback rather than on `pw-dump` JSON — so no subprocess and no
/// `PATH` dependency is involved.
#[derive(Debug, Clone)]
pub(crate) enum TargetQuery {
    /// Match a *device/sink/source* node by its [`DeviceId`] string against a
    /// snapshot node's `object.serial` (or global-id fallback) `id`.
    ///
    /// [`DeviceId`]: crate::core::config::DeviceId
    Device(String),
    /// Match an application stream node by exact process id.
    ByPid(u32),
    /// Match an application stream node by name (see
    /// [`app_name_matches`] for the exact/relaxed contract).
    ByAppName(String),
    /// Match the first application stream node whose PID is in the set (process
    /// tree capture).
    ByPidSet(Vec<u32>),
}

/// The tightened `ApplicationByName` matching contract (native + subprocess).
///
/// A candidate application name matches `query` when either:
/// 1. it is an **exact**, case-insensitive equality, or
/// 2. after case-folding, the candidate's file-stem (the binary name with any
///    directory prefix and trailing `.exe`/version suffix stripped) equals the
///    query.
///
/// It intentionally does **not** do arbitrary substring containment: a query of
/// `"Fire"` must not silently bind to `"Firefox"` (that surprised callers and
/// could attach to the wrong app). Callers wanting fuzzy discovery should
/// enumerate via `list_audio_applications()` and pick a PID explicitly.
///
/// `candidate` is compared both as-is and as its trailing path component so a
/// registry `application.process.binary` like `/usr/lib/firefox/firefox` still
/// matches a query of `firefox`.
pub(crate) fn app_name_matches(candidate: &str, query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return false;
    }
    if candidate.eq_ignore_ascii_case(q) {
        return true;
    }
    // Compare the candidate's trailing path component (basename) too, so a full
    // binary path resolves against a bare program name.
    let base = candidate.rsplit(['/', '\\']).next().unwrap_or(candidate);
    if base.eq_ignore_ascii_case(q) {
        return true;
    }
    // Strip a trailing ".exe" (cross-platform binary names) from the basename
    // for the final comparison — never a broad substring match.
    let stem = base.strip_suffix(".exe").unwrap_or(base);
    stem.eq_ignore_ascii_case(q)
}

/// Resolve a [`TargetQuery`] against native registry snapshots, returning the
/// matched node's `object.serial` (the string the capture path stamps into
/// `TARGET_OBJECT`).
///
/// `devices` and `apps` are the owned snapshots taken on the PipeWire loop
/// thread (via [`SnapshotDevices`](PipeWireCommand::SnapshotDevices) /
/// [`SnapshotApplications`](PipeWireCommand::SnapshotApplications)). This is a
/// **pure** function — no I/O, no subprocess — so it is unit-testable without a
/// live daemon.
///
/// Returns `None` when nothing matches; the caller decides whether that is a
/// `*NotFound` error or a reason to try the `pw-dump` fallback.
pub(crate) fn resolve_target_from_snapshot(
    query: &TargetQuery,
    devices: &[PwDeviceSnapshot],
    apps: &[PwAppSnapshot],
) -> Option<String> {
    match query {
        TargetQuery::Device(id) => devices
            .iter()
            .find(|d| &d.id == id || d.node_name == *id || d.name == *id)
            .map(|d| d.id.clone()),
        TargetQuery::ByPid(pid) => apps
            .iter()
            .find(|a| a.pid == *pid)
            .map(|a| a.node_serial.clone()),
        TargetQuery::ByAppName(name) => apps
            .iter()
            .find(|a| app_name_matches(&a.app_name, name))
            .map(|a| a.node_serial.clone()),
        TargetQuery::ByPidSet(pids) => apps
            .iter()
            .find(|a| pids.contains(&a.pid))
            .map(|a| a.node_serial.clone()),
    }
}

// ── pw-dump Node Lookup ──────────────────────────────────────────────────

/// Specifies how to look up a PipeWire node via `pw-dump`.
#[allow(clippy::enum_variant_names)] // By* prefix is intentional — describes lookup strategy
enum PwNodeLookup<'a> {
    /// Match by application name (case-insensitive against `application.name`
    /// or `application.process.binary`).
    ByAppName(&'a str),
    /// Match by process ID (exact match against `application.process.id`).
    /// Used to resolve [`CaptureTarget::Application`], which — like Windows and
    /// macOS — carries a numeric PID string in its [`ApplicationId`].
    ///
    /// [`ApplicationId`]: crate::core::config::ApplicationId
    ByPid(u32),
    /// Match by any PID in a set (for process tree capture).
    /// Searches for the first audio output node whose `application.process.id`
    /// matches any PID in the provided set.
    ByPidSet(&'a [u32]),
    /// Match a *device/sink* node by its [`DeviceId`] string.
    ///
    /// Device enumeration (see `mod.rs`) emits the PipeWire node `id`, whereas
    /// every capture path keys `TARGET_OBJECT` on `object.serial`. This variant
    /// normalises the two: it matches a node whose top-level `id` **or**
    /// `object.serial` equals the supplied string and whose `media.class` names
    /// an `Audio/Sink` or `Audio/Source`, then returns that node's
    /// `object.serial` (audit finding M8).
    ///
    /// [`DeviceId`]: crate::core::config::DeviceId
    Device(&'a str),
}

/// Runs `pw-dump`, parses the JSON output, and finds the `object.serial` of
/// the first PipeWire node that:
/// - has `type == "PipeWire:Interface:Node"`
/// - has a matching `info.props.media.class`: `"Stream/Output/Audio"` for the
///   application/process lookups, or `"Audio/Sink"`/`"Audio/Source"` for the
///   [`PwNodeLookup::Device`] lookup
/// - matches the given [`PwNodeLookup`] criteria
///
/// Returns the `object.serial` as a `String` suitable for use as `TARGET_OBJECT`.
///
/// # Errors
///
/// - [`AudioError::BackendError`] if `pw-dump` cannot be executed or returns
///   non-zero, or if the output cannot be parsed as JSON.
/// - [`AudioError::ApplicationNotFound`] if no matching application/process node
///   is found.
/// - [`AudioError::DeviceNotFound`] if no matching device node is found
///   ([`PwNodeLookup::Device`]).
fn find_pipewire_node_serial(lookup: &PwNodeLookup<'_>) -> AudioResult<String> {
    // Run pw-dump and capture its JSON output.
    let output = std::process::Command::new("pw-dump")
        .arg("--no-colors")
        .output()
        .map_err(|e| AudioError::BackendError {
            backend: "PipeWire".to_string(),
            operation: "pw-dump".to_string(),
            message: format!("Failed to run pw-dump: {}. Is pipewire-utils installed?", e),
            context: None,
        })?;

    if !output.status.success() {
        return Err(AudioError::BackendError {
            backend: "PipeWire".to_string(),
            operation: "pw-dump".to_string(),
            message: format!(
                "pw-dump exited with status: {}; stderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            context: None,
        });
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    let entries: serde_json::Value =
        serde_json::from_str(&json_str).map_err(|e| AudioError::BackendError {
            backend: "PipeWire".to_string(),
            operation: "pw-dump parse".to_string(),
            message: format!("Failed to parse pw-dump JSON: {}", e),
            context: None,
        })?;

    let array = entries.as_array().ok_or_else(|| AudioError::BackendError {
        backend: "PipeWire".to_string(),
        operation: "pw-dump parse".to_string(),
        message: "pw-dump output is not a JSON array".to_string(),
        context: None,
    })?;

    let pid_string; // storage for PID → String conversion (avoids per-iteration alloc)
    let pid_str = match lookup {
        PwNodeLookup::ByPid(pid) => {
            pid_string = pid.to_string();
            Some(pid_string.as_str())
        }
        _ => None,
    };

    // For ByPidSet, pre-compute string representations of all PIDs.
    let pid_set_strings: Vec<String> = match lookup {
        PwNodeLookup::ByPidSet(pids) => pids.iter().map(|p| p.to_string()).collect(),
        _ => Vec::new(),
    };

    for entry in array {
        // Filter: must be a PipeWire node.
        let entry_type = entry.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if entry_type != "PipeWire:Interface:Node" {
            continue;
        }

        // Get info.props (where all the metadata lives).
        let props = match entry.get("info").and_then(|i| i.get("props")) {
            Some(p) => p,
            None => continue,
        };

        // Filter: media.class must match the expected node category for this
        // lookup kind. Application/process captures attach to per-application
        // output *streams* (`Stream/Output/Audio`), whereas a device target
        // names a sink/source *device* node (`Audio/Sink` / `Audio/Source`).
        let media_class = props
            .get("media.class")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let class_ok = match lookup {
            PwNodeLookup::Device(_) => {
                media_class.contains("Audio/Sink") || media_class.contains("Audio/Source")
            }
            _ => media_class.contains("Stream/Output/Audio"),
        };
        if !class_ok {
            continue;
        }

        // Check if this node matches the lookup criteria.
        let matches = match lookup {
            PwNodeLookup::ByAppName(name) => {
                let app_name = props
                    .get("application.name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let app_binary = props
                    .get("application.process.binary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                // Same tightened contract as the native path (exact / basename /
                // .exe-stripped, case-insensitive — never arbitrary substring).
                app_name_matches(app_name, name) || app_name_matches(app_binary, name)
            }
            PwNodeLookup::ByPid(_) => {
                let proc_id = props
                    .get("application.process.id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                proc_id == pid_str.unwrap()
            }
            PwNodeLookup::ByPidSet(_) => {
                let proc_id = props
                    .get("application.process.id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                pid_set_strings.iter().any(|s| s.as_str() == proc_id)
            }
            PwNodeLookup::Device(device_id) => {
                // Match against the top-level node `id` (what enumeration emits)
                // OR the `object.serial` (what TARGET_OBJECT expects), so a
                // DeviceId produced by either convention resolves correctly.
                let top_id = entry
                    .get("id")
                    .and_then(|v| v.as_u64())
                    .map(|n| n.to_string());
                let serial = props.get("object.serial").and_then(|v| {
                    v.as_str()
                        .map(|s| s.to_string())
                        .or_else(|| v.as_u64().map(|n| n.to_string()))
                });
                top_id.as_deref() == Some(*device_id) || serial.as_deref() == Some(*device_id)
            }
        };

        if !matches {
            continue;
        }

        // Extract object.serial — may be a JSON string or number.
        if let Some(serial) = props.get("object.serial") {
            if let Some(s) = serial.as_str() {
                log::debug!("pw-dump: matched node with object.serial={}", s);
                return Ok(s.to_string());
            }
            if let Some(n) = serial.as_u64() {
                log::debug!("pw-dump: matched node with object.serial={}", n);
                return Ok(n.to_string());
            }
        }

        // Fallback: use the top-level node `id` if object.serial is missing.
        if let Some(id) = entry.get("id").and_then(|v| v.as_u64()) {
            log::warn!(
                "pw-dump: matched node has no object.serial, falling back to id={}",
                id
            );
            return Ok(id.to_string());
        }
    }

    // No matching node found.
    match lookup {
        PwNodeLookup::ByAppName(name) => Err(AudioError::ApplicationNotFound {
            identifier: name.to_string(),
        }),
        PwNodeLookup::ByPid(pid) => Err(AudioError::ApplicationNotFound {
            identifier: format!("PID {}", pid),
        }),
        PwNodeLookup::ByPidSet(pids) => Err(AudioError::ApplicationNotFound {
            identifier: format!("process tree PIDs {:?}", pids),
        }),
        PwNodeLookup::Device(device_id) => Err(AudioError::DeviceNotFound {
            device_id: device_id.to_string(),
        }),
    }
}

/// Resolve a [`CaptureTarget`] into a ready-to-use [`ResolvedTarget`].
///
/// This is the off-the-event-loop resolution step (audit findings M2/M3): it
/// resolves the target **on the caller thread** so the capture PipeWire event
/// loop never blocks on discovery while pumping audio. The returned
/// [`ResolvedTarget`] carries only a plain `object.serial` string (or "no
/// target" for the default sink monitor).
///
/// # Native-first resolution (H4 part 3 / `rsac-nat1`)
///
/// `Device`/`Application`/`ApplicationByName`/`ProcessTree` are resolved by
/// taking an **in-process registry snapshot** on a short-lived
/// [`PipeWireThread`] and matching it via [`resolve_target_from_snapshot`] — no
/// `pw-dump` subprocess and no `PATH` dependency. `pw-dump` is used only as a
/// **fallback** when the native path cannot resolve (the snapshot settled but
/// held no match, or the short-lived thread failed to spawn — e.g. a
/// permission-restricted registry that hides per-application stream nodes). A
/// native match always wins; the subprocess is never consulted when the
/// registry already answered.
///
/// # Target semantics
///
/// - [`SystemDefault`](CaptureTarget::SystemDefault) — no `TARGET_OBJECT`; never
///   touches the registry or `pw-dump`.
/// - [`Device`](CaptureTarget::Device) — the [`DeviceId`] is a PipeWire node
///   `object.serial` (or node name); matched against the registry snapshot and
///   normalised to the node's `object.serial`. Returns
///   [`AudioError::DeviceNotFound`] if absent in both paths.
/// - [`Application`](CaptureTarget::Application) — the [`ApplicationId`] is a
///   **numeric PID string**, matching the Windows/macOS contract. Matched
///   against each stream node's `application.process.id`.
/// - [`ApplicationByName`](CaptureTarget::ApplicationByName) — matched by name
///   using the tightened [`app_name_matches`] contract (exact / basename /
///   `.exe`-stripped, case-insensitive — never arbitrary substring).
/// - [`ProcessTree`](CaptureTarget::ProcessTree) — `/proc` is walked for the
///   PID's descendants, then any tree member's audio-output node is matched.
///
/// [`DeviceId`]: crate::core::config::DeviceId
/// [`ApplicationId`]: crate::core::config::ApplicationId
fn resolve_capture_target(target: &CaptureTarget) -> AudioResult<ResolvedTarget> {
    match target {
        CaptureTarget::SystemDefault => Ok(ResolvedTarget::SystemDefault),
        CaptureTarget::Device(device_id) => {
            // Native-first: match the DeviceId against the registry snapshot;
            // fall back to pw-dump only if the native path can't resolve.
            let serial = resolve_serial_native_or_subprocess(
                TargetQuery::Device(device_id.0.clone()),
                &PwNodeLookup::Device(device_id.0.as_str()),
            )?;
            log::debug!(
                "PipeWire: Device '{}' validated, resolved to node serial={}",
                device_id.0,
                serial
            );
            Ok(ResolvedTarget::Serial(serial))
        }
        CaptureTarget::Application(app_id) => {
            // ApplicationId carries a numeric PID string — same contract as the
            // Windows/macOS backends (audit finding M7). Parse PID → match the
            // registry snapshot's `application.process.id`, pw-dump fallback.
            let pid: u32 = app_id
                .0
                .parse()
                .map_err(|_| AudioError::ApplicationNotFound {
                    identifier: format!(
                        "Cannot parse PID from ApplicationId '{}': expected numeric PID",
                        app_id.0
                    ),
                })?;
            let serial = resolve_serial_native_or_subprocess(
                TargetQuery::ByPid(pid),
                &PwNodeLookup::ByPid(pid),
            )?;
            log::debug!(
                "PipeWire: Application PID {} resolved to node serial={}",
                pid,
                serial
            );
            Ok(ResolvedTarget::Serial(serial))
        }
        CaptureTarget::ApplicationByName(name) => {
            let serial = resolve_serial_native_or_subprocess(
                TargetQuery::ByAppName(name.clone()),
                &PwNodeLookup::ByAppName(name),
            )?;
            log::debug!(
                "PipeWire: ApplicationByName '{}' resolved to node serial={}",
                name,
                serial
            );
            Ok(ResolvedTarget::Serial(serial))
        }
        CaptureTarget::ProcessTree(pid) => {
            // Walk /proc for the full descendant set (falls back to the single
            // PID when /proc is unavailable), then match any tree member's
            // audio-output node.
            let tree_pids = discover_process_tree_pids(pid.0);
            log::debug!(
                "PipeWire: ProcessTree PID {} — discovered {} PIDs in tree: {:?}",
                pid.0,
                tree_pids.len(),
                tree_pids
            );
            let serial = resolve_serial_native_or_subprocess(
                TargetQuery::ByPidSet(tree_pids.clone()),
                &PwNodeLookup::ByPidSet(&tree_pids),
            )?;
            log::debug!(
                "PipeWire: ProcessTree PID {} resolved to node serial={} (searched {} PIDs)",
                pid.0,
                serial,
                tree_pids.len()
            );
            Ok(ResolvedTarget::Serial(serial))
        }
    }
}

/// Resolve a target to a node `object.serial`, preferring the **native**
/// in-process registry snapshot and falling back to the `pw-dump` subprocess
/// only when the native path cannot answer (H4 part 3 / `rsac-nat1`).
///
/// Resolution order:
/// 1. Spawn a short-lived [`PipeWireThread`] and ask it to
///    [`resolve_target`](PipeWireThread::resolve_target) `query` against the
///    settled registry snapshot. A `Some(serial)` wins immediately — no
///    subprocess is ever run.
/// 2. If the native path returns `Ok(None)` (registry settled, no match) or an
///    `Err` (thread spawn/roundtrip failure), fall back to
///    [`find_pipewire_node_serial`] with `lookup`.
///
/// The subprocess fallback preserves the historical error taxonomy
/// (`DeviceNotFound` / `ApplicationNotFound` / `BackendError`) so callers and
/// tests that pattern-match on it keep working even when `pw-dump` is absent.
fn resolve_serial_native_or_subprocess(
    query: TargetQuery,
    lookup: &PwNodeLookup<'_>,
) -> AudioResult<String> {
    match PipeWireThread::spawn() {
        Ok(pw_thread) => match pw_thread.resolve_target(query) {
            Ok(Some(serial)) => {
                log::debug!(
                    "PipeWire: native registry resolution matched serial={}",
                    serial
                );
                return Ok(serial);
            }
            Ok(None) => {
                log::debug!(
                    "PipeWire: native registry settled with no match; trying pw-dump fallback"
                );
            }
            Err(e) => {
                log::debug!(
                    "PipeWire: native registry resolution failed ({}); trying pw-dump fallback",
                    e
                );
            }
        },
        Err(e) => {
            log::debug!(
                "PipeWire: could not spawn thread for native resolution ({}); \
                 trying pw-dump fallback",
                e
            );
        }
    }
    find_pipewire_node_serial(lookup)
}

// ── PipeWire Thread Main Function ────────────────────────────────────────

/// The main function for the dedicated PipeWire thread.
///
/// This runs on the spawned thread and owns all PipeWire `Rc` objects.
/// It communicates with the caller thread via the command channel and
/// reports initialization status via `init_tx`.
///
/// # Event Loop
///
/// The loop alternates between:
/// 1. Pumping PipeWire events via `main_loop.loop_().iterate(50ms)` — this is
///    where PipeWire callbacks (including the `process` callback) fire.
/// 2. Checking for incoming commands via `command_rx.try_recv()`.
///
/// The loop exits on `Shutdown` command or if the command channel disconnects.
///
/// # Audio Data Flow
///
/// When a `StartCapture` command is received, the thread:
/// 1. Translates the already-resolved [`ResolvedTarget`] into stream properties
///    (the `pw-dump`/`/proc` resolution happened on the caller thread, M2/M3, so
///    the event loop never blocks on a subprocess here)
/// 2. Creates a PipeWire `Stream` with those properties
/// 3. Registers a **process callback** that converts raw PipeWire audio data
///    (F32LE bytes) into [`AudioBuffer`]s and pushes them to the [`BridgeProducer`]
/// 4. Registers a **param_changed callback** for format negotiation
/// 5. Connects the stream with `AUTOCONNECT | MAP_BUFFERS` flags
///
/// The `BridgeProducer::push_or_drop()` call in the process callback is lock-free
/// and non-blocking, making it safe for the real-time PipeWire callback context.
fn pw_thread_main(
    command_rx: std_mpsc::Receiver<PipeWireCommand>,
    init_tx: std_mpsc::Sender<AudioResult<()>>,
    is_running: Arc<AtomicBool>,
) {
    use std::cell::RefCell;
    use std::rc::Rc;

    use pipewire::context::ContextBox;
    use pipewire::main_loop::MainLoopBox;
    use pipewire::metadata::{Metadata, MetadataListener};
    use pipewire::node::Node;
    use pipewire::properties::properties;
    use pipewire::registry::Listener as RegistryListener;
    use pipewire::stream::{StreamBox, StreamFlags, StreamListener};
    // PipeWire's own stream-state enum (the `.state_changed` callback arg),
    // aliased to avoid clashing with the bridge's `StreamState`
    // (`crate::bridge::state::StreamState`) used by the clean-exit teardown
    // transition below. See ADR-0010.
    use pipewire::stream::StreamState as PwStreamState;
    use pipewire::types::ObjectType;

    use libspa::param::audio::{AudioFormat as SpaAudioFormat, AudioInfoRaw};
    use libspa::param::format::{MediaSubtype, MediaType};
    use libspa::param::{format_utils, ParamType};
    use libspa::pod::{Object, Pod};

    // Step 1: Initialize PipeWire library.
    pipewire::init();

    // Step 2: Create the MainLoop (non-threaded — we drive it manually via iterate()).
    let main_loop = match MainLoopBox::new(None) {
        Ok(ml) => ml,
        Err(e) => {
            let _ = init_tx.send(Err(AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: format!("Failed to create MainLoop: {}", e),
            }));
            is_running.store(false, Ordering::SeqCst);
            return;
        }
    };

    // Step 3: Create Context and connect to the PipeWire daemon.
    let context = match ContextBox::new(main_loop.loop_(), None) {
        Ok(ctx) => ctx,
        Err(e) => {
            let _ = init_tx.send(Err(AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: format!("Failed to create PipeWire Context: {}", e),
            }));
            is_running.store(false, Ordering::SeqCst);
            return;
        }
    };

    let core = match context.connect(None) {
        Ok(c) => c,
        Err(e) => {
            let _ = init_tx.send(Err(AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: format!("Failed to connect to PipeWire daemon: {}", e),
            }));
            is_running.store(false, Ordering::SeqCst);
            return;
        }
    };

    let registry = match core.get_registry() {
        Ok(r) => r,
        Err(e) => {
            let _ = init_tx.send(Err(AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: format!("Failed to get PipeWire registry: {}", e),
            }));
            is_running.store(false, Ordering::SeqCst);
            return;
        }
    };

    // ── Native registry + metadata listeners (H4 / rsac-bfd8) ─────────
    //
    // Shared snapshot populated by the registry `global` callback (audio
    // nodes) and the `default` metadata `property` callback (default
    // sink/source names). Registry/metadata global callbacks are `Fn + 'static`,
    // so we clone the `Rc<RefCell<…>>` into each closure (the wiremix idiom).
    // All callbacks fire on THIS loop thread; nothing here touches the audio
    // callback or crosses a thread boundary.
    let snapshot: Rc<RefCell<RegistrySnapshot>> =
        Rc::new(RefCell::new(RegistrySnapshot::default()));

    // Registry global id of the `default` metadata object, recorded by the
    // registry `global` callback. We bind the proxy on the loop thread (which
    // has direct `&registry` access) rather than inside the `Fn` closure, so
    // no unsafe registry reborrow is needed. `None` until the global appears.
    let pending_default_meta_id: Rc<RefCell<Option<u32>>> = Rc::new(RefCell::new(None));

    // The bound `default` Metadata proxy + its property listener. Kept alive
    // for the thread's lifetime (the proxy owns the C listener registration).
    let mut default_metadata: Option<(Metadata, MetadataListener)> = None;

    // Bound with a leading underscore so the listener stays alive for the
    // thread's lifetime (its C registration is released on drop) while not
    // tripping the unused-variable lint.
    let _registry_listener: RegistryListener = {
        let snapshot = Rc::clone(&snapshot);
        // Separate clone for the `global_remove` closure: the `global` closure
        // below MOVES `snapshot`, so `global_remove` needs its own handle.
        let snapshot_remove = Rc::clone(&snapshot);
        let pending_default_meta_id = Rc::clone(&pending_default_meta_id);
        registry
            .add_listener_local()
            .global(move |global| match global.type_ {
                ObjectType::Node => {
                    let Some(props) = global.props else {
                        return;
                    };
                    let media_class = props.get("media.class").unwrap_or("");

                    if media_class.contains("Audio/Sink") || media_class.contains("Audio/Source") {
                        // Physical audio sink/source = an enumerable *device*.
                        // Identity = object.serial (round-trips through
                        // PwNodeLookup::Device), falling back to the registry
                        // global id when a node advertises no serial.
                        let id = props
                            .get("object.serial")
                            .map(str::to_owned)
                            .unwrap_or_else(|| global.id.to_string());
                        let node_name = props.get("node.name").unwrap_or("").to_owned();
                        let name = props
                            .get("node.description")
                            .or_else(|| props.get("node.nick"))
                            .or_else(|| props.get("node.name"))
                            .unwrap_or("PipeWire Device")
                            .to_owned();
                        snapshot.borrow_mut().devices.insert(
                            global.id,
                            PwDeviceSnapshot {
                                id,
                                name,
                                node_name,
                                media_class: media_class.to_owned(),
                            },
                        );
                    } else if media_class.contains("Stream") {
                        // A per-application stream node (e.g.
                        // `Stream/Output/Audio`). It is an enumerable
                        // *application* source iff it advertises a parseable
                        // numeric `application.process.id` — the same predicate
                        // the old `pw-dump` parser used (media.class contains
                        // "Stream" + a non-zero PID). Nodes without a usable PID
                        // are skipped (the old `pid == 0` filter).
                        let Some(pid) = props
                            .get("application.process.id")
                            .and_then(|s| s.parse::<u32>().ok())
                            .filter(|&p| p != 0)
                        else {
                            return;
                        };
                        let app_name = props
                            .get("application.name")
                            .or_else(|| props.get("application.process.binary"))
                            .unwrap_or("Unknown")
                            .to_owned();
                        // node_serial mirrors the device-identity contract:
                        // object.serial, falling back to the registry global id.
                        let node_serial = props
                            .get("object.serial")
                            .map(str::to_owned)
                            .unwrap_or_else(|| global.id.to_string());
                        snapshot.borrow_mut().applications.insert(
                            global.id,
                            PwAppSnapshot {
                                pid,
                                app_name,
                                node_serial,
                            },
                        );
                    }
                }
                ObjectType::Metadata => {
                    // Record the `default` metadata global id so the loop body
                    // can bind a property listener for default sink/source.
                    let is_default = global
                        .props
                        .and_then(|p| p.get("metadata.name"))
                        .map(|n| n == "default")
                        .unwrap_or(false);
                    if is_default {
                        *pending_default_meta_id.borrow_mut() = Some(global.id);
                    }
                }
                _ => {}
            })
            .global_remove(move |id| {
                // A node going away must not linger in the snapshot — clear it
                // from both the device and the application maps (a given global
                // id is only ever in one of them, so the extra remove is a
                // cheap no-op when it isn't an application node).
                let mut snap = snapshot_remove.borrow_mut();
                snap.devices.remove(&id);
                snap.applications.remove(&id);
            })
            .register()
    };

    // Signal successful initialization back to the caller.
    if init_tx.send(Ok(())).is_err() {
        // Caller dropped the receiver — no point continuing.
        is_running.store(false, Ordering::SeqCst);
        return;
    }

    // ── Snapshot roundtrip helper ────────────────────────────────────
    //
    // A snapshot of the registry/metadata state is only meaningful *after* the
    // daemon has finished its initial dump. `core.sync()` posts a sequence to
    // the server; when the matching `done` event comes back, every `global`
    // (and metadata `property`) event posted before it has already been
    // delivered. We register a one-shot `done` listener, fire `sync`, and pump
    // the loop until the sequence completes (bounded by `SNAPSHOT_TIMEOUT`).
    //
    // Sequence: roundtrip (settle the registry dump so the `default` metadata
    // global's id is known) → bind the metadata proxy → second roundtrip (settle
    // its `property` callbacks) → read. Binding before the first roundtrip raced
    // and left the default unresolved on the first call (rsac-bfd8).
    let run_snapshot_roundtrip = |default_metadata: &mut Option<(Metadata, MetadataListener)>| {
        // Pump one `core.sync()` roundtrip: post a sequence and iterate the loop
        // until the matching `done` arrives (every `global`/metadata `property`
        // event posted before it is then guaranteed delivered), bounded by
        // SNAPSHOT_TIMEOUT.
        let pump_sync = || {
            let done = Rc::new(std::cell::Cell::new(false));
            let pending = match core.sync(0) {
                Ok(seq) => seq.seq(),
                Err(e) => {
                    log::debug!("PipeWire: core.sync failed: {}", e);
                    return;
                }
            };
            let done_cb = Rc::clone(&done);
            let core_listener = core
                .add_listener_local()
                .done(move |id, seq| {
                    if id == pipewire::core::PW_ID_CORE && seq.seq() >= pending {
                        done_cb.set(true);
                    }
                })
                .register();
            let deadline = std::time::Instant::now() + SNAPSHOT_TIMEOUT;
            while !done.get() && std::time::Instant::now() < deadline {
                let _ = main_loop.loop_().iterate(Duration::from_millis(50));
            }
            drop(core_listener);
        };

        // ORDER MATTERS (rsac-bfd8 fix): on the first call `pending_default_meta_id`
        // is unknown until the registry's initial dump has been delivered. So:
        // (1) settle the registry dump so the `default` metadata global appears and
        // its id is recorded; (2) THEN bind the metadata proxy; (3) THEN a second
        // roundtrip so the proxy's `property` callbacks fire before we read.
        // Binding before any roundtrip raced and left the default unresolved.
        pump_sync();

        // Bind the default metadata proxy (once) so its property callback can
        // populate default sink/source. Binding happens here on the loop thread
        // — never inside the `Fn` registry closure — so no unsafe reborrow.
        if default_metadata.is_none() {
            if let Some(meta_id) = *pending_default_meta_id.borrow() {
                let object = pipewire::registry::GlobalObject {
                    id: meta_id,
                    permissions: pipewire::permissions::PermissionFlags::empty(),
                    type_: ObjectType::Metadata,
                    version: 0,
                    props: None::<pipewire::properties::PropertiesBox>,
                };
                match registry.bind::<Metadata, _>(&object) {
                    Ok(metadata) => {
                        let snapshot = Rc::clone(&snapshot);
                        let listener = metadata
                            .add_listener_local()
                            .property(move |_subject, key, _type, value| {
                                // `default.audio.sink`/`.source` values are JSON
                                // objects like {"name":"alsa_output..."}; pull out
                                // the node name. `None` value = property removed.
                                match key {
                                    Some("default.audio.sink") => {
                                        snapshot.borrow_mut().default.sink_name =
                                            parse_default_metadata_name(value);
                                    }
                                    Some("default.audio.source") => {
                                        snapshot.borrow_mut().default.source_name =
                                            parse_default_metadata_name(value);
                                    }
                                    _ => {}
                                }
                                0
                            })
                            .register();
                        *default_metadata = Some((metadata, listener));
                    }
                    Err(e) => {
                        log::debug!("PipeWire: failed to bind default metadata: {}", e);
                    }
                }
            }
        }

        // Second roundtrip: now that the metadata proxy is bound, settle its
        // `property` callbacks (default sink/source) before the snapshot is read.
        pump_sync();
    };

    // ── Step 4: Enter the event loop ─────────────────────────────────

    // Thread-local state for the current capture session.
    // The stream must outlive its listener (the listener registers C callbacks
    // against the stream's raw pointer). We enforce this by dropping the
    // listener before the stream in all cleanup paths.
    let mut capture_stream: Option<StreamBox> = None;
    let mut capture_listener: Option<StreamListener<CaptureStreamData>> = None;
    // Producer-terminal-signal (FH-1 / ADR-0010): a clone of the active session's
    // `BridgeShared`, retained on THIS thread so every capture-loop teardown path
    // (StopCapture, Shutdown, command-channel disconnect, final loop exit) can
    // drive the bridge to a terminal/ending state instead of leaving a Linux
    // reader hung. Set together with `capture_stream`/`capture_listener` in the
    // StartCapture arm and cleared together in StopCapture so a later StartCapture
    // on the same thread never transitions a stale prior session. The `.process`
    // / `.state_changed` callbacks own the spontaneous-death path (signal_error);
    // this Arc owns the GRACEFUL clean-exit path (signal_done → Stopping).
    let mut active_shared: Option<Arc<BridgeShared>> = None;

    loop {
        // Pump PipeWire events. The `process` callback fires during iterate()
        // on this same thread, pushing audio data via BridgeProducer::push_or_drop().
        let _ = main_loop.loop_().iterate(Duration::from_millis(50));

        // Check for incoming commands (non-blocking).
        match command_rx.try_recv() {
            Ok(PipeWireCommand::StartCapture {
                config,
                resolved,
                producer,
                response_tx,
            }) => {
                log::debug!(
                    "PipeWire thread: StartCapture received (target={:?}, {}Hz, {}ch)",
                    config.target,
                    config.sample_rate,
                    config.channels
                );

                // Clean up any existing capture session first.
                if capture_listener.is_some() || capture_stream.is_some() {
                    log::debug!("PipeWire thread: cleaning up previous capture session");
                    capture_listener = None;
                    capture_stream = None;
                }

                // ── Build PipeWire stream properties from the resolved target ──
                //
                // Resolution (pw-dump / /proc) already happened on the caller
                // thread in `start_capture()` (M2/M3): here we only translate
                // the pre-computed `object.serial` into stream properties, which
                // never blocks the event loop.

                let mut props = properties! {
                    *pipewire::keys::NODE_NAME => "rsac-capture",
                    *pipewire::keys::STREAM_CAPTURE_SINK => "true",
                    *pipewire::keys::STREAM_MONITOR => "true",
                };

                match &resolved {
                    ResolvedTarget::SystemDefault => {
                        // No TARGET_OBJECT — captures from the default output
                        // sink monitor. STREAM_CAPTURE_SINK + STREAM_MONITOR
                        // handle the routing.
                        log::debug!("PipeWire: SystemDefault — no TARGET_OBJECT");
                    }
                    ResolvedTarget::Serial(serial) => {
                        // TARGET_OBJECT = the resolved node `object.serial`.
                        props.insert(*pipewire::keys::TARGET_OBJECT, serial.as_str());
                        log::debug!("PipeWire: TARGET_OBJECT={}", serial);
                    }
                }

                // ── Create the PipeWire stream ──

                let stream = match StreamBox::new(&core, "rsac-capture", props) {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = response_tx.send(Err(AudioError::BackendError {
                            backend: "PipeWire".to_string(),
                            operation: "create_stream".to_string(),
                            message: format!("Failed to create PipeWire stream: {}", e),
                            context: None,
                        }));
                        continue;
                    }
                };

                // ── Build user data for stream callbacks ──

                // Producer-terminal-signal (FH-1 / ADR-0010): retain a clone of
                // this session's `BridgeShared` for the GRACEFUL clean-exit
                // teardown transition below, BEFORE `producer` is moved into the
                // listener's user data (cloning the Arc is cheap and lock-free).
                let session_shared = Arc::clone(producer.shared());

                let user_data = CaptureStreamData {
                    format: AudioInfoRaw::new(),
                    producer,
                    channels: config.channels,
                    sample_rate: config.sample_rate,
                    // Pre-size the misaligned-edge fallback scratch off the RT
                    // thread so the rare realignment path (#30) reuses capacity
                    // rather than allocating in the `.process` callback. Matches
                    // the bridge's worst-case stereo period seed (ADR-0001).
                    realign_scratch: Vec::with_capacity(2048),
                    misaligned_chunks: 0,
                    misaligned_truncated_bytes: 0,
                };

                // ── Register stream listener with callbacks ──

                let listener = match stream
                    .add_local_listener_with_user_data(user_data)
                    .state_changed(|_stream, user_data, _old, new| {
                        // Producer-terminal-signal (FH-1 / ADR-0010): catch
                        // SPONTANEOUS producer death that never flows through the
                        // `.process` data callback — daemon/proxy death (Error)
                        // and node removal / disconnect (Unconnected). Drive the
                        // bridge to the terminal Error state so a parked Linux
                        // reader observes StreamEnded instead of hanging forever.
                        //
                        // RT-context note: PipeWire may invoke this from the loop
                        // thread; `signal_error()` is lock-free + alloc-free (a
                        // single atomic state store + a waker wake, ADR-0001), so
                        // it is safe regardless of which thread fires it. Benign
                        // transitions (Connecting / Paused / Streaming) and the
                        // normal connect handshake MUST NOT poison the stream, so
                        // only Error / Unconnected signal.
                        match new {
                            PwStreamState::Error(_) | PwStreamState::Unconnected => {
                                user_data.producer.signal_error();
                            }
                            _ => {}
                        }
                    })
                    .param_changed(|_stream, user_data, id, param| {
                        // Format negotiation callback.
                        // PipeWire calls this when the actual stream format is
                        // negotiated (may differ from what we requested).

                        let Some(param) = param else {
                            // NULL param means format cleared.
                            return;
                        };

                        if id != ParamType::Format.as_raw() {
                            // Not a format parameter — ignore.
                            return;
                        }

                        let (media_type, media_subtype) = match format_utils::parse_format(param) {
                            Ok(v) => v,
                            Err(_) => return,
                        };

                        // Only accept raw audio.
                        if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
                            return;
                        }

                        // Parse the negotiated format into our AudioInfoRaw.
                        let _ = user_data.format.parse(param);

                        // Update channels/sample_rate from the negotiated format
                        // so the process callback creates AudioBuffers with the
                        // correct metadata. This keeps PER-BUFFER metadata
                        // authoritative (`AudioBuffer::channels()/sample_rate()`
                        // reflect the negotiated values).
                        let negotiated_channels = user_data.format.channels();
                        let negotiated_rate = user_data.format.rate();
                        if negotiated_channels > 0 {
                            user_data.channels = negotiated_channels as u16;
                        }
                        if negotiated_rate > 0 {
                            user_data.sample_rate = negotiated_rate;
                        }

                        // PU-1/PERF-07 (rsac-2c56): publish the negotiated
                        // *delivery* format onto the bridge so `stream.format()`
                        // and `StreamStats.format_description` report what is
                        // actually delivered, not merely what was requested. The
                        // bridge normalizes `sample_format` to F32 internally
                        // (the process callback always pushes interleaved f32),
                        // so the value passed here is ignored — only the
                        // negotiated rate/channels carry through. Cheap, lock-free
                        // (a single `Release` store), and called only on the
                        // negotiation event, never the per-buffer hot path.
                        user_data.producer.set_negotiated_format(
                            &crate::core::config::AudioFormat {
                                sample_rate: user_data.sample_rate,
                                channels: user_data.channels,
                                sample_format: crate::core::config::SampleFormat::F32,
                            },
                        );

                        log::debug!(
                            "PipeWire format negotiated: {:?}, {}ch @ {}Hz",
                            user_data.format.format(),
                            negotiated_channels,
                            negotiated_rate
                        );
                    })
                    .process(|stream, user_data| {
                        // Audio data callback — runs in the PipeWire real-time
                        // context during main_loop.iterate().
                        //
                        // REAL-TIME SAFETY:
                        // - No heap allocation: `push_samples_guarded` sources its
                        //   buffer from the bridge's free-list return ring, so the
                        //   only work here is a bulk reinterpret + the copy that
                        //   the push performs internally.
                        // - Lock-free (rtrb), no blocking, no I/O, no logging.
                        //
                        // FFI-BOUNDARY PANIC GUARD (PS-4 / rsac-5a48): this
                        // `.process` closure is registered as a C callback against
                        // the PipeWire stream and is invoked from inside
                        // `main_loop.iterate()`, so a panic unwinding out of here
                        // would cross PipeWire's C frames — undefined behavior. We
                        // therefore use the panic-GUARDED push below
                        // (`push_samples_guarded`), which wraps the push in
                        // `catch_unwind` so a panic can never escape into the C
                        // frame: on a caught panic it logs once, counts a drop, and
                        // poisons the stream to Error so a parked reader observes
                        // end-of-stream instead of spinning on a dead callback. The
                        // guard is alloc-free on the happy path (its closure only
                        // borrows the producer and the sample slice), so ADR-0001's
                        // steady-state zero-allocation guarantee is preserved.
                        // Windows runs the equivalent push on rsac's *own* Rust
                        // thread, where an unwind is well-defined, so it stays on
                        // the unguarded `push_samples_or_drop`.

                        let Some(mut buffer) = stream.dequeue_buffer() else {
                            return;
                        };

                        let datas = buffer.datas_mut();
                        if datas.is_empty() {
                            return;
                        }

                        let data = &mut datas[0];

                        // Honor the SPA chunk's offset/size: the valid audio
                        // region is `[offset, offset + size)` within the mapped
                        // buffer, NOT always `[0, size)`.
                        let chunk = data.chunk();
                        let offset = chunk.offset() as usize;
                        let size = chunk.size() as usize;
                        if size == 0 {
                            return;
                        }

                        let channels = user_data.channels;
                        let sample_rate = user_data.sample_rate;

                        if let Some(raw_bytes) = data.data() {
                            // Clamp the valid region to the actually-mapped bytes
                            // to stay memory-safe regardless of offset/size.
                            let end = offset.saturating_add(size).min(raw_bytes.len());
                            if offset >= end {
                                return;
                            }
                            let valid = &raw_bytes[offset..end];

                            // Reinterpret the negotiated F32LE bytes as `&[f32]`
                            // instead of a per-sample `from_le_bytes` loop. On the
                            // little-endian hosts PipeWire runs on, the in-memory
                            // representation equals the F32LE byte layout. PipeWire
                            // audio buffers are normally word-aligned, so the
                            // `head`/`tail` `align_to` yields are empty and we take
                            // the zero-copy fast path consuming the whole aligned
                            // run of samples.
                            //
                            // SAFETY: every bit pattern is a valid `f32`, and we
                            // only read initialized bytes within `valid`.
                            let (head, samples, tail) = unsafe { valid.align_to::<f32>() };

                            if head.is_empty() && tail.is_empty() {
                                // Fast path: the valid region is word-aligned, so
                                // `samples` covers it whole with no copy.
                                if !samples.is_empty() {
                                    // Push to the ring buffer through the
                                    // FFI-boundary panic guard (see the REAL-TIME
                                    // SAFETY note above). If the ring is full the
                                    // data is dropped (back-pressure) and the
                                    // overrun counter is incremented; a panic, if
                                    // one ever occurred, is contained rather than
                                    // unwinding into PipeWire's C frames.
                                    // `_stamped`: stream-position timestamps
                                    // (frames offered / rate — integer math, same
                                    // alloc-free path; rsac-522b / rsac-ec25).
                                    user_data.producer.push_samples_guarded_stamped(
                                        samples,
                                        channels,
                                        sample_rate,
                                    );
                                }
                            } else {
                                // Misaligned-edge fallback (#30): the valid region
                                // does NOT start on a 4-byte boundary, so `align_to`
                                // left bytes in `head`/`tail` that the fast path
                                // would SILENTLY DROP (up to a whole sample). Re-decode
                                // every complete f32 from the raw bytes via
                                // `from_le_bytes` into the pre-sized reusable scratch
                                // (alloc-free in steady state — ADR-0001) so no whole
                                // sample is lost regardless of the start offset. Only a
                                // genuinely truncated trailing partial sample (byte
                                // length not a multiple of 4) is unrecoverable.
                                //
                                // NOTE (rsac-9096): a `debug_assert!` on the head/tail
                                // sizes used to live here, but (a) `align_to`'s
                                // documented contract does NOT guarantee a maximal
                                // middle slice (only that the middle is aligned), so
                                // the assert leaned on an implementation detail, and
                                // (b) it was a panic site inside this C callback in
                                // debug builds, OUTSIDE any unwind guard — a firing
                                // assert would unwind into PipeWire's C frames (UB).
                                // Correctness never depended on it: the decode below
                                // reads the raw bytes directly, however align_to
                                // split them.
                                let truncated_bytes =
                                    decode_unaligned_f32_le(valid, &mut user_data.realign_scratch);
                                // rsac-9096: NO logging here — this closure runs
                                // inside PipeWire's C `.process` callback (real-time
                                // path, outside any panic guard), where `log::warn!`
                                // would format (allocate) and a typical logger backend
                                // would lock + syscall. Record the occurrence in the
                                // plain per-session counters instead (exclusive `&mut`
                                // access; saturating adds — RT-safe) and let the
                                // non-RT `CaptureStreamData::drop` teardown emit the
                                // one-shot summary.
                                user_data.misaligned_chunks =
                                    user_data.misaligned_chunks.saturating_add(1);
                                user_data.misaligned_truncated_bytes = user_data
                                    .misaligned_truncated_bytes
                                    .saturating_add(truncated_bytes as u64);
                                if !user_data.realign_scratch.is_empty() {
                                    // Disjoint field borrows: `realign_scratch` is read
                                    // immutably while `producer` is used — allowed.
                                    // Same stamped push as the aligned fast path so
                                    // the stream-position timeline stays contiguous
                                    // across a realigned chunk.
                                    let realigned = user_data.realign_scratch.as_slice();
                                    user_data.producer.push_samples_guarded_stamped(
                                        realigned,
                                        channels,
                                        sample_rate,
                                    );
                                }
                            }
                        }

                        // The PipeWire buffer is automatically re-queued when
                        // `buffer` goes out of scope (RAII).
                    })
                    .register()
                {
                    Ok(l) => l,
                    Err(e) => {
                        let _ = response_tx.send(Err(AudioError::BackendError {
                            backend: "PipeWire".to_string(),
                            operation: "register_listener".to_string(),
                            message: format!("Failed to register PipeWire stream listener: {}", e),
                            context: None,
                        }));
                        continue;
                    }
                };

                // ── Build format Pod for stream.connect() ──

                let mut audio_info = AudioInfoRaw::new();
                audio_info.set_format(SpaAudioFormat::F32LE);
                audio_info.set_rate(config.sample_rate);
                audio_info.set_channels(config.channels as u32);

                let pod_object = Object {
                    type_: pipewire::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
                    id: ParamType::EnumFormat.as_raw(),
                    properties: audio_info.into(),
                };

                let values: Vec<u8> = match pipewire::spa::pod::serialize::PodSerializer::serialize(
                    std::io::Cursor::new(Vec::new()),
                    &pipewire::spa::pod::Value::Object(pod_object),
                ) {
                    Ok(result) => result.0.into_inner(),
                    Err(e) => {
                        let _ = response_tx.send(Err(AudioError::BackendError {
                            backend: "PipeWire".to_string(),
                            operation: "format_pod".to_string(),
                            message: format!("Failed to serialize format Pod: {:?}", e),
                            context: None,
                        }));
                        continue;
                    }
                };

                let Some(pod) = Pod::from_bytes(&values) else {
                    let _ = response_tx.send(Err(AudioError::BackendError {
                        backend: "PipeWire".to_string(),
                        operation: "format_pod".to_string(),
                        message: "Failed to create Pod from serialized bytes".to_string(),
                        context: None,
                    }));
                    continue;
                };
                let mut params = [pod];

                // ── Connect the stream ──

                if let Err(e) = stream.connect(
                    libspa::utils::Direction::Input,
                    None,
                    StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
                    &mut params,
                ) {
                    let _ = response_tx.send(Err(AudioError::BackendError {
                        backend: "PipeWire".to_string(),
                        operation: "stream_connect".to_string(),
                        message: format!("Failed to connect PipeWire stream: {}", e),
                        context: None,
                    }));
                    continue;
                }

                log::debug!(
                    "PipeWire thread: stream created and connected (state={:?})",
                    stream.state()
                );

                // Store the stream and listener — they must stay alive for
                // callbacks to fire. Listener is dropped before stream in all
                // cleanup paths.
                capture_stream = Some(stream);
                capture_listener = Some(listener);
                // Producer-terminal-signal (FH-1 / ADR-0010): retain the session
                // handle so the graceful teardown paths below can end the stream.
                active_shared = Some(session_shared);

                let _ = response_tx.send(Ok(()));
            }

            Ok(PipeWireCommand::StopCapture { response_tx }) => {
                log::debug!("PipeWire thread: StopCapture received");

                // rsac-78b2 (force_set-vs-graceful ordering): tear down the
                // listener+stream FIRST, then signal the graceful end. The
                // `.state_changed` arm calls the UNGUARDED `signal_error()`
                // (`force_set(Error)`, last-writer-wins), so destroying the stream
                // can synchronously deliver a `StreamState::Unconnected` transition
                // whose callback writes terminal `Error`. If the graceful
                // `Running → Stopping` CAS ran first (the previous order), that
                // synchronous `Unconnected` during the drop would DOWNGRADE the
                // just-set graceful `Stopping` to `Error`, mis-reporting a clean
                // stop as a fatal one. Dropping the listener first removes its hook
                // (`StreamListener::drop` → `hook::remove`), so no further
                // `.state_changed` callback can fire; the graceful CAS that follows
                // then runs with no racing writer.
                //
                // Teardown order within this block is unchanged: listener before
                // stream (the listener's C callbacks reference the stream pointer).
                capture_listener = None;
                capture_stream = None;

                // Producer-terminal-signal (FH-1 / ADR-0010): now that no callback
                // can race us, drive the bridge to a graceful ending state so a
                // parked Linux reader unblocks promptly. `Running → Stopping` (the
                // graceful sibling) keeps any buffered tail drainable; the
                // subsequent `BridgeStream::stop` path completes Stopping →
                // Stopped. Idempotent: the CAS no-ops if a `.state_changed`
                // spontaneous-death already poisoned the stream to `Error` during
                // the drop above — a genuine error correctly wins over the stop.
                signal_session_graceful_end(active_shared.take());

                let _ = response_tx.send(Ok(()));
            }

            Ok(PipeWireCommand::SnapshotDevices { response_tx }) => {
                log::debug!("PipeWire thread: SnapshotDevices received");
                // Settle the registry dump before reading (else we race an
                // empty registry and report "no devices" on a healthy system).
                run_snapshot_roundtrip(&mut default_metadata);
                let devices: Vec<PwDeviceSnapshot> =
                    snapshot.borrow().devices.values().cloned().collect();
                log::debug!(
                    "PipeWire thread: SnapshotDevices → {} node(s)",
                    devices.len()
                );
                // Only owned Vecs cross the channel.
                let _ = response_tx.send(Ok(devices));
            }

            Ok(PipeWireCommand::SnapshotDefault { response_tx }) => {
                log::debug!("PipeWire thread: SnapshotDefault received");
                // Same settle: also gives the `default` metadata property
                // callbacks a chance to fire after the proxy is bound.
                run_snapshot_roundtrip(&mut default_metadata);
                let default = snapshot.borrow().default.clone();
                let _ = response_tx.send(Ok(default));
            }

            Ok(PipeWireCommand::SnapshotApplications { response_tx }) => {
                log::debug!("PipeWire thread: SnapshotApplications received");
                // Settle the registry dump before reading (else we race an
                // empty registry and report "no applications" on a host that is
                // actively playing audio).
                run_snapshot_roundtrip(&mut default_metadata);
                // PID-deduplicate: an application may own several stream nodes
                // (each a distinct registry global), but it is a single source.
                // The first node seen for a PID wins, mirroring the old
                // subprocess parser's "skip if app:<pid> already present".
                let mut seen_pids: std::collections::BTreeSet<u32> =
                    std::collections::BTreeSet::new();
                let mut apps: Vec<PwAppSnapshot> = Vec::new();
                for app in snapshot.borrow().applications.values() {
                    if seen_pids.insert(app.pid) {
                        apps.push(app.clone());
                    }
                }
                log::debug!(
                    "PipeWire thread: SnapshotApplications → {} application(s)",
                    apps.len()
                );
                // Only owned Vecs cross the channel.
                let _ = response_tx.send(Ok(apps));
            }

            Ok(PipeWireCommand::ResolveTarget { query, response_tx }) => {
                log::debug!("PipeWire thread: ResolveTarget received ({:?})", query);
                // Settle the registry dump before matching (else we race an
                // empty registry and report "no match" on a healthy system).
                run_snapshot_roundtrip(&mut default_metadata);
                // Build owned, PID-deduplicated snapshots exactly as the
                // SnapshotDevices / SnapshotApplications arms do, then match
                // natively (pure function, no subprocess).
                let devices: Vec<PwDeviceSnapshot> =
                    snapshot.borrow().devices.values().cloned().collect();
                let mut seen_pids: std::collections::BTreeSet<u32> =
                    std::collections::BTreeSet::new();
                let mut apps: Vec<PwAppSnapshot> = Vec::new();
                for app in snapshot.borrow().applications.values() {
                    if seen_pids.insert(app.pid) {
                        apps.push(app.clone());
                    }
                }
                let matched = resolve_target_from_snapshot(&query, &devices, &apps);
                log::debug!(
                    "PipeWire thread: ResolveTarget → {:?} (from {} device(s), {} app(s))",
                    matched,
                    devices.len(),
                    apps.len()
                );
                let _ = response_tx.send(Ok(matched));
            }

            Ok(PipeWireCommand::EnumNodeFormats {
                serial,
                response_tx,
            }) => {
                log::debug!(
                    "PipeWire thread: EnumNodeFormats received (serial={})",
                    serial
                );

                // The advertised formats are discovered by binding the node and
                // pumping `enum_params(EnumFormat)`; the `param` callbacks fire
                // on THIS loop thread and accumulate into this cell. Only the
                // owned Vec crosses the channel. ADVISORY ONLY (L2 / EF-3):
                // this never touches the capture stream's negotiated format.
                let formats: Rc<RefCell<Vec<crate::core::config::AudioFormat>>> =
                    Rc::new(RefCell::new(Vec::new()));

                // Settle the registry's initial dump first so the serial →
                // global-id resolution below sees every audio node (the same
                // race SnapshotDevices guards against).
                run_snapshot_roundtrip(&mut default_metadata);

                // Resolve the requested object.serial to its registry global id.
                // The device snapshot keys nodes by global id and stores the
                // object.serial in `PwDeviceSnapshot::id`; we invert that here.
                let global_id = snapshot
                    .borrow()
                    .devices
                    .iter()
                    .find(|(_, dev)| dev.id == serial)
                    .map(|(&gid, _)| gid);

                let Some(global_id) = global_id else {
                    // Unknown serial (node gone, or never an enumerable device):
                    // documented-empty fallback, not an error.
                    log::debug!(
                        "PipeWire thread: EnumNodeFormats serial={} not in registry — empty",
                        serial
                    );
                    let _ = response_tx.send(Ok(Vec::new()));
                    continue;
                };

                // Bind the Node proxy on the loop thread (direct `&registry`
                // access — never inside a `Fn` closure, so no unsafe reborrow).
                let object = pipewire::registry::GlobalObject {
                    id: global_id,
                    permissions: pipewire::permissions::PermissionFlags::empty(),
                    type_: ObjectType::Node,
                    version: 0,
                    props: None::<pipewire::properties::PropertiesBox>,
                };
                let node = match registry.bind::<Node, _>(&object) {
                    Ok(n) => n,
                    Err(e) => {
                        // Cannot bind (permissions, node vanished): empty, not a
                        // hard error — discovery is best-effort/advisory.
                        log::debug!(
                            "PipeWire thread: EnumNodeFormats bind(global_id={}) failed: {}",
                            global_id,
                            e
                        );
                        let _ = response_tx.send(Ok(Vec::new()));
                        continue;
                    }
                };

                // Register a `param` listener that parses each emitted
                // EnumFormat pod and records the mapped rsac AudioFormat.
                let formats_cb = Rc::clone(&formats);
                let _node_listener = node
                    .add_listener_local()
                    .param(move |_seq, param_type, _index, _next, param| {
                        // Compare via `as_raw()` for parity with the
                        // `param_changed` path (which matches on the raw id) and
                        // to avoid relying on `ParamType: PartialEq`.
                        if param_type.as_raw() != ParamType::EnumFormat.as_raw() {
                            return;
                        }
                        let Some(param) = param else {
                            return;
                        };
                        // Only raw audio formats are mappable.
                        match format_utils::parse_format(param) {
                            Ok((MediaType::Audio, MediaSubtype::Raw)) => {}
                            _ => return,
                        }
                        // Reuse the negotiation parser: `AudioInfoRaw::parse`
                        // pulls the default value out of any SPA choice
                        // (enum/range) the node advertises, mirroring what the
                        // `param_changed` capture path does.
                        let mut info = AudioInfoRaw::new();
                        if info.parse(param).is_err() {
                            return;
                        }
                        let Some(sample_format) = spa_audio_format_to_sample_format(info.format())
                        else {
                            return;
                        };
                        let rate = info.rate();
                        let channels = info.channels();
                        // A choice may not pin rate/channels to a usable default
                        // (0 = "any"); skip those rather than fabricate a 0-Hz /
                        // 0-channel format.
                        if rate == 0 || channels == 0 {
                            return;
                        }
                        let candidate = crate::core::config::AudioFormat {
                            sample_rate: rate,
                            channels: channels as u16,
                            sample_format,
                        };
                        let mut list = formats_cb.borrow_mut();
                        if !list.contains(&candidate) {
                            list.push(candidate);
                        }
                    })
                    .register();

                // Kick off enumeration of ALL EnumFormat params, then settle.
                node.enum_params(0, Some(ParamType::EnumFormat), 0, u32::MAX);

                // Pump a `core.sync()`/`done` roundtrip so every `param` event
                // posted by `enum_params` is delivered before we read (bounded
                // by SNAPSHOT_TIMEOUT — a wedged daemon yields an empty list, not
                // a hang). Mirrors the snapshot roundtrip's `pump_sync`.
                {
                    let done = Rc::new(std::cell::Cell::new(false));
                    if let Ok(seq) = core.sync(0) {
                        let pending = seq.seq();
                        let done_cb = Rc::clone(&done);
                        let core_listener = core
                            .add_listener_local()
                            .done(move |id, seq| {
                                if id == pipewire::core::PW_ID_CORE && seq.seq() >= pending {
                                    done_cb.set(true);
                                }
                            })
                            .register();
                        let deadline = std::time::Instant::now() + SNAPSHOT_TIMEOUT;
                        while !done.get() && std::time::Instant::now() < deadline {
                            let _ = main_loop.loop_().iterate(Duration::from_millis(50));
                        }
                        drop(core_listener);
                    }
                }

                // Drop the node listener before the node proxy (the C listener
                // registration references the proxy). Both go out of scope at
                // the end of the arm, but make the order explicit for parity
                // with the stream listener/stream teardown ordering.
                drop(_node_listener);
                let result: Vec<crate::core::config::AudioFormat> = formats.borrow().clone();
                drop(node);
                log::debug!(
                    "PipeWire thread: EnumNodeFormats serial={} → {} format(s)",
                    serial,
                    result.len()
                );
                let _ = response_tx.send(Ok(result));
            }

            Ok(PipeWireCommand::Shutdown) => {
                log::debug!("PipeWire thread: Shutdown received, exiting event loop");
                // rsac-78b2 (force_set-vs-graceful ordering): tear down the
                // listener+stream BEFORE the graceful signal (same race as the
                // StopCapture arm). Destroying the stream can synchronously deliver
                // a `StreamState::Unconnected` whose `.state_changed` callback calls
                // the unguarded `signal_error()` (`force_set(Error)`); doing the
                // graceful `Running → Stopping` CAS first would let that downgrade a
                // clean shutdown to terminal `Error`. Dropping the listener first
                // removes its hook so no callback can race the CAS below.
                // Drop listener before stream — listener callbacks reference stream internals.
                drop(capture_listener.take());
                drop(capture_stream.take());
                // Producer-terminal-signal (FH-1 / ADR-0010): now end any active
                // session so a parked reader unblocks. Idempotent: a genuine error
                // recorded during the drop above correctly wins over the stop.
                signal_session_graceful_end(active_shared.take());
                break;
            }

            Err(std_mpsc::TryRecvError::Empty) => {
                // No commands waiting — continue pumping PipeWire events.
            }

            Err(std_mpsc::TryRecvError::Disconnected) => {
                // Command channel closed — caller is gone, exit gracefully.
                log::debug!("PipeWire thread: command channel disconnected, exiting");
                // rsac-78b2 (force_set-vs-graceful ordering): tear down the
                // listener+stream BEFORE the graceful signal (same race as the
                // StopCapture/Shutdown arms): a synchronous `Unconnected` from the
                // stream destroy would otherwise let the unguarded `signal_error()`
                // downgrade the graceful `Stopping` to terminal `Error`. Dropping
                // the listener first removes its hook so no callback can race the
                // CAS below.
                // Drop listener before stream — listener callbacks reference stream internals.
                drop(capture_listener.take());
                drop(capture_stream.take());
                // Producer-terminal-signal (FH-1 / ADR-0010): now end any active
                // session so a parked reader unblocks. Idempotent: a genuine error
                // recorded during the drop above correctly wins over the stop.
                signal_session_graceful_end(active_shared.take());
                break;
            }
        }
    }

    // Producer-terminal-signal (FH-1 / ADR-0010): final safety net — if the loop
    // exits via any path that did not already end the session, drive the bridge
    // to a terminal/ending state here so no Linux reader is left hung. `take()`
    // makes this a no-op when a teardown arm above already consumed the handle.
    signal_session_graceful_end(active_shared.take());

    // Cleanup: PipeWire objects are dropped via RAII when this function returns.
    // The MainLoop, Context, Core, and Registry are all dropped here.
    is_running.store(false, Ordering::SeqCst);
    log::debug!("PipeWire thread: exited cleanly");
}

/// Drive a capture session's bridge to a graceful ending state on clean loop
/// teardown (producer-terminal-signal, FH-1 / ADR-0010).
///
/// Mirrors [`BridgeProducer::signal_done`]: a best-effort `Running → Stopping`
/// CAS plus an async waker wake. `Stopping` is the GRACEFUL sibling — it keeps
/// any buffered tail drainable (it is not terminal), which is correct for a
/// clean capture-loop exit; spontaneous producer death is handled separately by
/// the `.state_changed` `signal_error()` path (`Running → Error`, terminal).
///
/// Idempotent and lock-free: the CAS no-ops if the state already advanced past
/// `Running` (e.g. a prior `signal_error()` poisoned it to `Error`), and the
/// whole call is a single atomic store + a waker wake — no allocation, no lock.
/// A `None` handle (no active session) is a no-op.
fn signal_session_graceful_end(shared: Option<Arc<BridgeShared>>) {
    if let Some(shared) = shared {
        let _ = shared
            .state
            .transition(StreamState::Running, StreamState::Stopping);
        #[cfg(feature = "async-stream")]
        shared.waker.wake();
    }
}

// ── Device-change watcher (M10 Linux arm / rsac-b92e) ─────────────────────
//
// `DeviceEnumerator::watch` needs a *persistent* PipeWire main-loop + registry
// + `default` metadata listener that lives for as long as the caller holds the
// returned `DeviceWatcher` — distinct from the short-lived per-call snapshot
// threads above (which spawn, settle one roundtrip, and exit). The OS
// notification callbacks fire on this watch thread's loop; we invoke the user
// `FnMut` from there (never the audio callback thread), satisfying the
// thread-handoff contract: the persistent loop thread IS the delivery thread.

/// Upper bound on how long [`spawn_device_watcher`] blocks waiting for the
/// watch thread to report PipeWire init success or failure (mirrors the
/// `PipeWireThread::spawn` init handshake).
const WATCH_INIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Spawn a persistent PipeWire registry + `default` metadata listener thread
/// and return a [`DeviceWatcher`] whose `Drop` tears it down (M10 Linux arm /
/// `rsac-b92e`).
///
/// The spawned thread owns its own `MainLoop`/`Context`/`Core`/`Registry`
/// (all `Rc`/`!Send`, so they must live on one thread) plus a bound `default`
/// metadata proxy. Its registry `global`/`global_remove` callbacks translate
/// audio `Audio/Sink` / `Audio/Source` node arrivals/departures into
/// [`DeviceEvent::DeviceAdded`] / [`DeviceEvent::DeviceRemoved`], and the
/// `default` metadata `property` callback translates `default.audio.sink` /
/// `default.audio.source` changes into [`DeviceEvent::DefaultChanged`]. Each
/// event is handed to the user `on_event` closure **on this loop thread**.
///
/// # Real-time safety
///
/// The watch thread is a plain notification thread — it is *not* the audio
/// callback thread, so allocating / locking / invoking the user closure here is
/// fine. The audio `process` callback (in [`pw_thread_main`]) is untouched.
///
/// # Lifecycle
///
/// The loop is pumped manually via `iterate(50 ms)` (the same idiom
/// [`pw_thread_main`] uses) so a thread-safe shutdown can be signalled without
/// the `!Send` `MainLoop::quit`: the returned watcher's teardown sets a shared
/// `AtomicBool` and joins the thread. `Drop` therefore unregisters the OS
/// listeners (their `Rc`-owned C registrations drop when the thread's locals
/// drop) and joins — no leaked thread, no hang.
///
/// # Errors
///
/// - [`AudioError::BackendInitializationFailed`] if the thread cannot be
///   spawned or PipeWire initialization fails on it.
/// - [`AudioError::Timeout`] if init does not complete within
///   [`WATCH_INIT_TIMEOUT`] (a wedged daemon surfaces as an error, never a
///   hang).
#[cfg(feature = "feat_linux")]
pub(crate) fn spawn_device_watcher(
    on_event: crate::core::interface::DeviceEventHandler,
) -> AudioResult<crate::core::interface::DeviceWatcher> {
    use crate::core::interface::DeviceWatcher;

    let (init_tx, init_rx) = std_mpsc::channel::<AudioResult<()>>();
    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_thread = Arc::clone(&shutdown);

    let thread_handle = std::thread::Builder::new()
        .name("rsac-pw-watch".to_string())
        .spawn(move || {
            watch_thread_main(on_event, init_tx, shutdown_thread);
        })
        .map_err(|e| AudioError::BackendInitializationFailed {
            backend: "PipeWire".to_string(),
            reason: format!("Failed to spawn PipeWire watch thread: {}", e),
        })?;

    // Block (bounded) until the watch thread reports init success or failure.
    // A wedged daemon must surface as Timeout, not an unbounded recv() hang.
    // Each non-Ok path JOINS the thread and RETURNS, so `thread_handle` is moved
    // only on a single path (no use-after-move) and no failure leaks a thread.
    match init_rx.recv_timeout(WATCH_INIT_TIMEOUT) {
        // The thread reported a PipeWire init error; it has already exited (or
        // is exiting). Signal + join so the failure path leaves no thread.
        Ok(Err(e)) => {
            shutdown.store(true, Ordering::SeqCst);
            let _ = thread_handle.join();
            return Err(e);
        }
        // Init succeeded — fall through to build the watcher.
        Ok(Ok(())) => {}
        Err(std_mpsc::RecvTimeoutError::Timeout) => {
            // Signal the thread to stop and join it so we never leak it on the
            // timeout path, then report the timeout.
            shutdown.store(true, Ordering::SeqCst);
            let _ = thread_handle.join();
            return Err(AudioError::Timeout {
                operation: "PipeWire watch init".to_string(),
                duration: WATCH_INIT_TIMEOUT,
            });
        }
        Err(std_mpsc::RecvTimeoutError::Disconnected) => {
            // Thread exited before reporting — join to reap it, then error.
            let _ = thread_handle.join();
            return Err(AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: "PipeWire watch thread exited before reporting init status".to_string(),
            });
        }
    }

    // Build the RAII teardown: flip the shared flag (the loop notices on its
    // next 50 ms iterate tick and exits) and join the thread exactly once. The
    // closure is the single owner of the JoinHandle, so it cannot be joined
    // twice; `DeviceWatcher::drop` already guarantees it runs at most once.
    let mut handle = Some(thread_handle);
    let teardown: Box<dyn FnOnce() + Send> = Box::new(move || {
        shutdown.store(true, Ordering::SeqCst);
        if let Some(handle) = handle.take() {
            // Best-effort join: a panicked watch thread must not panic the
            // teardown (it runs in Drop, possibly while unwinding).
            let _ = handle.join();
        }
    });

    Ok(DeviceWatcher::from_teardown(teardown))
}

/// A device the watch thread is tracking, keyed by registry global id.
///
/// Retained so `global_remove` (which only carries the registry global id) can
/// emit a [`DeviceEvent::DeviceRemoved`] with the *same* [`DeviceId`] the
/// matching [`DeviceEvent::DeviceAdded`] carried, and so a `default` metadata
/// change (which carries a node *name*) can be resolved back to that id.
#[cfg(feature = "feat_linux")]
struct WatchedDevice {
    /// The `DeviceId` string (`object.serial`, falling back to global id) — the
    /// same identity contract the snapshot path uses. Re-emitted verbatim in the
    /// matching [`DeviceEvent::DeviceRemoved`] / [`DeviceEvent::DefaultChanged`].
    id: String,
    /// Verbatim `node.name`, matched against `default.audio.sink/source` values
    /// so a default change resolves back to this device's `id`.
    node_name: String,
}

/// Body of the persistent device-watch thread (M10 / `rsac-b92e`).
///
/// Owns the PipeWire `Rc`/`!Send` objects, registers the registry + `default`
/// metadata listeners, reports init status over `init_tx`, then pumps the loop
/// via `iterate(50 ms)` until `shutdown` is set (by the watcher teardown).
#[cfg(feature = "feat_linux")]
fn watch_thread_main(
    on_event: crate::core::interface::DeviceEventHandler,
    init_tx: std_mpsc::Sender<AudioResult<()>>,
    shutdown: Arc<AtomicBool>,
) {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use pipewire::context::ContextBox;
    use pipewire::main_loop::MainLoopBox;
    use pipewire::metadata::{Metadata, MetadataListener};
    use pipewire::registry::Listener as RegistryListener;
    use pipewire::types::ObjectType;

    use crate::core::config::DeviceId;
    use crate::core::interface::{DeviceEvent, DeviceKind};

    // Macro-free init failure helper: report the error and bail. The thread
    // exits; `spawn_device_watcher` joins it on the error path.
    macro_rules! init_fail {
        ($reason:expr) => {{
            let _ = init_tx.send(Err(AudioError::BackendInitializationFailed {
                backend: "PipeWire".to_string(),
                reason: $reason,
            }));
            return;
        }};
    }

    // Step 1: init PipeWire + build the loop/context/core/registry.
    pipewire::init();

    let main_loop = match MainLoopBox::new(None) {
        Ok(ml) => ml,
        Err(e) => init_fail!(format!("Failed to create MainLoop: {}", e)),
    };
    let context = match ContextBox::new(main_loop.loop_(), None) {
        Ok(ctx) => ctx,
        Err(e) => init_fail!(format!("Failed to create PipeWire Context: {}", e)),
    };
    let core = match context.connect(None) {
        Ok(c) => c,
        Err(e) => init_fail!(format!("Failed to connect to PipeWire daemon: {}", e)),
    };
    let registry = match core.get_registry() {
        Ok(r) => r,
        Err(e) => init_fail!(format!("Failed to get PipeWire registry: {}", e)),
    };

    // Shared user closure. It is `Send` but used only on THIS thread; the
    // registry `global`/`global_remove` and metadata `property` callbacks are
    // each `Fn + 'static`, so the closure lives behind `Rc<RefCell<…>>` and we
    // clone the handle once per callback (the !Send PW idiom; heed the
    // Rc-clone-per-closure gotcha — every closure that MOVES a handle needs its
    // own clone). `RefCell` guards against re-entrancy: these callbacks fire
    // sequentially from a single loop thread, so no borrow overlaps in practice.
    let on_event: Rc<RefCell<crate::core::interface::DeviceEventHandler>> =
        Rc::new(RefCell::new(on_event));

    // Tracked devices, keyed by registry global id — so `global_remove` and
    // `default` metadata resolution can recover the DeviceId/kind/name. Lives on
    // this thread only; shared (cloned) into the callbacks.
    let devices: Rc<RefCell<BTreeMap<u32, WatchedDevice>>> = Rc::new(RefCell::new(BTreeMap::new()));

    // Records the `default` metadata global id when its registry global appears,
    // so the loop body can bind the proxy on the loop thread (direct `&registry`
    // access — never inside the `Fn` closure, so no unsafe reborrow). `None`
    // until the global is announced.
    let pending_default_meta_id: Rc<RefCell<Option<u32>>> = Rc::new(RefCell::new(None));

    // The bound `default` metadata proxy + its property listener, kept alive for
    // the thread's lifetime (the proxy owns the C listener registration).
    let mut default_metadata: Option<(Metadata, MetadataListener)> = None;

    // ── Registry listener: emit DeviceAdded / DeviceRemoved ──────────────
    let _registry_listener: RegistryListener = {
        // Per the Rc-clone-per-closure gotcha: the `global` closure MOVES these
        // handles, so `global_remove` gets its own separate clones below.
        let devices_add = Rc::clone(&devices);
        let on_event_add = Rc::clone(&on_event);
        let pending_default_meta_id = Rc::clone(&pending_default_meta_id);

        let devices_remove = Rc::clone(&devices);
        let on_event_remove = Rc::clone(&on_event);

        registry
            .add_listener_local()
            .global(move |global| match global.type_ {
                ObjectType::Node => {
                    let Some(props) = global.props else {
                        return;
                    };
                    let media_class = props.get("media.class").unwrap_or("");
                    let is_source = media_class.contains("Audio/Source");
                    let is_sink = media_class.contains("Audio/Sink");
                    if !is_source && !is_sink {
                        // Not an enumerable audio device (e.g. a Stream node).
                        return;
                    }

                    // Identity contract (parity with PwDeviceSnapshot): id =
                    // object.serial, falling back to the registry global id.
                    let id = props
                        .get("object.serial")
                        .map(str::to_owned)
                        .unwrap_or_else(|| global.id.to_string());
                    let node_name = props.get("node.name").unwrap_or("").to_owned();
                    let name = props
                        .get("node.description")
                        .or_else(|| props.get("node.nick"))
                        .or_else(|| props.get("node.name"))
                        .unwrap_or("PipeWire Device")
                        .to_owned();
                    // A device that is both source and sink (e.g. a monitor)
                    // reports Input, matching AudioDevice::kind's documented
                    // Linux behaviour.
                    let kind = if is_source {
                        DeviceKind::Input
                    } else {
                        DeviceKind::Output
                    };

                    // Record it so global_remove / default-resolution can find
                    // it later. Re-announcement of the same global id overwrites
                    // (idempotent), but we only emit DeviceAdded the first time
                    // to avoid duplicate notifications.
                    let first_seen = devices_add
                        .borrow_mut()
                        .insert(
                            global.id,
                            WatchedDevice {
                                id: id.clone(),
                                node_name,
                            },
                        )
                        .is_none();

                    if first_seen {
                        (on_event_add.borrow_mut())(DeviceEvent::DeviceAdded {
                            id: DeviceId(id),
                            name,
                            kind,
                        });
                    }
                }
                ObjectType::Metadata => {
                    // Record the `default` metadata global id so the loop body
                    // can bind a property listener for default sink/source.
                    let is_default = global
                        .props
                        .and_then(|p| p.get("metadata.name"))
                        .map(|n| n == "default")
                        .unwrap_or(false);
                    if is_default {
                        *pending_default_meta_id.borrow_mut() = Some(global.id);
                    }
                }
                _ => {}
            })
            .global_remove(move |id| {
                // A node going away: drop it from the tracking map and, if it
                // was a device we had announced, emit DeviceRemoved with the
                // same DeviceId. Non-device globals are not in the map, so the
                // lookup is a cheap no-op for them.
                let removed = devices_remove.borrow_mut().remove(&id);
                if let Some(dev) = removed {
                    (on_event_remove.borrow_mut())(DeviceEvent::DeviceRemoved {
                        id: DeviceId(dev.id),
                    });
                }
            })
            .register()
    };

    // ── Settle the initial registry dump, then bind `default` metadata ───
    //
    // Pump one `core.sync()`/`done` roundtrip so the daemon's initial registry
    // dump (and thus the `default` metadata global's id) is delivered before we
    // bind the metadata proxy. Binding before the dump settles raced and left
    // the default unresolved (mirrors the rsac-bfd8 ordering in pw_thread_main).
    //
    // NOTE: the initial dump's `global` events fire DeviceAdded for every device
    // already present when watch() is called — i.e. the watcher reports the
    // current device set as it comes up, then live changes thereafter.
    let pump_sync = |main_loop: &MainLoopBox| {
        let done = Rc::new(std::cell::Cell::new(false));
        let pending = match core.sync(0) {
            Ok(seq) => seq.seq(),
            Err(e) => {
                log::debug!("PipeWire watch: core.sync failed: {}", e);
                return;
            }
        };
        let done_cb = Rc::clone(&done);
        let core_listener = core
            .add_listener_local()
            .done(move |id, seq| {
                if id == pipewire::core::PW_ID_CORE && seq.seq() >= pending {
                    done_cb.set(true);
                }
            })
            .register();
        let deadline = std::time::Instant::now() + SNAPSHOT_TIMEOUT;
        while !done.get() && std::time::Instant::now() < deadline {
            let _ = main_loop.loop_().iterate(Duration::from_millis(50));
        }
        drop(core_listener);
    };

    pump_sync(&main_loop);

    // Bind the `default` metadata proxy (once) on the loop thread. Its property
    // callback emits DefaultChanged whenever default.audio.sink/source changes.
    if let Some(meta_id) = *pending_default_meta_id.borrow() {
        let object = pipewire::registry::GlobalObject {
            id: meta_id,
            permissions: pipewire::permissions::PermissionFlags::empty(),
            type_: ObjectType::Metadata,
            version: 0,
            props: None::<pipewire::properties::PropertiesBox>,
        };
        match registry.bind::<Metadata, _>(&object) {
            Ok(metadata) => {
                let devices_meta = Rc::clone(&devices);
                let on_event_meta = Rc::clone(&on_event);
                let listener = metadata
                    .add_listener_local()
                    .property(move |_subject, key, _type, value| {
                        let kind = match key {
                            Some("default.audio.sink") => DeviceKind::Output,
                            Some("default.audio.source") => DeviceKind::Input,
                            _ => return 0,
                        };
                        // `value` is a JSON object {"name":"..."} (or a bare
                        // quoted string on older daemons); pull the node name.
                        // `None` (property removed) → nothing to report.
                        let Some(name) = parse_default_metadata_name(value) else {
                            return 0;
                        };
                        // Resolve the node *name* to the round-trippable DeviceId
                        // (the metadata keys on node.name, devices store it
                        // verbatim). If we can't resolve it yet (e.g. the node's
                        // global hasn't been seen), skip rather than emit an
                        // unusable id.
                        let resolved = devices_meta
                            .borrow()
                            .values()
                            .find(|d| d.node_name == name)
                            .map(|d| d.id.clone());
                        if let Some(id) = resolved {
                            (on_event_meta.borrow_mut())(DeviceEvent::DefaultChanged {
                                id: DeviceId(id),
                                kind,
                            });
                        }
                        0
                    })
                    .register();
                default_metadata = Some((metadata, listener));
            }
            Err(e) => {
                log::debug!("PipeWire watch: failed to bind default metadata: {}", e);
            }
        }
    }

    // Settle the metadata proxy's initial `property` callbacks (so the current
    // default is reflected) before we report init success.
    pump_sync(&main_loop);

    // Init done — tell the caller we are live. If the caller is already gone
    // (dropped the watcher synchronously), stop now.
    if init_tx.send(Ok(())).is_err() {
        return;
    }

    // ── Step: pump the loop until shutdown is signalled ──────────────────
    //
    // Manual `iterate(50 ms)` (not `run()`) so the teardown can stop us via the
    // shared AtomicBool without the `!Send` `MainLoop::quit`. Each tick delivers
    // any pending registry/metadata events (firing the user closure on THIS
    // thread), then we re-check the flag.
    while !shutdown.load(Ordering::SeqCst) {
        let _ = main_loop.loop_().iterate(Duration::from_millis(50));
    }

    // Drop the metadata proxy/listener first (its C registration references the
    // proxy), then the rest of the PipeWire objects via RAII as the function
    // returns. The user closure is dropped here too — it will not run again.
    drop(default_metadata.take());
    log::debug!("PipeWire watch thread: exited cleanly");
}

// ── Compile-time assertions ──────────────────────────────────────────────

/// Assert that `LinuxPlatformStream` is `Send` (required by `PlatformStream`).
fn _assert_linux_platform_stream_send() {
    fn _assert<T: Send>() {}
    _assert::<LinuxPlatformStream>();
}

/// Assert that `PipeWireThread` is `Send`.
fn _assert_pipewire_thread_send() {
    fn _assert<T: Send>() {}
    _assert::<PipeWireThread>();
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use super::*;

    // ── parse_ppid_from_stat ─────────────────────────────────────────

    #[test]
    fn test_parse_ppid_from_stat_typical() {
        // Typical /proc/{pid}/stat: pid (name) state ppid ...
        let stat = "1234 (my_process) S 567 1234 1234 0 -1 4194560 100 0 0 0 0 0 0 0 20 0 1 0 123456 12345678 100 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
        assert_eq!(parse_ppid_from_stat(stat), Some(567));
    }

    #[test]
    fn test_parse_ppid_from_stat_name_with_spaces() {
        // Process name can contain spaces
        let stat = "5678 (Web Content) S 1000 5678 5678 0 -1 4194560 200 0 0 0 0 0 0 0 20 0 3 0 654321 87654321 500 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
        assert_eq!(parse_ppid_from_stat(stat), Some(1000));
    }

    #[test]
    fn test_parse_ppid_from_stat_name_with_parens() {
        // Process name can contain parentheses: "(sd-pam)"
        let stat = "42 ((sd-pam)) S 1 42 42 0 -1 1077936384 50 0 0 0 0 0 0 0 20 0 1 0 100 1234567 10 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0";
        assert_eq!(parse_ppid_from_stat(stat), Some(1));
    }

    #[test]
    fn test_parse_ppid_from_stat_pid_1_init() {
        // PID 1 (init/systemd) has PPID 0
        let stat = "1 (systemd) S 0 1 1 0 -1 4194560 100000 200000 10 20 1000 500 2000 300 20 0 1 0 1 200000000 1500 18446744073709551615 0 0 0 0 0 0 671173123 4096 1260 0 0 0 17 0 0 0 0 0 0";
        assert_eq!(parse_ppid_from_stat(stat), Some(0));
    }

    #[test]
    fn test_parse_ppid_from_stat_empty_string() {
        assert_eq!(parse_ppid_from_stat(""), None);
    }

    #[test]
    fn test_parse_ppid_from_stat_malformed() {
        // No closing parenthesis
        assert_eq!(parse_ppid_from_stat("1234 (broken"), None);
    }

    #[test]
    fn test_parse_ppid_from_stat_truncated() {
        // Only comm field, no state or ppid
        assert_eq!(parse_ppid_from_stat("1234 (name)"), None);
    }

    #[test]
    fn test_parse_ppid_from_stat_state_only() {
        // Has state but no ppid
        assert_eq!(parse_ppid_from_stat("1234 (name) S"), None);
    }

    // ── discover_process_tree_pids ───────────────────────────────────

    #[test]
    fn test_discover_process_tree_pids_current_process() {
        // The current process PID should always be in the result
        let current_pid = std::process::id();
        let pids = discover_process_tree_pids(current_pid);
        assert!(
            pids.contains(&current_pid),
            "Result should contain the parent PID. Got: {:?}",
            pids
        );
    }

    #[test]
    fn test_discover_process_tree_pids_nonexistent_pid() {
        // A PID that (almost certainly) doesn't exist should return
        // just that PID (graceful degradation).
        let fake_pid = 4_000_000_000; // max PID on Linux is typically 4194304
        let pids = discover_process_tree_pids(fake_pid);
        assert_eq!(pids, vec![fake_pid]);
    }

    #[test]
    fn test_discover_process_tree_pids_pid_1() {
        // PID 1 (init/systemd) should have children
        let pids = discover_process_tree_pids(1);
        assert!(
            pids.len() > 1,
            "PID 1 should have child processes. Got: {:?}",
            pids
        );
        assert!(pids.contains(&1), "Result should contain PID 1");
        // Result should be sorted
        for window in pids.windows(2) {
            assert!(window[0] <= window[1], "PIDs should be sorted: {:?}", pids);
        }
    }

    #[test]
    fn test_discover_process_tree_pids_sorted_and_deduped() {
        let current_pid = std::process::id();
        let pids = discover_process_tree_pids(current_pid);

        // Check sorted
        for window in pids.windows(2) {
            assert!(
                window[0] < window[1],
                "PIDs should be sorted and unique: {:?}",
                pids
            );
        }
    }

    // ── PwNodeLookup::ByPidSet matching ──────────────────────────────

    #[test]
    fn test_pw_node_lookup_by_pid_set_display() {
        // Verify the error message for ByPidSet includes the PID list
        let pids = vec![100, 200, 300];
        let result = find_pipewire_node_serial(&PwNodeLookup::ByPidSet(&pids));
        // This will fail (pw-dump not available in test), but we can verify
        // the error message format if pw-dump is available or the error type
        match result {
            Err(AudioError::ApplicationNotFound { identifier }) => {
                assert!(
                    identifier.contains("100")
                        && identifier.contains("200")
                        && identifier.contains("300"),
                    "Error should list all PIDs. Got: {}",
                    identifier
                );
            }
            Err(AudioError::BackendError { message, .. }) => {
                // pw-dump not available — that's fine for this test
                assert!(
                    message.contains("pw-dump"),
                    "Should mention pw-dump in error: {}",
                    message
                );
            }
            Ok(_) => {
                // Unlikely but possible if pw-dump returns matching data
            }
            Err(e) => {
                panic!("Unexpected error type: {:?}", e);
            }
        }
    }

    // ── resolve_capture_target ───────────────────────────────────────
    // `CaptureTarget` is already in scope via `super::*`.
    use crate::core::config::{ApplicationId, DeviceId, ProcessId};

    #[test]
    fn test_resolve_capture_target_system_default_no_pw_dump() {
        // SystemDefault must resolve to ResolvedTarget::SystemDefault without
        // invoking pw-dump at all (so it works even with PipeWire absent).
        let resolved = resolve_capture_target(&CaptureTarget::SystemDefault)
            .expect("SystemDefault should always resolve");
        match resolved {
            ResolvedTarget::SystemDefault => {}
            other => panic!("Expected SystemDefault, got {:?}", other),
        }
    }

    #[test]
    fn test_resolve_capture_target_application_non_numeric_is_app_not_found() {
        // ApplicationId carries a numeric PID string (Windows/macOS contract,
        // M7). A non-numeric id must fail fast with ApplicationNotFound BEFORE
        // any pw-dump call.
        let target = CaptureTarget::Application(ApplicationId("not_a_pid".to_string()));
        match resolve_capture_target(&target) {
            Err(AudioError::ApplicationNotFound { identifier }) => {
                assert!(
                    identifier.contains("not_a_pid"),
                    "error should echo the bad id: {}",
                    identifier
                );
                assert!(
                    identifier.contains("PID"),
                    "error should mention PID expectation: {}",
                    identifier
                );
            }
            other => panic!("Expected ApplicationNotFound, got {:?}", other),
        }
    }

    #[test]
    fn test_resolve_capture_target_application_numeric_pid_uses_pw_dump() {
        // A numeric ApplicationId parses to a PID and then goes through pw-dump.
        // Without a matching node it is ApplicationNotFound; without pw-dump it
        // is a BackendError. Either is acceptable — what matters is that the
        // numeric id is NOT inserted verbatim as TARGET_OBJECT (the M7 bug).
        let target = CaptureTarget::Application(ApplicationId("424242".to_string()));
        match resolve_capture_target(&target) {
            Err(AudioError::ApplicationNotFound { identifier }) => {
                assert!(
                    identifier.contains("424242"),
                    "lookup error should reference the PID: {}",
                    identifier
                );
            }
            Err(AudioError::BackendError { message, .. }) => {
                assert!(
                    message.contains("pw-dump"),
                    "expected pw-dump-related backend error: {}",
                    message
                );
            }
            Ok(ResolvedTarget::Serial(_)) => {
                // A node for PID 424242 actually existed — fine.
            }
            other => panic!("Unexpected resolve result: {:?}", other),
        }
    }

    #[test]
    fn test_resolve_capture_target_device_missing_is_device_not_found() {
        // A device id that cannot exist must surface as DeviceNotFound (M8),
        // not a silent connect-to-nothing. If pw-dump is unavailable we get a
        // BackendError instead — also acceptable.
        let target = CaptureTarget::Device(DeviceId("rsac-no-such-device".to_string()));
        match resolve_capture_target(&target) {
            Err(AudioError::DeviceNotFound { device_id }) => {
                assert_eq!(device_id, "rsac-no-such-device");
            }
            Err(AudioError::BackendError { message, .. }) => {
                assert!(
                    message.contains("pw-dump"),
                    "expected pw-dump-related backend error: {}",
                    message
                );
            }
            other => panic!("Expected DeviceNotFound or BackendError, got {:?}", other),
        }
    }

    #[test]
    fn test_resolve_capture_target_process_tree_walks_proc() {
        // ProcessTree should walk /proc (always available on Linux CI) and then
        // attempt pw-dump resolution. Result is ApplicationNotFound (no node) or
        // BackendError (no pw-dump); never a panic and never a verbatim PID.
        let target = CaptureTarget::ProcessTree(ProcessId(std::process::id()));
        match resolve_capture_target(&target) {
            Err(AudioError::ApplicationNotFound { .. })
            | Err(AudioError::BackendError { .. })
            | Ok(ResolvedTarget::Serial(_)) => {}
            other => panic!("Unexpected resolve result: {:?}", other),
        }
    }

    #[test]
    fn test_find_node_device_missing_returns_device_not_found() {
        // Direct lookup-level check that the Device variant maps a no-match to
        // DeviceNotFound (and not ApplicationNotFound).
        match find_pipewire_node_serial(&PwNodeLookup::Device("definitely-not-here")) {
            Err(AudioError::DeviceNotFound { device_id }) => {
                assert_eq!(device_id, "definitely-not-here");
            }
            Err(AudioError::BackendError { message, .. }) => {
                assert!(message.contains("pw-dump"), "got: {}", message);
            }
            other => panic!("Expected DeviceNotFound or BackendError, got {:?}", other),
        }
    }

    // ── Native registry resolution (H4 part 3 / rsac-nat1) ───────────
    //
    // These exercise the PURE matching helpers against synthetic snapshots, so
    // they run without a live PipeWire daemon and without `pw-dump`/`PATH` —
    // proving the capture path can resolve every non-default target natively.

    fn dev(id: &str, node_name: &str, name: &str, class: &str) -> PwDeviceSnapshot {
        PwDeviceSnapshot {
            id: id.to_string(),
            name: name.to_string(),
            node_name: node_name.to_string(),
            media_class: class.to_string(),
        }
    }

    fn app_snap(pid: u32, app_name: &str, node_serial: &str) -> PwAppSnapshot {
        PwAppSnapshot {
            pid,
            app_name: app_name.to_string(),
            node_serial: node_serial.to_string(),
        }
    }

    #[test]
    fn test_app_name_matches_exact_case_insensitive() {
        assert!(app_name_matches("Firefox", "firefox"));
        assert!(app_name_matches("firefox", "FIREFOX"));
        assert!(app_name_matches("VLC media player", "vlc media player"));
    }

    #[test]
    fn test_app_name_matches_basename_and_exe_stripped() {
        // A full binary path resolves against a bare program name.
        assert!(app_name_matches("/usr/lib/firefox/firefox", "firefox"));
        assert!(app_name_matches("C:\\Program Files\\VLC\\vlc.exe", "vlc"));
        assert!(app_name_matches("spotify.exe", "spotify"));
    }

    #[test]
    fn test_app_name_matches_rejects_substring() {
        // The tightened contract: NO arbitrary substring containment. "Fire"
        // must not bind to "Firefox" (that could attach to the wrong app).
        assert!(!app_name_matches("Firefox", "Fire"));
        assert!(!app_name_matches("Firefox", "fox"));
        assert!(!app_name_matches("Spotify", "Spot"));
    }

    #[test]
    fn test_app_name_matches_empty_query_is_no_match() {
        assert!(!app_name_matches("Firefox", ""));
        assert!(!app_name_matches("Firefox", "   "));
    }

    #[test]
    fn test_resolve_target_from_snapshot_device_by_serial_and_name() {
        let devices = vec![
            dev(
                "55",
                "alsa_output.pci-0000_00_1f.3",
                "Built-in Audio",
                "Audio/Sink",
            ),
            dev(
                "56",
                "alsa_input.pci-0000_00_1f.3",
                "Built-in Mic",
                "Audio/Source",
            ),
        ];
        // Match by object.serial (id).
        assert_eq!(
            resolve_target_from_snapshot(&TargetQuery::Device("55".to_string()), &devices, &[]),
            Some("55".to_string())
        );
        // Match by node.name → returns the node's serial (round-trippable).
        assert_eq!(
            resolve_target_from_snapshot(
                &TargetQuery::Device("alsa_input.pci-0000_00_1f.3".to_string()),
                &devices,
                &[]
            ),
            Some("56".to_string())
        );
        // Match by display name.
        assert_eq!(
            resolve_target_from_snapshot(
                &TargetQuery::Device("Built-in Audio".to_string()),
                &devices,
                &[]
            ),
            Some("55".to_string())
        );
        // No match.
        assert_eq!(
            resolve_target_from_snapshot(
                &TargetQuery::Device("no-such".to_string()),
                &devices,
                &[]
            ),
            None
        );
    }

    #[test]
    fn test_resolve_target_from_snapshot_by_pid() {
        let apps = vec![
            app_snap(1234, "Firefox", "9001"),
            app_snap(5678, "Spotify", "9002"),
        ];
        assert_eq!(
            resolve_target_from_snapshot(&TargetQuery::ByPid(5678), &[], &apps),
            Some("9002".to_string())
        );
        // A PID with no stream node does not match.
        assert_eq!(
            resolve_target_from_snapshot(&TargetQuery::ByPid(4242), &[], &apps),
            None
        );
    }

    #[test]
    fn test_resolve_target_from_snapshot_by_app_name_uses_tight_contract() {
        let apps = vec![
            app_snap(1234, "Firefox", "9001"),
            app_snap(5678, "/usr/bin/spotify", "9002"),
        ];
        assert_eq!(
            resolve_target_from_snapshot(
                &TargetQuery::ByAppName("firefox".to_string()),
                &[],
                &apps
            ),
            Some("9001".to_string())
        );
        // Basename match against a full path.
        assert_eq!(
            resolve_target_from_snapshot(
                &TargetQuery::ByAppName("spotify".to_string()),
                &[],
                &apps
            ),
            Some("9002".to_string())
        );
        // Substring must NOT match (tightened contract).
        assert_eq!(
            resolve_target_from_snapshot(&TargetQuery::ByAppName("Fire".to_string()), &[], &apps),
            None
        );
    }

    #[test]
    fn test_resolve_target_from_snapshot_by_pid_set() {
        let apps = vec![
            app_snap(1000, "parent", "9001"),
            app_snap(1002, "child", "9002"),
        ];
        // Any tree member's node matches; the first found in the app list wins.
        let matched =
            resolve_target_from_snapshot(&TargetQuery::ByPidSet(vec![999, 1002, 1003]), &[], &apps);
        assert_eq!(matched, Some("9002".to_string()));
        // A tree with no audio-producing member does not match.
        assert_eq!(
            resolve_target_from_snapshot(&TargetQuery::ByPidSet(vec![7, 8, 9]), &[], &apps),
            None
        );
    }

    // ── parse_default_metadata_name (H4 / rsac-bfd8) ─────────────────

    #[test]
    fn test_parse_default_metadata_name_json_object() {
        // PipeWire stores default sink/source as a JSON object with "name".
        let v = r#"{"name":"alsa_output.pci-0000_00_1f.3.analog-stereo"}"#;
        assert_eq!(
            parse_default_metadata_name(Some(v)),
            Some("alsa_output.pci-0000_00_1f.3.analog-stereo".to_string())
        );
    }

    #[test]
    fn test_parse_default_metadata_name_json_object_with_extra_fields() {
        let v = r#"{"name":"my_sink","other":42}"#;
        assert_eq!(
            parse_default_metadata_name(Some(v)),
            Some("my_sink".to_string())
        );
    }

    #[test]
    fn test_parse_default_metadata_name_bare_quoted_fallback() {
        // Older daemons may store a bare quoted string rather than an object.
        // We fall back to the de-quoted raw value.
        assert_eq!(
            parse_default_metadata_name(Some("\"bare_name\"")),
            Some("bare_name".to_string())
        );
    }

    #[test]
    fn test_parse_default_metadata_name_none_is_removal() {
        // `None` value (property removed) → `None`.
        assert_eq!(parse_default_metadata_name(None), None);
    }

    #[test]
    fn test_parse_default_metadata_name_non_json_raw() {
        // A non-JSON, unquoted value falls back to itself.
        assert_eq!(
            parse_default_metadata_name(Some("plain_node_name")),
            Some("plain_node_name".to_string())
        );
    }

    // ── Snapshot type shape ──────────────────────────────────────────

    #[test]
    fn test_pw_device_snapshot_clone_and_fields() {
        let snap = PwDeviceSnapshot {
            id: "42".to_string(),
            name: "Built-in Audio".to_string(),
            node_name: "alsa_output.pci-0000_00_1f.3.analog-stereo".to_string(),
            media_class: "Audio/Sink".to_string(),
        };
        let cloned = snap.clone();
        assert_eq!(cloned.id, "42");
        assert_eq!(cloned.name, "Built-in Audio");
        assert_eq!(
            cloned.node_name,
            "alsa_output.pci-0000_00_1f.3.analog-stereo"
        );
        assert_eq!(cloned.media_class, "Audio/Sink");
    }

    #[test]
    fn test_pw_default_snapshot_default_is_empty() {
        let d = PwDefaultSnapshot::default();
        assert!(d.sink_name.is_none());
        assert!(d.source_name.is_none());
    }

    #[test]
    fn test_registry_snapshot_dedups_by_global_id() {
        // The registry keys devices by global id, so a re-announced node is
        // recorded once. Exercise the BTreeMap-backed dedup directly.
        let mut snap = RegistrySnapshot::default();
        let dev = |id: &str| PwDeviceSnapshot {
            id: id.to_string(),
            name: "n".to_string(),
            node_name: "n".to_string(),
            media_class: "Audio/Sink".to_string(),
        };
        snap.devices.insert(7, dev("100"));
        snap.devices.insert(7, dev("100")); // same global id → overwrite, not dup
        snap.devices.insert(8, dev("200"));
        assert_eq!(snap.devices.len(), 2);
        snap.devices.remove(&7);
        assert_eq!(snap.devices.len(), 1);
        assert_eq!(snap.devices.values().next().unwrap().id, "200");
    }

    // ── Application snapshot (H4 part 2 / rsac-8ebb) ─────────────────

    #[test]
    fn test_pw_app_snapshot_clone_and_fields() {
        let app = PwAppSnapshot {
            pid: 4242,
            app_name: "Firefox".to_string(),
            node_serial: "1234".to_string(),
        };
        let cloned = app.clone();
        assert_eq!(cloned.pid, 4242);
        assert_eq!(cloned.app_name, "Firefox");
        assert_eq!(cloned.node_serial, "1234");
    }

    /// Build the owned, PID-deduplicated application Vec exactly as the
    /// `SnapshotApplications` handler does, so the dedup contract is testable
    /// without a live daemon.
    fn dedup_apps(snap: &RegistrySnapshot) -> Vec<PwAppSnapshot> {
        let mut seen_pids: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
        let mut apps: Vec<PwAppSnapshot> = Vec::new();
        for app in snap.applications.values() {
            if seen_pids.insert(app.pid) {
                apps.push(app.clone());
            }
        }
        apps
    }

    #[test]
    fn test_application_snapshot_dedups_by_pid() {
        // One application can own several stream nodes (distinct registry global
        // ids) but must collapse to a single PID-keyed source — the same dedup
        // the old `pw-dump` parser applied via the `app:<pid>` set.
        let mut snap = RegistrySnapshot::default();
        let app = |pid: u32, name: &str, serial: &str| PwAppSnapshot {
            pid,
            app_name: name.to_string(),
            node_serial: serial.to_string(),
        };
        // PID 100 appears on two different global ids; PID 200 on one.
        snap.applications.insert(10, app(100, "Firefox", "1000"));
        snap.applications.insert(11, app(100, "Firefox", "1001"));
        snap.applications.insert(12, app(200, "Spotify", "1002"));

        let deduped = dedup_apps(&snap);
        assert_eq!(deduped.len(), 2, "two distinct PIDs after dedup");
        let pids: Vec<u32> = deduped.iter().map(|a| a.pid).collect();
        assert!(pids.contains(&100));
        assert!(pids.contains(&200));
        // First node seen for a PID wins (BTreeMap iterates by ascending key,
        // so global id 10 — serial "1000" — represents PID 100).
        let fx = deduped.iter().find(|a| a.pid == 100).unwrap();
        assert_eq!(fx.node_serial, "1000");
    }

    #[test]
    fn test_application_snapshot_removal_clears_entry() {
        // A node going away (global_remove) must drop the entry from the
        // application map, mirroring the device map behaviour.
        let mut snap = RegistrySnapshot::default();
        snap.applications.insert(
            10,
            PwAppSnapshot {
                pid: 100,
                app_name: "App".to_string(),
                node_serial: "1000".to_string(),
            },
        );
        assert_eq!(dedup_apps(&snap).len(), 1);
        // global_remove on a node clears both maps; applications loses its entry.
        snap.devices.remove(&10);
        snap.applications.remove(&10);
        assert!(dedup_apps(&snap).is_empty());
    }

    #[test]
    fn test_snapshot_applications_when_spawn_fails_is_backend_error() {
        // Mirror of test_snapshot_devices_when_spawn_fails_is_backend_error for
        // the application path: in a sandbox without a daemon, spawn fails with
        // BackendInitializationFailed; when it succeeds, snapshot_applications
        // must return Ok (possibly empty) or a bounded Timeout/BackendError —
        // never a panic.
        match PipeWireThread::spawn() {
            Ok(thread) => match thread.snapshot_applications() {
                Ok(_apps) => {}
                Err(AudioError::Timeout { .. }) | Err(AudioError::BackendError { .. }) => {}
                Err(e) => panic!("Unexpected snapshot_applications error: {:?}", e),
            },
            Err(AudioError::BackendInitializationFailed { backend, .. }) => {
                assert_eq!(backend, "PipeWire");
            }
            Err(e) => panic!("Unexpected spawn error: {:?}", e),
        }
    }

    // ── Snapshot accessors honest-failure when daemon unavailable ────

    #[test]
    fn test_snapshot_devices_when_spawn_fails_is_backend_error() {
        // We can't spawn a thread without a daemon in many CI sandboxes; if
        // spawn fails it's a BackendInitializationFailed. If it succeeds,
        // snapshot_devices must return Ok (possibly empty) or a bounded
        // Timeout/BackendError — never a panic.
        match PipeWireThread::spawn() {
            Ok(thread) => match thread.snapshot_devices() {
                Ok(_devices) => {}
                Err(AudioError::Timeout { .. }) | Err(AudioError::BackendError { .. }) => {}
                Err(e) => panic!("Unexpected snapshot_devices error: {:?}", e),
            },
            Err(AudioError::BackendInitializationFailed { backend, .. }) => {
                assert_eq!(backend, "PipeWire");
            }
            Err(e) => panic!("Unexpected spawn error: {:?}", e),
        }
    }

    // ── SPA → rsac format mapping (PR-5 / rsac-7469) ─────────────────

    #[test]
    fn test_spa_audio_format_maps_integer_and_float_families() {
        use crate::core::config::SampleFormat;
        use libspa::param::audio::AudioFormat as Spa;

        // S16 family → I16.
        assert_eq!(
            spa_audio_format_to_sample_format(Spa::S16),
            Some(SampleFormat::I16)
        );
        assert_eq!(
            spa_audio_format_to_sample_format(Spa::S16LE),
            Some(SampleFormat::I16)
        );
        // S24 family (incl. 24-in-32 container) → I24.
        assert_eq!(
            spa_audio_format_to_sample_format(Spa::S24),
            Some(SampleFormat::I24)
        );
        assert_eq!(
            spa_audio_format_to_sample_format(Spa::S24LE),
            Some(SampleFormat::I24)
        );
        assert_eq!(
            spa_audio_format_to_sample_format(Spa::S24_32LE),
            Some(SampleFormat::I24)
        );
        // S32 family → I32.
        assert_eq!(
            spa_audio_format_to_sample_format(Spa::S32),
            Some(SampleFormat::I32)
        );
        assert_eq!(
            spa_audio_format_to_sample_format(Spa::S32LE),
            Some(SampleFormat::I32)
        );
        // F32 (little-endian) → F32.
        assert_eq!(
            spa_audio_format_to_sample_format(Spa::F32LE),
            Some(SampleFormat::F32)
        );
    }

    #[test]
    fn test_spa_audio_format_unmapped_families_are_none() {
        use libspa::param::audio::AudioFormat as Spa;

        // Formats rsac does not model must map to None (so the caller omits
        // them from the advisory list rather than guessing): unknown, unsigned,
        // 8-bit, 64-bit float, big-endian, and planar layouts.
        assert!(spa_audio_format_to_sample_format(Spa::Unknown).is_none());
        assert!(spa_audio_format_to_sample_format(Spa::U8).is_none());
        assert!(spa_audio_format_to_sample_format(Spa::U16LE).is_none());
        assert!(spa_audio_format_to_sample_format(Spa::F64LE).is_none());
        assert!(spa_audio_format_to_sample_format(Spa::F32BE).is_none());
        assert!(spa_audio_format_to_sample_format(Spa::F32P).is_none());
    }

    // ── Misaligned-edge f32 realignment (#30) ────────────────────────

    /// Build a byte vector that, when sliced at `start_offset`, presents the
    /// `f32` samples in LE form but with a non-word-aligned start so the
    /// `.process` fast path's `align_to` leaves a non-empty head.
    fn le_bytes(samples: &[f32]) -> Vec<u8> {
        let mut v = Vec::with_capacity(samples.len() * 4);
        for s in samples {
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    #[test]
    fn test_decode_unaligned_f32_le_recovers_every_whole_sample() {
        // Whole, complete f32 stream — no truncation regardless of how the
        // backing slice happens to be aligned. Force a misaligned VIEW by
        // prepending one filler byte and decoding the bytes AFTER it.
        let samples = [0.0f32, 1.0, -2.5, 1234.5, f32::MIN, f32::MAX];
        let mut backing = vec![0xAAu8]; // 1-byte filler → guarantees misalignment
        backing.extend_from_slice(&le_bytes(&samples));
        let valid = &backing[1..]; // starts at byte offset 1 (not 4-aligned)

        let mut scratch = Vec::new();
        let truncated = decode_unaligned_f32_le(valid, &mut scratch);

        assert_eq!(truncated, 0, "a multiple-of-4 byte run has no partial tail");
        assert_eq!(
            scratch.as_slice(),
            samples.as_slice(),
            "every whole sample must be recovered bit-for-bit, none dropped"
        );
    }

    #[test]
    fn test_decode_unaligned_f32_le_reports_truncated_tail() {
        // A byte run whose length is not a multiple of 4: the final partial
        // sample is genuinely truncated and unrecoverable, but ALL preceding
        // whole samples must still be decoded (no whole sample silently lost).
        let samples = [3.0f32, 4.0, 5.0];
        let mut bytes = le_bytes(&samples);
        bytes.push(0x01); // one extra byte → a 1-byte truncated trailing sample
        bytes.push(0x02); // two extra bytes total

        let mut scratch = Vec::new();
        let truncated = decode_unaligned_f32_le(&bytes, &mut scratch);

        assert_eq!(truncated, 2, "two trailing bytes do not form a whole f32");
        assert_eq!(
            scratch.as_slice(),
            samples.as_slice(),
            "the three whole samples must survive even with a truncated tail"
        );
    }

    #[test]
    fn test_decode_unaligned_f32_le_reuses_scratch_capacity() {
        // The realign scratch is reused across callbacks: a second decode must
        // clear the previous contents and must NOT grow capacity when the new
        // chunk is no larger than the first (RT-safety: alloc-free in steady
        // state, ADR-0001).
        let mut scratch = Vec::new();

        let first = [1.0f32, 2.0, 3.0, 4.0];
        decode_unaligned_f32_le(&le_bytes(&first), &mut scratch);
        assert_eq!(scratch.as_slice(), first.as_slice());
        let cap_after_first = scratch.capacity();
        assert!(cap_after_first >= first.len());

        let second = [9.0f32, 8.0];
        decode_unaligned_f32_le(&le_bytes(&second), &mut scratch);
        assert_eq!(
            scratch.as_slice(),
            second.as_slice(),
            "scratch must be cleared and refilled, not appended"
        );
        assert_eq!(
            scratch.capacity(),
            cap_after_first,
            "a smaller follow-up chunk must reuse capacity, not reallocate"
        );
    }

    #[test]
    fn test_decode_unaligned_f32_le_empty_and_sub_sample_inputs() {
        let mut scratch = Vec::new();

        // Empty input → no samples, no truncation.
        assert_eq!(decode_unaligned_f32_le(&[], &mut scratch), 0);
        assert!(scratch.is_empty());

        // Fewer than 4 bytes → the whole thing is a truncated partial sample.
        let truncated = decode_unaligned_f32_le(&[0x01, 0x02, 0x03], &mut scratch);
        assert_eq!(truncated, 3);
        assert!(
            scratch.is_empty(),
            "no whole sample can be formed from 3 bytes"
        );
    }

    #[test]
    fn test_enum_node_formats_unknown_serial_or_unavailable_is_empty_or_honest() {
        // EnumNodeFormats is advisory discovery: an unknown serial yields an
        // empty Vec, never a fabricated format. In a sandbox without a daemon,
        // spawn fails with BackendInitializationFailed; when it succeeds, asking
        // for a serial that is not in the registry must return Ok(vec![]) (or a
        // bounded Timeout/BackendError) — never a panic, never a guess.
        match PipeWireThread::spawn() {
            Ok(thread) => match thread.enum_node_formats("rsac-no-such-serial") {
                Ok(formats) => assert!(
                    formats.is_empty(),
                    "unknown serial must enumerate to an empty advisory list, got {:?}",
                    formats
                ),
                Err(AudioError::Timeout { .. }) | Err(AudioError::BackendError { .. }) => {}
                Err(e) => panic!("Unexpected enum_node_formats error: {:?}", e),
            },
            Err(AudioError::BackendInitializationFailed { backend, .. }) => {
                assert_eq!(backend, "PipeWire");
            }
            Err(e) => panic!("Unexpected spawn error: {:?}", e),
        }
    }

    // ── force_set-vs-graceful ordering (rsac-78b2) ───────────────────
    //
    // The StopCapture / Shutdown / channel-disconnect teardown arms now drop the
    // stream listener+stream BEFORE calling `signal_session_graceful_end`. The
    // reason: the `.state_changed` arm invokes the UNGUARDED `signal_error()`
    // (`force_set(StreamState::Error)`, last-writer-wins), so destroying the
    // stream can synchronously deliver a `StreamState::Unconnected` whose callback
    // writes terminal `Error`. If the graceful `Running → Stopping` CAS ran first,
    // that synchronous `Unconnected` during the drop would DOWNGRADE the just-set
    // graceful `Stopping` to `Error`, mis-reporting a clean stop as fatal.
    //
    // The synchronous disconnect delivery needs a live daemon + real stream and is
    // not deterministically reproducible in a unit test. These tests instead pin
    // the two invariants the ordering relies on, exercised directly against the
    // bridge via the real `signal_session_graceful_end` helper:
    //   1. From `Running`, the graceful end lands `Stopping` (clean-stop path).
    //   2. `Error` is STICKY: a graceful end issued AFTER `Error` was force-set
    //      (the simulated synchronous `signal_error()` during the drop) does NOT
    //      downgrade it — proving WHY the drop must precede the graceful CAS, and
    //      that a genuine error correctly wins over a concurrent stop.

    use crate::bridge::ring_buffer::create_bridge;

    fn ordering_test_format() -> crate::core::config::AudioFormat {
        crate::core::config::AudioFormat {
            sample_rate: 48000,
            channels: 2,
            sample_format: crate::core::config::SampleFormat::F32,
        }
    }

    #[test]
    fn test_graceful_end_from_running_lands_stopping() {
        let (producer, consumer) = create_bridge(8, ordering_test_format());
        // A live session is Running while capturing.
        producer.shared().state.force_set(StreamState::Running);

        // Clean stop with no racing `.state_changed` Error: the helper used by the
        // StopCapture/Shutdown/disconnect arms drives Running → Stopping.
        signal_session_graceful_end(Some(Arc::clone(producer.shared())));

        assert_eq!(
            consumer.shared().state.get(),
            StreamState::Stopping,
            "a clean stop must land the bridge in graceful Stopping"
        );
    }

    #[test]
    fn test_graceful_end_does_not_downgrade_terminal_error() {
        let (producer, consumer) = create_bridge(8, ordering_test_format());
        producer.shared().state.force_set(StreamState::Running);

        // Simulate the synchronous `.state_changed(Unconnected)` → `signal_error()`
        // that can fire WHILE the listener/stream is being dropped: terminal Error
        // is force-set first. With the corrected ordering (drop BEFORE graceful
        // signal), this is exactly the state the subsequent graceful CAS sees.
        producer.signal_error();
        assert_eq!(consumer.shared().state.get(), StreamState::Error);

        // The graceful end must NOT downgrade a genuine terminal Error: its CAS is
        // `Running → Stopping`, which no-ops because the state is Error, not
        // Running. A real device-loss error correctly wins over the stop.
        signal_session_graceful_end(Some(Arc::clone(producer.shared())));

        assert_eq!(
            consumer.shared().state.get(),
            StreamState::Error,
            "a graceful end must never downgrade terminal Error to Stopping"
        );
        assert!(
            consumer.shared().state.is_terminal(),
            "Error remains terminal so a blocking reader returns Fatal StreamEnded"
        );
    }
}
