//! Engine-loop tests: drive the full compositor thread with scripted sources
//! and read the composed output through a real `BridgeStream` — the same data
//! plane a device-backed composition uses, with zero hardware dependency.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::core::buffer::AudioBuffer;
use crate::core::config::{AudioFormat, SampleFormat};
use crate::core::error::{AudioError, AudioResult, LifecycleStage};
use crate::core::interface::CapturingStream;

use super::builder::{CompositionBuilder, Group, GroupLayout};
use super::engine::{
    Engine, EngineConfig, EngineStatsShared, GroupSpec, GroupStatsShared, SourceReader, SourceSpec,
    SourceStatsShared,
};
use super::stream::{assemble_pipeline, ComposedStreamView};

// ── Scripted source ─────────────────────────────────────────────────────

/// A `SourceReader` that plays back a fixed script of buffers, then either
/// ends (fatal `StreamEnded`) or stays live returning `Ok(None)`.
struct ScriptedSource {
    buffers: VecDeque<AudioBuffer>,
    end_when_empty: bool,
    stopped: Arc<AtomicBool>,
}

impl ScriptedSource {
    fn ending(buffers: Vec<AudioBuffer>) -> (Self, Arc<AtomicBool>) {
        let stopped = Arc::new(AtomicBool::new(false));
        (
            Self {
                buffers: buffers.into(),
                end_when_empty: true,
                stopped: Arc::clone(&stopped),
            },
            stopped,
        )
    }

    fn live(buffers: Vec<AudioBuffer>) -> (Self, Arc<AtomicBool>) {
        let stopped = Arc::new(AtomicBool::new(false));
        (
            Self {
                buffers: buffers.into(),
                end_when_empty: false,
                stopped: Arc::clone(&stopped),
            },
            stopped,
        )
    }
}

impl SourceReader for ScriptedSource {
    fn try_read(&mut self) -> AudioResult<Option<AudioBuffer>> {
        match self.buffers.pop_front() {
            Some(b) => Ok(Some(b)),
            None if self.end_when_empty => Err(AudioError::StreamEnded {
                reason: "scripted source exhausted".to_string(),
            }),
            None => Ok(None),
        }
    }

    fn stop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
}

// ── Harness ─────────────────────────────────────────────────────────────

fn const_buffer(value: f32, channels: u16, rate: u32, frames: usize) -> AudioBuffer {
    AudioBuffer::new(vec![value; frames * usize::from(channels)], channels, rate)
}

/// Like [`const_buffer`], but stamped with a stream-position timestamp — the
/// shape every backend now delivers (rsac-ae4e gap-compensation tests).
fn stamped_buffer(
    value: f32,
    channels: u16,
    rate: u32,
    frames: usize,
    ts: Duration,
) -> AudioBuffer {
    AudioBuffer::with_timestamp(
        vec![value; frames * usize::from(channels)],
        AudioFormat {
            sample_rate: rate,
            channels,
            sample_format: SampleFormat::F32,
        },
        ts,
    )
}

