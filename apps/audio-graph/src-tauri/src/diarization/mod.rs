//! Speaker diarization module — MVP implementation.
//!
//! Uses simple audio features (RMS energy, zero-crossing rate, mean absolute
//! deviation) as a lightweight "speaker fingerprint" for clustering speech
//! segments into speaker groups. This is intentionally a pure-Rust, no-ML
//! approach suitable as an MVP — it can be upgraded to full pyannote-rs /
//! ONNX-based embedding extraction later.

use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};

use crate::state::{SpeakerInfo, TranscriptSegment};

// ── Speaker color palette ────────────────────────────────────────────────

/// Predefined color palette for distinguishing speakers in the UI.
const SPEAKER_COLORS: &[&str] = &[
    "#4A90D9", // blue
    "#E74C3C", // red
    "#2ECC71", // green
    "#F39C12", // orange
    "#9B59B6", // purple
    "#1ABC9C", // teal
    "#E67E22", // dark orange
    "#3498DB", // light blue
    "#E91E63", // pink
    "#00BCD4", // cyan
];

// ── Types ────────────────────────────────────────────────────────────────

/// Audio features used as a simple speaker fingerprint.
///
/// These three scalar values form a compact "embedding" based on easily
/// computed signal properties. The `spectral_centroid` field is actually
/// the *mean absolute deviation* (MAD) of the waveform — a measure of
/// how "spread out" the signal is — kept under the generic name so the
/// struct can be extended to a real spectral centroid later.
#[derive(Debug, Clone, Copy)]
pub struct AudioFeatures {
    /// Root-mean-square energy of the signal.
    pub rms_energy: f32,
    /// Fraction of consecutive sample pairs that cross zero.
    pub zero_crossing_rate: f32,
    /// Mean absolute deviation (MAD) of the signal.
    pub spectral_centroid: f32,
}

/// A known speaker profile, accumulated over time.
#[derive(Debug, Clone)]
pub struct SpeakerProfile {
    /// Unique identifier (e.g. `"speaker-1"`).
    pub id: String,
    /// Human-readable label (e.g. `"Speaker 1"`).
    pub label: String,
    /// Hex colour for the UI.
    pub color: String,
    /// Running average of audio features for this speaker.
    pub features: AudioFeatures,
    /// Number of segments attributed to this speaker.
    pub segment_count: u32,
    /// Cumulative speaking time in seconds.
    pub total_speaking_time: f64,
}

/// Configuration knobs for the diarization worker.
pub struct DiarizationConfig {
    /// Maximum normalised feature distance to consider "same speaker".
    pub similarity_threshold: f32,
    /// Hard cap on the number of distinct speakers.
    pub max_speakers: usize,
    /// Time gap (seconds) that increases likelihood of a speaker change.
    pub gap_threshold_secs: f64,
}

impl Default for DiarizationConfig {
    fn default() -> Self {
        Self {
            similarity_threshold: 0.7,
            max_speakers: 10,
            gap_threshold_secs: 2.0,
        }
    }
}

/// Input to the diarization worker — a transcript segment paired with the
/// raw speech audio that produced it.
#[derive(Debug, Clone)]
pub struct DiarizationInput {
    /// The transcript segment (with `speaker_id` / `speaker_label` = `None`).
    pub transcript: TranscriptSegment,
    /// 16 kHz mono f32 audio for this segment.
    pub speech_audio: Vec<f32>,
    /// Absolute start time of the speech.
    pub speech_start_time: Duration,
    /// Absolute end time of the speech.
    pub speech_end_time: Duration,
}

/// Output from diarization: the transcript enriched with speaker info.
#[derive(Debug, Clone)]
pub struct DiarizedTranscript {
    /// Transcript segment with `speaker_id` and `speaker_label` filled in.
    pub segment: TranscriptSegment,
    /// Current state of the assigned speaker.
    pub speaker_info: SpeakerInfo,
}

// ── Worker ───────────────────────────────────────────────────────────────

