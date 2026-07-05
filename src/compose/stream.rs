//! The public composition handle: [`Composition`], its stats snapshots, and
//! the `PlatformStream` shim that plugs the compositor into the standard
//! bridge ring.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::api::AudioCaptureBuilder;
use crate::bridge::ring_buffer::create_bridge;
use crate::bridge::state::StreamState;
use crate::bridge::stream::{BridgeStream, PlatformStream};
use crate::core::buffer::AudioBuffer;
use crate::core::config::{AudioFormat, SampleFormat};
use crate::core::error::{AudioError, AudioResult};
use crate::core::interface::CapturingStream;

use super::builder::{
    ChannelMap, ChannelOrigin, CompositionPlan, GroupLayout, MAX_COMPOSED_CHANNELS,
};
use super::engine::{
    Engine, EngineConfig, EngineStatsShared, GroupSpec, SourceReader, SourceSpec, SourceStatsShared,
};

/// How long `start()` polls a keep-channels source for its negotiated format
/// before falling back to the requested channel count.
const FORMAT_POLL_TIMEOUT: Duration = Duration::from_secs(2);
const FORMAT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Ring capacity (in composed buffers) between the compositor thread and the
/// consumer. At the default 10 ms quantum this buffers ~1.3 s of audio.
const COMPOSED_RING_CAPACITY: usize = 128;

// ── PlatformStream shim ─────────────────────────────────────────────────

/// The compositor's `PlatformStream`: "stopping the OS capture" means telling
/// the engine thread to stop (which stops every inner capture and signals the
/// composed ring done).
pub(crate) struct ComposePlatformStream {
    stop_flag: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
}

impl ComposePlatformStream {
    pub(crate) fn new(stop_flag: Arc<AtomicBool>, active: Arc<AtomicBool>) -> Self {
        Self { stop_flag, active }
    }
}

impl PlatformStream for ComposePlatformStream {
    fn stop_capture(&self) -> AudioResult<()> {
        self.stop_flag.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}

// ── Real capture-backed SourceReader ────────────────────────────────────

/// Wraps a started `AudioCapture` as an engine source. Uses the
/// terminal-observable read path so the engine sees `StreamEnded` (fatal)
/// instead of a masked recoverable error.
struct CaptureSource {
    capture: crate::api::AudioCapture,
}

impl SourceReader for CaptureSource {
    fn try_read(&mut self) -> AudioResult<Option<AudioBuffer>> {
        self.capture.read_chunk_nonblocking()
    }

    fn stop(&mut self) {
        let _ = self.capture.stop();
    }
}

// ── Composed stream view (drain-complete promotion) ─────────────────────

/// The composed stream all consumer paths read through.
///
/// Wraps the bridge stream with one extra rule the engine makes possible: the
/// compositor knows when **no more data can ever arrive** (every source ended
/// and the tail was flushed before the engine exited). The platform backends
/// cannot know this — their graceful end (`signal_done`) parks the stream in
/// the drainable `Stopping` state forever. Here, once the engine has exited
/// (`engine_active == false`) *and* the ring is observed empty, the view
/// promotes `Stopping → Stopped` and surfaces the fatal
/// [`AudioError::StreamEnded`] — so read loops, `drain_to`, and async
/// consumers all end on their own after a composition finishes (ADR-0003
/// drain-before-terminal is preserved: the promotion only fires on an empty
/// ring).
pub(crate) struct ComposedStreamView {
    stream: BridgeStream<ComposePlatformStream>,
    engine_active: Arc<AtomicBool>,
}

impl ComposedStreamView {
    pub(crate) fn new(
        stream: BridgeStream<ComposePlatformStream>,
        engine_active: Arc<AtomicBool>,
    ) -> Self {
        Self {
            stream,
            engine_active,
        }
    }
}

impl CapturingStream for ComposedStreamView {
    fn read_chunk(&self) -> AudioResult<AudioBuffer> {
        loop {
            // Fast path + drain-complete promotion.
            match self.try_read_chunk() {
                Ok(Some(buffer)) => return Ok(buffer),
                Ok(None) => {}
                Err(e) => return Err(e),
            }
            // Park on the inner blocking read; a Timeout re-runs the promotion
            // check (an engine exit mid-park surfaces within one timeout slice).
            match self.stream.read_chunk() {
                Ok(buffer) => return Ok(buffer),
                Err(AudioError::Timeout { .. }) => continue,
                Err(e) => return Err(e),
            }
        }
    }

