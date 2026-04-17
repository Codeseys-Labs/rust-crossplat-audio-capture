//! rsac — Cross-platform audio capture CLI demo.
//!
//! This is a thin demo application that exercises the `rsac` public library API.
//! It intentionally avoids platform-specific code (`#[cfg(target_os)]`) so it
//! compiles on every supported OS.

use clap::{Parser, Subcommand};
use color_eyre::eyre::{Result, WrapErr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rsac::{
    get_device_enumerator, AudioCaptureBuilder, CaptureTarget, PlatformCapabilities, ProcessId,
};

// ── CLI definition ───────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "rsac",
    version,
    about = "Cross-platform audio capture demo",
    long_about = "A thin CLI demonstrating the rsac library's streaming-first \
                  audio capture API.  Supports system audio, per-application, and \
                  per-process capture on Windows (WASAPI), Linux (PipeWire), and \
                  macOS (CoreAudio Process Tap)."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show platform capabilities
    Info,
    /// List available audio devices
    List,
    /// Capture audio and show a live level meter
    Capture {
        /// Capture a specific application by name
        #[arg(long, conflicts_with = "pid")]
        app: Option<String>,
        /// Capture a specific process by PID
        #[arg(long)]
        pid: Option<u32>,
        /// Sample rate in Hz
        #[arg(long, default_value = "48000")]
        sample_rate: u32,
        /// Number of audio channels
        #[arg(long, default_value = "2")]
        channels: u16,
    },
    /// Record audio to a WAV file
    Record {
        /// Output WAV file path
        output: String,
        /// Capture a specific application by name
        #[arg(long, conflicts_with = "pid")]
        app: Option<String>,
        /// Capture a specific process by PID
        #[arg(long)]
        pid: Option<u32>,
        /// Recording duration in seconds (omit for unbounded)
        #[arg(long)]
        duration: Option<u64>,
        /// Sample rate in Hz
        #[arg(long, default_value = "48000")]
        sample_rate: u32,
        /// Number of audio channels
        #[arg(long, default_value = "2")]
        channels: u16,
    },
}

// ── Entry point ──────────────────────────────────────────────────────────

fn main() -> Result<()> {
    color_eyre::install()?;
    env_logger::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Info => cmd_info(),
        Commands::List => cmd_list(),
        Commands::Capture {
            app,
            pid,
            sample_rate,
            channels,
        } => cmd_capture(app, pid, sample_rate, channels),
        Commands::Record {
            output,
            app,
            pid,
            duration,
            sample_rate,
            channels,
        } => cmd_record(output, app, pid, duration, sample_rate, channels),
    }
}

// ── Subcommands ──────────────────────────────────────────────────────────