struct Harness {
    stream: Arc<ComposedStreamView>,
    stats: Arc<EngineStatsShared>,
    stop_flag: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Harness {
    /// Spawns a real engine thread over the given config and sources; the
    /// returned harness reads composed buffers from the bridge stream. Uses
    /// the SAME pipeline assembly as `Composition::start()`
    /// (`assemble_pipeline`) so these tests exercise the production wiring.
    fn spawn(cfg: EngineConfig, readers: Vec<Box<dyn SourceReader>>) -> Self {
        let pipeline = assemble_pipeline(cfg.composed_format.clone(), 256, Duration::from_secs(2))
            .expect("pipeline assembly");
        let stats = Arc::new(EngineStatsShared {
            sources: (0..readers.len())
                .map(|_| Arc::new(SourceStatsShared::default()))
                .collect(),
            groups: (0..cfg.groups.len())
                .map(|_| Arc::new(GroupStatsShared::default()))
                .collect(),
            ..Default::default()
        });
        let engine = Engine::new(
            cfg,
            readers,
            pipeline.producer,
            Arc::clone(&pipeline.stop_flag),
            pipeline.active,
            Arc::clone(&stats),
        );
        let stop_flag = pipeline.stop_flag;
        let stream = pipeline.view;
        let join = std::thread::spawn(move || engine.run());
        Self {
            stream,
            stats,
            stop_flag,
            join: Some(join),
        }
    }

    /// Drains the composed stream until the terminal `StreamEnded`, returning
    /// every composed buffer. Panics if the stream doesn't end within ~5 s.
    fn drain_to_end(&self) -> Vec<AudioBuffer> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut out = Vec::new();
        loop {
            match self.stream.try_read_chunk() {
                Ok(Some(b)) => out.push(b),
                Ok(None) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "composed stream did not end in time"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) if e.is_fatal() => break,
                Err(e) => panic!("unexpected recoverable error: {e:?}"),
            }
        }
        out
    }

    /// Reads the next composed buffer (blocking-poll to a deadline) and returns
    /// its first sample. Panics if none arrives in ~5 s or the stream ends.
    /// Used by the live gain/mute tests (rsac-5a2d).
    fn read_first_sample(&self) -> f32 {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match self.stream.try_read_chunk() {
                Ok(Some(b)) => return b.data()[0],
                Ok(None) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "engine produced no buffer in time"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => panic!("unexpected error while reading: {e:?}"),
            }
        }
    }

    fn shutdown(mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn fmt(channels: u16, rate: u32) -> AudioFormat {
    AudioFormat {
        sample_rate: rate,
        channels,
        sample_format: SampleFormat::F32,
    }
}

/// Convenience: config for `sources` with 10 ms quantum @ 48 kHz.
fn cfg(
    total_channels: u16,
    groups: Vec<GroupSpec>,
    sources: Vec<SourceSpec>,
    master_index: usize,
) -> EngineConfig {
    EngineConfig {
        composed_format: fmt(total_channels, 48_000),
        quantum_frames: 480,
        max_fifo_frames: 48_000,
        stall_timeout: Duration::from_millis(250),
        clamp_output: false,
        master_index,
        groups,
        sources,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────

/// Two sources, two groups (mono mixdown + keep-channels stereo): the
/// composed stream is 3 channels wide with the group values in declaration
/// order, ends cleanly when both sources end, and loses no frames.
#[test]
fn composes_mono_and_keep_groups_end_to_end() {
    let n_buffers = 10usize;
    // Source 0 (group 0, mono mixdown): stereo 0.25s.
    let (src_a, stopped_a) = ScriptedSource::ending(
        (0..n_buffers)
            .map(|_| const_buffer(0.25, 2, 48_000, 480))
            .collect(),
    );
    // Source 1 (group 1, keep stereo, master): stereo 0.5s.
    let (src_b, stopped_b) = ScriptedSource::ending(
        (0..n_buffers)
            .map(|_| const_buffer(0.5, 2, 48_000, 480))
            .collect(),
    );

    let harness = Harness::spawn(
        cfg(
            3,
            vec![
                GroupSpec {
                    layout: GroupLayout::Mono,
                    offset: 0,
                    width: 1,
                },
                GroupSpec {
                    layout: GroupLayout::KeepChannels,
                    offset: 1,
                    width: 2,
                },
            ],
            vec![
                SourceSpec {
                    gain: 1.0,
                    group: 0,
                    channels: 0,
                    clock_candidate: false,
                },
                SourceSpec {
                    gain: 1.0,
                    group: 1,
                    channels: 2,
                    clock_candidate: false,
                },
            ],
            1, // master = keep source
        ),
        vec![Box::new(src_a), Box::new(src_b)],
    );

    let buffers = harness.drain_to_end();
    let total_frames: usize = buffers.iter().map(|b| b.num_frames()).sum();
    assert_eq!(total_frames, n_buffers * 480, "no frames lost or invented");

    // Composed buffers carry contiguous stream-position timestamps
    // (frames emitted / session rate), mirroring the backends' stamped pushes.
    let mut expected_frames = 0u64;
    for b in &buffers {
        let expected = Duration::from_nanos(expected_frames * 1_000_000_000 / 48_000);
        assert_eq!(
            b.timestamp(),
            Some(expected),
            "composed tick timestamp must be its stream position"
        );
        expected_frames += b.num_frames() as u64;
    }

    for b in &buffers {
        assert_eq!(b.channels(), 3);
        assert_eq!(b.sample_rate(), 48_000);
        let data = b.data();
        for f in 0..b.num_frames() {
            assert!((data[f * 3] - 0.25).abs() < 1e-6, "mono group value");
            assert!((data[f * 3 + 1] - 0.5).abs() < 1e-6, "keep L value");
            assert!((data[f * 3 + 2] - 0.5).abs() < 1e-6, "keep R value");
        }
    }

    // Engine teardown stopped both readers.
    assert!(stopped_a.load(Ordering::SeqCst));
    assert!(stopped_b.load(Ordering::SeqCst));
    harness.shutdown();
}

/// A source that runs dry before the master is silence-padded (zeros in its
/// channels) and its `padded_frames` counter reflects the shortfall.
#[test]
fn behind_source_is_silence_padded_and_counted() {
    let master_buffers = 10usize;
    let short_buffers = 4usize;
    let (short_src, _) = ScriptedSource::ending(
        (0..short_buffers)
            .map(|_| const_buffer(0.8, 1, 48_000, 480))
            .collect(),
    );
    let (master_src, _) = ScriptedSource::ending(
        (0..master_buffers)
            .map(|_| const_buffer(0.5, 1, 48_000, 480))
            .collect(),
    );

    let harness = Harness::spawn(
        cfg(
            2,
            vec![
                GroupSpec {
                    layout: GroupLayout::Mono,
                    offset: 0,
                    width: 1,
                },
                GroupSpec {
                    layout: GroupLayout::Mono,
                    offset: 1,
                    width: 1,
                },
            ],
            vec![
                SourceSpec {
                    gain: 1.0,
                    group: 0,
                    channels: 0,
                    clock_candidate: false,
                },
                SourceSpec {
                    gain: 1.0,
                    group: 1,
                    channels: 0,
                    clock_candidate: false,
                },
            ],
            1,
        ),
        vec![Box::new(short_src), Box::new(master_src)],
    );

    let buffers = harness.drain_to_end();
    let total_frames: usize = buffers.iter().map(|b| b.num_frames()).sum();
    assert_eq!(
        total_frames,
        master_buffers * 480,
        "master paces the output"
    );

    // Early frames carry the short source; late frames are silence in ch 0.
    let all: Vec<f32> = buffers
        .iter()
        .flat_map(|b| b.data().iter().copied())
        .collect();
    let first_frame_ch0 = all[0];
    let last_frame_ch0 = all[all.len() - 2];
    assert!((first_frame_ch0 - 0.8).abs() < 1e-6);
    assert!(last_frame_ch0.abs() < 1e-9, "tail must be padded silence");

    let padded = harness.stats.sources[0]
        .padded_frames
        .load(Ordering::Relaxed);
    assert_eq!(
        padded as usize,
        (master_buffers - short_buffers) * 480,
        "padding counter matches the shortfall"
    );
    harness.shutdown();
}

/// A source drifting far ahead of the master has its oldest samples trimmed
/// at the buffering bound, and the trim counter records it.
#[test]
fn ahead_source_is_bounded_and_trimmed() {
    // Master delivers 1 buffer; the other source delivers 1 s + extra.
    let (master_src, _) = ScriptedSource::live(vec![const_buffer(0.5, 1, 48_000, 480)]);
    let (fast_src, _) = ScriptedSource::live(
        (0..150)
            .map(|_| const_buffer(0.1, 1, 48_000, 480))
            .collect(),
    );

    let mut config = cfg(
        2,
        vec![
            GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            },
            GroupSpec {
                layout: GroupLayout::Mono,
                offset: 1,
                width: 1,
            },
        ],
        vec![
            SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            },
            SourceSpec {
                gain: 1.0,
                group: 1,
                channels: 0,
                clock_candidate: false,
            },
        ],
        0,
    );
    // Tight bound: half a second.
    config.max_fifo_frames = 24_000;
    // Long stall timeout so the fallback doesn't drain the fast source first.
    config.stall_timeout = Duration::from_secs(10);

    let harness = Harness::spawn(config, vec![Box::new(master_src), Box::new(fast_src)]);

    // Give the engine time to ingest and trim (sources stay live; no end).
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let trimmed = harness.stats.sources[1]
            .trimmed_frames
            .load(Ordering::Relaxed);
        if trimmed > 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fast source was never trimmed"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.shutdown();
}

/// A 44.1 kHz source is transparently resampled to the 48 kHz session: the
/// resampling flag flips on and the composed output still carries its signal.
#[test]
fn heterogeneous_rate_source_is_resampled() {
    // ~1 s of 44.1 kHz audio in 441-frame buffers (mono, constant 0.3).
    let n = 100usize;
    let (src, _) =
        ScriptedSource::ending((0..n).map(|_| const_buffer(0.3, 1, 44_100, 441)).collect());

    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(src)],
    );

    let buffers = harness.drain_to_end();
    assert!(
        harness.stats.sources[0].resampling.load(Ordering::Relaxed),
        "resampling flag must be set"
    );
    let total_frames: usize = buffers.iter().map(|b| b.num_frames()).sum();
    // rsac-fab0: with the end-of-stream resampler flush, the composed output
    // carries EXACTLY round(input * to/from) frames — the final partial input
    // chunk and the FFT delay residue included, trimmed to the exact length
    // owed. ±1 frame of slack for rounding-convention differences only.
    let expected = (44_100u64 * 48_000 + 44_100 / 2) / 44_100; // = 48_000
    assert!(
        (total_frames as i64 - expected as i64).abs() <= 1,
        "expected {expected}±1 resampled frames, got {total_frames}"
    );
    // Constant DC input should come out near-constant after the transient.
    let all: Vec<f32> = buffers
        .iter()
        .flat_map(|b| b.data().iter().copied())
        .collect();
    let mid = all[all.len() / 2];
    assert!((mid - 0.3).abs() < 0.05, "signal preserved, got {mid}");
    harness.shutdown();
}

/// rsac-7d97: a mid-stream input-rate change must FLUSH the old resampler —
/// its buffered partial input chunk plus the FFT `output_delay()` residue
/// (~25–45 ms of real audio) — into the FIFO before the replacement
/// resampler is built. Without the flush that tail is silently dropped and
/// the composed output comes up hundreds of frames short.
#[test]
fn mid_stream_rate_change_flushes_old_resampler_tail() {
    // Phase A: 4410 frames @ 44.1 kHz. Deliberately NOT a multiple of the
    // resampler's 1024-frame input chunk, so a partial chunk is pending at
    // the switch (on top of the always-present delay residue).
    let a_buffers = 10usize;
    let a_frames = 441usize;
    // Phase B: 3200 frames @ 32 kHz — a different rate, still resampled.
    let b_buffers = 10usize;
    let b_frames = 320usize;
    let mut script: Vec<AudioBuffer> = (0..a_buffers)
        .map(|_| const_buffer(0.3, 1, 44_100, a_frames))
        .collect();
    script.extend((0..b_buffers).map(|_| const_buffer(0.6, 1, 32_000, b_frames)));
    let (src, _) = ScriptedSource::ending(script);

    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(src)],
    );

    let buffers = harness.drain_to_end();
    let total_frames: usize = buffers.iter().map(|b| b.num_frames()).sum();

    // Each phase owes exactly round(in * 48_000/rate) output frames (both
    // divide exactly here): the rate-change flush recovers phase A's tail
    // (rsac-7d97) and the natural-end flush recovers phase B's (rsac-fab0).
    // Without the rate-change flush, phase A alone comes up short by its
    // pending partial chunk plus the FFT delay residue — several hundred
    // frames, far beyond this tolerance.
    let expected_a = (a_buffers * a_frames) as u64 * 48_000 / 44_100; // = 4800
    let expected_b = (b_buffers * b_frames) as u64 * 48_000 / 32_000; // = 4800
    let expected = expected_a + expected_b;
    assert!(
        (total_frames as i64 - expected as i64).abs() <= 2,
        "expected {expected}±2 composed frames across the rate change, got {total_frames}"
    );

    // The signal on both sides of the switch survives (DC levels preserved).
    let all: Vec<f32> = buffers
        .iter()
        .flat_map(|b| b.data().iter().copied())
        .collect();
    let a_mid = all[all.len() / 4];
    let b_mid = all[3 * all.len() / 4];
    assert!((a_mid - 0.3).abs() < 0.05, "phase A signal, got {a_mid}");
    assert!((b_mid - 0.6).abs() < 0.05, "phase B signal, got {b_mid}");
    harness.shutdown();
}

