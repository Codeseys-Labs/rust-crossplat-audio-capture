use color_eyre::Result;
use rsac::{
    pipeline::{
        AudioChunk, DiarizationComponent, DiarizationConfig, PipelineComponent,
        TranscriptionComponent, TranscriptionConfig,
    },
    process_audio,
};
use std::fs;

const TARGET_SAMPLE_RATE: u32 = 16000;
const CHUNK_DURATION: usize = 10; // 10 seconds

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize error handling
    color_eyre::install()?;

    // First verify the models exist
    println!("Checking model files...");
    let segment_model = "models/sherpa-onnx-pyannote-segmentation-3-0/model.onnx";
    let embedding_model = "models/3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx";
    let whisper_model = "models\\whisper-base.bin";

    assert!(
        fs::metadata(segment_model).is_ok(),
        "Segmentation model not found"
    );
    assert!(
        fs::metadata(embedding_model).is_ok(),
        "Embedding model not found"
    );
    assert!(
        fs::metadata(whisper_model).is_ok(),
        "Whisper model not found"
    );

    println!("All model files present.");

    // Read audio file
    println!("Reading audio file...");
    let (samples, sample_rate) = sherpa_rs::read_audio_file("podcast_16k.wav")?;
    println!("Read {} samples at {} Hz", samples.len(), sample_rate);

    // Test transcription component first
    println!("\nTesting transcription component...");
    let transcription_config = TranscriptionConfig {
        model_path: whisper_model.to_string(),
        language: "en".to_string(),
        translate: false,
        min_segment_duration: 0.1,
        timestamp_enabled: true,
        sample_rate: TARGET_SAMPLE_RATE,
    };

    println!("Initializing transcriber...");
    let mut transcriber = TranscriptionComponent::initialize(transcription_config).await?;
    println!("Transcriber initialized.");

    // Process first chunk through transcriber
    println!("Testing transcriber with first chunk...");
    let chunk_size = TARGET_SAMPLE_RATE as usize * CHUNK_DURATION;
    if let Some(chunk) = samples.chunks(chunk_size).next() {
        let audio_chunk = AudioChunk::new(chunk.to_vec(), 0.0, TARGET_SAMPLE_RATE);

        let result = transcriber.process(audio_chunk).await?;
        println!(
            "Transcriber test successful: {} segments",
            result.segments.len()
        );
        for segment in result.segments {
            println!(
                "  [{}s - {}s]: {}",
                segment.start_time, segment.end_time, segment.text
            );
        }
    }

    // Test diarization component
    println!("\nTesting diarization component...");
    let diarization_config = DiarizationConfig {
        segment_model_path: segment_model.to_string(),
        embedding_model_path: embedding_model.to_string(),
        max_speakers: 2,
        min_segment_duration: 0.5,
        overlap_threshold: 0.5,
        sample_rate: TARGET_SAMPLE_RATE,
    };

    println!("Initializing diarizer...");
    let mut diarizer = DiarizationComponent::initialize(diarization_config).await?;
    println!("Diarizer initialized.");

    // Process first chunk through diarizer
    println!("Testing diarizer with first chunk...");
    if let Some(chunk) = samples.chunks(chunk_size).next() {
        let audio_chunk = AudioChunk::new(chunk.to_vec(), 0.0, TARGET_SAMPLE_RATE);

        let result = diarizer.process(audio_chunk).await?;
        println!(
            "Diarizer test successful: {} segments",
            result.segments.len()
        );
        for segment in result.segments {
            println!(
                "  [{}s - {}s] Speaker {}",
                segment.start_time, segment.end_time, segment.speaker_id
            );
        }
    }

    // If both components work individually, try processing through pipeline
    println!("\nTesting full pipeline...");
    for (i, chunk) in samples.chunks(chunk_size).enumerate() {
        println!("\nProcessing chunk {} ({} samples)...", i + 1, chunk.len());

        let audio_chunk = AudioChunk::new(
            chunk.to_vec(),
            (i * CHUNK_DURATION) as f64,
            TARGET_SAMPLE_RATE,
        );

        match process_audio(audio_chunk, &mut diarizer, &mut transcriber).await {
            Ok(segments) => {
                println!("  Segments: {}", segments.len());
                for segment in segments {
                    println!(
                        "  [{}s - {}s] Speaker {}: {}",
                        segment.start_time, segment.end_time, segment.speaker_id, segment.text
                    );
                }
            }
            Err(e) => eprintln!("Error processing chunk: {}", e),
        }
    }

    Ok(())
}