/// MVP diarization worker.
///
/// Runs on a dedicated thread. For each incoming [`DiarizationInput`] it:
/// 1. Extracts audio features from the speech audio.
/// 2. Finds the best matching known speaker (or creates a new one).
/// 3. Fills in speaker fields on the transcript segment.
/// 4. Sends a [`DiarizedTranscript`] downstream.
pub struct DiarizationWorker {
    config: DiarizationConfig,
    speakers: Vec<SpeakerProfile>,
    output_tx: Sender<DiarizedTranscript>,
    next_speaker_num: u32,
    last_segment_end: Option<f64>,
}

impl DiarizationWorker {
    /// Create a new diarization worker.
    pub fn new(config: DiarizationConfig, output_tx: Sender<DiarizedTranscript>) -> Self {
        log::info!(
            "DiarizationWorker created (threshold={}, max_speakers={}, gap={}s)",
            config.similarity_threshold,
            config.max_speakers,
            config.gap_threshold_secs,
        );
        Self {
            config,
            speakers: Vec::new(),
            output_tx,
            next_speaker_num: 1,
            last_segment_end: None,
        }
    }

    /// Run the diarization processing loop (blocking — spawn on a dedicated thread).
    ///
    /// Consumes `DiarizationInput`s from `input_rx` until the channel closes.
    pub fn run(mut self, input_rx: Receiver<DiarizationInput>) {
        log::info!("DiarizationWorker: entering processing loop");

        while let Ok(input) = input_rx.recv() {
            let result = self.process_input(input);

            if let Err(e) = self.output_tx.send(result) {
                log::warn!("DiarizationWorker: output channel closed, stopping: {}", e);
                return;
            }
        }

        log::info!(
            "DiarizationWorker: input channel closed, exiting. Tracked {} speaker(s)",
            self.speakers.len()
        );
    }

    /// Process a single diarization input and return an enriched transcript.
    pub fn process_input(&mut self, input: DiarizationInput) -> DiarizedTranscript {
        // 1. Extract audio features
        let features = Self::extract_features(&input.speech_audio);

        log::debug!(
            "DiarizationWorker: features for segment '{}': rms={:.4}, zcr={:.4}, mad={:.4}",
            input.transcript.id,
            features.rms_energy,
            features.zero_crossing_rate,
            features.spectral_centroid,
        );

        // 2. Compute time gap from previous segment
        let time_gap = match self.last_segment_end {
            Some(prev_end) => (input.transcript.start_time - prev_end).max(0.0),
            None => 0.0,
        };
        self.last_segment_end = Some(input.transcript.end_time);

        // 3. Find or create speaker
        let speaker_idx = self.find_or_create_speaker(&features, time_gap);

        // 4. Update the matched speaker's running features & stats
        let segment_duration =
            input.speech_end_time.as_secs_f64() - input.speech_start_time.as_secs_f64();
        {
            let speaker = &mut self.speakers[speaker_idx];
            update_features(&mut speaker.features, &features, speaker.segment_count);
            speaker.segment_count += 1;
            speaker.total_speaking_time += segment_duration;
        }

        let speaker = &self.speakers[speaker_idx];

        log::debug!(
            "DiarizationWorker: assigned to {} (distance-based, segments={}, total_time={:.1}s)",
            speaker.label,
            speaker.segment_count,
            speaker.total_speaking_time,
        );

        // 5. Build enriched transcript
        let mut segment = input.transcript;
        segment.speaker_id = Some(speaker.id.clone());
        segment.speaker_label = Some(speaker.label.clone());

        let speaker_info = SpeakerInfo {
            id: speaker.id.clone(),
            label: speaker.label.clone(),
            color: speaker.color.clone(),
            total_speaking_time: speaker.total_speaking_time,
            segment_count: speaker.segment_count,
        };

        DiarizedTranscript {
            segment,
            speaker_info,
        }
    }

    // ── Feature extraction ───────────────────────────────────────────

