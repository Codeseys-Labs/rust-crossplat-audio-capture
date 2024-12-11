use clap::Parser;
use hound::{SampleFormat, WavReader, WavWriter};
use rsac::audio::{get_audio_backend, AudioConfig, AudioFormat};
use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    time::Duration,
};

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

    /// Input file for testing
    #[arg(long)]
    file: Option<PathBuf>,

    /// Output file (default: audio_capture.raw)
    #[arg(long)]
    output: Option<PathBuf>,

    /// Audio format (f32le, s16le, s32le)
    #[arg(long)]
    format: Option<String>,

    /// Convert output to WAV format
    #[arg(long)]
    to_wav: bool,
}

fn process_wav_file(
    input: PathBuf,
    output: PathBuf,
    format: AudioFormat,
    to_wav: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut reader = WavReader::open(input)?;
    let spec = reader.spec();
    println!("Input WAV specs: {:?}", spec);

    if to_wav {
        // Create WAV writer
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48000,
            bits_per_sample: match format {
                AudioFormat::F32LE => 32,
                AudioFormat::S16LE => 16,
                AudioFormat::S32LE => 32,
            },
            sample_format: match format {
                AudioFormat::F32LE => SampleFormat::Float,
                AudioFormat::S16LE | AudioFormat::S32LE => SampleFormat::Int,
            },
        };
        let mut writer = WavWriter::create(output, spec)?;

        // Process samples
        match format {
            AudioFormat::F32LE => {
                for sample in reader.samples::<f32>() {
                    writer.write_sample(sample?)?;
                }
            }
            AudioFormat::S16LE => {
                for sample in reader.samples::<i16>() {
                    writer.write_sample(sample?)?;
                }
            }
            AudioFormat::S32LE => {
                for sample in reader.samples::<i32>() {
                    writer.write_sample(sample?)?;
                }
            }
        }
        writer.finalize()?;
    } else {
        // Write raw data
        let mut writer = File::create(output)?;
        let mut buffer = Vec::new();
        reader.into_inner().read_to_end(&mut buffer)?;
        writer.write_all(&buffer)?;
    }

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // File processing mode
    if let Some(input) = args.file {
        let output = args.output.unwrap_or_else(|| PathBuf::from("audio_capture.raw"));
        let format = match args.format.as_deref() {
            Some("f32le") => AudioFormat::F32LE,
            Some("s16le") => AudioFormat::S16LE,
            Some("s32le") => AudioFormat::S32LE,
            _ => AudioFormat::F32LE,
        };

        return process_wav_file(input, output, format, args.to_wav);
    }

    // Live capture mode
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
    let output = args.output.unwrap_or_else(|| PathBuf::from("audio_capture.raw"));
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
}