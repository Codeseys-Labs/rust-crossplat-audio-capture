use clap::Parser;
use rsac::audio::{get_audio_backend, AudioConfig, AudioFormat};
use std::{fs::File, io::Write, time::Duration};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// List available audio devices
    #[arg(long)]
    list_devices: bool,

    /// Audio device to capture from
    #[arg(long)]
    device: Option<String>,

    /// Duration in seconds to capture
    #[arg(long)]
    duration: Option<u64>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let backend = get_audio_backend()?;
    println!("Using audio backend: {}", backend.name());

    // List available applications/devices
    let apps = backend.list_applications()?;
    
    if args.list_devices {
        println!("\nAvailable audio sources:");
        for (i, app) in apps.iter().enumerate() {
            println!(
                "{}: {} (ID: {}, Process: {})",
                i, app.name, app.id, app.executable_name
            );
        }
        return Ok(());
    }

    // Find the requested device or use the first one
    let app = if let Some(device_name) = args.device {
        apps.iter()
            .find(|app| app.name.contains(&device_name))
            .ok_or_else(|| format!("No device found matching '{}'", device_name))?
    } else if !apps.is_empty() {
        &apps[0]
    } else {
        return Err("No audio sources found".into());
    };

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
    let mut file = File::create("audio_capture.raw")?;
    let mut buffer = vec![0u8; 4096];
    let start = std::time::Instant::now();
    let duration = Duration::from_secs(args.duration.unwrap_or(5));

    // Capture for the specified duration
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
    println!("Capture complete!");

    Ok(())
}