    fn try_read_chunk(&self) -> AudioResult<Option<AudioBuffer>> {
        // Sample the engine-liveness flag BEFORE reading the ring. The engine
        // flips it to inactive only AFTER its final pushes + signal_done(), so
        // "inactive observed first, then the ring read back empty" proves the
        // ring is truly drained. Sampling in the other order is a TOCTOU: the
        // engine's final flush could land between an empty pop and the flag
        // load, and the promotion below would strand that tail unreadable.
        let inactive_before_read = !self.engine_active.load(Ordering::SeqCst);
        match self.stream.try_read_chunk() {
            // Empty ring + engine already exited before the read → nothing can
            // ever arrive: promote the graceful Stopping to terminal Stopped
            // (idempotent CAS; a failed CAS means another reader already
            // promoted) and report the clean end-of-stream.
            Ok(None) if inactive_before_read => {
                let _ = self
                    .stream
                    .shared()
                    .state
                    .transition(StreamState::Stopping, StreamState::Stopped);
                Err(AudioError::StreamEnded {
                    reason: "Composition ended (all sources terminal, ring drained)".to_string(),
                })
            }
            other => other,
        }
    }

    fn stop(&self) -> AudioResult<()> {
        self.stream.stop()
    }

    fn format(&self) -> AudioFormat {
        self.stream.format()
    }

    fn is_running(&self) -> bool {
        self.stream.is_running()
    }

    fn overrun_count(&self) -> u64 {
        self.stream.overrun_count()
    }

    fn buffers_captured(&self) -> u64 {
        self.stream.buffers_captured()
    }

    fn buffers_pushed(&self) -> u64 {
        self.stream.buffers_pushed()
    }

    fn buffers_dropped(&self) -> u64 {
        CapturingStream::buffers_dropped(&self.stream)
    }

    fn is_under_backpressure(&self) -> bool {
        self.stream.is_under_backpressure()
    }

    fn drop_window_snapshot(&self) -> (u64, u64) {
        self.stream.drop_window_snapshot()
    }

    #[cfg(feature = "async-stream")]
    fn register_waker(&self, waker: &std::task::Waker) -> bool {
        self.stream.register_waker(waker)
    }

    #[cfg(feature = "async-stream")]
    fn is_stream_producing(&self) -> bool {
        self.stream.is_stream_producing()
    }
}

// ── Shared pipeline assembly ────────────────────────────────────────────

/// The composed data plane: producer half for the engine, consumer view for
/// the public handle, and the engine lifecycle flags.
pub(crate) struct ComposedPipeline {
    pub producer: crate::bridge::ring_buffer::BridgeProducer,
    pub view: Arc<ComposedStreamView>,
    pub stop_flag: Arc<AtomicBool>,
    pub active: Arc<AtomicBool>,
}

/// Assembles the composed bridge ring + stream view exactly once, shared by
/// `Composition::start()` and the engine test harness so the tests exercise
/// the production wiring (negotiated-format recording, state transition,
/// platform-stream flags) rather than a hand-rolled copy that can drift.
pub(crate) fn assemble_pipeline(
    composed_format: AudioFormat,
    ring_capacity: usize,
    read_timeout: Duration,
) -> AudioResult<ComposedPipeline> {
    let (producer, consumer) = create_bridge(ring_capacity, composed_format.clone());
    consumer.shared().set_negotiated_format(&composed_format);
    consumer
        .shared()
        .state
        .transition(StreamState::Created, StreamState::Running)
        .map_err(|e| AudioError::InternalError {
            message: format!("compose bridge state transition failed: {e:?}"),
            source: None,
        })?;

    let stop_flag = Arc::new(AtomicBool::new(false));
    let active = Arc::new(AtomicBool::new(true));
    let platform = ComposePlatformStream::new(Arc::clone(&stop_flag), Arc::clone(&active));
    let view = Arc::new(ComposedStreamView::new(
        BridgeStream::new(consumer, platform, composed_format, read_timeout),
        Arc::clone(&active),
    ));
    Ok(ComposedPipeline {
        producer,
        view,
        stop_flag,
        active,
    })
}

// ── Stats snapshots ─────────────────────────────────────────────────────

/// Point-in-time counters for one composed source.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct SourceStats {
    /// Name of the group the source belongs to.
    pub group: String,
    /// The source's capture target, rendered via `CaptureTarget`'s `Display`.
    pub target: String,
    /// Buffers received from the inner capture so far.
    pub buffers_received: u64,
    /// Frames of silence inserted because the source was behind at tick time.
    pub padded_frames: u64,
    /// Frames trimmed because the source drifted past the buffering bound.
    pub trimmed_frames: u64,
    /// Whether this source is being resampled to the session rate.
    pub resampling: bool,
    /// Whether the source's stream has ended.
    pub ended: bool,
}