    /// Compute simple audio features from a 16 kHz mono f32 waveform.
    ///
    /// Returns [`AudioFeatures`] containing:
    /// - **RMS energy** — `sqrt(mean(x²))`
    /// - **Zero-crossing rate** — fraction of consecutive pairs that cross zero
    /// - **Mean absolute deviation** — `mean(|x - mean(x)|)`
    pub fn extract_features(audio: &[f32]) -> AudioFeatures {
        if audio.is_empty() {
            return AudioFeatures {
                rms_energy: 0.0,
                zero_crossing_rate: 0.0,
                spectral_centroid: 0.0,
            };
        }

        let n = audio.len() as f32;

        // RMS energy
        let sum_sq: f32 = audio.iter().map(|&x| x * x).sum();
        let rms_energy = (sum_sq / n).sqrt();

        // Zero-crossing rate
        let zero_crossings: usize = audio
            .windows(2)
            .filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0))
            .count();
        let zero_crossing_rate = if audio.len() > 1 {
            zero_crossings as f32 / (audio.len() - 1) as f32
        } else {
            0.0
        };

        // Mean absolute deviation (MAD)
        let mean: f32 = audio.iter().sum::<f32>() / n;
        let mad: f32 = audio.iter().map(|&x| (x - mean).abs()).sum::<f32>() / n;

        AudioFeatures {
            rms_energy,
            zero_crossing_rate,
            spectral_centroid: mad,
        }
    }

    // ── Speaker matching ─────────────────────────────────────────────

    /// Find the best matching speaker for the given features, or create a new one.
    ///
    /// Returns the index into `self.speakers`.
    fn find_or_create_speaker(&mut self, features: &AudioFeatures, time_gap: f64) -> usize {
        if self.speakers.is_empty() {
            // First speaker ever
            return self.create_speaker(features);
        }

        // Find closest existing speaker
        let (best_idx, best_dist) = self
            .speakers
            .iter()
            .enumerate()
            .map(|(i, sp)| (i, feature_distance(features, &sp.features)))
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .expect("speakers is non-empty");

        // Apply gap penalty: if there's a significant time gap, raise the
        // effective threshold—making it harder to match the same speaker.
        let effective_threshold = if time_gap > self.config.gap_threshold_secs {
            self.config.similarity_threshold * 0.7 // tighter threshold after a gap
        } else {
            self.config.similarity_threshold
        };

        log::debug!(
            "DiarizationWorker: best match = {} (dist={:.4}, threshold={:.4}, gap={:.2}s)",
            self.speakers[best_idx].label,
            best_dist,
            effective_threshold,
            time_gap,
        );

        if best_dist < effective_threshold {
            // Close enough — same speaker
            best_idx
        } else if self.speakers.len() < self.config.max_speakers {
            // Far enough — new speaker
            self.create_speaker(features)
        } else {
            // Cap reached — assign to the closest speaker anyway
            log::debug!(
                "DiarizationWorker: max speakers reached ({}), assigning to closest",
                self.config.max_speakers,
            );
            best_idx
        }
    }

    /// Create a new speaker profile and return its index.
    fn create_speaker(&mut self, features: &AudioFeatures) -> usize {
        let num = self.next_speaker_num;
        self.next_speaker_num += 1;

        let color_idx = (num as usize - 1) % SPEAKER_COLORS.len();

        let profile = SpeakerProfile {
            id: format!("speaker-{}", num),
            label: format!("Speaker {}", num),
            color: SPEAKER_COLORS[color_idx].to_string(),
            features: *features,
            segment_count: 0,
            total_speaking_time: 0.0,
        };

        log::info!(
            "DiarizationWorker: created new speaker '{}' (color={})",
            profile.label,
            profile.color,
        );

        self.speakers.push(profile);
        self.speakers.len() - 1
    }
}

// ── Free functions ───────────────────────────────────────────────────────