/// rsac-b7d4: renegotiating a resampled source to EXACTLY the session rate
/// takes the direct/bypass path, which used to strand the old resampler's
/// tail (pending partial chunk + FFT delay residue) without flushing it —
/// silently dropping ~25–45 ms of real audio. The bypass must flush the
/// stranded resampler just like the rate-change path (rsac-7d97) does.
#[test]
fn renegotiation_to_session_rate_flushes_stranded_resampler_tail() {
    // Phase A: 4410 frames @ 44.1 kHz — resampled, and NOT a multiple of the
    // 1024-frame input chunk, so a partial chunk is pending at the switch.
    let a_buffers = 10usize;
    let a_frames = 441usize;
    // Phase B: 4800 frames @ 48 kHz — exactly the session rate, direct path.
    let b_buffers = 10usize;
    let b_frames = 480usize;
    let mut script: Vec<AudioBuffer> = (0..a_buffers)
        .map(|_| const_buffer(0.3, 1, 44_100, a_frames))
        .collect();
    script.extend((0..b_buffers).map(|_| const_buffer(0.6, 1, 48_000, b_frames)));
    let (src, _) = ScriptedSource::ending(script);

    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(src)],
    );

    let buffers = harness.drain_to_end();
    let total_frames: usize = buffers.iter().map(|b| b.num_frames()).sum();

    // Phase A owes round(4410 * 48_000/44_100) = 4800 resampled frames —
    // recoverable in full only if the bypass flushes the stranded resampler.
    // Phase B passes through 1:1 (4800 frames). Without the fix, phase A
    // comes up short by its pending partial chunk plus the FFT delay
    // residue — several hundred frames, far beyond this tolerance.
    let expected_a = (a_buffers * a_frames) as u64 * 48_000 / 44_100; // = 4800
    let expected_b = (b_buffers * b_frames) as u64; // = 4800, direct
    let expected = expected_a + expected_b;
    assert!(
        (total_frames as i64 - expected as i64).abs() <= 2,
        "expected {expected}±2 composed frames across the renegotiation to \
         the session rate, got {total_frames}"
    );

    // The resampling flag must drop back to false once the source is direct.
    assert!(
        !harness.stats.sources[0].resampling.load(Ordering::Relaxed),
        "resampling flag must clear after renegotiating to the session rate"
    );

    // The signal on both sides of the switch survives (DC levels preserved).
    let all: Vec<f32> = buffers
        .iter()
        .flat_map(|b| b.data().iter().copied())
        .collect();
    let a_mid = all[all.len() / 4];
    let b_mid = all[3 * all.len() / 4];
    assert!((a_mid - 0.3).abs() < 0.05, "phase A signal, got {a_mid}");
    assert!((b_mid - 0.6).abs() < 0.05, "phase B signal, got {b_mid}");
    harness.shutdown();
}

/// When the master stalls, the wall-clock fallback keeps ticking (with the
/// master padded) so a live secondary source still flows.
#[test]
fn stalled_master_triggers_fallback_ticks() {
    let (master_src, _) = ScriptedSource::live(vec![]); // never produces
    let (live_src, _) =
        ScriptedSource::live((0..50).map(|_| const_buffer(0.4, 1, 48_000, 480)).collect());

    let mut config = cfg(
        2,
        vec![
            GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            },
            GroupSpec {
                layout: GroupLayout::Mono,
                offset: 1,
                width: 1,
            },
        ],
        vec![
            SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            },
            SourceSpec {
                gain: 1.0,
                group: 1,
                channels: 0,
                clock_candidate: false,
            },
        ],
        0, // master is the silent source
    );
    config.stall_timeout = Duration::from_millis(20);

    let harness = Harness::spawn(config, vec![Box::new(master_src), Box::new(live_src)]);

    // Wait for fallback ticks to appear.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if harness.stats.fallback_ticks.load(Ordering::Relaxed) >= 3 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no fallback ticks despite stalled master"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    // Composed output must exist and carry the live source's signal in ch 1.
    let mut saw_live_signal = false;
    while let Ok(Some(b)) = harness.stream.try_read_chunk() {
        let data = b.data();
        if data.chunks(2).any(|f| (f[1] - 0.4).abs() < 1e-6) {
            saw_live_signal = true;
        }
        // Master channel is always padded silence.
        assert!(data.chunks(2).all(|f| f[0].abs() < 1e-9));
        if saw_live_signal {
            break;
        }
    }
    assert!(
        saw_live_signal,
        "live source audio must flow during fallback"
    );
    harness.shutdown();
}

/// `clamp_output` bounds a hot sum to [-1, 1]; without it the sum exceeds 1.
#[test]
fn clamp_output_bounds_hot_sum() {
    let make_sources = || -> Vec<Box<dyn SourceReader>> {
        let (a, _) = ScriptedSource::ending(vec![const_buffer(0.8, 1, 48_000, 480)]);
        let (b, _) = ScriptedSource::ending(vec![const_buffer(0.7, 1, 48_000, 480)]);
        vec![Box::new(a), Box::new(b)]
    };
    let group = || {
        vec![GroupSpec {
            layout: GroupLayout::Mono,
            offset: 0,
            width: 1,
        }]
    };
    let sources_spec = || {
        vec![
            SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            },
            SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            },
        ]
    };

    // Unclamped: 0.8 + 0.7 = 1.5.
    let harness = Harness::spawn(cfg(1, group(), sources_spec(), 0), make_sources());
    let buffers = harness.drain_to_end();
    let peak = buffers
        .iter()
        .flat_map(|b| b.data().iter())
        .fold(0.0f32, |m, &v| m.max(v));
    assert!((peak - 1.5).abs() < 1e-6, "unclamped sum, got {peak}");
    harness.shutdown();

    // Clamped: bounded at 1.0.
    let mut config = cfg(1, group(), sources_spec(), 0);
    config.clamp_output = true;
    let harness = Harness::spawn(config, make_sources());
    let buffers = harness.drain_to_end();
    let peak = buffers
        .iter()
        .flat_map(|b| b.data().iter())
        .fold(0.0f32, |m, &v| m.max(v));
    assert!((peak - 1.0).abs() < 1e-6, "clamped sum, got {peak}");
    harness.shutdown();
}

/// Per-source gain weights the mixdown sum.
#[test]
fn per_source_gain_is_applied() {
    let (a, _) = ScriptedSource::ending(vec![const_buffer(0.5, 1, 48_000, 480)]);
    let (b, _) = ScriptedSource::ending(vec![const_buffer(0.5, 1, 48_000, 480)]);

    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![
                SourceSpec {
                    gain: 1.0,
                    group: 0,
                    channels: 0,
                    clock_candidate: false,
                },
                SourceSpec {
                    gain: 0.5,
                    group: 0,
                    channels: 0,
                    clock_candidate: false,
                },
            ],
            0,
        ),
        vec![Box::new(a), Box::new(b)],
    );
    let buffers = harness.drain_to_end();
    let v = buffers[0].data()[0];
    assert!((v - 0.75).abs() < 1e-6, "0.5*1.0 + 0.5*0.5 = 0.75, got {v}");
    harness.shutdown();
}

/// rsac-5a2d: the build-time `SourceSpec.gain` seeds the *effective* runtime
/// gain via `Engine::new`, with no setter call — the first composed frame
/// already reflects 0.5, proving the seeding point (not the leaked `0.0`
/// `SourceStatsShared::default()` bits).
#[test]
fn build_time_gain_seeds_effective_gain() {
    let (a, _) = ScriptedSource::ending(vec![const_buffer(1.0, 1, 48_000, 480)]);
    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![SourceSpec {
                gain: 0.5,
                group: 0,
                channels: 0,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(a)],
    );
    let buffers = harness.drain_to_end();
    let v = buffers[0].data()[0];
    assert!((v - 0.5).abs() < 1e-6, "1.0 * seeded 0.5 = 0.5, got {v}");
    harness.shutdown();
}

