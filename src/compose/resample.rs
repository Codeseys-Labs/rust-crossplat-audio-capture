//! Streaming per-source resampler: wraps `rubato`'s synchronous FFT resampler
//! to convert a source's delivered rate to the composition session rate.
//!
//! Runs exclusively on the compositor thread (non-RT — allocation is fine
//! here; ADR-0001 concerns the OS callback threads, which this code never
//! touches). Input and output are interleaved f32, adapted for rubato via
//! `audioadapter`'s [`InterleavedSlice`].

use std::collections::VecDeque;

use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};

use crate::core::error::{AudioError, AudioResult};

/// Fixed input chunk size (frames) fed to the FFT resampler. A middle-ground
/// per rubato's guidance ("a few hundred to a few thousand frames"); the
/// wrapper buffers arbitrary-size pushes into chunks of this size.
const CHUNK_FRAMES: usize = 1024;

/// Streaming rate converter for one source (fixed channel count).
pub(crate) struct StreamResampler {
    inner: Fft<f32>,
    channels: usize,
    /// Input (delivered) rate in Hz — with `to_rate`, drives the exact
    /// output-length accounting in [`flush`](Self::flush).
    from_rate: u32,
    /// Output (session) rate in Hz.
    to_rate: u32,
    /// Interleaved input samples not yet consumed by the resampler.
    pending: Vec<f32>,
    /// Interleaved output scratch, sized for the resampler's max output chunk.
    out_scratch: Vec<f32>,
    /// Leading output frames still to swallow (the resampler's algorithmic
    /// delay is silence we trim so composition alignment isn't skewed by it).
    delay_frames_remaining: usize,
    /// Cumulative input frames accepted by [`push`](Self::push) (including
    /// any still buffered in `pending`). The stream owes exactly
    /// `round(frames_in * to/from)` output frames in total — the target
    /// [`flush`](Self::flush) trims to (rsac-fab0).
    frames_in: u64,
    /// Cumulative output frames emitted (after the algorithmic-delay skip).
    frames_out: u64,
    /// Set once [`flush`](Self::flush) has run; a flushed resampler emits
    /// nothing further (flush is idempotent).
    flushed: bool,
}

impl std::fmt::Debug for StreamResampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamResampler")
            .field("channels", &self.channels)
            .field("pending_samples", &self.pending.len())
            .field("delay_frames_remaining", &self.delay_frames_remaining)
            .field("frames_in", &self.frames_in)
            .field("frames_out", &self.frames_out)
            .field("flushed", &self.flushed)
            .finish()
    }
}

impl StreamResampler {
    /// Creates a resampler converting `from_rate` → `to_rate` for `channels`
    /// interleaved channels.
    pub fn new(from_rate: u32, to_rate: u32, channels: u16) -> AudioResult<Self> {
        let channels = usize::from(channels.max(1));
        let inner = Fft::<f32>::new(
            from_rate as usize,
            to_rate as usize,
            CHUNK_FRAMES,
            2,
            channels,
            FixedSync::Input,
        )
        .map_err(|e| AudioError::ConfigurationError {
            message: format!(
                "Failed to create resampler {from_rate} Hz -> {to_rate} Hz ({channels} ch): {e}"
            ),
        })?;
        let max_out_frames = inner.output_frames_max();
        let delay = inner.output_delay();
        Ok(Self {
            inner,
            channels,
            from_rate: from_rate.max(1),
            to_rate: to_rate.max(1),
            pending: Vec::with_capacity(CHUNK_FRAMES * channels * 2),
            out_scratch: vec![0.0; max_out_frames.max(1) * channels],
            delay_frames_remaining: delay,
            frames_in: 0,
            frames_out: 0,
            flushed: false,
        })
    }

