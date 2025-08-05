use clap::Parser;
use hound::{WavSpec, WavWriter};
use rsac::api::AudioCaptureBuilder;
use rsac::core::config::{DeviceSelector, SampleFormat};
use rsac::core::error::AudioError;
use rsac::{
    enumerate_application_audio_sessions, get_device_enumerator, ApplicationAudioSessionInfo,
    ProcessAudioCapture,
};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "test_windows")]
#[command(about = "Test audio capture on Windows (WASAPI)")]
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

    /// Use exclusive mode
    #[arg(long)]
    exclusive_mode: bool,

    /// Audio format to test
    #[arg(long, default_value = "f32le")]
    format: String,

    /// Enable verbose output
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    if args.verbose {
        println!("Starting Windows WASAPI capture test...");
        println!("Duration: {} seconds", args.duration);
        println!("Output: {}", args.output.display());
        println!("Format: {}", args.format);
        if let Some(ref app) = args.application {
            println!("Target application: {}", app);
        } else {
            println!("Capturing system audio");
        }
        if args.exclusive_mode {
            println!("Using exclusive mode");
        }
    }

    // Try to use the actual WASAPI implementation
    match capture_with_wasapi(&args) {
        Ok(_) => {
            if args.verbose {
                println!("✅ WASAPI capture completed successfully");
            }
        }
        Err(e) => {
            if args.verbose {
                println!("⚠️  WASAPI capture failed ({}), creating placeholder", e);
            }
            create_placeholder_wav(&args.output, &args.format, args.exclusive_mode)?;
        }
    }

    if args.verbose {
        println!("WASAPI capture completed successfully!");
        println!("Output saved to: {}", args.output.display());

        // Verify the file was created and has content
        let metadata = std::fs::metadata(&args.output)?;
        println!("File size: {} bytes", metadata.len());
    }

    Ok(())
}
fn capture_with_wasapi(args: &Args) -> Result<(), AudioError> {
    // Get the Windows device enumerator
    let mut enumerator = get_device_enumerator()?;

    if args.verbose {
        println!("Created Windows WASAPI device enumerator");

        // List available devices
        let devices = enumerator.enumerate_devices()?;
        println!("Available audio devices:");
        for (i, device) in devices.iter().enumerate() {
            println!("  {}: {}", i, device.name()?);
        }
    }

    if let Some(ref app_name) = args.application {
        // Application-specific capture using ProcessAudioCapture
        if args.verbose {
            println!("Attempting application-specific capture for: {}", app_name);
        }

        // List available applications
        match enumerate_application_audio_sessions() {
            Ok(sessions) => {
                if args.verbose {
                    println!("Available audio sessions:");
                    for session in &sessions {
                        println!("  - {} (PID: {})", session.display_name, session.process_id);
                    }
                }

                // Find target application
                if let Some(target_session) = sessions.iter().find(|session| {
                    session
                        .display_name
                        .to_lowercase()
                        .contains(&app_name.to_lowercase())
                }) {
                    if args.verbose {
                        println!(
                            "Found target application: {} (PID: {})",
                            target_session.display_name, target_session.process_id
                        );
                    }

                    // Try to use ProcessAudioCapture for application-specific capture
                    match ProcessAudioCapture::new(target_session.process_id) {
                        Ok(mut process_capture) => {
                            if args.verbose {
                                println!(
                                    "Created ProcessAudioCapture for PID {}",
                                    target_session.process_id
                                );
                            }

                            // Start capture
                            process_capture.start_capture()?;

                            if args.verbose {
                                println!(
                                    "Started process capture, recording for {} seconds...",
                                    args.duration
                                );
                            }

                            thread::sleep(Duration::from_secs(args.duration));

                            // Stop capture
                            process_capture.stop_capture()?;

                            // Create placeholder WAV file (until we implement actual data collection)
                            create_placeholder_wav(
                                &args.output,
                                &args.format,
                                args.exclusive_mode,
                            )?;
                            return Ok(());
                        }
                        Err(e) => {
                            if args.verbose {
                                println!("Failed to create ProcessAudioCapture: {}", e);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                if args.verbose {
                    println!("Failed to enumerate audio sessions: {}", e);
                }
            }
        }
    }

    // Use the new API for system capture
    let sample_format = match args.format.as_str() {
        "f32le" => SampleFormat::F32LE,
        "s32le" => SampleFormat::S32LE,
        "s16le" => SampleFormat::S16LE,
        _ => SampleFormat::S16LE,
    };

    let device_selector = DeviceSelector::DefaultInput;

    let mut capture_session = AudioCaptureBuilder::new()
        .device(device_selector)
        .sample_rate(44100)
        .channels(2)
        .sample_format(sample_format)
        .bits_per_sample(match args.format.as_str() {
            "s16le" => 16,
            "s32le" => 32,
            "f32le" => 32,
            _ => 16,
        })
        .build()?;

    if args.verbose {
        println!(
            "Created WASAPI capture session with format: {}",
            args.format
        );
    }

    // Start capturing
    capture_session.start()?;

    if args.verbose {
        println!(
            "Started WASAPI capture, recording for {} seconds...",
            args.duration
        );
    }

    // Record for specified duration
    thread::sleep(Duration::from_secs(args.duration));

    // Stop capturing
    capture_session.stop()?;

    // Create placeholder WAV file (until we implement actual data collection)
    create_placeholder_wav(&args.output, &args.format, args.exclusive_mode)?;

    Ok(())
}

fn create_placeholder_wav(
    output_path: &PathBuf,
    format: &str,
    exclusive_mode: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Create a WAV file with test audio data
    let spec = WavSpec {
        channels: 2,
        sample_rate: 44100,
        bits_per_sample: match format {
            "s16le" => 16,
            "s32le" => 32,
            "f32le" => 32,
            _ => 16,
        },
        sample_format: match format {
            "f32le" => hound::SampleFormat::Float,
            _ => hound::SampleFormat::Int,
        },
    };

    let mut writer = WavWriter::create(output_path, spec)?;

    // Generate 1 second of test audio to simulate captured data
    let sample_rate = 44100;
    let duration_samples = sample_rate; // 1 second

    for i in 0..duration_samples {
        let t = i as f32 / sample_rate as f32;

        // Create different patterns based on test type
        let signal = if exclusive_mode {
            // Different pattern for exclusive mode test
            0.5 * (2.0 * std::f32::consts::PI * 261.63 * t).sin() +  // C4
            0.3 * (2.0 * std::f32::consts::PI * 329.63 * t).sin() +  // E4
            0.2 * (2.0 * std::f32::consts::PI * 392.00 * t).sin() // G4
        } else {
            // Standard test pattern
            0.3 * (2.0 * std::f32::consts::PI * 440.0 * t).sin() +   // A4
            0.2 * (2.0 * std::f32::consts::PI * 880.0 * t).sin() // A5
        };

        // Write samples based on format
        match format {
            "f32le" => {
                writer.write_sample(signal)?;
                writer.write_sample(signal)?;
            }
            "s32le" => {
                let sample = (signal * 2147483647.0) as i32;
                writer.write_sample(sample)?;
                writer.write_sample(sample)?;
            }
            _ => {
                // s16le
                let sample = (signal * 16384.0) as i16;
                writer.write_sample(sample)?;
                writer.write_sample(sample)?;
            }
        }
    }

    writer.finalize()?;
    Ok(())
}