/// `rsac info` — print platform capabilities.
fn cmd_info() -> Result<()> {
    let caps = PlatformCapabilities::query();

    println!("rsac — Platform Capabilities");
    println!("════════════════════════════════════════");
    println!("  Backend:              {}", caps.backend_name);
    println!(
        "  System capture:       {}",
        yes_no(caps.supports_system_capture)
    );
    println!(
        "  Application capture:  {}",
        yes_no(caps.supports_application_capture)
    );
    println!(
        "  Process tree capture: {}",
        yes_no(caps.supports_process_tree_capture)
    );
    println!(
        "  Device selection:     {}",
        yes_no(caps.supports_device_selection)
    );
    println!(
        "  Sample rate range:    {} – {} Hz",
        caps.sample_rate_range.0, caps.sample_rate_range.1
    );
    println!("  Max channels:         {}", caps.max_channels);
    println!(
        "  Sample formats:       {}",
        caps.supported_sample_formats
            .iter()
            .map(|f| format!("{:?}", f))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}

/// `rsac list` — list available audio devices.
fn cmd_list() -> Result<()> {
    let caps = PlatformCapabilities::query();
    println!("rsac — Audio Devices");
    println!("════════════════════════════════════════");
    println!("  Backend: {}", caps.backend_name);
    println!();

    // Capabilities overview
    println!("  Capabilities:");
    println!(
        "    System audio capture:      {}",
        if caps.supports_system_capture {
            "✓"
        } else {
            "✗"
        }
    );
    println!(
        "    Application capture:       {}",
        if caps.supports_application_capture {
            "✓"
        } else {
            "✗"
        }
    );
    println!(
        "    Process tree capture:      {}",
        if caps.supports_process_tree_capture {
            "✓"
        } else {
            "✗"
        }
    );
    println!(
        "    Device selection:          {}",
        if caps.supports_device_selection {
            "✓"
        } else {
            "✗"
        }
    );
    println!();

    // Device enumeration
    match get_device_enumerator() {
        Ok(enumerator) => {
            // Default device
            match enumerator.get_default_device() {
                Ok(device) => {
                    println!("  Default device: {} (ID: {})", device.name(), device.id());
                }
                Err(e) => {
                    println!("  Default device: unavailable ({})", e);
                }
            }
            println!();

            // All devices
            match enumerator.enumerate_devices() {
                Ok(devices) => {
                    if devices.is_empty() {
                        println!("  No audio devices found.");
                    } else {
                        println!("  Found {} device(s):", devices.len());
                        println!();
                        for device in &devices {
                            let default_marker = if device.is_default() {
                                " [default]"
                            } else {
                                ""
                            };
                            println!("    • {}{}", device.name(), default_marker);
                            println!("      ID: {}", device.id());
                            let formats = device.supported_formats();
                            if !formats.is_empty() {
                                for fmt in &formats {
                                    println!(
                                        "      Format: {}ch {}Hz {:?}",
                                        fmt.channels, fmt.sample_rate, fmt.sample_format
                                    );
                                }
                            }
                            println!();
                        }
                    }
                }
                Err(e) => {
                    println!("  Failed to enumerate devices: {}", e);
                }
            }
        }
        Err(e) => {
            println!("  Device enumeration unavailable: {}", e);
            println!();
            println!("  Use `rsac capture` or `rsac record` with default settings");
            println!("  to capture from the system default device.");
        }
    }

    Ok(())
}

/// `rsac capture` — capture audio and display a live ASCII level meter.
fn cmd_capture(
    app: Option<String>,
    pid: Option<u32>,
    sample_rate: u32,
    channels: u16,
) -> Result<()> {
    let target = build_target(&app, &pid);
    let target_label = target_label(&app, &pid);

    eprintln!("🎙  Capture target: {}", target_label);
    eprintln!(
        "    Sample rate: {} Hz, Channels: {}",
        sample_rate, channels
    );
    eprintln!("    Press Ctrl+C to stop.\n");

    // Build the capture session
    let mut capture = AudioCaptureBuilder::new()
        .with_target(target)
        .sample_rate(sample_rate)
        .channels(channels)
        .build()
        .wrap_err("Failed to build audio capture")?;

    capture.start().wrap_err("Failed to start capture")?;

    // Ctrl+C handler
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })
    .wrap_err("Failed to set Ctrl+C handler")?;

    let start = Instant::now();
    let mut total_frames: u64 = 0;
    let mut buffer_count: u64 = 0;

    while running.load(Ordering::SeqCst) {
        match capture.read_buffer() {
            Ok(Some(buffer)) => {
                let data = buffer.data();
                let frames = buffer.num_frames();
                total_frames += frames as u64;
                buffer_count += 1;

                let level = rms_level(data);
                let bar = level_bar(level, 40);
                let db = if level > 0.0 {
                    20.0 * level.log10()
                } else {
                    -f32::INFINITY
                };

                eprint!(
                    "\r  {} {:6.1} dB  | frames: {:>8} | buffers: {:>6}",
                    bar, db, total_frames, buffer_count
                );
            }
            Ok(None) => {
                // No data available yet — brief sleep to avoid busy-wait
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => {
                eprintln!("\n⚠  Read error: {}", e);
                break;
            }
        }
    }

    capture.stop().wrap_err("Failed to stop capture")?;

    let elapsed = start.elapsed();
    eprintln!("\n");
    eprintln!("📊 Capture Summary");
    eprintln!("───────────────────────────────────");
    eprintln!("  Duration:   {:.1}s", elapsed.as_secs_f64());
    eprintln!("  Buffers:    {}", buffer_count);
    eprintln!("  Frames:     {}", total_frames);
    if elapsed.as_secs_f64() > 0.0 {
        eprintln!(
            "  Throughput: {:.0} frames/s",
            total_frames as f64 / elapsed.as_secs_f64()
        );
    }

    Ok(())
}