/// Point-in-time snapshot of a running composition.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct CompositionStats {
    /// Composed buffers (ticks) emitted so far.
    pub ticks: u64,
    /// Ticks emitted by the wall-clock stall fallback (master had no data).
    pub fallback_ticks: u64,
    /// Per-source counters, in flat declaration order.
    pub sources: Vec<SourceStats>,
}

// ── Composition ─────────────────────────────────────────────────────────

/// Handle to the engine thread, kept so `stop()`/`Drop` can join it.
struct EngineHandle {
    stop_flag: Arc<AtomicBool>,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// A multi-source composed capture session (ADR-0011).
///
/// Created by [`CompositionBuilder::build`](super::CompositionBuilder::build);
/// inert until [`start`](Self::start). See the [module docs](crate::compose)
/// for the composition model.
///
/// # Reading
///
/// `Composition` implements [`CapturingStream`], and the inherent
/// [`read_buffer`](Self::read_buffer) mirrors
/// [`AudioCapture::read_buffer`](crate::api::AudioCapture::read_buffer).
/// Composed buffers are interleaved f32 at the session rate with
/// [`channel_map().channels()`](ChannelMap::channels) channels.
///
/// # Ownership of inner captures
///
/// The composition owns and consumes its inner captures; do not read the same
/// sources through other handles while it runs.
pub struct Composition {
    plan: CompositionPlan,
    stream: Option<Arc<ComposedStreamView>>,
    engine: Option<EngineHandle>,
    channel_map: Option<ChannelMap>,
    stats: Option<Arc<EngineStatsShared>>,
    /// Flat `(group_name, target_display)` per source, for stats snapshots.
    source_labels: Vec<(String, String)>,
}

impl std::fmt::Debug for Composition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Composition")
            .field("started", &self.stream.is_some())
            .field("groups", &self.plan.groups.len())
            .field("session_rate", &self.plan.session_rate)
            .field("channel_map", &self.channel_map)
            .finish()
    }
}

impl Composition {
    pub(crate) fn from_plan(plan: CompositionPlan) -> Self {
        let source_labels = plan
            .groups
            .iter()
            .flat_map(|g| {
                g.sources()
                    .iter()
                    .map(|(t, _)| (g.name().to_string(), t.to_string()))
            })
            .collect();
        Self {
            plan,
            stream: None,
            engine: None,
            channel_map: None,
            stats: None,
            source_labels,
        }
    }

