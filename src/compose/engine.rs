//! The compositor engine: per-source ingest FIFOs, master-clock pacing,
//! silence padding / bounded trimming, per-group mixdown, and the thread that
//! runs it all.
//!
//! Everything here executes on the dedicated `rsac-compose` thread (plus the
//! caller thread during setup) — never on an OS audio callback thread, so
//! allocation is acceptable (ADR-0001 governs the RT producer paths inside
//! each inner capture, which are untouched).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::bridge::ring_buffer::BridgeProducer;
use crate::core::buffer::AudioBuffer;
use crate::core::config::AudioFormat;
use crate::core::error::AudioResult;

use super::builder::GroupLayout;
use super::resample::StreamResampler;

/// How many buffers to drain from one source per ingest pass, so a single
/// bursty source cannot starve the others or the tick check.
const MAX_DRAIN_PER_POLL: usize = 32;

/// Idle sleep between engine loop passes when no data moved.
const IDLE_SLEEP: Duration = Duration::from_millis(1);

// ── Source abstraction (test seam) ──────────────────────────────────────

/// What the engine needs from a capture source. Implemented by the real
/// `AudioCapture`-backed source in `stream.rs` and by scripted fakes in tests.
pub(crate) trait SourceReader: Send {
    /// Non-blocking, terminal-observable read: `Ok(None)` = no data yet,
    /// fatal `Err` = source ended (matches
    /// [`CapturingStream::try_read_chunk`](crate::core::interface::CapturingStream::try_read_chunk)
    /// semantics).
    fn try_read(&mut self) -> AudioResult<Option<AudioBuffer>>;

    /// Best-effort stop of the underlying capture (idempotent).
    fn stop(&mut self);
}

// ── Shared stats (engine writes, handle reads) ──────────────────────────

/// Lock-free per-source counters shared between the engine thread and the
/// public [`Composition`](super::Composition) handle.
#[derive(Debug, Default)]
pub(crate) struct SourceStatsShared {
    pub buffers_received: AtomicU64,
    pub padded_frames: AtomicU64,
    pub trimmed_frames: AtomicU64,
    pub resampling: AtomicBool,
    pub ended: AtomicBool,
}

/// Lock-free composition-wide counters.
#[derive(Debug, Default)]
pub(crate) struct EngineStatsShared {
    /// Total ticks emitted (master-paced + fallback).
    pub ticks: AtomicU64,
    /// Ticks emitted by the wall-clock stall fallback rather than master data.
    pub fallback_ticks: AtomicU64,
    /// Per-source counters, in flat declaration order.
    pub sources: Vec<Arc<SourceStatsShared>>,
}

// ── Engine configuration ────────────────────────────────────────────────

/// Static mixdown spec for one source (flat declaration order).
#[derive(Debug, Clone)]
pub(crate) struct SourceSpec {
    /// Linear gain applied during mixdown.
    pub gain: f32,
    /// Index of the group this source belongs to.
    pub group: usize,
    /// The source's fixed channel width. For keep-channels sources this is
    /// the polled negotiated width; for mixdown sources it is locked to the
    /// first delivered buffer (0 = not yet known).
    pub channels: u16,
    /// Whether this source targets a system/device endpoint whose clock ticks
    /// through silence — i.e. a preferred candidate when the master clock has
    /// to be re-elected after the current master ends.
    pub clock_candidate: bool,
}

/// Static spec for one group's slice of the composed frame.
#[derive(Debug, Clone)]
pub(crate) struct GroupSpec {
    pub layout: GroupLayout,
    /// First output channel this group writes.
    pub offset: usize,
    /// Number of output channels this group owns.
    pub width: usize,
}

/// Everything the engine thread needs, assembled by `Composition::start()`.
pub(crate) struct EngineConfig {
    pub composed_format: AudioFormat,
    pub quantum_frames: usize,
    pub max_fifo_frames: usize,
    pub stall_timeout: Duration,
    pub clamp_output: bool,
    pub master_index: usize,
    pub groups: Vec<GroupSpec>,
    pub sources: Vec<SourceSpec>,
}

impl EngineConfig {
    /// The session rate every source is aligned to. Single-sourced from the
    /// composed delivery format so the two can never disagree.
    pub fn session_rate(&self) -> u32 {
        self.composed_format.sample_rate
    }
}

// ── Per-source runtime state ────────────────────────────────────────────

