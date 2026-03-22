//! Audio processing pipeline — resampling and chunk accumulation.
//!
//! Receives raw AudioChunks from capture threads (48kHz stereo),
//! resamples to 16kHz mono, and emits fixed-size ProcessedAudioChunks
//! suitable for downstream ASR/VAD processing.

use std::time::Duration;

use audioadapter_buffers::direct::SequentialSliceOfVecs;
use crossbeam_channel::{Receiver, Sender};
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

use super::capture::AudioChunk;

/// Resampled, mono audio chunk ready for downstream processing (ASR/VAD).
#[derive(Debug, Clone)]
pub struct ProcessedAudioChunk {
    pub source_id: String,
    pub data: Vec<f32>,
    pub sample_rate: u32,
    pub num_frames: usize,
    pub timestamp: Option<Duration>,
}

/// Target output sample rate for ASR/VAD.
const TARGET_SAMPLE_RATE: u32 = 16000;

/// Target chunk size in frames (~32ms at 16kHz).
const TARGET_CHUNK_FRAMES: usize = 512;

/// Resampler processing block size (input frames per rubato call).
const RESAMPLER_CHUNK_SIZE: usize = 1024;

/// Audio pipeline that resamples 48kHz stereo → 16kHz mono and emits fixed-size chunks.
pub struct AudioPipeline {
    /// Receives raw AudioChunks from capture threads.
    audio_rx: Receiver<AudioChunk>,
    /// Sends processed chunks downstream (ASR, VAD, etc.).
    output_tx: Sender<ProcessedAudioChunk>,
    /// rubato resampler (created lazily on first chunk).
    resampler: Option<Async<f32>>,
    /// Input sample rate the resampler was created for.
    resampler_input_rate: u32,
    /// Buffer accumulating mono samples waiting for the resampler.
    /// rubato requires exactly `input_frames_next()` samples per call.
    resampler_input_buffer: Vec<f32>,
    /// Buffer accumulating resampled output, drained in TARGET_CHUNK_FRAMES-sized pieces.
    accumulation_buffer: Vec<f32>,
    /// Source ID for the current accumulation (for tagging output chunks).
    current_source_id: Option<String>,
    /// Timestamp of the current accumulation start.
    current_timestamp: Option<Duration>,
}

impl AudioPipeline {
    /// Create a new audio pipeline.
    pub fn new(audio_rx: Receiver<AudioChunk>, output_tx: Sender<ProcessedAudioChunk>) -> Self {
        Self {
            audio_rx,
            output_tx,
            resampler: None,
            resampler_input_rate: 0,
            resampler_input_buffer: Vec::with_capacity(RESAMPLER_CHUNK_SIZE * 2),
            accumulation_buffer: Vec::with_capacity(TARGET_CHUNK_FRAMES * 4),
            current_source_id: None,
            current_timestamp: None,
        }
    }

    /// Run the pipeline processing loop (blocking — spawn in a dedicated thread).
    pub fn run(&mut self) {
        log::info!("AudioPipeline: starting processing loop");
        while let Ok(chunk) = self.audio_rx.recv() {
            self.process_chunk(chunk);
        }
        self.flush();
        log::info!("AudioPipeline: processing loop ended (channel closed)");
    }

    /// Process a single audio chunk: mixdown → resample → accumulate → emit.
    fn process_chunk(&mut self, chunk: AudioChunk) {
        // Track source ID and timestamp for output tagging
        if self.current_source_id.is_none() {
            self.current_source_id = Some(chunk.source_id.clone());
        }
        if self.current_timestamp.is_none() {
            self.current_timestamp = chunk.timestamp;
        }

        // Step 1: Stereo (or multi-channel) → mono mixdown
        let mono = Self::stereo_to_mono(&chunk.data, chunk.channels);

        // Step 2: Resample if needed
        if chunk.sample_rate == TARGET_SAMPLE_RATE {
            // No resampling needed — push directly to accumulation
            self.accumulation_buffer.extend_from_slice(&mono);
        } else {
            // Ensure resampler exists and matches input rate
            if self.resampler.is_none() || self.resampler_input_rate != chunk.sample_rate {
                match Self::create_resampler(chunk.sample_rate) {
                    Ok(r) => {
                        self.resampler = Some(r);
                        self.resampler_input_rate = chunk.sample_rate;
                        self.resampler_input_buffer.clear();
                        log::info!(
                            "AudioPipeline: created resampler {}Hz → {}Hz",
                            chunk.sample_rate,
                            TARGET_SAMPLE_RATE
                        );
                    }
                    Err(e) => {
                        log::error!("AudioPipeline: failed to create resampler: {}", e);
                        return;
                    }
                }
            }

            // Add mono samples to resampler input buffer
            self.resampler_input_buffer.extend_from_slice(&mono);

            // Feed resampler in exact input_frames_next() batches
            self.drain_resampler();
        }

        // Step 3: Emit complete chunks from accumulation buffer
        self.emit_chunks();
    }

    /// Feed the resampler with buffered input in exact chunk sizes.
    fn drain_resampler(&mut self) {
        let resampler = match self.resampler.as_mut() {
            Some(r) => r,
            None => return,
        };

        loop {
            let needed = resampler.input_frames_next();
            if self.resampler_input_buffer.len() < needed {
                break;
            }

            // Drain exactly `needed` samples into a channel vec
            let input_chunk: Vec<f32> = self.resampler_input_buffer.drain(..needed).collect();
            let waves_in = vec![input_chunk];

            // Wrap in audioadapter SequentialSliceOfVecs for rubato 1.0 API
            let input_adapter = match SequentialSliceOfVecs::new(&waves_in, 1, needed) {
                Ok(a) => a,
                Err(e) => {
                    log::error!("AudioPipeline: failed to create input adapter: {}", e);
                    break;
                }
            };

            match resampler.process(&input_adapter, 0, None) {
                Ok(interleaved_out) => {
                    // For mono, interleaved data is just the samples directly
                    let resampled = interleaved_out.take_data();
                    self.accumulation_buffer.extend_from_slice(&resampled);
                }
                Err(e) => {
                    log::error!("AudioPipeline: resampling error: {}", e);
                    break;
                }
            }
        }
    }

