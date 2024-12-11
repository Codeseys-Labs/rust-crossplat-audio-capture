use color_eyre::Result;
use rsac::pipeline::{AudioChunk, PipelineComponent, TranscriptionComponent, TranscriptionConfig};

const TARGET_SAMPLE_RATE: u32 = 16000;
const CHUNK_DURATION: usize = 10; // 10 seconds

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize error handling
    color_eyre::install()?;

    println!("Testing transcription component...");
    let whisper_model = "models/whisper-base.bin";

    // Create transcription configuration
    let transcription_config = TranscriptionConfig {
        model_path: whisper_model.to_string(),
        language: "en".to_string(),
        translate: false,
        min_segment_duration: 0.1,
        timestamp_enabled: true,
        sample_rate: TARGET_SAMPLE_RATE,
    };

    // Initialize transcriber
    println!("Initializing transcriber...");
    let mut transcriber = TranscriptionComponent::initialize(transcription_config).await?;
    println!("Transcriber initialized.");

    // Read audio file
    println!("Reading audio file...");
    let (samples, sample_rate) = sherpa_rs::read_audio_file("podcast_16k.wav")?;
    println!("Read {} samples at {} Hz", samples.len(), sample_rate);

    // Process audio in chunks
    println!("\nProcessing audio...");
    let chunk_size = TARGET_SAMPLE_RATE as usize * CHUNK_DURATION;
    for (i, chunk) in samples.chunks(chunk_size).enumerate() {
        println!("\nProcessing chunk {} ({} samples)...", i + 1, chunk.len());

        // Create audio chunk
        let audio_chunk = AudioChunk::new(
            chunk.to_vec(),
            (i * CHUNK_DURATION) as f64,
            TARGET_SAMPLE_RATE,
        );

        // Process through transcriber
        match transcriber.process(audio_chunk).await {
            Ok(output) => {
                println!("  Segments: {}", output.segments.len());
                for segment in output.segments {
                    println!(
                        "  [{}s - {}s]: {}",
                        segment.start_time, segment.end_time, segment.text
                    );
                }
            }
            Err(e) => eprintln!("Error processing chunk: {}", e),
        }
    }

    Ok(())
}
