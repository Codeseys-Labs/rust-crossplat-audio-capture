//! Automatic Speech Recognition (ASR) module.
//!
//! Uses whisper-rs to transcribe speech utterances into text segments.
//! The ASR worker runs in its own thread, receiving utterances from the
//! audio pipeline and producing TranscriptSegments.

/// ASR worker that processes speech utterances into transcript segments.
#[allow(dead_code)]
pub struct AsrWorker {
    // TODO: Fields will be added when whisper-rs is integrated:
    // utterance_rx: crossbeam_channel::Receiver<SpeechUtterance>,
    // transcript_tx: crossbeam_channel::Sender<TranscriptSegment>,
    // ctx: whisper_rs::WhisperContext,
    _placeholder: (),
}

impl AsrWorker {
    /// Create a new ASR worker.
    pub fn new() -> Self {
        // TODO: Load Whisper model from configured path
        // TODO: Set up channels
        Self { _placeholder: () }
    }

    /// Run the ASR processing loop (blocking, should be spawned in a thread).
    pub fn run(&mut self) {
        // TODO: Main loop:
        // 1. Receive SpeechUtterance from utterance_rx
        // 2. Run Whisper inference
        // 3. Create TranscriptSegment with timestamps and confidence
        // 4. Send to transcript_tx
        log::info!("AsrWorker::run() stub — not yet implemented");
    }
}

impl Default for AsrWorker {
    fn default() -> Self {
        Self::new()
    }
}
