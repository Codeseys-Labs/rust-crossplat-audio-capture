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
    /// Interleaved input samples not yet consumed by the resampler.
    pending: Vec<f32>,
    /// Interleaved output scratch, sized for the resampler's max output chunk.
    out_scratch: Vec<f32>,
    /// Leading output frames still to swallow (the resampler's algorithmic
    /// delay is silence we trim so composition alignment isn't skewed by it).
    delay_frames_remaining: usize,
}

impl std::fmt::Debug for StreamResampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamResampler")
            .field("channels", &self.channels)
            .field("pending_samples", &self.pending.len())
            .field("delay_frames_remaining", &self.delay_frames_remaining)
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
            pending: Vec::with_capacity(CHUNK_FRAMES * channels * 2),
            out_scratch: vec![0.0; max_out_frames.max(1) * channels],
            delay_frames_remaining: delay,
        })
    }

    /// Feeds interleaved input samples and appends resampled interleaved
    /// output samples to `out`. Input that doesn't yet fill a full resampler
    /// chunk is buffered internally for the next call.
    pub fn push(&mut self, interleaved: &[f32], out: &mut VecDeque<f32>) -> AudioResult<()> {
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

            self.pending.drain(..consumed_frames * self.channels);
        }
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
}