/// rsac-5a2d: writing the live gain atomic mid-stream changes the mixed output
/// on the *next* tick, and it REPLACES (not multiplies) the seeded gain. A DC
/// source seeded at gain 1.0 reads ~1.0; after storing 0.5 a subsequent buffer
/// reads ~0.5. Proves the engine reads the atomic each tick — exactly what the
/// public `set_gain` does under the hood.
#[test]
fn live_set_gain_changes_mixed_output_on_next_tick() {
    let (a, _) = ScriptedSource::live(
        (0..2000)
            .map(|_| const_buffer(1.0, 1, 48_000, 480))
            .collect(),
    );
    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(a)],
    );

    // Read a pre-change buffer: must reflect the seeded gain 1.0.
    let before = harness.read_first_sample();
    assert!(
        (before - 1.0).abs() < 1e-6,
        "pre-change buffer reflects seeded gain 1.0, got {before}"
    );

    // Live gain change — identical to what `Composition::set_gain` performs.
    harness.stats.sources[0]
        .gain_bits
        .store(f32::to_bits(0.5), Ordering::Relaxed);

    // Poll subsequent buffers until one reflects the new gain (~1 quantum
    // latency; allow a few in-flight buffers to clear).
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let v = harness.read_first_sample();
        if (v - 0.5).abs() < 1e-6 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "gain change never reached the composed output (last={v})"
        );
    }
    harness.shutdown();
}

/// rsac-5a2d: setting the mute flag silences the source; clearing it restores
/// the (untouched) gain. Proves mute is a separate flag from gain.
#[test]
fn mute_silences_then_unmute_restores() {
    let (a, _) = ScriptedSource::live(
        (0..2000)
            .map(|_| const_buffer(0.8, 1, 48_000, 480))
            .collect(),
    );
    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(a)],
    );

    // Baseline: nonzero output.
    let before = harness.read_first_sample();
    assert!((before - 0.8).abs() < 1e-6, "baseline 0.8, got {before}");

    // Mute → a later fully-populated buffer must be all-silence.
    harness.stats.sources[0]
        .muted
        .store(true, Ordering::Relaxed);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let v = harness.read_first_sample();
        if v.abs() < 1e-9 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "muted source never went silent (last={v})"
        );
    }

    // Unmute → output returns to the untouched gain (0.8), proving gain_bits
    // was never disturbed by muting.
    harness.stats.sources[0]
        .muted
        .store(false, Ordering::Relaxed);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let v = harness.read_first_sample();
        if (v - 0.8).abs() < 1e-6 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "unmute did not restore the prior gain (last={v})"
        );
    }
    harness.shutdown();
}

/// rsac-5a2d: a muted source still drains its FIFO (drain-before-gain order
/// preserved), so a second live unmuted source's output is unaffected and
/// stays time-aligned — muting one lane does not desync the others.
#[test]
fn muted_source_still_drains_fifo() {
    // Source 0 (group 0): muted immediately.
    let (muted_src, _) = ScriptedSource::live(
        (0..2000)
            .map(|_| const_buffer(0.5, 1, 48_000, 480))
            .collect(),
    );
    // Source 1 (group 1, master): stays audible at 0.3.
    let (live_src, _) = ScriptedSource::live(
        (0..2000)
            .map(|_| const_buffer(0.3, 1, 48_000, 480))
            .collect(),
    );

    let harness = Harness::spawn(
        cfg(
            2,
            vec![
                GroupSpec {
                    layout: GroupLayout::Mono,
                    offset: 0,
                    width: 1,
                },
                GroupSpec {
                    layout: GroupLayout::Mono,
                    offset: 1,
                    width: 1,
                },
            ],
            vec![
                SourceSpec {
                    gain: 1.0,
                    group: 0,
                    channels: 0,
                    clock_candidate: false,
                },
                SourceSpec {
                    gain: 1.0,
                    group: 1,
                    channels: 0,
                    clock_candidate: false,
                },
            ],
            1,
        ),
        vec![Box::new(muted_src), Box::new(live_src)],
    );

    harness.stats.sources[0]
        .muted
        .store(true, Ordering::Relaxed);

    // Read several buffers: the muted lane (ch 0) is silent, the live lane
    // (ch 1) stays at its steady value — the muted source did not desync it.
    for _ in 0..5 {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match harness.stream.try_read_chunk() {
                Ok(Some(b)) => {
                    let d = b.data();
                    // Fully-populated frames only (skip any padded tail frames).
                    for f in 0..b.num_frames() {
                        assert!(
                            d[f * 2].abs() < 1e-9,
                            "muted lane must be silent, got {}",
                            d[f * 2]
                        );
                    }
                    // The live lane must show its steady value on at least the
                    // first frame (padding only ever trails, never leads here).
                    assert!(
                        (d[1] - 0.3).abs() < 1e-6,
                        "live lane unaffected by muting the other source, got {}",
                        d[1]
                    );
                    break;
                }
                Ok(None) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "engine produced no buffer"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => panic!("unexpected error: {e:?}"),
            }
        }
    }
    harness.shutdown();
}

// ── Group master gain (rsac-1ce7) ───────────────────────────────────────

/// rsac-1ce7: `Engine::new` seeds every group master gain to identity (1.0),
/// so a composition with no `set_group_gain` call mixes exactly as before —
/// the first composed frame reflects the source gain unchanged (not the leaked
/// `0.0` `GroupStatsShared::default()` bits). Mirrors
/// `build_time_gain_seeds_effective_gain` one level up.
#[test]
fn group_gain_seeds_to_unity() {
    let (a, _) = ScriptedSource::ending(vec![const_buffer(1.0, 1, 48_000, 480)]);
    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(a)],
    );
    let buffers = harness.drain_to_end();
    let v = buffers[0].data()[0];
    assert!(
        (v - 1.0).abs() < 1e-6,
        "seeded group gain 1.0 → 1.0, got {v}"
    );
    harness.shutdown();
}

/// rsac-1ce7: writing the live group-gain atomic mid-stream scales the mixed
/// output on the *next* tick — exactly what `Composition::set_group_gain` does
/// under the hood. A DC source at source gain 1.0 reads ~1.0; after storing a
/// group gain of 0.5, a subsequent buffer reads ~0.5.
#[test]
fn live_set_group_gain_multiplies_on_next_tick() {
    let (a, _) = ScriptedSource::live(
        (0..2000)
            .map(|_| const_buffer(1.0, 1, 48_000, 480))
            .collect(),
    );
    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(a)],
    );

    let before = harness.read_first_sample();
    assert!(
        (before - 1.0).abs() < 1e-6,
        "pre-change buffer reflects seeded group gain 1.0, got {before}"
    );

    // Live group-gain change — identical to what `set_group_gain` performs.
    harness.stats.groups[0]
        .gain_bits
        .store(f32::to_bits(0.5), Ordering::Relaxed);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let v = harness.read_first_sample();
        if (v - 0.5).abs() < 1e-6 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "group-gain change never reached the composed output (last={v})"
        );
    }
    harness.shutdown();
}

/// rsac-1ce7: the group master gain MULTIPLIES the per-source gain (does not
/// replace it). Source gain 0.5 × group gain 0.5 → 0.25. A live source is
/// polled until convergence so the measured buffer definitely postdates the
/// group-gain store (avoiding a race with the engine's first tick).
#[test]
fn group_gain_multiplies_source_gain_not_replaces() {
    let (a, _) = ScriptedSource::live(
        (0..4000)
            .map(|_| const_buffer(1.0, 1, 48_000, 480))
            .collect(),
    );
    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![SourceSpec {
                gain: 0.5,
                group: 0,
                channels: 0,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(a)],
    );

    // Seeded source gain 0.5 × seeded group gain 1.0 → baseline 0.5.
    let before = harness.read_first_sample();
    assert!(
        (before - 0.5).abs() < 1e-6,
        "baseline = source gain 0.5 × group gain 1.0 = 0.5, got {before}"
    );

    // Group gain 0.5 → output converges to 0.5 × 0.5 = 0.25 (multiply).
    harness.stats.groups[0]
        .gain_bits
        .store(f32::to_bits(0.5), Ordering::Relaxed);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let v = harness.read_first_sample();
        if (v - 0.25).abs() < 1e-6 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "source 0.5 × group 0.5 = 0.25 never reached output (last={v})"
        );
    }
    harness.shutdown();
}

