use rsac::audio::{get_audio_backend, AudioConfig, AudioFormat};
use std::{fs::File, io::Write, time::Duration};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Get the audio backend (will be CoreAudio on macOS)
    let backend = get_audio_backend()?;
    println!("Using audio backend: {}", backend.name());

    // List available applications
    let apps = backend.list_applications()?;
    println!("\nAvailable audio sources:");
    for (i, app) in apps.iter().enumerate() {
        println!(
            "{}: {} (PID: {}, Bundle: {})",
            i, app.name, app.pid, app.executable_name
        );
    }

    if apps.is_empty() {
        println!("No audio sources found.");
        return Ok(());
    }

    // Select an application (skip system audio)
    let app = apps.iter().find(|a| a.pid != 0).unwrap_or(&apps[0]);
    println!("\nCapturing audio from: {} (PID: {})", app.name, app.pid);

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
    let filename = format!("{}_audio.raw", app.name.replace(" ", "_"));
    let mut file = File::create(&filename)?;
    let mut buffer = vec![0u8; 4096];
    let start = std::time::Instant::now();

    // Capture for 5 seconds
    while start.elapsed() < Duration::from_secs(5) {
        let bytes_read = stream.read(&mut buffer)?;
        if bytes_read > 0 {
            file.write_all(&buffer[..bytes_read])?;
            print!("\rCaptured {} bytes", bytes_read);
        }
    }
    println!();

    // Stop capturing
    stream.stop()?;
    println!("Capture complete! Audio saved to: {}", filename);

    Ok(())
}