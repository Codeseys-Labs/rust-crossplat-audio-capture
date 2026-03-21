//! Audio processing pipeline — resampling, VAD, and utterance buffering.
//!
//! Responsibilities:
//! - Receive tagged audio buffers from capture threads
//! - Resample to 16kHz mono (required by Whisper)
//! - Run Voice Activity Detection (Silero VAD) to detect speech segments
//! - Buffer speech into utterances and forward to ASR worker
//! - Also forward raw segments to diarization worker

/// Audio pipeline that processes raw audio into speech utterances.
#[allow(dead_code)]
pub struct AudioPipeline {
    // TODO: Fields will be added when implementing:
    // audio_rx: crossbeam_channel::Receiver<TaggedAudioBuffer>,
    // asr_tx: crossbeam_channel::Sender<SpeechUtterance>,
    // diarization_tx: crossbeam_channel::Sender<AudioSegment>,
    // resampler: rubato::SincFixedIn<f32>,
    // vad: VoiceActivityDetector,
    _placeholder: (),
}

impl AudioPipeline {
    /// Create a new audio pipeline.
    pub fn new() -> Self {
        // TODO: Initialize resampler (48kHz stereo → 16kHz mono)
        // TODO: Initialize VAD with configured threshold
        // TODO: Set up channel receivers/senders
        Self { _placeholder: () }
    }

    /// Run the pipeline processing loop (blocking, should be spawned in a thread).
    pub fn run(&mut self) {
        // TODO: Main loop:
        // 1. Receive TaggedAudioBuffer from audio_rx
        // 2. Resample to 16kHz mono
        // 3. Run VAD on resampled audio
        // 4. Buffer speech segments
        // 5. When utterance complete, send to asr_tx
        // 6. Send raw segment to diarization_tx
        log::info!("AudioPipeline::run() stub — not yet implemented");
    }
}

impl Default for AudioPipeline {
    fn default() -> Self {
        Self::new()
    }
}
