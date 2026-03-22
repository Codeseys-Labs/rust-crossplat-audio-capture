//! Voice Activity Detection (VAD) processor.
//!
//! Receives `ProcessedAudioChunk` (16kHz mono f32, 512-frame chunks) from the
//! audio pipeline and segments speech from silence. Emits `SpeechSegment`s
//! containing contiguous speech audio with pre/post-speech padding.
//!
//! Uses the Silero VAD v5 model via the `voice_activity_detector` crate.

use std::collections::VecDeque;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use voice_activity_detector::VoiceActivityDetector;

use super::pipeline::ProcessedAudioChunk;

/// Sample rate used throughout the VAD pipeline (16kHz).
const SAMPLE_RATE: u32 = 16_000;

/// Chunk size in frames expected by Silero VAD at 16kHz.
const CHUNK_FRAMES: usize = 512;

/// Duration of a single chunk at 16kHz / 512 frames = 32ms.
const CHUNK_DURATION_MS: f64 = (CHUNK_FRAMES as f64 / SAMPLE_RATE as f64) * 1000.0;

/// A segment of speech audio extracted by the VAD processor.
#[derive(Debug, Clone)]
pub struct SpeechSegment {
    /// Identifier of the audio source that produced this segment.
    pub source_id: String,
    /// 16kHz mono f32 audio data for the speech segment.
    pub audio: Vec<f32>,
    /// Start time relative to stream start.
    pub start_time: Duration,
    /// End time relative to stream start.
    pub end_time: Duration,
    /// Number of audio frames (equal to `audio.len()`).
    pub num_frames: usize,
}

/// Configuration for the VAD processor.
#[derive(Debug, Clone)]
pub struct VadConfig {
    /// VAD probability threshold — above this is considered speech.
    pub threshold: f32,
    /// Minimum speech duration in milliseconds (reject shorter bursts).
    pub min_speech_ms: u32,
    /// Maximum speech duration in milliseconds (force-emit to bound memory).
    pub max_speech_ms: u32,
    /// Silence duration after speech to end a segment (ms).
    pub silence_timeout_ms: u32,
    /// Pre-speech audio padding to include (ms).
    pub pre_speech_padding_ms: u32,
    /// Post-speech audio padding to include (ms).
    ///
    /// M5: In the current implementation the `silence_timeout_ms` effectively
    /// serves as post-speech padding — once enough silence accumulates, the
    /// segment is emitted and the accumulated silence IS the trailing padding.
    /// This field is kept for future use if finer-grained control is needed
    /// (e.g., collecting extra audio beyond the silence timeout), but is not
    /// separately enforced today.
    pub post_speech_padding_ms: u32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            min_speech_ms: 500,
            max_speech_ms: 30_000,
            silence_timeout_ms: 300,
            pre_speech_padding_ms: 300,
            post_speech_padding_ms: 500,
        }
    }
}

/// Voice Activity Detection processor.
///
/// Receives `ProcessedAudioChunk`s, runs them through Silero VAD, and emits
/// `SpeechSegment`s on the output channel when speech boundaries are detected.
///
/// # Thread model
///
/// Create with [`VadProcessor::new`], then call [`VadProcessor::run`] on a
/// dedicated thread. The VAD model is created inside `run()` to keep the
/// processor `Send`-friendly (the underlying ONNX session is `Send` via
/// `Arc<Mutex<Session>>`).
pub struct VadProcessor {
    // Configuration
    config: VadConfig,

    // State
    is_speech_active: bool,
    speech_buffer: Vec<f32>,
    /// Rolling ring buffer holding recent non-speech audio for pre-speech padding.
    pre_speech_ring: VecDeque<f32>,
    /// Maximum number of samples to keep in the pre-speech ring buffer.
    pre_speech_ring_capacity: usize,
    speech_start_time: Option<Duration>,
    last_speech_time: Option<Duration>,
    /// Accumulated silence duration (in ms) since last speech frame.
    silence_duration_ms: f64,
    /// Current stream time tracked from incoming chunk timestamps.
    current_time: Duration,
    source_id: String,

    // Output channel
    output_tx: Sender<SpeechSegment>,
}