/// rsac-1ce7: the group master gain scales EVERY source in that group, and a
/// source in a *different* group is unaffected — proving per-group scoping via
/// `spec.group`.
#[test]
fn group_gain_scales_all_sources_in_group() {
    // Group 0 (mono): two sources, values 0.5 and 0.3 → baseline sum 0.8.
    let (a0, _) = ScriptedSource::live(
        (0..4000)
            .map(|_| const_buffer(0.5, 1, 48_000, 480))
            .collect(),
    );
    let (a1, _) = ScriptedSource::live(
        (0..4000)
            .map(|_| const_buffer(0.3, 1, 48_000, 480))
            .collect(),
    );
    // Group 1 (mono, master): one source at 0.7 — must stay unaffected.
    let (b0, _) = ScriptedSource::live(
        (0..4000)
            .map(|_| const_buffer(0.7, 1, 48_000, 480))
            .collect(),
    );

    let harness = Harness::spawn(
        cfg(
            2,
            vec![
                GroupSpec {
                    layout: GroupLayout::Mono,
                    offset: 0,
                    width: 1,
                },
                GroupSpec {
                    layout: GroupLayout::Mono,
                    offset: 1,
                    width: 1,
                },
            ],
            vec![
                SourceSpec {
                    gain: 1.0,
                    group: 0,
                    channels: 0,
                    clock_candidate: false,
                },
                SourceSpec {
                    gain: 1.0,
                    group: 0,
                    channels: 0,
                    clock_candidate: false,
                },
                SourceSpec {
                    gain: 1.0,
                    group: 1,
                    channels: 0,
                    clock_candidate: false,
                },
            ],
            2,
        ),
        vec![Box::new(a0), Box::new(a1), Box::new(b0)],
    );

    // Baseline: group 0 sums to 0.8, group 1 stays at 0.7.
    let read_frame = |h: &Harness| -> (f32, f32) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match h.stream.try_read_chunk() {
                Ok(Some(b)) => {
                    let d = b.data();
                    return (d[0], d[1]);
                }
                Ok(None) => {
                    assert!(std::time::Instant::now() < deadline, "no buffer in time");
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => panic!("unexpected error: {e:?}"),
            }
        }
    };
    let (g0, g1) = read_frame(&harness);
    assert!((g0 - 0.8).abs() < 1e-6, "group 0 baseline 0.8, got {g0}");
    assert!((g1 - 0.7).abs() < 1e-6, "group 1 baseline 0.7, got {g1}");

    // Scale group 0 by 0.5 → converges to 0.4; group 1 stays 0.7.
    harness.stats.groups[0]
        .gain_bits
        .store(f32::to_bits(0.5), Ordering::Relaxed);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (g0, g1) = read_frame(&harness);
        assert!(
            (g1 - 0.7).abs() < 1e-6,
            "group 1 must be unaffected by group 0's gain, got {g1}"
        );
        if (g0 - 0.4).abs() < 1e-6 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "group 0 never converged to 0.4 (last={g0})"
        );
    }
    harness.shutdown();
}

/// rsac-1ce7: a group gain of 0.0 silences the whole group but leaves each
/// source's `muted`/`gain_bits` untouched — raising the group gain restores the
/// group. Complement (mute-orthogonality): a muted source stays silent even at
/// group gain 2.0 (mute zeroes the source contribution first).
#[test]
fn group_gain_zero_silences_but_preserves_mute_and_gain() {
    let (a, _) = ScriptedSource::live(
        (0..4000)
            .map(|_| const_buffer(0.5, 1, 48_000, 480))
            .collect(),
    );
    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(a)],
    );

    // Baseline nonzero.
    let before = harness.read_first_sample();
    assert!((before - 0.5).abs() < 1e-6, "baseline 0.5, got {before}");

    // Group gain 0.0 → silence.
    harness.stats.groups[0]
        .gain_bits
        .store(f32::to_bits(0.0), Ordering::Relaxed);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let v = harness.read_first_sample();
        if v.abs() < 1e-9 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "group gain 0.0 never silenced the group (last={v})"
        );
    }
    // The source's mute flag and gain are untouched.
    assert!(!harness.stats.sources[0].muted.load(Ordering::Relaxed));
    assert!(
        (f32::from_bits(harness.stats.sources[0].gain_bits.load(Ordering::Relaxed)) - 1.0).abs()
            < 1e-6,
        "source gain must be untouched by group gain 0.0"
    );

    // Raise group gain back to 1.0 → group returns.
    harness.stats.groups[0]
        .gain_bits
        .store(f32::to_bits(1.0), Ordering::Relaxed);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let v = harness.read_first_sample();
        if (v - 0.5).abs() < 1e-6 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "raising the group gain did not restore the group (last={v})"
        );
    }

    // Mute-orthogonality: mute the source, set group gain 2.0 → still silent.
    harness.stats.sources[0]
        .muted
        .store(true, Ordering::Relaxed);
    harness.stats.groups[0]
        .gain_bits
        .store(f32::to_bits(2.0), Ordering::Relaxed);
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let v = harness.read_first_sample();
        if v.abs() < 1e-9 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "a muted source must stay silent regardless of group gain (last={v})"
        );
    }
    harness.shutdown();
}

/// Regression guard for the ended-master pacing flaw: when the configured
/// master source ends while another source stays live, the clock is
/// re-elected to the live source and output continues at **full data rate**
/// (master-paced ticks), not at the wall-clock fallback cadence — so the live
/// source's audio is neither slowed to ~quantum-per-stall_timeout nor
/// trim-discarded.
#[test]
fn ended_master_reelects_live_clock_at_full_rate() {
    let master_buffers = 2usize;
    let live_buffers = 50usize;
    // Configured master: ends almost immediately.
    let (master_src, _) = ScriptedSource::ending(
        (0..master_buffers)
            .map(|_| const_buffer(0.5, 1, 48_000, 480))
            .collect(),
    );
    // Live source: delivers 0.5 s of audio and stays live.
    let (live_src, _) = ScriptedSource::live(
        (0..live_buffers)
            .map(|_| const_buffer(0.25, 1, 48_000, 480))
            .collect(),
    );

    let mut config = cfg(
        2,
        vec![
            GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            },
            GroupSpec {
                layout: GroupLayout::Mono,
                offset: 1,
                width: 1,
            },
        ],
        vec![
            SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: true, // the configured master
            },
            SourceSpec {
                gain: 1.0,
                group: 1,
                channels: 0,
                clock_candidate: true, // preferred re-election target
            },
        ],
        0,
    );
    // A long stall timeout proves progress comes from re-election, not from
    // the wall-clock fallback (which would need 10 s per emitted quantum).
    config.stall_timeout = Duration::from_secs(10);

    let harness = Harness::spawn(config, vec![Box::new(master_src), Box::new(live_src)]);

    // All 50 quanta must be emitted promptly, paced by the re-elected master.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if harness.stats.ticks.load(Ordering::Relaxed) >= live_buffers as u64 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "output stalled after the master ended (ticks={}) — clock was not re-elected",
            harness.stats.ticks.load(Ordering::Relaxed)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        harness.stats.fallback_ticks.load(Ordering::Relaxed),
        0,
        "progress must come from data-paced ticks, not the wall-clock fallback"
    );
    assert_eq!(
        harness.stats.sources[1]
            .trimmed_frames
            .load(Ordering::Relaxed),
        0,
        "the live source must not be trim-discarded after the master ends"
    );
    harness.shutdown();
}

// ── Engine panic containment (rsac-1b83) ────────────────────────────────