    /// Feeds interleaved input samples and appends resampled interleaved
    /// output samples to `out`. Input that doesn't yet fill a full resampler
    /// chunk is buffered internally for the next call.
    pub fn push(&mut self, interleaved: &[f32], out: &mut VecDeque<f32>) -> AudioResult<()> {
        self.frames_in += (interleaved.len() / self.channels) as u64;
        self.pending.extend_from_slice(interleaved);

        loop {
            let need = self.inner.input_frames_next();
            let have = self.pending.len() / self.channels;
            if have < need {
                return Ok(());
            }

            let input = InterleavedSlice::new(self.pending.as_slice(), self.channels, have)
                .map_err(|e| AudioError::InternalError {
                    message: format!("resampler input adapter: {e}"),
                    source: None,
                })?;
            let out_capacity = self.out_scratch.len() / self.channels;
            let mut output = InterleavedSlice::new_mut(
                self.out_scratch.as_mut_slice(),
                self.channels,
                out_capacity,
            )
            .map_err(|e| AudioError::InternalError {
                message: format!("resampler output adapter: {e}"),
                source: None,
            })?;
            let indexing = Indexing {
                input_offset: 0,
                output_offset: 0,
                active_channels_mask: None,
                partial_len: None,
            };

            let (consumed_frames, produced_frames) = self
                .inner
                .process_into_buffer(&input, &mut output, Some(&indexing))
                .map_err(|e| AudioError::InternalError {
                    message: format!("resampler process failed: {e}"),
                    source: None,
                })?;

            // Swallow the algorithmic-delay prefix, then forward the rest.
            let skip = self.delay_frames_remaining.min(produced_frames);
            self.delay_frames_remaining -= skip;
            out.extend(&self.out_scratch[skip * self.channels..produced_frames * self.channels]);
            self.frames_out += (produced_frames - skip) as u64;

            self.pending.drain(..consumed_frames * self.channels);
        }
    }

