//! Automatic Speech Recognition (ASR) module.
//!
//! Uses whisper-rs to transcribe speech utterances into text segments.
//! The ASR worker runs in its own thread, receiving `SpeechSegment`s from the
//! VAD processor via a crossbeam channel and producing `TranscriptSegment`s.

use std::path::{Path, PathBuf};

use crossbeam_channel::{Receiver, Sender};
use log::{debug, error, info, warn};
use uuid::Uuid;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

use crate::audio::vad::SpeechSegment;
use crate::state::TranscriptSegment;

/// Configuration for the ASR worker.
pub struct AsrConfig {
    /// Path to the Whisper GGML model file (e.g. `models/ggml-small.en.bin`).
    pub model_path: PathBuf,
    /// Language code for transcription (e.g. `"en"`).
    pub language: String,
    /// Number of threads for whisper inference. Default: 4.
    pub n_threads: i32,
    /// Sampling temperature. 0.0 = greedy. Default: 0.0.
    pub temperature: f32,
    /// Beam size (only used with beam-search strategy). Default: 5.
    pub beam_size: i32,
}

impl AsrConfig {
    /// Create an `AsrConfig` with the model path resolved under the given
    /// models directory.
    pub fn with_models_dir(models_dir: &Path) -> Self {
        Self {
            model_path: models_dir.join("ggml-small.en.bin"),
            language: "en".to_string(),
            n_threads: 4,
            temperature: 0.0,
            beam_size: 5,
        }
    }
}

impl Default for AsrConfig {
    fn default() -> Self {
        Self {
            model_path: PathBuf::from("models/ggml-small.en.bin"),
            language: "en".to_string(),
            n_threads: 4,
            temperature: 0.0,
            beam_size: 5,
        }
    }
}

/// ASR worker that processes speech segments into transcript segments.
///
/// Designed to run on a dedicated thread. Call [`AsrWorker::run`] with the
/// incoming `SpeechSegment` receiver to enter the processing loop.
pub struct AsrWorker {
    config: AsrConfig,
    output_tx: Sender<TranscriptSegment>,
    segments_processed: u64,
}

impl AsrWorker {
    /// Create a new ASR worker with the given config and output channel.
    pub fn new(config: AsrConfig, output_tx: Sender<TranscriptSegment>) -> Self {
        Self {
            config,
            output_tx,
            segments_processed: 0,
        }
    }

    /// Run the ASR processing loop (blocking — should be spawned in a thread).
    ///
    /// Loads the Whisper model, then enters a receive loop consuming
    /// `SpeechSegment`s from `speech_rx`. Each segment is transcribed and
    /// the resulting `TranscriptSegment`s are sent on `output_tx`.
    ///
    /// Returns gracefully if the model fails to load or the channel disconnects.
    pub fn run(mut self, speech_rx: Receiver<SpeechSegment>) {
        // ── Load Whisper model ──────────────────────────────────────────
        let model_path_str = self.config.model_path.display().to_string();

        // Pre-validate model file to avoid UCRT debug assertion crash
        // (`_osfile(fh) & FOPEN` in read.cpp:381) when whisper.cpp tries
        // to read from a missing or corrupted file in debug builds.
        {
            let model_path = &self.config.model_path;
            if !model_path.exists() {
                error!(
                    "Whisper model not found at '{}'. ASR worker cannot start.",
                    model_path_str
                );
                return;
            }
            match std::fs::metadata(model_path) {
                Ok(meta) if meta.len() < 1_000_000 => {
                    error!(
                        "Whisper model at '{}' appears corrupted ({} bytes). \
                         ASR worker cannot start.",
                        model_path_str,
                        meta.len()
                    );
                    return;
                }
                Err(e) => {
                    error!(
                        "Cannot read model file metadata at '{}': {}. \
                         ASR worker cannot start.",
                        model_path_str, e
                    );
                    return;
                }
                Ok(_) => {}
            }
        }

        let ctx = match WhisperContext::new_with_params(
            &model_path_str,
            WhisperContextParameters::default(),
        ) {
            Ok(ctx) => {
                info!("Whisper model loaded successfully from {}", model_path_str);
                ctx
            }
            Err(e) => {
                error!(
                    "Failed to load Whisper model from {}: {}",
                    model_path_str, e
                );
                return;
            }
        };

        let mut state: whisper_rs::WhisperState = match ctx.create_state() {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to create Whisper state: {}", e);
                return;
            }
        };