/// A panic anywhere in the engine loop must surface as a **fatal terminal**
/// on the composed stream — never a hang — and `is_running()` must go false.
/// The engine's catch-unwind teardown poisons the ring via `signal_error`;
/// without it the ring stays `Running` forever: blocking reads loop on
/// Timeout, pumps spin, and the composition is permanently non-terminal.
#[test]
fn engine_panic_poisons_stream_with_fatal_terminal() {
    /// Delivers a few buffers, then panics inside `try_read` — i.e. inside
    /// the engine's ingest path on the compositor thread. (The panic prints
    /// to stderr via the default hook; that noise is expected here.)
    struct PanickingSource {
        reads_left: usize,
    }
    impl SourceReader for PanickingSource {
        fn try_read(&mut self) -> AudioResult<Option<AudioBuffer>> {
            if self.reads_left == 0 {
                panic!("scripted engine panic (rsac-1b83 test)");
            }
            self.reads_left -= 1;
            Ok(Some(const_buffer(0.5, 1, 48_000, 480)))
        }
        fn stop(&mut self) {}
    }

    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(PanickingSource { reads_left: 3 })],
    );

    // Timeout-bounded: a regression (permanently non-terminal composition)
    // fails the deadline assert instead of hanging the suite.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let fatal = loop {
        match harness.stream.try_read_chunk() {
            // Pre-panic ticks may or may not surface before the poison ends
            // readability; either way, keep polling until the terminal.
            Ok(_) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "engine panic never surfaced a terminal error (composition hung)"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => break e,
        }
    };
    assert!(
        fatal.is_fatal(),
        "the panic terminal must be fatal, got {fatal:?}"
    );

    // The blocking read path returns the same fatal terminal immediately —
    // the state is already terminal, so this cannot park.
    let blocking = harness.stream.read_chunk();
    assert!(
        matches!(blocking, Err(ref e) if e.is_fatal()),
        "read_chunk after an engine panic must be the fatal terminal, got {blocking:?}"
    );

    assert!(
        !harness.stream.is_running(),
        "is_running() must report the engine's death, not lie"
    );
    harness.shutdown();
}

// ── Intra-source gap compensation (rsac-ae4e) ───────────────────────────

/// A timestamped source whose stamps jump past the expected next position
/// (how an inner ring overflow manifests) has the hole re-inserted as
/// silence: output stays time-continuous and `gap_padded_frames` counts it.
#[test]
fn timestamp_gap_is_compensated_with_silence() {
    // Buffer 0 spans 0..10 ms; buffer 1 starts at 20 ms → a 10 ms hole
    // (480 frames @ 48 kHz) where the inner ring dropped data.
    let (src, _) = ScriptedSource::ending(vec![
        stamped_buffer(0.5, 1, 48_000, 480, Duration::ZERO),
        stamped_buffer(0.5, 1, 48_000, 480, Duration::from_millis(20)),
    ]);
    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(src)],
    );

    let buffers = harness.drain_to_end();
    let all: Vec<f32> = buffers
        .iter()
        .flat_map(|b| b.data().iter().copied())
        .collect();
    assert_eq!(
        all.len(),
        3 * 480,
        "output must span the full 30 ms timeline (data + gap + data)"
    );
    assert!(
        all[..480].iter().all(|v| (v - 0.5).abs() < 1e-6),
        "first 10 ms carries buffer 0"
    );
    assert!(
        all[480..960].iter().all(|v| v.abs() < 1e-9),
        "the 10 ms hole must be silence, not time-compressed away"
    );
    assert!(
        all[960..].iter().all(|v| (v - 0.5).abs() < 1e-6),
        "last 10 ms carries buffer 1"
    );
    assert_eq!(
        harness.stats.sources[0]
            .gap_padded_frames
            .load(Ordering::Relaxed),
        480,
        "gap_padded_frames must count the inserted silence"
    );
    harness.shutdown();
}

/// A source delivering buffers WITHOUT timestamps keeps the exact legacy
/// behavior: no gap detection, no inserted silence, counter stays zero.
#[test]
fn untimestamped_source_gets_no_gap_compensation() {
    let n = 4usize;
    let (src, _) =
        ScriptedSource::ending((0..n).map(|_| const_buffer(0.5, 1, 48_000, 480)).collect());
    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(src)],
    );
    let buffers = harness.drain_to_end();
    let total_frames: usize = buffers.iter().map(|b| b.num_frames()).sum();
    assert_eq!(total_frames, n * 480, "delivered frames only, no padding");
    assert_eq!(
        harness.stats.sources[0]
            .gap_padded_frames
            .load(Ordering::Relaxed),
        0,
        "no timestamps → no gap compensation"
    );
    harness.shutdown();
}

/// rsac-ae4e(3): the engine snapshots each source's inner overrun count
/// (`SourceReader::overruns`) into the shared stats.
#[test]
fn inner_overruns_are_snapshotted_into_stats() {
    struct OverrunningSource {
        inner: ScriptedSource,
    }
    impl SourceReader for OverrunningSource {
        fn try_read(&mut self) -> AudioResult<Option<AudioBuffer>> {
            self.inner.try_read()
        }
        fn stop(&mut self) {
            self.inner.stop();
        }
        fn overruns(&self) -> u64 {
            7
        }
    }

    let (inner, _) = ScriptedSource::ending(vec![const_buffer(0.5, 1, 48_000, 480)]);
    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(OverrunningSource { inner })],
    );
    let _ = harness.drain_to_end();
    assert_eq!(
        harness.stats.sources[0]
            .inner_dropped
            .load(Ordering::Relaxed),
        7,
        "inner overrun count must be snapshotted into the per-source stats"
    );
    harness.shutdown();
}

// ── Ragged-buffer truncation (rsac-2195) ────────────────────────────────

/// A delivered buffer with a dangling partial frame must be truncated to
/// whole frames on ingest — otherwise the half-frame rotates the source's
/// channel interleave (every later L sample lands in R) for the rest of the
/// session.
#[test]
fn ragged_buffer_is_truncated_and_interleave_survives() {
    // Buffer 0: 480 clean stereo frames of (0.1, 0.2) + ONE dangling sample.
    let mut ragged_data = Vec::with_capacity(480 * 2 + 1);
    for _ in 0..480 {
        ragged_data.extend_from_slice(&[0.1, 0.2]);
    }
    ragged_data.push(0.1); // the dangling half frame
    let ragged = AudioBuffer::new(ragged_data, 2, 48_000);
    // Buffer 1: 480 clean stereo frames with DIFFERENT per-channel values, so
    // any interleave rotation is unmistakable.
    let clean = {
        let mut data = Vec::with_capacity(480 * 2);
        for _ in 0..480 {
            data.extend_from_slice(&[0.3, 0.4]);
        }
        AudioBuffer::new(data, 2, 48_000)
    };
    let (src, _) = ScriptedSource::ending(vec![ragged, clean]);

    let harness = Harness::spawn(
        cfg(
            2,
            vec![GroupSpec {
                layout: GroupLayout::KeepChannels,
                offset: 0,
                width: 2,
            }],
            vec![SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 2,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(src)],
    );

    let buffers = harness.drain_to_end();
    let all: Vec<f32> = buffers
        .iter()
        .flat_map(|b| b.data().iter().copied())
        .collect();
    assert_eq!(
        all.len(),
        960 * 2,
        "dangling half-frame dropped: exactly 960 whole frames flow through"
    );
    for f in 0..960 {
        let (l, r) = (all[f * 2], all[f * 2 + 1]);
        assert!(
            (l - 0.1).abs() < 1e-6 || (l - 0.3).abs() < 1e-6,
            "L slot polluted at frame {f}: {l} (interleave rotated)"
        );
        assert!(
            (r - 0.2).abs() < 1e-6 || (r - 0.4).abs() < 1e-6,
            "R slot polluted at frame {f}: {r} (interleave rotated)"
        );
    }
    harness.shutdown();
}

// ── Push + async delivery over the composed view ────────────────────────

/// The shared subscribe pump delivers composed buffers and then the fatal
/// terminal as the FINAL item before the channel disconnects (same contract
/// as `AudioCapture::subscribe_with_errors`, exercised over the composed
/// view + drain-complete promotion).
#[test]
fn subscribe_with_errors_delivers_buffers_then_terminal() {
    let n = 5usize;
    let (src, _) =
        ScriptedSource::ending((0..n).map(|_| const_buffer(0.5, 1, 48_000, 480)).collect());
    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(src)],
    );

    let rx = crate::api::spawn_subscribe_with_errors_thread(
        harness.stream.clone(),
        Arc::new(AtomicU64::new(0)),
    )
    .expect("subscribe pump spawns");

    let mut frames = 0usize;
    let mut saw_terminal = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(buffer)) => frames += buffer.num_frames(),
            Ok(Err(e)) => {
                assert!(
                    e.is_fatal(),
                    "final delivered error must be fatal, got {e:?}"
                );
                saw_terminal = true;
                break;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(saw_terminal, "terminal error must arrive as the final item");
    assert_eq!(frames, n * 480, "all composed frames delivered via push");
    // After the terminal item the channel disconnects.
    assert!(matches!(
        rx.recv_timeout(Duration::from_secs(1)),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
    ));
    harness.shutdown();
}

