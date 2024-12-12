#[cfg(target_os = "windows")]
use rsac::{
    // Trait-based API
    get_audio_backend,
    AudioConfig,
    AudioFormat,
    // Legacy ProcessAudioCapture API
    ProcessAudioCapture,
};
use std::{fs::File, io::Write, time::Duration};

#[cfg(target_os = "windows")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Example 1: Using the trait-based API
    println!("Example 1: Using trait-based API");
    {
        let backend = get_audio_backend()?;
        println!("Using audio backend: {}", backend.name());

        // List available applications
        let apps = backend.list_applications()?;
        println!("\nAvailable audio sources:");
        for (i, app) in apps.iter().enumerate() {
            println!(
                "{}: {} (ID: {}, Process: {})",
                i, app.name, app.id, app.executable_name
            );
        }

        if let Some(app) = apps.first() {
            println!("\nCapturing audio from: {}", app.name);

            // Create audio configuration
            let config = AudioConfig {
                sample_rate: 48000,
                channels: 2,
                format: AudioFormat::F32LE,
            };

            // Create capture stream
            let mut stream = backend.capture_application(app, config)?;

            // Start capturing
            stream.start()?;
            println!("Started capturing...");

            // Create output file
            let mut file = File::create("trait_api_capture.raw")?;
            let mut buffer = vec![0u8; 4096];
            let start = std::time::Instant::now();
            let duration = Duration::from_secs(5);

            // Capture for 5 seconds
            while start.elapsed() < duration {
                let bytes_read = stream.read(&mut buffer)?;
                if bytes_read > 0 {
                    file.write_all(&buffer[..bytes_read])?;
                    print!("\rCaptured {} bytes", bytes_read);
                }
            }
            println!();

            // Stop capturing
            stream.stop()?;
            println!("Capture complete! Saved to trait_api_capture.raw");
        }
    }

    println!("\n-----------------------------------\n");

    // Example 2: Using the legacy ProcessAudioCapture API
    println!("Example 2: Using ProcessAudioCapture API");
    {
        let mut capture = ProcessAudioCapture::new()?;
        capture.set_verbose(true);

        // List available processes
        let processes = ProcessAudioCapture::list_processes()?;
        println!("\nAvailable processes:");
        for (i, process) in processes.iter().enumerate() {
            println!("{}: {}", i, process);
        }

        if let Some(process) = processes.first() {
            println!("\nCapturing audio from: {}", process);

            // Initialize capture for the process
            capture.init_for_process(process)?;

            // Start capturing
            capture.start()?;
            println!("Started capturing...");

            // Create output file
            let mut file = File::create("legacy_api_capture.raw")?;
            let start = std::time::Instant::now();
            let duration = Duration::from_secs(5);

            // Capture for 5 seconds
            while start.elapsed() < duration {
                let data = capture.get_data()?;
                if !data.is_empty() {
                    file.write_all(&data)?;
                }
            }

            // Stop capturing
            capture.stop()?;
            println!("Capture complete! Saved to legacy_api_capture.raw");

            // Print audio format info
            println!("\nAudio format:");
            println!("  Channels: {}", capture.channels().unwrap_or(0));
            println!("  Sample rate: {} Hz", capture.sample_rate().unwrap_or(0));
            println!(
                "  Bits per sample: {}",
                capture.bits_per_sample().unwrap_or(0)
            );
        }
    }

    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn main() {
    println!("This example only works on Windows");
}