struct SourceState {
    reader: Box<dyn SourceReader>,
    spec: SourceSpec,
    /// Interleaved samples at the session rate, `spec.channels` wide.
    fifo: VecDeque<f32>,
    resampler: Option<StreamResampler>,
    /// Input rate the current resampler was built for (0 = none needed yet).
    resampler_in_rate: u32,
    /// Whether a channel-width mismatch was already warned about.
    warned_channel_adapt: bool,
    ended: bool,
    stats: Arc<SourceStatsShared>,
}

impl SourceState {
    fn fifo_frames(&self) -> usize {
        if self.spec.channels == 0 {
            0
        } else {
            self.fifo.len() / usize::from(self.spec.channels)
        }
    }
}

// ── Engine ──────────────────────────────────────────────────────────────

pub(crate) struct Engine {
    cfg: EngineConfig,
    sources: Vec<SourceState>,
    producer: BridgeProducer,
    stop_flag: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    stats: Arc<EngineStatsShared>,
    last_tick: Instant,
    /// Reused per-tick output scratch (composed interleaved frame block).
    out_scratch: Vec<f32>,
    /// Reused per-source drain scratch.
    src_scratch: Vec<f32>,
}

impl Engine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: EngineConfig,
        readers: Vec<Box<dyn SourceReader>>,
        producer: BridgeProducer,
        stop_flag: Arc<AtomicBool>,
        active: Arc<AtomicBool>,
        stats: Arc<EngineStatsShared>,
    ) -> Self {
        debug_assert_eq!(readers.len(), cfg.sources.len());
        debug_assert_eq!(readers.len(), stats.sources.len());
        let sources = readers
            .into_iter()
            .zip(cfg.sources.iter().cloned())
            .zip(stats.sources.iter().cloned())
            .map(|((reader, spec), stats)| SourceState {
                reader,
                spec,
                fifo: VecDeque::new(),
                resampler: None,
                resampler_in_rate: 0,
                warned_channel_adapt: false,
                ended: false,
                stats,
            })
            .collect();
        let total_channels = usize::from(cfg.composed_format.channels);
        let quantum = cfg.quantum_frames;
        Self {
            out_scratch: vec![0.0; quantum * total_channels],
            src_scratch: Vec::new(),
            cfg,
            sources,
            producer,
            stop_flag,
            active,
            stats,
            last_tick: Instant::now(),
        }
    }

    /// The engine thread body: loop until stopped or every source has ended
    /// and drained, then tear down (stop readers, signal the ring done).
    pub fn run(mut self) {
        self.last_tick = Instant::now();
        // Wall-clock cadence for fallback ticks: one quantum's worth of time.
        let quantum_duration = Duration::from_nanos(
            (self.cfg.quantum_frames as u128 * 1_000_000_000u128
                / self.cfg.session_rate().max(1) as u128) as u64,
        );
        // `true` once the wall-clock fallback has engaged; reset by the next
        // master-paced tick. While engaged, fallback ticks run at *quantum*
        // cadence (real-time rate) — the stall timeout only gates *entering*
        // fallback, otherwise a stalled master would collapse throughput to
        // one quantum per stall_timeout and force `trim_all` to discard most
        // of the still-live sources' audio.
        let mut fallback_engaged = false;
        loop {
            if self.stop_flag.load(Ordering::SeqCst) {
                break;
            }

            let ingested = self.ingest_all();

            // Master-paced ticks: emit while the master has a full quantum.
            // The master is re-elected if the configured one has ended (see
            // `effective_master_frames`), so a dead master hands the clock to
            // a live source at full data rate instead of starving the session.
            let mut emitted = false;
            while self.effective_master_frames() >= self.cfg.quantum_frames {
                self.emit_tick(self.cfg.quantum_frames, false);
                emitted = true;
                fallback_engaged = false;
            }

            // Wall-clock fallback: a stalled master never freezes the session.
            // Entering costs a full stall_timeout; once engaged, ticks continue
            // at quantum cadence until master data resumes.
            if !emitted {
                let threshold = if fallback_engaged {
                    quantum_duration
                } else {
                    self.cfg.stall_timeout
                };
                if self.last_tick.elapsed() >= threshold {
                    self.emit_tick(self.cfg.quantum_frames, true);
                    emitted = true;
                    fallback_engaged = true;
                }
            }

            self.trim_all();

            // Terminal condition: every source ended → flush remaining tails
            // as final ticks, then exit.
            if self.sources.iter().all(|s| s.ended) {
                self.flush_tail();
                break;
            }

            if !ingested && !emitted {
                std::thread::sleep(IDLE_SLEEP);
            }
        }

        // Teardown: stop inner captures, end the composed ring, then flip the
        // active flag. Order matters: `signal_done()` (Running → Stopping)
        // must happen-before `active = false`, because the consumer-side view
        // promotes Stopping → Stopped when it observes `!active` on an empty
        // ring — flipping the flag first could promote from a still-Running
        // state and fail the CAS.
        for s in &mut self.sources {
            s.reader.stop();
        }
        self.producer.signal_done();
        self.active.store(false, Ordering::SeqCst);
        log::debug!(
            "rsac-compose engine exited (ticks={}, fallback_ticks={})",
            self.stats.ticks.load(Ordering::Relaxed),
            self.stats.fallback_ticks.load(Ordering::Relaxed)
        );
    }

    /// FIFO depth (frames) of the *effective* master: the configured master
    /// while it lives; after it ends, the clock is re-elected to the first
    /// live clock-candidate source (system/device — ticks through silence),
    /// else the first live source. Without re-election an ended master would
    /// pin the session to wall-clock fallback pacing while `trim_all`
    /// discards the still-live sources' audio.
    fn effective_master_frames(&self) -> usize {
        let configured = self.sources.get(self.cfg.master_index);
        let master = match configured {
            Some(s) if !s.ended => Some(s),
            _ => self
                .sources
                .iter()
                .find(|s| !s.ended && s.spec.clock_candidate)
                .or_else(|| self.sources.iter().find(|s| !s.ended)),
        };
        master.map(|s| s.fifo_frames()).unwrap_or(0)
    }

    /// Drain pending buffers from every live source into its FIFO.
    /// Returns `true` if any data moved.
    fn ingest_all(&mut self) -> bool {
        let mut moved = false;
        for s in &mut self.sources {
            if s.ended {
                continue;
            }
            for _ in 0..MAX_DRAIN_PER_POLL {
                match s.reader.try_read() {
                    Ok(Some(buffer)) => {
                        Self::ingest_buffer(s, buffer, self.cfg.session_rate());
                        moved = true;
                    }
                    Ok(None) => break,
                    Err(e) if e.is_fatal() => {
                        log::debug!("compose source ended: {e:?}");
                        s.ended = true;
                        s.stats.ended.store(true, Ordering::Relaxed);
                        break;
                    }
                    Err(e) => {
                        // Transient read error: retry on the next pass.
                        log::warn!("compose source transient read error (retrying): {e:?}");
                        break;
                    }
                }
            }
        }
        moved
    }

    /// Normalize one delivered buffer (channel width, then rate) and append it
    /// to the source FIFO.
    fn ingest_buffer(s: &mut SourceState, buffer: AudioBuffer, session_rate: u32) {
        s.stats.buffers_received.fetch_add(1, Ordering::Relaxed);
        let buf_channels = buffer.channels().max(1);
        let buf_rate = buffer.sample_rate();

        // Lock the source's width on first data (keep-channels sources have it
        // pre-set from the polled negotiated format).
        if s.spec.channels == 0 {
            s.spec.channels = buf_channels;
        }

        // Channel-width normalization keeps the FIFO stride constant even if a
        // source's delivery width changes mid-stream (it shouldn't).
        let data: &[f32] = buffer.data();
        let adapted: Vec<f32>;
        let samples: &[f32] = if buf_channels == s.spec.channels {
            data
        } else {
            if !s.warned_channel_adapt {
                log::warn!(
                    "compose source delivered {} channels; adapting to its locked width {}",
                    buf_channels,
                    s.spec.channels
                );
                s.warned_channel_adapt = true;
            }
            adapted = adapt_channels(data, buf_channels, s.spec.channels);
            &adapted
        };

        if buf_rate == session_rate {
            s.fifo.extend(samples.iter().copied());
            return;
        }

        // Rate conversion path. (Re)create the resampler if the delivered rate
        // changed or none exists yet.
        if s.resampler.is_none() || s.resampler_in_rate != buf_rate {
            if s.resampler.is_some() {
                log::warn!(
                    "compose source input rate changed {} -> {} Hz; rebuilding resampler",
                    s.resampler_in_rate,
                    buf_rate
                );
            }
            match StreamResampler::new(buf_rate, session_rate, s.spec.channels) {
                Ok(rs) => {
                    s.resampler = Some(rs);
                    s.resampler_in_rate = buf_rate;
                    s.stats.resampling.store(true, Ordering::Relaxed);
                }
                Err(e) => {
                    log::error!("compose: cannot resample source ({e}); dropping its data");
                    return;
                }
            }
        }
        if let Some(rs) = s.resampler.as_mut() {
            if let Err(e) = rs.push(samples, &mut s.fifo) {
                log::error!("compose resampler error ({e}); dropping buffer");
            }
        }
    }

    /// Bound every FIFO: a source drifting ahead of consumption has its oldest
    /// samples trimmed so latency stays bounded.
    fn trim_all(&mut self) {
        for s in &mut self.sources {
            let frames = s.fifo_frames();
            if frames > self.cfg.max_fifo_frames {
                let excess = frames - self.cfg.max_fifo_frames;
                let drop_samples = excess * usize::from(s.spec.channels);
                s.fifo.drain(..drop_samples);
                s.stats
                    .trimmed_frames
                    .fetch_add(excess as u64, Ordering::Relaxed);
            }
        }
    }

    /// After every source has ended, drain whatever is left in the FIFOs as
    /// final (possibly partial) ticks so no captured audio is discarded.
    fn flush_tail(&mut self) {
        loop {
            let remaining = self
                .sources
                .iter()
                .map(|s| s.fifo_frames())
                .max()
                .unwrap_or(0);
            if remaining == 0 {
                break;
            }
            let frames = remaining.min(self.cfg.quantum_frames);
            self.emit_tick(frames, false);
        }
    }

    /// Mix one tick of `frames` frames from every source FIFO into a composed
    /// interleaved buffer and push it into the bridge ring. Sources without
    /// enough data are zero-padded (counted per source).
    fn emit_tick(&mut self, frames: usize, fallback: bool) {
        let total_channels = usize::from(self.cfg.composed_format.channels);
        let needed = frames * total_channels;
        if self.out_scratch.len() < needed {
            self.out_scratch.resize(needed, 0.0);
        }
        let out = &mut self.out_scratch[..needed];
        out.fill(0.0);

        for s in &mut self.sources {
            let ch = usize::from(s.spec.channels.max(1));
            let group = &self.cfg.groups[s.spec.group];

            let have_frames = s.fifo_frames();
            let take = have_frames.min(frames);
            let pad = frames - take;
            if pad > 0 {
                s.stats
                    .padded_frames
                    .fetch_add(pad as u64, Ordering::Relaxed);
            }
            if take == 0 {
                continue; // fully padded: contributes silence (already zeroed)
            }

            // Drain `take` frames into contiguous scratch for strided access.
            let take_samples = take * ch;
            self.src_scratch.clear();
            self.src_scratch
                .extend(s.fifo.drain(..take_samples.min(s.fifo.len())));
            let src = &self.src_scratch;

            mix_source_into(
                out,
                total_channels,
                src,
                ch,
                take,
                s.spec.gain,
                group.layout,
                group.offset,
                group.width,
            );
        }

        if self.cfg.clamp_output {
            for v in out.iter_mut() {
                *v = v.clamp(-1.0, 1.0);
            }
        }

        let buffer = AudioBuffer::with_format(out.to_vec(), self.cfg.composed_format.clone());
        // Drop-on-full: the bridge counts drops, surfacing consumer lag via
        // the composed stream's overrun/backpressure counters.
        let _ = self.producer.push_or_drop(buffer);

        self.stats.ticks.fetch_add(1, Ordering::Relaxed);
        if fallback {
            self.stats.fallback_ticks.fetch_add(1, Ordering::Relaxed);
        }
        self.last_tick = Instant::now();
    }
}