/// rsac-7aa2(1): `Composition::subscribe{,_with_errors}` must accept the
/// drainable `Stopping` window. A composition whose scripted sources end
/// immediately leaves the engine exited and the composed ring parked in
/// `Stopping` with the entire output still buffered; subscribing only AFTER
/// the engine finished (the old `is_running()` gate rejected exactly this
/// call, stranding the buffered output) must still deliver every composed
/// frame followed by the clean terminal.
#[test]
fn subscribe_after_engine_finished_drains_buffered_tail() {
    let n = 5usize;
    let (src, _) =
        ScriptedSource::ending((0..n).map(|_| const_buffer(0.5, 1, 48_000, 480)).collect());
    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(src)],
    );

    // Wait until the engine has finished: its signal_done parks the ring in
    // the drainable Stopping state, so is_running() goes false with the whole
    // composed output still buffered (nobody has read yet).
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while harness.stream.is_running() {
        assert!(
            std::time::Instant::now() < deadline,
            "engine never finished"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    // Route through the real public handle so the PUBLIC gate is what's under
    // test: a device-free composition with the harness view attached.
    let mut composition = CompositionBuilder::new()
        .group(
            Group::new("g")
                .source(crate::core::config::CaptureTarget::SystemDefault)
                .mixdown(GroupLayout::Mono),
        )
        .build()
        .expect("device-free build");
    composition.attach_stream_for_tests(Arc::clone(&harness.stream));
    assert!(
        !composition.is_running(),
        "precondition: the engine already finished (Stopping window)"
    );

    let rx = composition
        .subscribe_with_errors()
        .expect("subscribe in the Stopping window must be accepted");

    let mut frames = 0usize;
    let mut saw_terminal = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(buffer)) => frames += buffer.num_frames(),
            Ok(Err(e)) => {
                assert!(
                    e.is_fatal(),
                    "the final error must be the clean terminal, got {e:?}"
                );
                saw_terminal = true;
                break;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert_eq!(
        frames,
        n * 480,
        "the entire buffered output must be drained by the late subscription"
    );
    assert!(saw_terminal, "clean end delivered after the drained tail");
    assert_eq!(
        composition.subscriber_dropped_count(),
        0,
        "an attentive subscriber loses nothing"
    );
    harness.shutdown();
}

/// After the composition ends and the ring drains, the async stream yields a
/// clean `Ready(None)` (end-of-stream), not a hang and not an error.
#[cfg(feature = "async-stream")]
#[test]
fn async_stream_ends_cleanly_after_composition_end() {
    use futures_core::Stream as _;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    let (src, _) = ScriptedSource::ending(vec![const_buffer(0.25, 1, 48_000, 480)]);
    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(src)],
    );

    let mut stream = crate::bridge::AsyncAudioStream::new(&*harness.stream);
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);

    let mut yielded_frames = 0usize;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "async stream never reached end-of-stream"
        );
        match Pin::new(&mut stream).poll_next(&mut cx) {
            Poll::Ready(Some(Ok(buffer))) => yielded_frames += buffer.num_frames(),
            Poll::Ready(Some(Err(e))) => panic!("unexpected stream error: {e:?}"),
            Poll::Ready(None) => break, // clean end-of-stream
            // Noop waker never fires; the engine is asynchronous to this
            // poll loop, so just re-poll after a short sleep.
            Poll::Pending => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    assert_eq!(
        yielded_frames, 480,
        "the composed buffer flowed through async"
    );
    harness.shutdown();
}

// ── Not-started Composition handle behavior (device-free) ──────────────

#[test]
fn unstarted_composition_reads_error_and_reports_honestly() {
    let composition = CompositionBuilder::new()
        .group(
            Group::new("main")
                .source(crate::core::config::CaptureTarget::SystemDefault)
                .mixdown(GroupLayout::Stereo),
        )
        .build()
        .expect("build touches no devices");

    assert!(!composition.is_running());
    assert!(composition.channel_map().is_none());
    assert!(composition.stats().is_none());
    // rsac-90b1: the read error classifies structurally as NotInitialized —
    // "no stream exists yet" — via lifecycle_stage(), not just by variant.
    let read_err = composition
        .read_chunk_nonblocking()
        .expect_err("an unstarted composition read must error");
    assert!(matches!(read_err, AudioError::StreamReadError { .. }));
    assert_eq!(
        read_err.lifecycle_stage(),
        Some(LifecycleStage::NotInitialized)
    );
    // Push + async delivery modes reject a not-started composition uniformly.
    assert!(matches!(
        composition.subscribe(),
        Err(AudioError::StreamReadError { .. })
    ));
    assert!(matches!(
        composition.subscribe_with_errors(),
        Err(AudioError::StreamReadError { .. })
    ));
    #[cfg(feature = "async-stream")]
    assert!(matches!(
        composition.audio_data_stream(),
        Err(AudioError::StreamReadError { .. })
    ));
    // Both stop paths tolerate the not-started state identically (no Err).
    assert!(CapturingStream::stop(&composition).is_ok());
    // CapturingStream::format falls back to the provisional (2ch stereo grp).
    let f = CapturingStream::format(&composition);
    assert_eq!(f.sample_rate, 48_000);
    assert_eq!(f.channels, 2);
}

// ── Live gain/mute public-handle control (device-free) ─────────────────
// (rsac-5a2d) These build a device-free Composition and attach an external
// shared stats block via `attach_stats_for_tests`, so the PUBLIC setter/getter
// path (`set_gain`/`set_muted`/`gain`/`is_muted`) is under test end-to-end
// without a device-backed engine. The composition has two groups —
// "a" (2 sources) and "b" (1 source) — to exercise group-name + within-group
// index resolution.

/// Builds a device-free composition (groups "a": 2 srcs, "b": 1 src) with a
/// 3-source shared stats block attached, all seeded to gain 1.0 / unmuted (the
/// `Engine::new` seeding is not exercised here; the setters write directly).
fn control_composition() -> super::Composition {
    let mut composition = CompositionBuilder::new()
        .group(
            Group::new("a")
                .source(crate::core::config::CaptureTarget::SystemDefault)
                .source(crate::core::config::CaptureTarget::SystemDefault)
                .mixdown(GroupLayout::Mono),
        )
        .group(
            Group::new("b")
                .source(crate::core::config::CaptureTarget::SystemDefault)
                .mixdown(GroupLayout::Mono),
        )
        .build()
        .expect("device-free build");
    let stats = Arc::new(EngineStatsShared {
        sources: (0..3)
            .map(|_| {
                let s = SourceStatsShared::default();
                s.gain_bits.store(f32::to_bits(1.0), Ordering::Relaxed);
                Arc::new(s)
            })
            .collect(),
        groups: (0..2)
            .map(|_| {
                let g = GroupStatsShared::default();
                g.gain_bits.store(f32::to_bits(1.0), Ordering::Relaxed);
                Arc::new(g)
            })
            .collect(),
        ..Default::default()
    });
    composition.attach_stats_for_tests(stats);
    composition
}

#[test]
fn set_gain_rejects_invalid_value() {
    let c = control_composition();
    for bad in [-0.5f32, f32::NAN, f32::INFINITY] {
        assert!(
            matches!(
                c.set_gain("a", 0, bad),
                Err(AudioError::ConfigurationError { .. })
            ),
            "gain {bad} must be rejected"
        );
    }
    // A valid value still succeeds (guard against a blanket-reject bug).
    assert!(c.set_gain("a", 0, 0.0).is_ok());
    assert!(c.set_gain("a", 0, 2.0).is_ok());
}

