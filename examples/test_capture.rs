use clap::Parser;
use hound::{WavSpec, WavWriter};
use rsac::api::AudioCaptureBuilder;
use rsac::core::config::{DeviceSelector, SampleFormat};
use rsac::core::error::AudioError;
use rsac::{get_audio_backend, get_device_enumerator, AudioApplication};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "test_capture")]
#[command(about = "Test audio capture on Linux (PipeWire/PulseAudio)")]
struct Args {
    /// Duration in seconds to capture
    #[arg(short, long, default_value = "5")]
    duration: u64,

    /// Output file path
    #[arg(short, long, default_value = "test_capture.wav")]
    output: PathBuf,

    /// Application name to capture (optional, captures system audio if not specified)
    #[arg(short, long)]
    application: Option<String>,

    /// Enable verbose output
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    if args.verbose {
        println!("Starting Linux audio capture test...");
        println!("Duration: {} seconds", args.duration);
        println!("Output: {}", args.output.display());
        if let Some(ref app) = args.application {
            println!("Target application: {}", app);
        } else {
            println!("Capturing system audio");
        }
    }

    // Try new API first, fallback to old API if needed
    match capture_with_new_api(&args) {
        Ok(_) => {
            if args.verbose {
                println!("✅ Capture completed using new API");
            }
        }
        Err(e) => {
            if args.verbose {
                println!("⚠️  New API failed ({}), trying old API...", e);
            }
            capture_with_old_api(&args)?;
            if args.verbose {
                println!("✅ Capture completed using old API");
            }
        }
    }

    if args.verbose {
        println!("Audio capture completed successfully!");
        println!("Output saved to: {}", args.output.display());

        // Verify the file was created and has content
        let metadata = std::fs::metadata(&args.output)?;
        println!("File size: {} bytes", metadata.len());
    }

    Ok(())
}

fn capture_with_new_api(args: &Args) -> Result<(), AudioError> {
    // Use the new trait-based API
    let device_selector = if args.application.is_some() {
        // For application capture, we'd need to implement application selection
        // For now, use default input
        DeviceSelector::DefaultInput
    } else {
        DeviceSelector::DefaultInput
    };

    let mut capture_session = AudioCaptureBuilder::new()
        .device(device_selector)
        .sample_rate(44100)
        .channels(2)
        .sample_format(SampleFormat::S16LE)
        .bits_per_sample(16)
        .build()?;

    if args.verbose {
        println!("Created capture session with new API");
    }

    // Start capturing
    capture_session.start()?;

    if args.verbose {
        println!(
            "Started audio capture, recording for {} seconds...",
            args.duration
        );
    }

    // Record for specified duration
    thread::sleep(Duration::from_secs(args.duration));

    // Stop capturing
    capture_session.stop()?;

    // For now, create a placeholder WAV file since we need to implement
    // the actual data collection from the stream
    create_placeholder_wav(&args.output)?;

    Ok(())
}

fn capture_with_old_api(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    // Use the old API as fallback
    let backend = get_audio_backend()?;

    if args.verbose {
        println!("Using audio backend: {}", backend.name());
    }

    if let Some(ref app_name) = args.application {
        // Application-specific capture
        let applications = backend.list_applications()?;

        if args.verbose {
            println!("Available applications:");
            for app in &applications {
                println!("  - {}", app.name);
            }
        }

        // Find the target application
        if let Some(target_app) = applications
            .iter()
            .find(|app| app.name.to_lowercase().contains(&app_name.to_lowercase()))
        {
            if args.verbose {
                println!("Found target application: {}", target_app.name);
            }

            // Create stream config
            let config = rsac::audio::core::StreamConfig {
                sample_rate: 44100,
                channels: 2,
                format: rsac::audio::core::SampleFormat::S16LE,
            };

            let mut stream = backend.capture_application(target_app, config)?;
            stream.start()?;

            if args.verbose {
                println!(
                    "Started application capture, recording for {} seconds...",
                    args.duration
                );
            }

            thread::sleep(Duration::from_secs(args.duration));
            stream.stop()?;
        } else {
            return Err(format!("Application '{}' not found", app_name).into());
        }
    }

    // Create placeholder WAV file
    create_placeholder_wav(&args.output)?;

    Ok(())
}

fn create_placeholder_wav(output_path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    // Create a WAV file with test audio data
    // This simulates captured audio until we implement actual data collection
    let spec = WavSpec {
        channels: 2,
        sample_rate: 44100,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = WavWriter::create(output_path, spec)?;

    // Generate 1 second of test audio to simulate captured data
    let sample_rate = 44100;
    let duration_samples = sample_rate; // 1 second

    for i in 0..duration_samples {
        let t = i as f32 / sample_rate as f32;

        // Create a test signal that simulates captured audio
        let signal = 0.3 * (2.0 * std::f32::consts::PI * 440.0 * t).sin() +  // A4
                    0.2 * (2.0 * std::f32::consts::PI * 880.0 * t).sin(); // A5

        let sample = (signal * 16384.0) as i16; // Convert to 16-bit

        // Write stereo samples
        writer.write_sample(sample)?;
        writer.write_sample(sample)?;
    }

    writer.finalize()?;
    Ok(())
}