/// Mix `take` frames of one source (interleaved `src`, `src_ch` wide) into the
/// composed output slice according to the group layout.
#[allow(clippy::too_many_arguments)]
fn mix_source_into(
    out: &mut [f32],
    total_channels: usize,
    src: &[f32],
    src_ch: usize,
    take: usize,
    gain: f32,
    layout: GroupLayout,
    offset: usize,
    width: usize,
) {
    match layout {
        GroupLayout::Mono => {
            for f in 0..take {
                let frame = &src[f * src_ch..(f + 1) * src_ch];
                let mono = frame.iter().sum::<f32>() / src_ch as f32;
                out[f * total_channels + offset] += gain * mono;
            }
        }
        GroupLayout::Stereo => match src_ch {
            1 => {
                for f in 0..take {
                    let v = gain * src[f];
                    out[f * total_channels + offset] += v;
                    out[f * total_channels + offset + 1] += v;
                }
            }
            2 => {
                for f in 0..take {
                    out[f * total_channels + offset] += gain * src[f * 2];
                    out[f * total_channels + offset + 1] += gain * src[f * 2 + 1];
                }
            }
            n => {
                // Fold wider sources even→L / odd→R (per-side mean).
                let left_n = n.div_ceil(2) as f32;
                let right_n = (n / 2).max(1) as f32;
                for f in 0..take {
                    let frame = &src[f * n..(f + 1) * n];
                    let (mut l, mut r) = (0.0f32, 0.0f32);
                    for (c, &v) in frame.iter().enumerate() {
                        if c % 2 == 0 {
                            l += v;
                        } else {
                            r += v;
                        }
                    }
                    out[f * total_channels + offset] += gain * l / left_n;
                    out[f * total_channels + offset + 1] += gain * r / right_n;
                }
            }
        },
        GroupLayout::KeepChannels => {
            // Pass native channels through; if the locked width diverged from
            // the group width (shouldn't happen), truncate/zero-pad per frame.
            let copy = src_ch.min(width);
            for f in 0..take {
                let frame = &src[f * src_ch..(f + 1) * src_ch];
                let out_frame = &mut out[f * total_channels + offset..];
                for c in 0..copy {
                    out_frame[c] += gain * frame[c];
                }
            }
        }
    }
}