impl VadProcessor {
    /// Create a new VAD processor.
    ///
    /// The VAD model itself is created lazily inside [`run`] to avoid
    /// cross-thread issues with ONNX runtime initialization.
    pub fn new(config: VadConfig, output_tx: Sender<SpeechSegment>) -> Self {
        // Pre-speech ring buffer capacity: padding_ms * samples_per_ms
        let samples_per_ms = SAMPLE_RATE as usize / 1000;
        let pre_speech_ring_capacity = config.pre_speech_padding_ms as usize * samples_per_ms;

        Self {
            config,
            is_speech_active: false,
            speech_buffer: Vec::with_capacity(SAMPLE_RATE as usize * 5), // ~5s initial
            pre_speech_ring: VecDeque::with_capacity(pre_speech_ring_capacity),
            pre_speech_ring_capacity,
            speech_start_time: None,
            last_speech_time: None,
            silence_duration_ms: 0.0,
            current_time: Duration::ZERO,
            source_id: String::new(),
            output_tx,
        }
    }

    /// Run the VAD processing loop (blocking — call from a dedicated thread).
    ///
    /// Creates the Silero VAD model, then loops receiving `ProcessedAudioChunk`s
    /// from `processed_rx`. Exits when the channel is disconnected.
    pub fn run(&mut self, processed_rx: Receiver<ProcessedAudioChunk>) {
        log::info!("VadProcessor: creating Silero VAD model (16kHz, 512-frame chunks)");

        let mut vad = match VoiceActivityDetector::builder()
            .sample_rate(SAMPLE_RATE)
            .chunk_size(CHUNK_FRAMES)
            .build()
        {
            Ok(v) => v,
            Err(e) => {
                log::error!("VadProcessor: failed to create VAD model: {:?}", e);
                return;
            }
        };

        log::info!(
            "VadProcessor: starting processing loop (threshold={}, silence_timeout={}ms, \
             min_speech={}ms, max_speech={}ms, pre_pad={}ms, post_pad={}ms)",
            self.config.threshold,
            self.config.silence_timeout_ms,
            self.config.min_speech_ms,
            self.config.max_speech_ms,
            self.config.pre_speech_padding_ms,
            self.config.post_speech_padding_ms,
        );

        while let Ok(chunk) = processed_rx.recv() {
            self.process_chunk(&chunk, &mut vad);
        }

        // Channel closed — flush any remaining speech
        if self.is_speech_active {
            log::info!("VadProcessor: channel closed, flushing active speech segment");
            self.emit_segment();
        }

        log::info!("VadProcessor: processing loop ended (channel closed)");
    }

    /// Process a single audio chunk through the VAD.
    fn process_chunk(&mut self, chunk: &ProcessedAudioChunk, vad: &mut VoiceActivityDetector) {
        // Update tracking state
        if self.source_id.is_empty() {
            self.source_id = chunk.source_id.clone();
        }
        if let Some(ts) = chunk.timestamp {
            self.current_time = ts;
        }

        // Run VAD prediction on the chunk
        let probability = vad.predict(chunk.data.iter().copied());

        let is_speech = probability > self.config.threshold;

        if is_speech {
            if !self.is_speech_active {
                // Speech onset — transition from IDLE to SPEECH_ACTIVE
                log::debug!(
                    "VadProcessor: speech onset detected (prob={:.3}, time={:?})",
                    probability,
                    self.current_time,
                );
                self.is_speech_active = true;

                // Record speech start time (adjusted back by pre-speech padding)
                let pre_pad = Duration::from_millis(self.config.pre_speech_padding_ms as u64);
                self.speech_start_time = Some(self.current_time.saturating_sub(pre_pad));

                // Prepend pre-speech padding from ring buffer
                let ring_samples: Vec<f32> = self.pre_speech_ring.iter().copied().collect();
                self.speech_buffer.clear();
                self.speech_buffer.extend_from_slice(&ring_samples);
            }

            // Accumulate speech audio
            self.speech_buffer.extend_from_slice(&chunk.data);
            self.last_speech_time = Some(self.current_time);
            self.silence_duration_ms = 0.0;

            // Check max speech duration
            if let Some(start) = self.speech_start_time {
                let speech_duration_ms = self.current_time.saturating_sub(start).as_millis() as u32;
                if speech_duration_ms >= self.config.max_speech_ms {
                    log::warn!(
                        "VadProcessor: max speech duration exceeded ({}ms >= {}ms), force-emitting",
                        speech_duration_ms,
                        self.config.max_speech_ms,
                    );
                    self.force_emit();
                }
            }
        } else {
            // Silence detected
            if self.is_speech_active {
                // We're in speech — accumulate silence as potential post-speech padding
                self.speech_buffer.extend_from_slice(&chunk.data);
                self.silence_duration_ms += CHUNK_DURATION_MS;

                // Check if silence timeout exceeded
                if self.silence_duration_ms >= self.config.silence_timeout_ms as f64 {
                    log::debug!(
                        "VadProcessor: silence timeout reached ({:.0}ms >= {}ms), ending speech segment",
                        self.silence_duration_ms,
                        self.config.silence_timeout_ms,
                    );

                    // Continue collecting post-speech padding if we haven't yet
                    // The silence_timeout already provides some post-speech audio.
                    // If post_speech_padding > silence_timeout, we'd need more,
                    // but typically silence_timeout triggers the emit and the
                    // accumulated silence IS the post-speech padding.
                    self.emit_segment();
                }
            } else {
                // IDLE state — just maintain the pre-speech ring buffer
                for &sample in &chunk.data {
                    if self.pre_speech_ring.len() >= self.pre_speech_ring_capacity {
                        self.pre_speech_ring.pop_front();
                    }
                    self.pre_speech_ring.push_back(sample);
                }
            }
        }
    }

