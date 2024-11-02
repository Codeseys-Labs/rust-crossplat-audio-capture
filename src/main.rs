use clap::{Parser, ValueEnum};
use color_eyre::eyre::Result;
use hound::{SampleFormat, WavSpec, WavWriter};
use indicatif::{ProgressBar, ProgressStyle};
use inquire::{Select, Text};
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, ValueEnum)]
enum OutputFormat {
    Raw,
    Wav,
    Both,
}

#[derive(Parser, Debug)]
#[command(author, version, about = "Record audio from specific applications")]
struct Args {
    /// Process name or substring to capture audio from
    #[arg(short, long)]
    process: Option<String>,

    /// Duration to capture in seconds (omit for unbounded recording)
    #[arg(short, long)]
    duration: Option<u64>,

    /// Output directory for captured audio
    #[arg(short, long, default_value = ".")]
    output_dir: PathBuf,

    /// Output format (raw, wav, or both)
    #[arg(short = 'f', long, value_enum, default_value = "both")]
    format: OutputFormat,

    /// Filter process list (when selecting interactively)
    #[arg(short = 'i', long, help = "Filter the process list (e.g. 'spotify')")]
    filter: Option<String>,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<()> {
    // Initialize error handling
    color_eyre::install()?;

    // Parse command line arguments
    let args = Args::parse();

    // Setup Ctrl+C handler for unbounded recording
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    if args.verbose {
        println!("Creating audio capture instance...");
    }
    let mut capture = rsac::ProcessAudioCapture::new().map_err(|e| color_eyre::eyre::eyre!(e))?;
    capture.set_verbose(args.verbose);

    // Get process name either from args or interactive selection
    let process_name = if let Some(name) = args.process {
        name
    } else {
        select_process(args.filter.as_deref())?
    };

    println!("\n🎯 Initializing capture for {}", process_name);
    capture
        .init_for_process(&process_name)
        .map_err(|e| color_eyre::eyre::eyre!(e))?;

    println!("▶️  Starting capture...");
    capture.start().map_err(|e| color_eyre::eyre::eyre!(e))?;

    // Get audio format
    let channels = capture.channels().unwrap_or(2) as u16;
    let sample_rate = capture.sample_rate().unwrap_or(44100) as u32;
    let bits_per_sample = capture.bits_per_sample().unwrap_or(16) as u16;

    if args.verbose {
        println!("\n🎵 Audio format:");
        println!("  Channels: {}", channels);
        println!("  Sample rate: {} Hz", sample_rate);
        println!("  Bits per sample: {}", bits_per_sample);
    }

    // Create output directory if it doesn't exist
    std::fs::create_dir_all(&args.output_dir)?;

    // Track if we're creating WAV output
    let wav_path = if matches!(args.format, OutputFormat::Wav | OutputFormat::Both) {
        let path = args.output_dir.join(format!("{}_audio.wav", process_name));
        Some(path)
    } else {
        None
    };

    // Setup output files based on format
    let mut wav_writer = wav_path
        .as_ref()
        .map(|path| {
            let spec = WavSpec {
                channels,
                sample_rate,
                bits_per_sample,
                sample_format: SampleFormat::Float,
            };
            WavWriter::create(path, spec)
        })
        .transpose()?;

    let mut raw_file = match args.format {
        OutputFormat::Raw | OutputFormat::Both => {
            let raw_path = args.output_dir.join(format!("{}_audio.raw", process_name));
            Some(File::create(raw_path)?)
        }
        _ => None,
    };

    let log_path = args
        .output_dir
        .join(format!("{}_capture.log", process_name));
    let mut log_file = File::create(log_path)?;

    // Setup progress display
    let pb =
        match args.duration {
            Some(duration) => {
                let pb = ProgressBar::new(duration);
                pb.set_style(
                ProgressStyle::default_bar()
                    .template(
                        "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len}s",
                    )?
                    .progress_chars("█▉▊▋▌▍▎▏ "),
            );
                pb
            }
            None => {
                let pb = ProgressBar::new_spinner();
                pb.set_style(ProgressStyle::default_spinner().template(
                    "{spinner:.green} [{elapsed_precise}] Recording... (Ctrl+C to stop)",
                )?);
                pb
            }
        };

    println!(
        "\n⏱️  {}",
        match args.duration {
            Some(duration) => format!("Capturing audio for {} seconds...", duration),
            None => "Capturing audio (press Ctrl+C to stop)...".to_string(),
        }
    );

    let start = std::time::Instant::now();
    let mut total_bytes = 0;
    let mut packets = 0;
    let mut silent_packets = 0;
    let mut last_log = std::time::Instant::now();
    let log_interval = Duration::from_millis(500); // Log every 500ms

    while running.load(Ordering::SeqCst)
        && args
            .duration
            .map(|d| start.elapsed() < Duration::from_secs(d))
            .unwrap_or(true)
    {
        match capture.get_data().map_err(|e| color_eyre::eyre::eyre!(e))? {
            data if !data.is_empty() => {
                packets += 1;
                total_bytes += data.len();

                // Check if the packet contains any non-zero bytes
                let is_silent = data.iter().all(|&x| x == 0);
                if is_silent {
                    silent_packets += 1;
                }

                // Log less frequently to reduce console spam
                if last_log.elapsed() >= log_interval {
                    if args.verbose {
                        pb.set_message(format!("Packets: {} ({} silent)", packets, silent_packets));
                    }
                    last_log = std::time::Instant::now();
                }

                let log_msg = format!(
                    "[{:?}] Packet {}: {} bytes (total: {}) {}\n",
                    start.elapsed(),
                    packets,
                    data.len(),
                    total_bytes,
                    if is_silent { "[silent]" } else { "" }
                );
                log_file.write_all(log_msg.as_bytes())?;

                // Write audio data based on format
                if let Some(ref mut file) = raw_file {
                    file.write_all(&data)?;
                }

                if let Some(ref mut writer) = wav_writer {
                    for chunk in data.chunks(4) {
                        if chunk.len() == 4 {
                            let sample =
                                f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                            writer.write_sample(sample)?;
                        }
                    }
                }
            }
            _ => {
                thread::sleep(Duration::from_millis(1));
            }
        }

        match args.duration {
            Some(_duration) => pb.set_position(start.elapsed().as_secs()),
            None => pb.inc(1),
        }
    }

    pb.finish_and_clear();

    let avg_bytes = if packets > 0 {
        total_bytes / packets
    } else {
        0
    };
    let silent_percent = if packets > 0 {
        (silent_packets as f64 / packets as f64) * 100.0
    } else {
        0.0
    };

    println!("\n📊 Capture Summary");
    println!("───────────────────────────────");
    println!("Total packets:     {}", packets);
    println!(
        "Silent packets:    {} ({:.1}%)",
        silent_packets, silent_percent
    );
    println!(
        "Total data:        {:.2} MB",
        total_bytes as f64 / 1_048_576.0
    );
    println!("Avg packet size:   {} bytes", avg_bytes);
    println!("Duration:          {:.1}s", start.elapsed().as_secs_f64());
    println!("───────────────────────────────");

    println!("\n⏹️  Stopping capture...");
    capture.stop().map_err(|e| color_eyre::eyre::eyre!(e))?;

    // Finalize WAV file if it exists
    if let Some(writer) = wav_writer {
        writer.finalize()?;
    }

    // Print output locations
    println!("\n📁 Output Files");
    println!("───────────────────────────────");
    if raw_file.is_some() {
        println!(
            "📄 Raw audio:    {}",
            args.output_dir
                .join(format!("{}_audio.raw", process_name))
                .display()
        );
    }
    if wav_path.is_some() {
        println!(
            "🎵 WAV audio:    {}",
            args.output_dir
                .join(format!("{}_audio.wav", process_name))
                .display()
        );
    }
    println!(
        "📝 Capture log:  {}",
        args.output_dir
            .join(format!("{}_capture.log", process_name))
            .display()
    );

    Ok(())
}

fn select_process(filter: Option<&str>) -> Result<String> {
    println!("📋 Listing running processes...");
    let mut processes =
        rsac::ProcessAudioCapture::list_processes().map_err(|e| color_eyre::eyre::eyre!(e))?;

    // Apply filter if provided
    if let Some(filter) = filter {
        processes.retain(|p| p.to_lowercase().contains(&filter.to_lowercase()));
    }

    if processes.is_empty() {
        if filter.is_some() {
            return Err(color_eyre::eyre::eyre!(
                "No processes found matching filter: {}",
                filter.unwrap()
            ));
        } else {
            return Err(color_eyre::eyre::eyre!("No processes found"));
        }
    }

    // Allow manual input if needed
    let mut options = processes.clone();
    options.push("Enter process name manually".to_string());

    let selected = Select::new("Select a process to capture audio from:", options).prompt()?;

    if selected == "Enter process name manually" {
        let manual = Text::new("Enter process name:").prompt()?;
        Ok(manual)
    } else {
        Ok(selected)
    }
}