        // ── Main receive loop ───────────────────────────────────────────
        info!("ASR worker entering receive loop");

        while let Ok(segment) = speech_rx.recv() {
            match self.transcribe_segment(&mut state, &segment) {
                Ok(transcripts) => {
                    for t in transcripts {
                        if let Err(e) = self.output_tx.send(t) {
                            warn!("Failed to send transcript (downstream disconnected): {}", e);
                            return;
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        "Transcription failed for segment from source '{}': {}",
                        segment.source_id, e
                    );
                    // Continue processing next segments
                }
            }
        }

        info!(
            "ASR worker exiting — speech channel closed. Total segments processed: {}",
            self.segments_processed
        );
    }

    /// Transcribe a single speech segment into zero or more transcript segments.
    ///
    /// Configures Whisper parameters, runs inference, then extracts and filters
    /// the resulting segments. Whisper timestamps (in centiseconds) are converted
    /// to absolute seconds by adding the speech segment's `start_time` offset.
    pub fn transcribe_segment(
        &mut self,
        state: &mut whisper_rs::WhisperState,
        segment: &SpeechSegment,
    ) -> Result<Vec<TranscriptSegment>, String> {
        // ── Configure Whisper params ────────────────────────────────────
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some(&self.config.language));
        params.set_print_progress(false);
        params.set_print_timestamps(false);
        params.set_print_realtime(false);
        params.set_print_special(false);
        params.set_single_segment(false);
        params.set_no_context(true);
        params.set_n_threads(self.config.n_threads);
        params.set_temperature(self.config.temperature);

        // ── Run inference ───────────────────────────────────────────────
        state
            .full(params, &segment.audio)
            .map_err(|e| format!("Whisper inference failed: {}", e))?;

        // ── Extract results ─────────────────────────────────────────────
        let num_segments = state.full_n_segments();

        let mut transcripts = Vec::new();

        for i in 0..num_segments {
            let whisper_seg = match state.get_segment(i) {
                Some(s) => s,
                None => continue,
            };

            let text = whisper_seg
                .to_str()
                .map_err(|e| format!("Failed to get segment text: {}", e))?;

            let text = text.trim().to_string();
            if text.is_empty() {
                continue;
            }

            // Whisper returns timestamps in centiseconds (1/100th of a second)
            let t0 = whisper_seg.start_timestamp();
            let t1 = whisper_seg.end_timestamp();

            // Convert whisper timestamps (centiseconds) to absolute seconds
            // by adding the speech segment's start-time offset.
            let segment_start_secs = segment.start_time.as_secs_f64();
            let start_time = segment_start_secs + (t0 as f64 / 100.0);
            let end_time = segment_start_secs + (t1 as f64 / 100.0);

            // Use (1.0 - no_speech_probability) as a rough confidence proxy
            let confidence = 1.0 - whisper_seg.no_speech_probability();

            self.segments_processed += 1;

            let transcript = TranscriptSegment {
                id: Uuid::new_v4().to_string(),
                source_id: segment.source_id.clone(),
                speaker_id: None,    // filled by diarization later
                speaker_label: None, // filled by diarization later
                text: text.clone(),
                start_time,
                end_time,
                confidence,
            };

            debug!(
                "ASR segment {}: [{:.2}s - {:.2}s] conf={:.2} \"{}\"",
                self.segments_processed, start_time, end_time, confidence, &text
            );

            transcripts.push(transcript);
        }

        Ok(transcripts)
    }

    /// Returns the total number of transcript segments produced so far.
    pub fn segments_processed(&self) -> u64 {
        self.segments_processed
    }
}