    /// Drains everything the resampler still owes at end-of-stream
    /// (rsac-fab0): the final partial input chunk buffered in `pending` plus
    /// the FFT algorithmic-delay residue that [`push`](Self::push) swallows
    /// at the front.
    ///
    /// [`push`](Self::push) only processes full `CHUNK_FRAMES` input chunks,
    /// so at a source's natural end up to `CHUNK_FRAMES − 1` input frames sit
    /// unprocessed in `pending`, and `output_delay()` frames of real audio
    /// are still inside the FFT pipeline (the whole signal is time-shifted
    /// behind the delay prefix trimmed off the front). Dropping both loses
    /// ~25–45 ms per resampled source and violates the engine's flush-tail
    /// contract ("no captured audio is discarded").
    ///
    /// Mechanism — mirrors rubato's own `process_all_into_buffer` tail
    /// handling:
    ///
    /// 1. Process `pending` as a partial chunk via rubato v3's
    ///    `Indexing { partial_len: Some(valid_frames), .. }` (rubato
    ///    zero-pads the rest of the chunk internally).
    /// 2. Pump all-zero chunks (`partial_len: Some(0)`) to expel the
    ///    remaining delay residue.
    /// 3. Trim so cumulative output == `round(frames_in * to/from)` — exact
    ///    by construction from the `frames_in`/`frames_out` counters; the
    ///    surplus beyond that is resampler-internal zero padding, not
    ///    captured audio.
    ///
    /// Appends the flushed interleaved samples to `out` and returns the
    /// number of *frames* appended. Idempotent: a second call emits nothing
    /// and returns 0.
    pub fn flush(&mut self, out: &mut VecDeque<f32>) -> AudioResult<usize> {
        if self.flushed {
            return Ok(0);
        }
        self.flushed = true;

        // Total output the input stream owes: round(frames_in * to/from).
        let expected_total = ((self.frames_in as u128 * u128::from(self.to_rate)
            + u128::from(self.from_rate) / 2)
            / u128::from(self.from_rate)) as u64;
        let start = self.frames_out;

        // Safety bound on the pump loop. The shortfall is at most
        // `output_delay()` plus one output chunk, and every pass produces on
        // the order of `CHUNK_FRAMES * to/from` frames, so a handful of
        // passes always suffices; 64 is far beyond any real delay and turns
        // a logic regression into a truncated-tail warning instead of an
        // infinite loop on the compositor thread.
        const MAX_FLUSH_PASSES: usize = 64;

        let mut first = true;
        let mut passes = 0usize;
        while self.frames_out < expected_total {
            if passes >= MAX_FLUSH_PASSES {
                log::warn!(
                    "resampler flush did not converge after {MAX_FLUSH_PASSES} passes \
                     ({} of {} frames emitted); tail truncated",
                    self.frames_out,
                    expected_total
                );
                break;
            }
            passes += 1;

            // First pass processes the buffered partial chunk; later passes
            // feed pure zero-padding (partial_len == 0) to expel the delay.
            let partial_frames = if first {
                self.pending.len() / self.channels
            } else {
                0
            };
            first = false;

            let input = InterleavedSlice::new(
                &self.pending[..partial_frames * self.channels],
                self.channels,
                partial_frames,
            )
            .map_err(|e| AudioError::InternalError {
                message: format!("resampler flush input adapter: {e}"),
                source: None,
            })?;
            let out_capacity = self.out_scratch.len() / self.channels;
            let mut output = InterleavedSlice::new_mut(
                self.out_scratch.as_mut_slice(),
                self.channels,
                out_capacity,
            )
            .map_err(|e| AudioError::InternalError {
                message: format!("resampler flush output adapter: {e}"),
                source: None,
            })?;
            let indexing = Indexing {
                input_offset: 0,
                output_offset: 0,
                active_channels_mask: None,
                partial_len: Some(partial_frames),
            };

            let (_consumed_frames, produced_frames) = self
                .inner
                .process_into_buffer(&input, &mut output, Some(&indexing))
                .map_err(|e| AudioError::InternalError {
                    message: format!("resampler flush process failed: {e}"),
                    source: None,
                })?;

            // Same delay-skip as `push`, plus the end-trim: never emit past
            // what the input actually owes.
            let skip = self.delay_frames_remaining.min(produced_frames);
            self.delay_frames_remaining -= skip;
            let mut emit = produced_frames - skip;
            let owed = (expected_total - self.frames_out) as usize;
            if emit > owed {
                emit = owed;
            }
            out.extend(&self.out_scratch[skip * self.channels..(skip + emit) * self.channels]);
            self.frames_out += emit as u64;
        }
        self.pending.clear();
        Ok((self.frames_out - start) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a sine at `freq` Hz through the resampler and verify the output
    /// still contains that tone (Goertzel power dominance) at the new rate.
    fn goertzel_power(samples: &[f32], sample_rate: f32, freq: f32) -> f32 {
        let n = samples.len() as f32;
        let k = (0.5 + n * freq / sample_rate).floor();
        let w = 2.0 * std::f32::consts::PI * k / n;
        let coeff = 2.0 * w.cos();
        let (mut s_prev, mut s_prev2) = (0.0f32, 0.0f32);
        for &x in samples {
            let s = x + coeff * s_prev - s_prev2;
            s_prev2 = s_prev;
            s_prev = s;
        }
        s_prev2 * s_prev2 + s_prev * s_prev - coeff * s_prev * s_prev2
    }

    fn sine_interleaved(freq: f32, rate: u32, channels: usize, frames: usize) -> Vec<f32> {
        let mut data = Vec::with_capacity(frames * channels);
        for f in 0..frames {
            let t = f as f32 / rate as f32;
            let v = (2.0 * std::f32::consts::PI * freq * t).sin() * 0.5;
            for _ in 0..channels {
                data.push(v);
            }
        }
        data
    }

    fn run_case(from: u32, to: u32) {
        let channels = 2usize;
        let freq = 440.0f32;
        let input_frames = from as usize; // 1 second of input
        let input = sine_interleaved(freq, from, channels, input_frames);

        let mut rs = StreamResampler::new(from, to, channels as u16).expect("create");
        let mut out = VecDeque::new();
        // Push in awkward, uneven chunks to exercise the internal buffering.
        for chunk in input.chunks(1234 * channels) {
            rs.push(chunk, &mut out).expect("push");
        }

        let out: Vec<f32> = out.into_iter().collect();
        let out_frames = out.len() / channels;
        let expected = (input_frames as f64 * to as f64 / from as f64) as usize;
        // FixedInput mode retains up to a chunk internally; allow that slack.
        assert!(
            out_frames > expected.saturating_sub(3 * CHUNK_FRAMES),
            "{from}->{to}: produced {out_frames} frames, expected ~{expected}"
        );

        // Mono-ize channel 0 and check the 440 Hz tone dominates 1 kHz.
        let ch0: Vec<f32> = out.iter().step_by(channels).copied().collect();
        // Skip any residual leading transient.
        let steady = &ch0[ch0.len() / 4..];
        let p_tone = goertzel_power(steady, to as f32, freq);
        let p_other = goertzel_power(steady, to as f32, 1000.0);
        assert!(
            p_tone > 10.0 * p_other.max(f32::EPSILON),
            "{from}->{to}: tone power {p_tone} not dominant over {p_other}"
        );
    }

    #[test]
    fn resamples_44100_to_48000() {
        run_case(44_100, 48_000);
    }

    #[test]
    fn resamples_48000_to_44100() {
        run_case(48_000, 44_100);
    }

    #[test]
    fn small_pushes_buffer_until_chunk() {
        let mut rs = StreamResampler::new(44_100, 48_000, 1).expect("create");
        let mut out = VecDeque::new();
        // Fewer than CHUNK_FRAMES total → nothing can be produced yet.
        rs.push(&[0.1; 100], &mut out).expect("push");
        assert!(out.is_empty(), "sub-chunk input must be buffered");
        // Topping it past the chunk size produces output.
        rs.push(&vec![0.1; CHUNK_FRAMES], &mut out).expect("push");
        assert!(!out.is_empty(), "full chunk must produce output");
    }

    /// rsac-fab0: `flush` must recover BOTH the buffered partial input chunk
    /// and the FFT algorithmic-delay residue, converging on exactly
    /// `round(frames_in * to/from)` cumulative output frames.
    #[test]
    fn flush_recovers_partial_chunk_and_delay_tail() {
        let (from, to) = (44_100u32, 48_000u32);
        let input_frames = 5000usize; // deliberately NOT a multiple of 1024
        let input = sine_interleaved(440.0, from, 1, input_frames);

        let mut rs = StreamResampler::new(from, to, 1).expect("create");
        let mut out = VecDeque::new();
        rs.push(&input, &mut out).expect("push");
        let before_flush = out.len();
        let flushed = rs.flush(&mut out).expect("flush");

        let expected =
            (input_frames as u64 * u64::from(to) + u64::from(from) / 2) / u64::from(from);
        assert_eq!(
            out.len() as u64,
            expected,
            "cumulative output must be exactly round(in * to/from)"
        );
        assert_eq!(
            flushed,
            out.len() - before_flush,
            "flush must report the frames it appended"
        );
        // The tail was previously abandoned entirely (partial chunk + delay
        // residue) — the whole point of flush is that it is nonzero.
        assert!(flushed > 0, "flush must recover a nonzero tail");

        // The recovered tail is real audio (the end of the sine), not the
        // resampler's internal zero padding.
        let tail: Vec<f32> = out.iter().rev().take(128).copied().collect();
        let energy: f32 = tail.iter().map(|v| v * v).sum();
        assert!(
            energy > 1e-3,
            "flushed tail must carry the sine's energy, got {energy}"
        );

        // Idempotent: a second flush emits nothing.
        let mut extra = VecDeque::new();
        assert_eq!(rs.flush(&mut extra).expect("re-flush"), 0);
        assert!(extra.is_empty(), "re-flush must not emit");
    }

    /// The exact-length accounting also holds when downsampling and when the
    /// input arrives in awkward uneven pushes (stereo).
    #[test]
    fn flush_exact_length_downsampling_stereo() {
        let (from, to) = (48_000u32, 44_100u32);
        let channels = 2usize;
        let input_frames = 4321usize;
        let input = sine_interleaved(300.0, from, channels, input_frames);

        let mut rs = StreamResampler::new(from, to, channels as u16).expect("create");
        let mut out = VecDeque::new();
        for chunk in input.chunks(777 * channels) {
            rs.push(chunk, &mut out).expect("push");
        }
        rs.flush(&mut out).expect("flush");

        let expected =
            (input_frames as u64 * u64::from(to) + u64::from(from) / 2) / u64::from(from);
        assert_eq!(
            (out.len() / channels) as u64,
            expected,
            "cumulative output frames must be exactly round(in * to/from)"
        );
    }
}