/// Convert interleaved samples from `from` channels to `to` channels per
/// frame: shared channels copy through, extra target channels are silence,
/// extra source channels are discarded. (Used only to keep a source's FIFO
/// stride constant if its delivery width changes mid-stream; group mixdown
/// handles the real fold.)
fn adapt_channels(data: &[f32], from: u16, to: u16) -> Vec<f32> {
    let (from, to) = (usize::from(from.max(1)), usize::from(to.max(1)));
    let frames = data.len() / from;
    let mut out = vec![0.0f32; frames * to];
    let copy = from.min(to);
    for f in 0..frames {
        out[f * to..f * to + copy].copy_from_slice(&data[f * from..f * from + copy]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapt_channels_widen_and_narrow() {
        // 2ch -> 1ch: keeps channel 0.
        let stereo = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(adapt_channels(&stereo, 2, 1), vec![1.0, 3.0]);
        // 1ch -> 2ch: channel 1 is silence.
        assert_eq!(adapt_channels(&[5.0, 6.0], 1, 2), vec![5.0, 0.0, 6.0, 0.0]);
    }

    #[test]
    fn mix_mono_layout_folds_and_sums() {
        // 2 output channels total; mono group at offset 1.
        let mut out = vec![0.0f32; 2 * 2];
        let src = [1.0f32, 3.0, 5.0, 7.0]; // stereo source, 2 frames
        mix_source_into(&mut out, 2, &src, 2, 2, 0.5, GroupLayout::Mono, 1, 1);
        // frame 0: mean(1,3)=2 * 0.5 = 1.0 → out[1]; frame 1: mean(5,7)=6*0.5=3.0 → out[3]
        assert_eq!(out, vec![0.0, 1.0, 0.0, 3.0]);
    }

    #[test]
    fn mix_stereo_layout_mono_source_duplicates() {
        let mut out = vec![0.0f32; 2 * 2];
        let src = [0.25f32, 0.5];
        mix_source_into(&mut out, 2, &src, 1, 2, 2.0, GroupLayout::Stereo, 0, 2);
        assert_eq!(out, vec![0.5, 0.5, 1.0, 1.0]);
    }

    #[test]
    fn mix_stereo_layout_folds_wide_source() {
        // 4-channel source folded to stereo: L = mean(c0,c2), R = mean(c1,c3).
        let mut out = vec![0.0f32; 2];
        let src = [1.0f32, 2.0, 3.0, 4.0]; // one frame
        mix_source_into(&mut out, 2, &src, 4, 1, 1.0, GroupLayout::Stereo, 0, 2);
        assert_eq!(out, vec![2.0, 3.0]);
    }

    #[test]
    fn mix_keep_channels_passthrough_with_gain() {
        let mut out = vec![0.0f32; 3]; // 3 output channels, 1 frame
        let src = [1.0f32, -1.0]; // stereo frame
        mix_source_into(
            &mut out,
            3,
            &src,
            2,
            1,
            0.5,
            GroupLayout::KeepChannels,
            1,
            2,
        );
        assert_eq!(out, vec![0.0, 0.5, -0.5]);
    }

    #[test]
    fn mix_sums_two_sources_into_same_group() {
        let mut out = vec![0.0f32; 1];
        mix_source_into(&mut out, 1, &[0.25], 1, 1, 1.0, GroupLayout::Mono, 0, 1);
        mix_source_into(&mut out, 1, &[0.5], 1, 1, 0.5, GroupLayout::Mono, 0, 1);
        assert!((out[0] - 0.5).abs() < 1e-6, "0.25 + 0.25 = {}", out[0]);
    }
}