    /// Builds and starts one capture per source, resolves the composed channel
    /// layout, and spawns the compositor thread.
    ///
    /// On any failure every already-started inner capture is stopped before
    /// the error is returned — a failed `start()` leaks nothing.
    ///
    /// # Errors
    ///
    /// - Any error from building/starting an inner capture (device resolution,
    ///   format negotiation, stream creation).
    /// - [`AudioError::ConfigurationError`] if the resolved layout exceeds 32
    ///   output channels, or if the composition was already started.
    /// - [`AudioError::InternalError`] if the compositor thread cannot spawn.
    pub fn start(&mut self) -> AudioResult<()> {
        if self.stream.is_some() {
            return Err(AudioError::ConfigurationError {
                message: "Composition is already started".to_string(),
            });
        }

        // ── 1. Build + start one capture per source ──────────────────
        let mut captures: Vec<crate::api::AudioCapture> = Vec::new();
        let result = (|| -> AudioResult<()> {
            for group in &self.plan.groups {
                for (target, _gain) in group.sources() {
                    let mut capture = AudioCaptureBuilder::new()
                        .with_target(target.clone())
                        .sample_rate(self.plan.session_rate)
                        .build()?;
                    capture.start()?;
                    captures.push(capture);
                }
            }
            Ok(())
        })();
        if let Err(e) = result {
            for c in &mut captures {
                let _ = c.stop();
            }
            return Err(e);
        }

        // ── 2. Resolve group widths / channel map ────────────────────
        // Keep-channels groups need the source's negotiated width; poll its
        // format briefly (Linux only learns it at stream-open).
        let mut entries: Vec<ChannelOrigin> = Vec::new();
        let mut group_specs: Vec<GroupSpec> = Vec::new();
        let mut source_specs: Vec<SourceSpec> = Vec::new();
        let mut offset = 0usize;
        let mut flat_idx = 0usize;
        for (gi, group) in self.plan.groups.iter().enumerate() {
            let width = match group.layout().fixed_width() {
                Some(w) => usize::from(w),
                None => {
                    let w = poll_channels(&captures[flat_idx]);
                    usize::from(w)
                }
            };
            for c in 0..width {
                entries.push(ChannelOrigin {
                    group: group.name().to_string(),
                    group_index: gi,
                    channel_in_group: c as u16,
                });
            }
            group_specs.push(GroupSpec {
                layout: group.layout(),
                offset,
                width,
            });
            for (target, gain) in group.sources() {
                source_specs.push(SourceSpec {
                    gain: *gain,
                    group: gi,
                    // Keep sources lock their width now; mixdown sources lock
                    // on first delivered buffer.
                    channels: if group.layout() == GroupLayout::KeepChannels {
                        width as u16
                    } else {
                        0
                    },
                    // System/device endpoints tick through silence, so they are
                    // the preferred clock if the master has to be re-elected.
                    clock_candidate: matches!(
                        target,
                        crate::core::config::CaptureTarget::SystemDefault
                            | crate::core::config::CaptureTarget::Device(_)
                    ),
                });
                flat_idx += 1;
            }
            offset += width;
        }

        let total_channels = offset;
        if total_channels == 0 || total_channels > usize::from(MAX_COMPOSED_CHANNELS) {
            for c in &mut captures {
                let _ = c.stop();
            }
            return Err(AudioError::ConfigurationError {
                message: format!(
                    "Composed layout resolves to {total_channels} output channels \
                     (must be 1..={MAX_COMPOSED_CHANNELS})"
                ),
            });
        }

        let composed_format = AudioFormat {
            sample_rate: self.plan.session_rate,
            channels: total_channels as u16,
            sample_format: SampleFormat::F32,
        };

        // ── 3. Bridge ring + stream (shared assembly) ────────────────
        let pipeline = match assemble_pipeline(
            composed_format.clone(),
            COMPOSED_RING_CAPACITY,
            Duration::from_secs(1),
        ) {
            Ok(p) => p,
            Err(e) => {
                for c in &mut captures {
                    let _ = c.stop();
                }
                return Err(e);
            }
        };
        let ComposedPipeline {
            producer,
            view: stream,
            stop_flag,
            active,
        } = pipeline;

        // ── 4. Spawn the engine ──────────────────────────────────────
        let stats = Arc::new(EngineStatsShared {
            sources: (0..source_specs.len())
                .map(|_| Arc::new(SourceStatsShared::default()))
                .collect(),
            ..Default::default()
        });
        let readers: Vec<Box<dyn SourceReader>> = captures
            .into_iter()
            .map(|capture| Box::new(CaptureSource { capture }) as Box<dyn SourceReader>)
            .collect();
        let cfg = EngineConfig {
            composed_format,
            quantum_frames: self.plan.quantum_frames(),
            max_fifo_frames: self.plan.max_fifo_frames(),
            stall_timeout: self.plan.stall_timeout,
            clamp_output: self.plan.clamp_output,
            master_index: self.plan.master_index(),
            groups: group_specs,
            sources: source_specs,
        };
        let engine = Engine::new(
            cfg,
            readers,
            producer,
            Arc::clone(&stop_flag),
            Arc::clone(&active),
            Arc::clone(&stats),
        );
        let join = std::thread::Builder::new()
            .name("rsac-compose".into())
            .spawn(move || engine.run())
            .map_err(|e| AudioError::InternalError {
                message: format!("Failed to spawn compositor thread: {e}"),
                source: None,
            })?;

        self.stream = Some(stream);
        self.engine = Some(EngineHandle {
            stop_flag,
            join: Mutex::new(Some(join)),
        });
        self.channel_map = Some(ChannelMap::new(entries));
        self.stats = Some(stats);
        Ok(())
    }