/// Compute normalised Euclidean distance between two feature vectors.
///
/// Each dimension is divided by its expected range so that all features
/// contribute roughly equally:
/// - RMS energy: range ≈ 0.0..0.5
/// - Zero-crossing rate: range ≈ 0.0..0.3
/// - MAD: range ≈ 0.0..0.3
pub fn feature_distance(a: &AudioFeatures, b: &AudioFeatures) -> f32 {
    let d_rms = (a.rms_energy - b.rms_energy) / 0.5;
    let d_zcr = (a.zero_crossing_rate - b.zero_crossing_rate) / 0.3;
    let d_mad = (a.spectral_centroid - b.spectral_centroid) / 0.3;
    ((d_rms * d_rms + d_zcr * d_zcr + d_mad * d_mad) / 3.0).sqrt()
}

/// Incrementally update a speaker's running-average features with a new
/// observation using an exponential moving average.
fn update_features(existing: &mut AudioFeatures, new: &AudioFeatures, count: u32) {
    let alpha = 1.0 / (count as f32 + 1.0);
    existing.rms_energy = existing.rms_energy * (1.0 - alpha) + new.rms_energy * alpha;
    existing.zero_crossing_rate =
        existing.zero_crossing_rate * (1.0 - alpha) + new.zero_crossing_rate * alpha;
    existing.spectral_centroid =
        existing.spectral_centroid * (1.0 - alpha) + new.spectral_centroid * alpha;
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // -- AudioFeatures / extract_features ----------------------------------

    #[test]
    fn extract_features_empty_audio() {
        let f = DiarizationWorker::extract_features(&[]);
        assert_eq!(f.rms_energy, 0.0);
        assert_eq!(f.zero_crossing_rate, 0.0);
        assert_eq!(f.spectral_centroid, 0.0);
    }

    #[test]
    fn extract_features_silence() {
        let audio = vec![0.0_f32; 16000]; // 1 second of silence
        let f = DiarizationWorker::extract_features(&audio);
        assert!(f.rms_energy.abs() < 1e-6);
        assert!(f.zero_crossing_rate.abs() < 1e-6);
        assert!(f.spectral_centroid.abs() < 1e-6);
    }

    #[test]
    fn extract_features_dc_offset() {
        // Constant positive signal: no zero crossings, nonzero RMS, zero MAD
        let audio = vec![0.5_f32; 1000];
        let f = DiarizationWorker::extract_features(&audio);
        assert!((f.rms_energy - 0.5).abs() < 1e-4);
        assert_eq!(f.zero_crossing_rate, 0.0);
        assert!(
            f.spectral_centroid.abs() < 1e-6,
            "MAD should be ~0 for constant signal"
        );
    }

    #[test]
    fn extract_features_alternating_signal() {
        // Alternating +1 / -1: every pair crosses zero.
        let audio: Vec<f32> = (0..1000)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let f = DiarizationWorker::extract_features(&audio);
        assert!(
            (f.rms_energy - 1.0).abs() < 1e-4,
            "RMS of ±1 signal should be 1.0"
        );
        assert!(
            (f.zero_crossing_rate - 1.0).abs() < 1e-3,
            "ZCR of fully alternating signal should be ~1.0, got {}",
            f.zero_crossing_rate
        );
        // MAD of ±1 with mean=0 should be 1.0
        assert!(
            (f.spectral_centroid - 1.0).abs() < 1e-3,
            "MAD should be ~1.0, got {}",
            f.spectral_centroid
        );
    }

    #[test]
    fn extract_features_sine_wave() {
        // 440 Hz sine at 16 kHz for 0.1s → 1600 samples
        let n = 1600;
        let audio: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 16000.0).sin())
            .collect();
        let f = DiarizationWorker::extract_features(&audio);
        // RMS of a sine wave = 1/sqrt(2) ≈ 0.707
        assert!(
            (f.rms_energy - 0.707).abs() < 0.02,
            "RMS of unit sine should be ~0.707, got {}",
            f.rms_energy
        );
        // ZCR ≈ 2 * freq / sample_rate = 2 * 440 / 16000 ≈ 0.055
        assert!(
            (f.zero_crossing_rate - 0.055).abs() < 0.01,
            "ZCR of 440 Hz signal at 16 kHz should be ~0.055, got {}",
            f.zero_crossing_rate
        );
        // MAD should be > 0
        assert!(f.spectral_centroid > 0.1);
    }

    // -- feature_distance ---------------------------------------------------

    #[test]
    fn feature_distance_identical() {
        let a = AudioFeatures {
            rms_energy: 0.1,
            zero_crossing_rate: 0.05,
            spectral_centroid: 0.08,
        };
        assert!((feature_distance(&a, &a)).abs() < 1e-6);
    }

    #[test]
    fn feature_distance_symmetry() {
        let a = AudioFeatures {
            rms_energy: 0.1,
            zero_crossing_rate: 0.05,
            spectral_centroid: 0.08,
        };
        let b = AudioFeatures {
            rms_energy: 0.3,
            zero_crossing_rate: 0.15,
            spectral_centroid: 0.12,
        };
        let d_ab = feature_distance(&a, &b);
        let d_ba = feature_distance(&b, &a);
        assert!((d_ab - d_ba).abs() < 1e-6, "distance should be symmetric");
    }

    #[test]
    fn feature_distance_scales_correctly() {
        let base = AudioFeatures {
            rms_energy: 0.0,
            zero_crossing_rate: 0.0,
            spectral_centroid: 0.0,
        };
        let far = AudioFeatures {
            rms_energy: 0.5,
            zero_crossing_rate: 0.3,
            spectral_centroid: 0.3,
        };
        let dist = feature_distance(&base, &far);
        // Each normalised difference is 1.0, so distance = sqrt((1+1+1)/3) = 1.0
        assert!(
            (dist - 1.0).abs() < 1e-4,
            "distance from origin to max should be 1.0, got {}",
            dist
        );
    }

    // -- update_features ----------------------------------------------------

    #[test]
    fn update_features_first_observation() {
        let mut existing = AudioFeatures {
            rms_energy: 0.2,
            zero_crossing_rate: 0.1,
            spectral_centroid: 0.05,
        };
        let new = AudioFeatures {
            rms_energy: 0.4,
            zero_crossing_rate: 0.2,
            spectral_centroid: 0.15,
        };
        // count=0 → alpha=1.0 → fully replace
        update_features(&mut existing, &new, 0);
        assert!((existing.rms_energy - 0.4).abs() < 1e-5);
        assert!((existing.zero_crossing_rate - 0.2).abs() < 1e-5);
        assert!((existing.spectral_centroid - 0.15).abs() < 1e-5);
    }

    #[test]
    fn update_features_converges_toward_new() {
        let mut existing = AudioFeatures {
            rms_energy: 0.0,
            zero_crossing_rate: 0.0,
            spectral_centroid: 0.0,
        };
        let target = AudioFeatures {
            rms_energy: 1.0,
            zero_crossing_rate: 1.0,
            spectral_centroid: 1.0,
        };
        // Repeatedly update — should converge toward target
        for count in 0..100 {
            update_features(&mut existing, &target, count);
        }
        assert!(
            (existing.rms_energy - 1.0).abs() < 0.05,
            "should converge toward 1.0, got {}",
            existing.rms_energy
        );
    }

    // -- DiarizationConfig default ------------------------------------------

    #[test]
    fn default_config_values() {
        let cfg = DiarizationConfig::default();
        assert!((cfg.similarity_threshold - 0.7).abs() < f32::EPSILON);
        assert_eq!(cfg.max_speakers, 10);
        assert!((cfg.gap_threshold_secs - 2.0).abs() < f64::EPSILON);
    }

    // -- Speaker creation and assignment ------------------------------------

    #[test]
    fn process_input_creates_first_speaker() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut worker = DiarizationWorker::new(DiarizationConfig::default(), tx);

        let input = make_test_input(vec![0.1; 8000], 0.0, 0.5);
        let result = worker.process_input(input);

        assert_eq!(result.segment.speaker_id, Some("speaker-1".to_string()));
        assert_eq!(result.segment.speaker_label, Some("Speaker 1".to_string()));
        assert_eq!(result.speaker_info.id, "speaker-1");
        assert_eq!(result.speaker_info.color, "#4A90D9");
        assert_eq!(result.speaker_info.segment_count, 1);
        assert_eq!(worker.speakers.len(), 1);

        // Channel should be valid (nothing sent via `run`, but worker is usable)
        drop(rx);
    }

    #[test]
    fn process_input_same_speaker_for_similar_audio() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let mut worker = DiarizationWorker::new(DiarizationConfig::default(), tx);

        // Two segments with very similar audio
        let input1 = make_test_input(vec![0.1; 8000], 0.0, 0.5);
        let input2 = make_test_input(vec![0.1; 8000], 0.5, 1.0);

        let r1 = worker.process_input(input1);
        let r2 = worker.process_input(input2);

        assert_eq!(r1.segment.speaker_id, r2.segment.speaker_id);
        assert_eq!(worker.speakers.len(), 1);
        assert_eq!(worker.speakers[0].segment_count, 2);
    }

    #[test]
    fn process_input_different_speaker_for_different_audio() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let config = DiarizationConfig {
            similarity_threshold: 0.3, // tighter threshold to force separation
            ..DiarizationConfig::default()
        };
        let mut worker = DiarizationWorker::new(config, tx);

        // Very different audio characteristics
        let quiet_dc = vec![0.05_f32; 8000]; // quiet, no crossings
        let loud_alternating: Vec<f32> = (0..8000)
            .map(|i| if i % 2 == 0 { 0.8 } else { -0.8 })
            .collect();

        let input1 = make_test_input(quiet_dc, 0.0, 0.5);
        let input2 = make_test_input(loud_alternating, 1.0, 1.5);

        let r1 = worker.process_input(input1);
        let r2 = worker.process_input(input2);

        assert_ne!(
            r1.segment.speaker_id, r2.segment.speaker_id,
            "very different audio should yield different speakers"
        );
        assert_eq!(worker.speakers.len(), 2);
    }

    #[test]
    fn max_speakers_cap_is_respected() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let config = DiarizationConfig {
            similarity_threshold: 0.001, // extremely tight — almost everything is "different"
            max_speakers: 3,
            ..DiarizationConfig::default()
        };
        let mut worker = DiarizationWorker::new(config, tx);

        // Create inputs with varying amplitude to try to force different speakers
        for i in 0..5 {
            let amp = 0.1 + i as f32 * 0.15;
            let audio = vec![amp; 8000];
            let start = i as f64;
            let input = make_test_input(audio, start, start + 0.5);
            worker.process_input(input);
        }

        assert!(
            worker.speakers.len() <= 3,
            "should not exceed max_speakers=3, got {}",
            worker.speakers.len()
        );
    }

    #[test]
    fn speaker_colors_cycle() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let config = DiarizationConfig {
            similarity_threshold: 0.0, // every segment is a new speaker
            max_speakers: 12,
            ..DiarizationConfig::default()
        };
        let mut worker = DiarizationWorker::new(config, tx);

        for i in 0..12 {
            let amp = 0.05 + i as f32 * 0.05;
            let audio = vec![amp; 8000];
            let start = i as f64 * 10.0; // large gaps
            let input = make_test_input(audio, start, start + 0.5);
            worker.process_input(input);
        }

        // 11th speaker (index 10) should wrap around to color[0]
        assert_eq!(worker.speakers.len(), 12);
        assert_eq!(worker.speakers[10].color, SPEAKER_COLORS[0]);
    }

    // -- Helpers -----------------------------------------------------------

    fn make_test_input(audio: Vec<f32>, start_secs: f64, end_secs: f64) -> DiarizationInput {
        DiarizationInput {
            transcript: TranscriptSegment {
                id: uuid::Uuid::new_v4().to_string(),
                source_id: "test-source".to_string(),
                speaker_id: None,
                speaker_label: None,
                text: "test text".to_string(),
                start_time: start_secs,
                end_time: end_secs,
                confidence: 0.9,
            },
            speech_audio: audio,
            speech_start_time: Duration::from_secs_f64(start_secs),
            speech_end_time: Duration::from_secs_f64(end_secs),
        }
    }
}