    /// Emit the accumulated speech segment on the output channel.
    ///
    /// Applies minimum speech duration filtering — segments shorter than
    /// `min_speech_ms` are discarded as noise bursts.
    fn emit_segment(&mut self) {
        if self.speech_buffer.is_empty() {
            self.reset_state();
            return;
        }

        let start_time = self.speech_start_time.unwrap_or(Duration::ZERO);
        let end_time = self.current_time;
        let speech_duration_ms = end_time.saturating_sub(start_time).as_millis() as u32;

        // Enforce minimum speech duration
        if speech_duration_ms < self.config.min_speech_ms {
            log::debug!(
                "VadProcessor: discarding short segment ({}ms < {}ms min)",
                speech_duration_ms,
                self.config.min_speech_ms,
            );
            self.reset_state();
            return;
        }

        let audio = std::mem::take(&mut self.speech_buffer);
        let num_frames = audio.len();

        let segment = SpeechSegment {
            source_id: self.source_id.clone(),
            audio,
            start_time,
            end_time,
            num_frames,
        };

        log::info!(
            "VadProcessor: emitting speech segment (duration={}ms, frames={}, \
             start={:?}, end={:?})",
            speech_duration_ms,
            num_frames,
            start_time,
            end_time,
        );

        if let Err(e) = self.output_tx.send(segment) {
            log::warn!(
                "VadProcessor: output channel closed, cannot send segment: {}",
                e,
            );
        }

        self.reset_state();
    }

    /// Force-emit the current speech buffer when max duration is exceeded.
    ///
    /// Unlike [`emit_segment`], this does not check minimum duration — the
    /// segment is already long enough by definition.
    fn force_emit(&mut self) {
        if self.speech_buffer.is_empty() {
            self.reset_state();
            return;
        }

        let start_time = self.speech_start_time.unwrap_or(Duration::ZERO);
        let end_time = self.current_time;
        let audio = std::mem::take(&mut self.speech_buffer);
        let num_frames = audio.len();
        let speech_duration_ms = end_time.saturating_sub(start_time).as_millis() as u32;

        let segment = SpeechSegment {
            source_id: self.source_id.clone(),
            audio,
            start_time,
            end_time,
            num_frames,
        };

        log::info!(
            "VadProcessor: force-emitting speech segment (duration={}ms, frames={}, \
             start={:?}, end={:?})",
            speech_duration_ms,
            num_frames,
            start_time,
            end_time,
        );

        if let Err(e) = self.output_tx.send(segment) {
            log::warn!(
                "VadProcessor: output channel closed, cannot send segment: {}",
                e,
            );
        }

        // Reset but keep speech active — more speech may follow
        self.speech_buffer.clear();
        self.speech_start_time = Some(self.current_time);
        self.silence_duration_ms = 0.0;
    }