    /// Stops the composition: signals the engine (which stops every inner
    /// capture and ends the composed ring) and joins the compositor thread.
    /// Idempotent; a not-yet-started composition returns `Ok(())`.
    ///
    /// # Buffered tail is discarded
    ///
    /// An **explicit** `stop()` ends readability immediately — the underlying
    /// stream lands in the terminal `Stopped` state and subsequent reads
    /// return the fatal [`AudioError::StreamEnded`] without draining any
    /// composed buffers still in the ring (the same contract as
    /// [`AudioCapture::stop`](crate::api::AudioCapture::stop)). ADR-0003's
    /// drain-before-terminal semantics apply to the composition's **natural
    /// end** (all sources terminal → engine flushes its tail → reads drain
    /// the ring → then `StreamEnded`), not to an explicit stop. To capture
    /// everything, read until the terminal error *before* calling `stop()`.
    pub fn stop(&mut self) -> AudioResult<()> {
        if let Some(stream) = &self.stream {
            // Transitions the ring to Stopping and fires ComposePlatformStream::
            // stop_capture (the engine stop flag). Tolerate "already stopped".
            let _ = stream.stop();
        }
        if let Some(engine) = &self.engine {
            engine.stop_flag.store(true, Ordering::SeqCst);
            if let Ok(mut guard) = engine.join.lock() {
                if let Some(handle) = guard.take() {
                    if handle.thread().id() != std::thread::current().id() {
                        let _ = handle.join();
                    } else {
                        *guard = Some(handle);
                    }
                }
            }
        }
        Ok(())
    }

    /// Non-blocking read of the next composed buffer (mirrors
    /// [`AudioCapture::read_buffer`](crate::api::AudioCapture::read_buffer)'s
    /// terminal-observable sibling): `Ok(None)` = nothing yet;
    /// fatal [`AudioError::StreamEnded`] = composition finished and drained.
    pub fn read_buffer(&self) -> AudioResult<Option<AudioBuffer>> {
        self.started_stream()?.try_read_chunk()
    }

    /// Blocking read of the next composed buffer; returns the terminal
    /// [`AudioError::StreamEnded`] once the composition ends and drains.
    pub fn read_buffer_blocking(&self) -> AudioResult<AudioBuffer> {
        self.started_stream()?.read_chunk()
    }

    /// `true` while the composed stream is running (started and not yet
    /// stopped/ended). Mirrors
    /// [`AudioCapture::is_running`](crate::api::AudioCapture::is_running).
    pub fn is_running(&self) -> bool {
        self.stream
            .as_ref()
            .map(|s| s.is_running())
            .unwrap_or(false)
    }

    /// The composed channel layout. `None` until [`start`](Self::start)
    /// succeeds (keep-channels widths are resolved at start).
    pub fn channel_map(&self) -> Option<&ChannelMap> {
        self.channel_map.as_ref()
    }

    /// Point-in-time composition counters. `None` until
    /// [`start`](Self::start) succeeds.
    pub fn stats(&self) -> Option<CompositionStats> {
        let shared = self.stats.as_ref()?;
        let sources = shared
            .sources
            .iter()
            .zip(self.source_labels.iter())
            .map(|(s, (group, target))| SourceStats {
                group: group.clone(),
                target: target.clone(),
                buffers_received: s.buffers_received.load(Ordering::Relaxed),
                padded_frames: s.padded_frames.load(Ordering::Relaxed),
                trimmed_frames: s.trimmed_frames.load(Ordering::Relaxed),
                resampling: s.resampling.load(Ordering::Relaxed),
                ended: s.ended.load(Ordering::Relaxed),
            })
            .collect();
        Some(CompositionStats {
            ticks: shared.ticks.load(Ordering::Relaxed),
            fallback_ticks: shared.fallback_ticks.load(Ordering::Relaxed),
            sources,
        })
    }