#[test]
fn set_gain_rejects_unknown_group_and_oob_index() {
    let c = control_composition();
    assert!(matches!(
        c.set_gain("nope", 0, 1.0),
        Err(AudioError::ConfigurationError { .. })
    ));
    // Group "a" has 2 sources → index 2 is out of range.
    assert!(matches!(
        c.set_gain("a", 2, 1.0),
        Err(AudioError::ConfigurationError { .. })
    ));
    // Group "b" has 1 source → index 1 is out of range.
    assert!(matches!(
        c.set_gain("b", 1, 1.0),
        Err(AudioError::ConfigurationError { .. })
    ));
    // Same bounds errors on set_muted / getters.
    assert!(matches!(
        c.set_muted("nope", 0, true),
        Err(AudioError::ConfigurationError { .. })
    ));
    assert!(matches!(
        c.gain("a", 2),
        Err(AudioError::ConfigurationError { .. })
    ));
    assert!(matches!(
        c.is_muted("nope", 0),
        Err(AudioError::ConfigurationError { .. })
    ));
}

#[test]
fn set_gain_and_get_gain_roundtrip() {
    let c = control_composition();
    // Address the second source of group "a" (flat index 1) and the sole
    // source of group "b" (flat index 2) to prove resolution walks groups.
    c.set_gain("a", 1, 0.25).expect("valid set_gain");
    c.set_gain("b", 0, 0.75).expect("valid set_gain");
    assert!((c.gain("a", 1).unwrap() - 0.25).abs() < 1e-6);
    assert!((c.gain("b", 0).unwrap() - 0.75).abs() < 1e-6);

    // stats() reflects the same values in flat declaration order.
    let stats = c.stats().expect("stats present");
    assert!((stats.sources[1].gain - 0.25).abs() < 1e-6);
    assert!((stats.sources[2].gain - 0.75).abs() < 1e-6);
    assert!(!stats.sources[1].muted);

    // Mute roundtrip.
    assert!(!c.is_muted("a", 1).unwrap());
    c.set_muted("a", 1, true).expect("valid set_muted");
    assert!(c.is_muted("a", 1).unwrap());
    assert!(c.stats().unwrap().sources[1].muted);
    // Muting did not disturb the stored gain.
    assert!((c.gain("a", 1).unwrap() - 0.25).abs() < 1e-6);
    c.set_muted("a", 1, false).expect("unmute");
    assert!(!c.is_muted("a", 1).unwrap());
}

/// rsac-1ce7: `set_group_gain` then `group_gain` roundtrips; `stats().groups`
/// reflects the same value in declaration order; a sibling group stays 1.0.
#[test]
fn set_group_gain_and_group_gain_roundtrip() {
    let c = control_composition();
    c.set_group_gain("a", 0.5).expect("valid set_group_gain");
    assert!((c.group_gain("a").unwrap() - 0.5).abs() < 1e-6);
    // Sibling group "b" is untouched (seeded 1.0).
    assert!((c.group_gain("b").unwrap() - 1.0).abs() < 1e-6);

    // stats().groups reflects the same values in declaration order.
    let stats = c.stats().expect("stats present");
    assert_eq!(stats.groups.len(), 2);
    assert_eq!(stats.groups[0].group, "a");
    assert_eq!(stats.groups[1].group, "b");
    assert!((stats.groups[0].gain - 0.5).abs() < 1e-6);
    assert!((stats.groups[1].gain - 1.0).abs() < 1e-6);
}

/// rsac-1ce7: `set_group_gain` rejects a non-finite/negative gain with
/// `ConfigurationError`; a valid 0.0/2.0 still succeeds.
#[test]
fn set_group_gain_rejects_invalid_value() {
    let c = control_composition();
    for bad in [-0.5f32, f32::NAN, f32::INFINITY] {
        assert!(
            matches!(
                c.set_group_gain("a", bad),
                Err(AudioError::ConfigurationError { .. })
            ),
            "group gain {bad} must be rejected"
        );
    }
    assert!(c.set_group_gain("a", 0.0).is_ok());
    assert!(c.set_group_gain("a", 2.0).is_ok());
}

/// rsac-1ce7: `set_group_gain`/`group_gain` reject an unknown group name with
/// `ConfigurationError`.
#[test]
fn set_group_gain_rejects_unknown_group() {
    let c = control_composition();
    assert!(matches!(
        c.set_group_gain("nope", 1.0),
        Err(AudioError::ConfigurationError { .. })
    ));
    assert!(matches!(
        c.group_gain("nope"),
        Err(AudioError::ConfigurationError { .. })
    ));
}

/// rsac-1ce7: on a not-started composition (no stats attached),
/// `set_group_gain`/`group_gain` error with `StreamReadError` classified
/// `NotInitialized`.
#[test]
fn group_gain_on_unstarted_is_not_initialized() {
    let c = CompositionBuilder::new()
        .group(
            Group::new("a")
                .source(crate::core::config::CaptureTarget::SystemDefault)
                .mixdown(GroupLayout::Mono),
        )
        .build()
        .expect("device-free build");

    for err in [
        c.set_group_gain("a", 0.5)
            .expect_err("set_group_gain not started"),
        c.group_gain("a").expect_err("group_gain not started"),
    ] {
        assert!(matches!(err, AudioError::StreamReadError { .. }));
        assert_eq!(err.lifecycle_stage(), Some(LifecycleStage::NotInitialized));
    }
}

#[test]
fn control_on_unstarted_composition_is_not_initialized() {
    // No stats attached → not started.
    let c = CompositionBuilder::new()
        .group(
            Group::new("a")
                .source(crate::core::config::CaptureTarget::SystemDefault)
                .mixdown(GroupLayout::Mono),
        )
        .build()
        .expect("device-free build");

    for err in [
        c.set_gain("a", 0, 0.5).expect_err("set_gain not started"),
        c.set_muted("a", 0, true)
            .expect_err("set_muted not started"),
        c.gain("a", 0).expect_err("gain not started"),
        c.is_muted("a", 0).expect_err("is_muted not started"),
    ] {
        assert!(matches!(err, AudioError::StreamReadError { .. }));
        assert_eq!(err.lifecycle_stage(), Some(LifecycleStage::NotInitialized));
    }
}

/// After the composition stops/ends, no compositor tick will ever apply a
/// mutation — the setters must refuse (NotRunning) instead of reporting a
/// success that never takes effect; the getters keep reading the last-applied
/// values (rsac-5a2d review, PR #62).
#[test]
fn live_controls_refuse_after_composition_ends() {
    let (src, _) = ScriptedSource::ending(vec![const_buffer(0.5, 1, 48_000, 480)]);
    let harness = Harness::spawn(
        cfg(
            1,
            vec![GroupSpec {
                layout: GroupLayout::Mono,
                offset: 0,
                width: 1,
            }],
            vec![SourceSpec {
                gain: 1.0,
                group: 0,
                channels: 0,
                clock_candidate: false,
            }],
            0,
        ),
        vec![Box::new(src)],
    );
    // Wait for the engine to finish (ring parked in Stopping, is_running false).
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while harness.stream.is_running() {
        assert!(
            std::time::Instant::now() < deadline,
            "engine never finished"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    let mut composition = CompositionBuilder::new()
        .group(
            Group::new("g")
                .source(crate::core::config::CaptureTarget::SystemDefault)
                .mixdown(GroupLayout::Mono),
        )
        .build()
        .expect("device-free build");
    composition.attach_stream_for_tests(Arc::clone(&harness.stream));
    composition.attach_stats_for_tests(Arc::clone(&harness.stats));
    assert!(!composition.is_running(), "precondition: engine finished");

    for err in [
        composition
            .set_gain("g", 0, 0.5)
            .expect_err("set_gain after end"),
        composition
            .set_muted("g", 0, true)
            .expect_err("set_muted after end"),
        // rsac-1ce7: the group-gain setter refuses after the composition ends,
        // same as the per-source setters.
        composition
            .set_group_gain("g", 0.5)
            .expect_err("set_group_gain after end"),
    ] {
        assert!(matches!(err, AudioError::StreamReadError { .. }));
        assert_eq!(err.lifecycle_stage(), Some(LifecycleStage::NotRunning));
    }
    // Getters still read the last-applied values of the ended composition.
    assert!((composition.gain("g", 0).unwrap() - 1.0).abs() < 1e-6);
    assert!(!composition.is_muted("g", 0).unwrap());
    // rsac-1ce7: the group-gain getter keeps reading the last value (seeded 1.0).
    assert!((composition.group_gain("g").unwrap() - 1.0).abs() < 1e-6);
}