    /// Reset VAD state machine back to IDLE.
    fn reset_state(&mut self) {
        self.is_speech_active = false;
        self.speech_buffer.clear();
        self.speech_start_time = None;
        self.last_speech_time = None;
        self.silence_duration_ms = 0.0;
        // Keep pre_speech_ring — it continues accumulating for the next segment
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vad_config_default_values() {
        let config = VadConfig::default();
        assert!((config.threshold - 0.5).abs() < f32::EPSILON);
        assert_eq!(config.min_speech_ms, 500);
        assert_eq!(config.max_speech_ms, 30_000);
        assert_eq!(config.silence_timeout_ms, 300);
        assert_eq!(config.pre_speech_padding_ms, 300);
        assert_eq!(config.post_speech_padding_ms, 500);
    }

    #[test]
    fn pre_speech_ring_capacity() {
        let config = VadConfig::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let processor = VadProcessor::new(config, tx);
        // 300ms * 16 samples/ms = 4800 samples
        assert_eq!(processor.pre_speech_ring_capacity, 4800);
    }

    #[test]
    fn speech_segment_num_frames_matches_audio_len() {
        let segment = SpeechSegment {
            source_id: "test".to_string(),
            audio: vec![0.0; 16000],
            start_time: Duration::ZERO,
            end_time: Duration::from_secs(1),
            num_frames: 16000,
        };
        assert_eq!(segment.num_frames, segment.audio.len());
    }

    #[test]
    fn chunk_duration_is_32ms() {
        // 512 frames / 16000 Hz = 0.032s = 32ms
        assert!((CHUNK_DURATION_MS - 32.0).abs() < 0.01);
    }

    #[test]
    fn reset_state_clears_all_fields() {
        let config = VadConfig::default();
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut processor = VadProcessor::new(config, tx);

        // Simulate some state
        processor.is_speech_active = true;
        processor.speech_buffer.extend_from_slice(&[1.0; 1000]);
        processor.speech_start_time = Some(Duration::from_secs(1));
        processor.last_speech_time = Some(Duration::from_secs(2));
        processor.silence_duration_ms = 100.0;

        processor.reset_state();

        assert!(!processor.is_speech_active);
        assert!(processor.speech_buffer.is_empty());
        assert!(processor.speech_start_time.is_none());
        assert!(processor.last_speech_time.is_none());
        assert!((processor.silence_duration_ms - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn emit_segment_discards_short_speech() {
        let config = VadConfig {
            min_speech_ms: 500,
            ..VadConfig::default()
        };
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut processor = VadProcessor::new(config, tx);

        // Set up a very short speech segment (100ms worth of audio)
        processor.is_speech_active = true;
        processor.speech_buffer = vec![0.0; 1600]; // 100ms at 16kHz
        processor.speech_start_time = Some(Duration::from_millis(0));
        processor.current_time = Duration::from_millis(100);
        processor.source_id = "test".to_string();

        processor.emit_segment();

        // Should be discarded — nothing on the channel
        assert!(rx.try_recv().is_err());
        assert!(!processor.is_speech_active);
    }

    #[test]
    fn emit_segment_sends_long_enough_speech() {
        let config = VadConfig {
            min_speech_ms: 500,
            ..VadConfig::default()
        };
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut processor = VadProcessor::new(config, tx);

        // Set up a 600ms speech segment
        processor.is_speech_active = true;
        processor.speech_buffer = vec![0.0; 9600]; // 600ms at 16kHz
        processor.speech_start_time = Some(Duration::from_millis(0));
        processor.current_time = Duration::from_millis(600);
        processor.source_id = "test".to_string();

        processor.emit_segment();

        let segment = rx.try_recv().expect("should have received a segment");
        assert_eq!(segment.num_frames, 9600);
        assert_eq!(segment.source_id, "test");
        assert_eq!(segment.start_time, Duration::from_millis(0));
        assert_eq!(segment.end_time, Duration::from_millis(600));
    }

    #[test]
    fn pre_speech_ring_buffer_eviction() {
        let config = VadConfig {
            pre_speech_padding_ms: 100, // 1600 samples at 16kHz
            ..VadConfig::default()
        };
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut processor = VadProcessor::new(config, tx);

        assert_eq!(processor.pre_speech_ring_capacity, 1600);

        // Fill ring buffer beyond capacity
        for i in 0..2000 {
            if processor.pre_speech_ring.len() >= processor.pre_speech_ring_capacity {
                processor.pre_speech_ring.pop_front();
            }
            processor.pre_speech_ring.push_back(i as f32);
        }

        // Should be capped at capacity
        assert_eq!(processor.pre_speech_ring.len(), 1600);
        // First element should be 400 (2000 - 1600)
        assert!((processor.pre_speech_ring[0] - 400.0).abs() < f32::EPSILON);
    }
}