/// `rsac record` — record audio to a WAV file.
fn cmd_record(
    output: String,
    app: Option<String>,
    pid: Option<u32>,
    duration: Option<u64>,
    sample_rate: u32,
    channels: u16,
) -> Result<()> {
    let target = build_target(&app, &pid);
    let target_label = target_label(&app, &pid);

    eprintln!("🎙  Record target:  {}", target_label);
    eprintln!("    Output file:    {}", output);
    eprintln!("    Sample rate:    {} Hz", sample_rate);
    eprintln!("    Channels:       {}", channels);
    if let Some(d) = duration {
        eprintln!("    Duration:       {}s", d);
    } else {
        eprintln!("    Duration:       unbounded (Ctrl+C to stop)");
    }
    eprintln!();

    // Set up WAV writer
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut wav_writer =
        hound::WavWriter::create(&output, spec).wrap_err("Failed to create WAV file")?;

    // Build the capture session
    let mut capture = AudioCaptureBuilder::new()
        .with_target(target)
        .sample_rate(sample_rate)
        .channels(channels)
        .build()
        .wrap_err("Failed to build audio capture")?;

    capture.start().wrap_err("Failed to start capture")?;

    // Ctrl+C handler
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })
    .wrap_err("Failed to set Ctrl+C handler")?;

    let start = Instant::now();
    let mut total_frames: u64 = 0;
    let mut total_samples_written: u64 = 0;

    let deadline = duration.map(Duration::from_secs);

    while running.load(Ordering::SeqCst) {
        // Check duration limit
        if let Some(dl) = deadline {
            if start.elapsed() >= dl {
                break;
            }
        }

        match capture.read_buffer() {
            Ok(Some(buffer)) => {
                let data = buffer.data();
                let frames = buffer.num_frames();
                total_frames += frames as u64;

                for &sample in data {
                    wav_writer
                        .write_sample(sample)
                        .wrap_err("Failed to write WAV sample")?;
                    total_samples_written += 1;
                }

                let elapsed = start.elapsed().as_secs_f64();
                let bytes_approx = total_samples_written * 4; // f32 = 4 bytes
                eprint!(
                    "\r  ⏺  {:.1}s elapsed | {:>8} frames | {:.2} MB written",
                    elapsed,
                    total_frames,
                    bytes_approx as f64 / 1_048_576.0
                );
            }
            Ok(None) => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => {
                eprintln!("\n⚠  Read error: {}", e);
                break;
            }
        }
    }

    // Clean shutdown
    capture.stop().wrap_err("Failed to stop capture")?;
    wav_writer.finalize().wrap_err("Failed to finalize WAV")?;

    let elapsed = start.elapsed();
    eprintln!("\n");
    eprintln!("✅ Recording complete");
    eprintln!("───────────────────────────────────");
    eprintln!("  File:       {}", output);
    eprintln!("  Duration:   {:.1}s", elapsed.as_secs_f64());
    eprintln!("  Frames:     {}", total_frames);
    eprintln!(
        "  Size:       {:.2} MB",
        (total_samples_written * 4) as f64 / 1_048_576.0
    );

    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Build a [`CaptureTarget`] from CLI arguments.
fn build_target(app: &Option<String>, pid: &Option<u32>) -> CaptureTarget {
    if let Some(name) = app {
        CaptureTarget::ApplicationByName(name.clone())
    } else if let Some(p) = pid {
        CaptureTarget::ProcessTree(ProcessId(*p))
    } else {
        CaptureTarget::SystemDefault
    }
}

/// Human-readable label for the current capture target.
fn target_label(app: &Option<String>, pid: &Option<u32>) -> String {
    if let Some(name) = app {
        format!("application \"{}\"", name)
    } else if let Some(p) = pid {
        format!("process tree (PID {})", p)
    } else {
        "system default".to_string()
    }
}

/// Compute the RMS (root mean square) amplitude of a sample buffer.
fn rms_level(data: &[f32]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    let sum: f32 = data.iter().map(|s| s * s).sum();
    (sum / data.len() as f32).sqrt()
}

/// Render an ASCII level bar of the given width.
fn level_bar(level: f32, width: usize) -> String {
    let clamped = level.clamp(0.0, 1.0);
    let filled = (clamped * width as f32) as usize;
    format!("[{}{}]", "█".repeat(filled), "░".repeat(width - filled))
}

/// Format a boolean as a human-readable yes/no string.
fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}
