//! Engine-loop tests: drive the full compositor thread with scripted sources
//! and read the composed output through a real `BridgeStream` — the same data
//! plane a device-backed composition uses, with zero hardware dependency.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::core::buffer::AudioBuffer;
use crate::core::config::{AudioFormat, SampleFormat};
use crate::core::error::{AudioError, AudioResult};
use crate::core::interface::CapturingStream;

use super::builder::{CompositionBuilder, Group, GroupLayout};
use super::engine::{
    Engine, EngineConfig, EngineStatsShared, GroupSpec, SourceReader, SourceSpec, SourceStatsShared,
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
    assert!(matches!(
        composition.read_chunk_nonblocking(),
        Err(AudioError::StreamReadError { .. })
    ));
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
