use clap::Parser;
use rsac::{get_audio_backend, AudioConfig, AudioFormat};
use std::{fs::File, io::Write, time::Duration};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// List available audio sources
    #[arg(long)]
    list_sources: bool,

    /// Audio source to capture from
    #[arg(long)]
    source: Option<String>,

    /// Duration in seconds to capture
    #[arg(long)]
    duration: Option<u64>,

    /// Output file (default: audio_capture.raw)
    #[arg(long)]
    output: Option<String>,

    /// Audio format (f32le, s16le, s32le)
    #[arg(long)]
    format: Option<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Get the default audio backend
    let backend = get_audio_backend()?;
    println!("Using audio backend: {}", backend.name());

    // List available applications
    let apps = backend.list_applications()?;

    if args.list_sources {
        println!("\nAvailable audio sources:");
        for (i, app) in apps.iter().enumerate() {
            println!(
                "{}: {} (ID: {}, Process: {})",
                i, app.name, app.id, app.executable_name
            );
        }
        return Ok(());
    }

    // Find the requested source or use the first one
    let app = if let Some(source_name) = args.source {
        apps.iter()
            .find(|app| app.name.contains(&source_name))
            .ok_or_else(|| format!("No source found matching '{}'", source_name))?
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
        format: match args.format.as_deref() {
            Some("f32le") => AudioFormat::F32LE,
            Some("s16le") => AudioFormat::S16LE,
            Some("s32le") => AudioFormat::S32LE,
            _ => AudioFormat::F32LE,
        },
    };

    // Create capture stream
    let mut stream = backend.capture_application(app, config)?;

    // Start capturing
    stream.start()?;
    println!("Started capturing...");

    // Create output file
    let output = args
        .output
        .unwrap_or_else(|| "audio_capture.raw".to_string());
    let mut file = File::create(output)?;
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
