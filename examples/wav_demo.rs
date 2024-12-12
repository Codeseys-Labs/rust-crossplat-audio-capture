use rsac::{get_audio_backend, AudioConfig, AudioFormat};
use std::{fs::File, io::Write, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get the default audio backend
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
        let mut file = File::create("demo_capture.raw")?;
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
        println!("Capture complete! Saved to demo_capture.raw");
    }

    Ok(())
}
