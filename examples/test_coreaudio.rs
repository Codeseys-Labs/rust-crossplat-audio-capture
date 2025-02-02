use clap::Parser;
use hound::{WavSpec, WavWriter};
use rsac::{get_audio_backend, AudioConfig, AudioFormat};
use std::{
    process::Command,
    thread,
    time::{Duration, Instant},
};

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: u16 = 2;

#[derive(Parser, Debug)]
#[command(about = "Test CoreAudio capture functionality")]
struct Args {
    /// Duration in seconds to capture
    #[arg(long, default_value = "5")]
    duration: u64,

    /// Output WAV file path
    #[arg(long, default_value = "test_capture.wav")]
    output: String,

    /// Skip audio validation
    #[arg(long)]
    skip_validation: bool,
}

fn validate_wav_file(
    path: &str,
    expected_duration: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    let reader = hound::WavReader::open(path)?;
    let spec = reader.spec();

    // Validate format
    if spec.channels != CHANNELS {
        return Err(format!(
            "Invalid channel count: got {}, expected {}",
            spec.channels, CHANNELS
        )
        .into());
    }
    if spec.sample_rate != SAMPLE_RATE {
        return Err(format!(
            "Invalid sample rate: got {}, expected {}",
            spec.sample_rate, SAMPLE_RATE
        )
        .into());
    }

    // Validate duration
    let samples = reader.duration();
    let actual_duration = Duration::from_secs_f64(samples as f64 / spec.sample_rate as f64);
    let duration_diff = (actual_duration.as_secs_f64() - expected_duration.as_secs_f64()).abs();

    if duration_diff > 1.0 {
        return Err(format!(
            "Capture duration mismatch: got {:.2}s, expected {:.2}s",
            actual_duration.as_secs_f64(),
            expected_duration.as_secs_f64()
        )
        .into());
    }

    // Validate there is actual audio data
    let samples: Vec<f32> = reader.into_samples().filter_map(Result::ok).collect();
    if samples.is_empty() {
        return Err("No audio samples found in WAV file".into());
    }

    // Check for non-zero audio data
    let has_audio = samples.iter().any(|&s| s != 0.0);
    if !has_audio {
        return Err("WAV file contains only silence".into());
    }

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    println!("Starting CoreAudio capture test...");

    // Start test tone in background
    println!("Starting test tone...");
    let mut test_process = Command::new("cargo")
        .args(["run", "--example", "test_tone"])
        .spawn()?;

    // Give the process a moment to start
    thread::sleep(Duration::from_secs(1));

    // Get the audio backend
    let backend = get_audio_backend()?;
    println!("Using audio backend: {}", backend.name());

    // List and find our test process
    let apps = backend.list_applications()?;
    println!("\nAvailable audio sources:");
    for (i, app) in apps.iter().enumerate() {
        println!(
            "{}: {} (ID: {}, Process: {})",
            i, app.name, app.id, app.executable_name
        );
    }

    // Look for test_tone process
    let app = apps
        .iter()
        .find(|app| app.executable_name.to_lowercase().contains("test_tone"))
        .ok_or("Could not find test tone process")?;

    println!("\nCapturing audio from: {}", app.name);

    // Create audio configuration
    let config = AudioConfig {
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
        format: AudioFormat::F32LE,
    };

    // Create capture stream
    let mut stream = backend.capture_application(app, config)?;

    // Start capturing
    stream.start()?;
    println!("Started capturing...");

    // Create WAV writer
    let spec = WavSpec {
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut wav_writer = WavWriter::create(&args.output, spec)?;

    let mut buffer = vec![0u8; 4096];
    let start = Instant::now();
    let duration = Duration::from_secs(args.duration);
    let mut total_bytes = 0u32;

    // Capture for the specified duration
    while start.elapsed() < duration {
        let bytes_read = stream.read(&mut buffer)?;
        if bytes_read > 0 {
            // Convert bytes to f32 samples and write to WAV
            let samples = unsafe {
                std::slice::from_raw_parts(
                    buffer[..bytes_read].as_ptr() as *const f32,
                    bytes_read / 4,
                )
            };
            for &sample in samples {
                wav_writer.write_sample(sample)?;
            }
            total_bytes += bytes_read as u32;
            print!("\rCaptured {} bytes", total_bytes);
        }
    }
    println!();

    // Stop capturing and finalize WAV file
    stream.stop()?;
    wav_writer.finalize()?;

    println!("Capture complete! Saved to {}", args.output);
    println!("Total bytes captured: {}", total_bytes);

    // Validate the captured audio
    if !args.skip_validation {
        println!("Validating captured audio...");
        validate_wav_file(&args.output, duration)?;
        println!("Audio validation passed!");
    }

    // Clean up test process quietly
    let _ = test_process.kill();

    println!("Test completed successfully!");
    Ok(())
}
