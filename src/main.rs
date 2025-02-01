#[cfg(target_os = "windows")]
mod windows_main {
    use clap::{Parser, ValueEnum};
    use color_eyre::eyre::Result;
    use hound::{SampleFormat, WavSpec, WavWriter};
    use indicatif::{ProgressBar, ProgressStyle};
    use inquire::{Select, Text};
    use rsac::audio::ProcessAudioCapture;
    use std::fs::File;
    use std::io::{self, IsTerminal, Write};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[derive(Debug, Clone, ValueEnum)]
    #[value(rename_all = "lowercase")]
    enum OutputFormat {
        /// Raw PCM audio data
        Raw,
        /// WAV audio file
        Wav,
        /// Both RAW and WAV formats
        Both,
    }

    #[derive(Parser, Debug)]
    #[command(
        author,
        version,
        about = "Record audio from specific Windows applications",
        long_about = "A tool for capturing audio output from specific Windows applications. \
                    By default, outputs raw PCM audio data to stdout for piping to other tools. \
                    Can also save to files when an output directory is specified.",
        after_help = "EXAMPLES:\n\
                    # Pipe audio from Spotify to ffplay for real-time playback:\n\
                    rsac -p Spotify.exe | ffplay -f f32le -ar 48000 -ac 2 -i -\n\n\
                    # Save Spotify audio to WAV file:\n\
                    rsac -p Spotify.exe -o recordings -f wav\n\n\
                    # Record Chrome with logging enabled:\n\
                    rsac -p chrome.exe -o recordings -l\n\n\
                    # Interactive process selection with filter:\n\
                    rsac -i chrome -o recordings"
    )]
    struct Args {
        /// Process name to capture audio from (e.g., 'Spotify.exe', 'chrome.exe')
        #[arg(short, long, help_heading = "TARGET")]
        process: Option<String>,

        /// Filter the process list (e.g., 'spotify' shows only matching processes)
        #[arg(
            short = 'i',
            long,
            help_heading = "TARGET",
            help = "Filter the process list (e.g., 'spotify')"
        )]
        filter: Option<String>,

        /// Duration to capture in seconds (omit for unbounded recording)
        #[arg(
            short,
            long,
            help_heading = "RECORDING",
            help = "Duration to capture in seconds (Ctrl+C to stop if omitted)"
        )]
        duration: Option<u64>,

        /// Output directory for saving audio files (if not specified, output goes to stdout)
        #[arg(
            short,
            long,
            help_heading = "OUTPUT",
            help = "Directory to save audio files (omitted = pipe to stdout)"
        )]
        output_dir: Option<PathBuf>,

        /// Output format for saved files (raw, wav, or both)
        #[arg(
            short = 'f',
            long,
            value_enum,
            default_value = "raw",
            help_heading = "OUTPUT",
            help = "Format for saved files (only used with --output-dir)"
        )]
        format: OutputFormat,

        /// Enable detailed logging (only available with --output-dir)
        #[arg(
            short = 'l',
            long,
            help_heading = "OUTPUT",
            help = "Enable detailed logging (requires --output-dir)"
        )]
        enable_logging: bool,

        /// Show verbose output
        #[arg(
            short,
            long,
            help_heading = "DISPLAY",
            help = "Show additional status information"
        )]
        verbose: bool,
    }

    pub fn run() -> Result<()> {
        // Initialize error handling
        color_eyre::install()?;

        // Parse command line arguments
        let args = Args::parse();

        // Validate arguments
        if args.enable_logging && args.output_dir.is_none() {
            return Err(color_eyre::eyre::eyre!(
                "Logging can only be enabled when an output directory is specified (--output-dir)"
            ));
        }

        // Check if we're trying to pipe binary data to a terminal
        if args.output_dir.is_none() && io::stdout().is_terminal() {
            return Err(color_eyre::eyre::eyre!(
                "Cannot output raw audio data to terminal.\n\
                Either:\n\
                1. Pipe the output to an audio player:\n\
                    rsac -p Spotify.exe | ffplay -f f32le -ar 48000 -ac 2 -i -\n\
                2. Save to file:\n\
                    rsac -p Spotify.exe -o recordings"
            ));
        }

        // Setup Ctrl+C handler for unbounded recording
        let running = Arc::new(AtomicBool::new(true));
        let r = running.clone();
        ctrlc::set_handler(move || {
            r.store(false, Ordering::SeqCst);
        })?;

        if args.verbose {
            eprintln!("Creating audio capture instance...");
        }
        let mut capture = ProcessAudioCapture::new().map_err(|e| color_eyre::eyre::eyre!(e))?;
        capture.set_verbose(args.verbose);

        // Get process name either from args or interactive selection
        let process_name = if let Some(name) = args.process {
            name
        } else {
            select_process(args.filter.as_deref())?
        };

        eprintln!("\n🎯 Target process: {}", process_name);
        capture
            .init_for_process(&process_name)
            .map_err(|e| color_eyre::eyre::eyre!(e))?;

        // Get audio format
        let channels = capture.channels().unwrap_or(2) as u16;
        let sample_rate = capture.sample_rate().unwrap_or(44100) as u32;
        let bits_per_sample = capture.bits_per_sample().unwrap_or(16) as u16;

        if args.verbose {
            eprintln!("\n🎵 Audio format:");
            eprintln!("  Channels: {}", channels);
            eprintln!("  Sample rate: {} Hz", sample_rate);
            eprintln!("  Bits per sample: {}", bits_per_sample);
        }

        // Setup output mode
        if let Some(ref output_dir) = args.output_dir {
            eprintln!("\n💾 File output mode:");
            eprintln!("  Directory: {}", output_dir.display());
            eprintln!("  Format: {:?}", args.format);
            if args.enable_logging {
                eprintln!("  Logging: Enabled");
            }
        } else {
            eprintln!("\n🔄 Pipe output mode (raw PCM audio):");
            eprintln!(
                "  Format: 32-bit float, {} Hz, {} channels",
                sample_rate, channels
            );
            eprintln!("  Pipe command example:");
            eprintln!(
                "  | ffplay -f f32le -ar {} -ac {} -i -",
                sample_rate, channels
            );
        }

        // Setup file output if directory is specified
        let (mut wav_writer, mut raw_file, mut log_file) =
            if let Some(ref output_dir) = args.output_dir {
                std::fs::create_dir_all(output_dir)?;

                let wav_writer = if matches!(args.format, OutputFormat::Wav | OutputFormat::Both) {
                    let path = output_dir.join(format!("{}_audio.wav", process_name));
                    let spec = WavSpec {
                        channels,
                        sample_rate,
                        bits_per_sample,
                        sample_format: SampleFormat::Float,
                    };
                    Some(WavWriter::create(path, spec)?)
                } else {
                    None
                };

                let raw_file = match args.format {
                    OutputFormat::Raw | OutputFormat::Both => {
                        let raw_path = output_dir.join(format!("{}_audio.raw", process_name));
                        Some(File::create(raw_path)?)
                    }
                    _ => None,
                };

                let log_file = if args.enable_logging {
                    let log_path = output_dir.join(format!("{}_capture.log", process_name));
                    Some(File::create(log_path)?)
                } else {
                    None
                };

                (wav_writer, raw_file, log_file)
            } else {
                (None, None, None)
            };

        eprintln!("\n▶️  Starting capture...");
        capture.start().map_err(|e| color_eyre::eyre::eyre!(e))?;

        // Setup progress display
        let pb = match args.duration {
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

        eprintln!(
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
        let stdout = io::stdout();
        let mut stdout = stdout.lock();

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
                            pb.set_message(format!(
                                "Packets: {} ({} silent)",
                                packets, silent_packets
                            ));
                        }
                        last_log = std::time::Instant::now();
                    }

                    // Write to log file if enabled
                    if let Some(ref mut file) = log_file {
                        let log_msg = format!(
                            "[{:?}] Packet {}: {} bytes (total: {}) {}\n",
                            start.elapsed(),
                            packets,
                            data.len(),
                            total_bytes,
                            if is_silent { "[silent]" } else { "" }
                        );
                        file.write_all(log_msg.as_bytes())?;
                    }

                    // Write audio data based on output configuration
                    if args.output_dir.is_none() {
                        // Pipe to stdout when no output directory is specified
                        stdout.write_all(&data)?;
                    } else {
                        // Write to files based on format
                        if let Some(ref mut file) = raw_file {
                            file.write_all(&data)?;
                        }

                        if let Some(ref mut writer) = wav_writer {
                            for chunk in data.chunks(4) {
                                if chunk.len() == 4 {
                                    let sample = f32::from_le_bytes([
                                        chunk[0], chunk[1], chunk[2], chunk[3],
                                    ]);
                                    writer.write_sample(sample)?;
                                }
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

        // Only show summary and file locations when not piping to stdout
        if args.output_dir.is_some() {
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

            eprintln!("\n📊 Capture Summary");
            eprintln!("───────────────────────────────");
            eprintln!("Total packets:     {}", packets);
            eprintln!(
                "Silent packets:    {} ({:.1}%)",
                silent_packets, silent_percent
            );
            eprintln!(
                "Total data:        {:.2} MB",
                total_bytes as f64 / 1_048_576.0
            );
            eprintln!("Avg packet size:   {} bytes", avg_bytes);
            eprintln!("Duration:          {:.1}s", start.elapsed().as_secs_f64());
            eprintln!("───────────────────────────────");

            eprintln!("\n⏹️  Stopping capture...");
            capture.stop().map_err(|e| color_eyre::eyre::eyre!(e))?;

            // Finalize WAV file if it exists
            if let Some(writer) = wav_writer {
                writer.finalize()?;
            }

            // Print output locations
            eprintln!("\n📁 Output Files");
            eprintln!("───────────────────────────────");
            if raw_file.is_some() {
                eprintln!(
                    "📄 Raw audio:    {}",
                    args.output_dir
                        .as_ref()
                        .unwrap()
                        .join(format!("{}_audio.raw", process_name))
                        .display()
                );
            }
            if matches!(args.format, OutputFormat::Wav | OutputFormat::Both) {
                eprintln!(
                    "🎵 WAV audio:    {}",
                    args.output_dir
                        .as_ref()
                        .unwrap()
                        .join(format!("{}_audio.wav", process_name))
                        .display()
                );
            }
            if args.enable_logging {
                eprintln!(
                    "📝 Capture log:  {}",
                    args.output_dir
                        .as_ref()
                        .unwrap()
                        .join(format!("{}_capture.log", process_name))
                        .display()
                );
            }
        }

        Ok(())
    }

    fn select_process(filter: Option<&str>) -> Result<String> {
        eprintln!("📋 Listing running processes...");
        let mut processes =
            ProcessAudioCapture::list_processes().map_err(|e| color_eyre::eyre::eyre!(e))?;

        // Apply filter if provided
        if let Some(filter) = filter {
            processes.retain(|p| p.to_lowercase().contains(&filter.to_lowercase()));
            eprintln!("  Filter: '{}' ({} matches)", filter, processes.len());
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

        // Sort processes for easier selection
        processes.sort_by_key(|p| p.to_lowercase());

        // Allow manual input if needed
        let mut options = processes.clone();
        options.push("Enter process name manually".to_string());

        let selected = Select::new(
            "Select a process to capture audio from (type to filter):",
            options,
        )
        .prompt()?;

        if selected == "Enter process name manually" {
            let manual = Text::new("Enter process name (e.g., 'Spotify.exe'):").prompt()?;
            Ok(manual)
        } else {
            Ok(selected)
        }
    }
}

fn main() -> color_eyre::Result<()> {
    #[cfg(target_os = "windows")]
    {
        windows_main::run()
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err(color_eyre::eyre::eyre!(
            "This application is only supported on Windows"
        ))
    }
}