    /// Emit TARGET_CHUNK_FRAMES-sized chunks from the accumulation buffer.
    fn emit_chunks(&mut self) {
        while self.accumulation_buffer.len() >= TARGET_CHUNK_FRAMES {
            let chunk_data: Vec<f32> = self
                .accumulation_buffer
                .drain(..TARGET_CHUNK_FRAMES)
                .collect();

            let processed = ProcessedAudioChunk {
                source_id: self
                    .current_source_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                num_frames: chunk_data.len(),
                data: chunk_data,
                sample_rate: TARGET_SAMPLE_RATE,
                timestamp: self.current_timestamp,
            };

            if let Err(e) = self.output_tx.send(processed) {
                log::warn!("AudioPipeline: downstream channel closed: {}", e);
                return;
            }
        }
    }

    /// Flush remaining buffered audio on shutdown.
    fn flush(&mut self) {
        // Try to flush remaining resampler input by zero-padding
        if let Some(resampler) = self.resampler.as_mut() {
            let needed = resampler.input_frames_next();
            let current = self.resampler_input_buffer.len();
            if current > 0 && current < needed {
                self.resampler_input_buffer.resize(needed, 0.0);
                // drain_resampler will process this padded chunk
            }
        }
        self.drain_resampler();

        // Emit any remaining accumulated samples as a final (possibly undersized) chunk
        if !self.accumulation_buffer.is_empty() {
            let remaining: Vec<f32> = self.accumulation_buffer.drain(..).collect();
            let processed = ProcessedAudioChunk {
                source_id: self
                    .current_source_id
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                num_frames: remaining.len(),
                data: remaining,
                sample_rate: TARGET_SAMPLE_RATE,
                timestamp: self.current_timestamp,
            };

            if let Err(e) = self.output_tx.send(processed) {
                log::warn!("AudioPipeline: could not send final flush chunk: {}", e);
            }
        }

        log::info!("AudioPipeline: flushed remaining audio");
    }

    /// Create a rubato sinc resampler for the given input sample rate → 16kHz.
    fn create_resampler(input_rate: u32) -> Result<Async<f32>, String> {
        let ratio = TARGET_SAMPLE_RATE as f64 / input_rate as f64;

        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };

        Async::<f32>::new_sinc(
            ratio,
            2.0, // max_resample_ratio_relative
            &params,
            RESAMPLER_CHUNK_SIZE,
            1, // mono
            FixedAsync::Input,
        )
        .map_err(|e| format!("Failed to create resampler: {}", e))
    }

    /// Convert interleaved multi-channel audio to mono by averaging all channels per frame.
    fn stereo_to_mono(interleaved: &[f32], channels: u16) -> Vec<f32> {
        if channels <= 1 {
            return interleaved.to_vec();
        }

        let ch = channels as usize;
        let num_frames = interleaved.len() / ch;
        let mut mono = Vec::with_capacity(num_frames);

        for frame in 0..num_frames {
            let offset = frame * ch;
            let mut sum = 0.0_f32;
            for c in 0..ch {
                sum += interleaved[offset + c];
            }
            mono.push(sum / channels as f32);
        }

        mono
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_to_mono_basic() {
        let stereo = vec![1.0, 0.0, 0.5, 0.5, 0.0, 1.0];
        let mono = AudioPipeline::stereo_to_mono(&stereo, 2);
        assert_eq!(mono.len(), 3);
        assert!((mono[0] - 0.5).abs() < 1e-6);
        assert!((mono[1] - 0.5).abs() < 1e-6);
        assert!((mono[2] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn stereo_to_mono_passthrough() {
        let mono_in = vec![0.1, 0.2, 0.3];
        let mono_out = AudioPipeline::stereo_to_mono(&mono_in, 1);
        assert_eq!(mono_out, mono_in);
    }

    #[test]
    fn create_resampler_48k() {
        let r = AudioPipeline::create_resampler(48000);
        assert!(r.is_ok(), "Failed to create 48kHz resampler: {:?}", r.err());
    }

    #[test]
    fn create_resampler_44k() {
        let r = AudioPipeline::create_resampler(44100);
        assert!(
            r.is_ok(),
            "Failed to create 44.1kHz resampler: {:?}",
            r.err()
        );
    }

    #[test]
    fn pipeline_emits_chunks() {
        let (in_tx, in_rx) = crossbeam_channel::unbounded();
        let (out_tx, out_rx) = crossbeam_channel::unbounded();

        let mut pipeline = AudioPipeline::new(in_rx, out_tx);

        // Send a chunk of silence at 16kHz mono (no resampling needed)
        // 1024 frames should produce 2 chunks of 512
        let chunk = AudioChunk {
            source_id: "test".to_string(),
            data: vec![0.0; 1024],
            sample_rate: 16000,
            channels: 1,
            num_frames: 1024,
            timestamp: None,
        };
        in_tx.send(chunk).unwrap();
        drop(in_tx); // close channel so run() exits

        pipeline.run();

        // Should have emitted exactly 2 chunks of 512 frames
        let c1 = out_rx.recv().unwrap();
        assert_eq!(c1.num_frames, 512);
        assert_eq!(c1.sample_rate, 16000);
        assert_eq!(c1.source_id, "test");

        let c2 = out_rx.recv().unwrap();
        assert_eq!(c2.num_frames, 512);
    }
}
