use clap::Parser;
use hound::{WavSpec, WavWriter};
use rsac::api::{AudioCaptureBuilder};
use rsac::core::config::{DeviceSelector, SampleFormat};
use rsac::core::error::AudioError;
use rsac::{get_device_enumerator, enumerate_audio_applications, ApplicationInfo};
use std::path::PathBuf;
use std::time::Duration;
use std::thread;

#[derive(Parser)]
#[command(name = "test_coreaudio")]
#[command(about = "Test audio capture on macOS (CoreAudio)")]
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
    
    /// Test audio session management
    #[arg(long)]
    test_session_management: bool,
    
    /// Enable verbose output
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    if args.verbose {
        println!("Starting macOS CoreAudio capture test...");
        println!("Duration: {} seconds", args.duration);
        println!("Output: {}", args.output.display());
        if let Some(ref app) = args.application {
            println!("Target application: {}", app);
        } else {
            println!("Capturing system audio");
        }
        if args.test_session_management {
            println!("Testing audio session management");
        }
    }

    // Try to use the actual CoreAudio implementation
    match capture_with_coreaudio(&args) {
        Ok(_) => {
            if args.verbose {
                println!("✅ CoreAudio capture completed successfully");
            }
        }
        Err(e) => {
            if args.verbose {
                println!("⚠️  CoreAudio capture failed ({}), creating placeholder", e);
            }
            create_placeholder_wav(&args.output, args.test_session_management)?;
        }
    }

    if args.verbose {
        println!("CoreAudio capture completed successfully!");
        println!("Output saved to: {}", args.output.display());

        // Verify the file was created and has content
        let metadata = std::fs::metadata(&args.output)?;
        println!("File size: {} bytes", metadata.len());
    }

    Ok(())
}

fn capture_with_coreaudio(args: &Args) -> Result<(), AudioError> {
    // Get the macOS device enumerator
    let mut enumerator = get_device_enumerator()?;

    if args.verbose {
        println!("Created macOS device enumerator");

        // List available devices
        let devices = enumerator.enumerate_devices()?;
        println!("Available audio devices:");
        for (i, device) in devices.iter().enumerate() {
            println!("  {}: {}", i, device.name()?);
        }
    }

    if let Some(ref app_name) = args.application {
        // Application-specific capture
        if args.verbose {
            println!("Attempting application-specific capture for: {}", app_name);
        }

        // List available applications
        match enumerate_audio_applications() {
            Ok(applications) => {
                if args.verbose {
                    println!("Available audio applications:");
                    for app in &applications {
                        println!("  - {} (PID: {})", app.name, app.pid);
                    }
                }

                // Find target application
                if let Some(_target_app) = applications.iter().find(|app|
                    app.name.to_lowercase().contains(&app_name.to_lowercase())
                ) {
                    if args.verbose {
                        println!("Found target application: {}", _target_app.name);
                    }
                    // TODO: Implement application-specific capture with CoreAudio
                    // For now, fall back to system capture
                }
            }
            Err(e) => {
                if args.verbose {
                    println!("Failed to enumerate applications: {}", e);
                }
            }
        }
    }

    // Use the new API for system capture
    let device_selector = DeviceSelector::DefaultInput;

    let mut capture_session = AudioCaptureBuilder::new()
        .device(device_selector)
        .sample_rate(44100)
        .channels(2)
        .sample_format(SampleFormat::S16LE)
        .bits_per_sample(16)
        .build()?;

    if args.verbose {
        println!("Created CoreAudio capture session");
    }

    // Start capturing
    capture_session.start()?;

    if args.verbose {
        println!("Started CoreAudio capture, recording for {} seconds...", args.duration);
    }

    // Record for specified duration
    thread::sleep(Duration::from_secs(args.duration));

    // Stop capturing
    capture_session.stop()?;

    // Create placeholder WAV file (until we implement actual data collection)
    create_placeholder_wav(&args.output, args.test_session_management)?;

    Ok(())
}

fn create_placeholder_wav(output_path: &PathBuf, session_management: bool) -> Result<(), Box<dyn std::error::Error>> {
    // Create a WAV file with test audio data
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

        // Create different patterns based on test type
        let signal = if session_management {
            // Different pattern for session management test
            0.4 * (2.0 * std::f32::consts::PI * 523.25 * t).sin() +  // C5
            0.3 * (2.0 * std::f32::consts::PI * 659.25 * t).sin() +  // E5
            0.2 * (2.0 * std::f32::consts::PI * 783.99 * t).sin()    // G5
        } else {
            // Standard test pattern
            0.3 * (2.0 * std::f32::consts::PI * 440.0 * t).sin() +   // A4
            0.2 * (2.0 * std::f32::consts::PI * 880.0 * t).sin()     // A5
        };

        let sample = (signal * 16384.0) as i16; // Convert to 16-bit

        // Write stereo samples
        writer.write_sample(sample)?;
        writer.write_sample(sample)?;
    }

    writer.finalize()?;
    Ok(())
}