    /// Drains composed audio into an [`AudioSink`](crate::sink::AudioSink) on
    /// a dedicated background thread — the composition analogue of
    /// [`RunningCapture::drain_to`](crate::api::RunningCapture::drain_to)
    /// (same loop, same recoverable-vs-fatal policy, same flush/close
    /// finalization). Do not mix with manual reads on the same composition.
    pub fn drain_to<S>(&self, sink: S) -> AudioResult<crate::api::DrainHandle>
    where
        S: crate::sink::AudioSink + 'static,
    {
        let stream = self.started_stream()?.clone();
        crate::api::spawn_drain_thread(stream, sink)
    }

    /// Creates a push subscription delivering composed buffers over an
    /// [`mpsc`](std::sync::mpsc) channel — the composition analogue of
    /// [`AudioCapture::subscribe`](crate::api::AudioCapture::subscribe) (same
    /// background pump, same recoverable-vs-fatal policy, same ~1 ms idle-poll
    /// latency floor).
    ///
    /// The pump exits when the composition reaches its fatal terminal (all
    /// sources ended and the ring drained, or an explicit stop) — the channel
    /// then disconnects — or when the receiver is dropped. The background
    /// reader competes with [`read_buffer`](Self::read_buffer) and
    /// [`drain_to`](Self::drain_to) for buffers from the same ring; do not mix
    /// delivery modes on one composition.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::StreamReadError`] if the composition is not
    /// started or no longer running.
    pub fn subscribe(&self) -> AudioResult<std::sync::mpsc::Receiver<AudioBuffer>> {
        let stream = self.started_stream()?;
        if !stream.is_running() {
            return Err(AudioError::StreamReadError {
                reason: "Composition is not running".to_string(),
            });
        }
        crate::api::spawn_subscribe_thread(stream.clone())
    }

    /// Like [`subscribe`](Self::subscribe), but each item is an
    /// [`AudioResult<AudioBuffer>`] and the **fatal terminal** error is
    /// delivered as the final channel item before the disconnect — the
    /// composition analogue of
    /// [`AudioCapture::subscribe_with_errors`](crate::api::AudioCapture::subscribe_with_errors).
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::StreamReadError`] if the composition is not
    /// started or no longer running.
    pub fn subscribe_with_errors(
        &self,
    ) -> AudioResult<std::sync::mpsc::Receiver<AudioResult<AudioBuffer>>> {
        let stream = self.started_stream()?;
        if !stream.is_running() {
            return Err(AudioError::StreamReadError {
                reason: "Composition is not running".to_string(),
            });
        }
        crate::api::spawn_subscribe_with_errors_thread(stream.clone())
    }

    /// Returns an asynchronous stream of composed audio buffers — the
    /// composition analogue of
    /// [`AudioCapture::audio_data_stream`](crate::api::AudioCapture::audio_data_stream).
    ///
    /// The returned [`AsyncAudioStream`](crate::bridge::AsyncAudioStream)
    /// implements [`futures_core::Stream`], is waker-driven (the compositor
    /// wakes the task when it pushes a composed buffer), and yields a final
    /// `None` after the composition ends and the ring drains.
    ///
    /// # Errors
    ///
    /// Returns [`AudioError::StreamReadError`] if the composition has not been
    /// started.
    #[cfg(feature = "async-stream")]
    pub fn audio_data_stream(&self) -> AudioResult<crate::bridge::AsyncAudioStream<'_>> {
        let stream = self.started_stream()?;
        Ok(crate::bridge::AsyncAudioStream::new(&**stream))
    }

    fn started_stream(&self) -> AudioResult<&Arc<ComposedStreamView>> {
        self.stream
            .as_ref()
            .ok_or_else(|| AudioError::StreamReadError {
                reason: "Composition is not started. Call start() first.".to_string(),
            })
    }
}

impl Drop for Composition {
    fn drop(&mut self) {
        // Best-effort deterministic teardown; stop() is idempotent.
        let _ = self.stop();
    }
}

// ── CapturingStream delegation ──────────────────────────────────────────

impl CapturingStream for Composition {
    fn read_chunk(&self) -> AudioResult<AudioBuffer> {
        self.started_stream()?.read_chunk()
    }

    fn try_read_chunk(&self) -> AudioResult<Option<AudioBuffer>> {
        self.started_stream()?.try_read_chunk()
    }

    fn stop(&self) -> AudioResult<()> {
        // &self stop: signal the ring + engine; the join happens in the
        // inherent `stop(&mut self)` / `Drop`. The engine exits on its own
        // once signalled, so not joining here is safe (never leaks: Drop joins).
        // A not-started composition is a no-op `Ok(())`, matching the inherent
        // `stop(&mut self)` so the two same-named paths cannot disagree.
        // Like the inherent stop, this discards any buffered composed tail
        // (see [`Composition::stop`] — drain first if you need it).
        let Some(stream) = self.stream.as_ref() else {
            return Ok(());
        };
        stream.stop()?;
        if let Some(engine) = &self.engine {
            engine.stop_flag.store(true, Ordering::SeqCst);
        }
        Ok(())
    }

    /// Returns the composed delivery format.
    ///
    /// **Before [`start`](Composition::start)** this is a *provisional
    /// estimate*: keep-channels groups' widths are unknown until their
    /// source's format is negotiated at start, so they are estimated at 2
    /// channels. After a successful `start()` the returned format is
    /// authoritative and matches [`channel_map()`](Composition::channel_map)
    /// (which, unlike this method, honestly returns `None` until start —
    /// prefer it when you must distinguish an estimate from a resolved
    /// layout).
    fn format(&self) -> AudioFormat {
        if let Some(stream) = &self.stream {
            return stream.format();
        }
        // Not started: best-effort provisional (keep-channels widths unknown,
        // estimated at 2). Authoritative only after start().
        let mut channels = 0u16;
        for g in &self.plan.groups {
            channels = channels.saturating_add(g.layout().fixed_width().unwrap_or(2));
        }
        AudioFormat {
            sample_rate: self.plan.session_rate,
            channels: channels.max(1),
            sample_format: SampleFormat::F32,
        }
    }

    fn is_running(&self) -> bool {
        self.stream
            .as_ref()
            .map(|s| s.is_running())
            .unwrap_or(false)
    }

    fn overrun_count(&self) -> u64 {
        self.stream.as_ref().map(|s| s.overrun_count()).unwrap_or(0)
    }

    fn buffers_captured(&self) -> u64 {
        self.stream
            .as_ref()
            .map(|s| s.buffers_captured())
            .unwrap_or(0)
    }

    fn buffers_pushed(&self) -> u64 {
        self.stream
            .as_ref()
            .map(|s| s.buffers_pushed())
            .unwrap_or(0)
    }

    fn buffers_dropped(&self) -> u64 {
        self.stream
            .as_ref()
            .map(|s| s.buffers_dropped())
            .unwrap_or(0)
    }

    fn is_under_backpressure(&self) -> bool {
        self.stream
            .as_ref()
            .map(|s| s.is_under_backpressure())
            .unwrap_or(false)
    }

    fn drop_window_snapshot(&self) -> (u64, u64) {
        self.stream
            .as_ref()
            .map(|s| s.drop_window_snapshot())
            .unwrap_or((0, 0))
    }

    #[cfg(feature = "async-stream")]
    fn register_waker(&self, waker: &std::task::Waker) -> bool {
        // Honest wake promise: only a started stream can wake a parked task.
        self.stream
            .as_ref()
            .map(|s| s.register_waker(waker))
            .unwrap_or(false)
    }

    #[cfg(feature = "async-stream")]
    fn is_stream_producing(&self) -> bool {
        self.stream
            .as_ref()
            .map(|s| s.is_stream_producing())
            .unwrap_or(true)
    }
}

/// Poll a started capture for its negotiated channel count (Linux learns the
/// format at stream-open, slightly after `start()`); falls back to the
/// requested default (2) with a warning if it never materializes.
fn poll_channels(capture: &crate::api::AudioCapture) -> u16 {
    let deadline = Instant::now() + FORMAT_POLL_TIMEOUT;
    loop {
        if let Some(format) = capture.format() {
            return format.channels.max(1);
        }
        if Instant::now() >= deadline {
            log::warn!(
                "compose: keep-channels source did not report a negotiated format \
                 within {FORMAT_POLL_TIMEOUT:?}; assuming 2 channels"
            );
            return 2;
        }
        std::thread::sleep(FORMAT_POLL_INTERVAL);
    }
}
