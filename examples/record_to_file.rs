//! # Record Audio to WAV File
//!
//! Example showing how to capture audio and write it to a WAV file
//! using the rsac library API with the `hound` crate for WAV encoding.
//!
//! Run with: `cargo run --example record_to_file`
//! Or with arguments: `cargo run --example record_to_file -- --output recording.wav --duration 5`

use rsac::{AudioCaptureBuilder, CaptureTarget};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configuration from command-line arguments
    let output_path = std::env::args()
        .skip_while(|a| a != "--output")
        .nth(1)
        .unwrap_or_else(|| "capture.wav".to_string());

    let duration_secs: Option<u64> = std::env::args()
        .skip_while(|a| a != "--duration")
        .nth(1)
        .and_then(|s| s.parse().ok());

    let sample_rate = 48000u32;
    let channels = 2u16;

    println!("Recording to: {}", output_path);
    if let Some(d) = duration_secs {
        println!("Duration: {}s", d);
    } else {
        println!("Duration: until Ctrl+C");
    }

    // Build capture session
    let mut capture = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(sample_rate)
        .channels(channels)
        .build()?;

    // Set up WAV writer
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(&output_path, spec)?;

    // Ctrl+C handler for graceful shutdown
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    // Start capture
    capture.start()?;
    println!("Recording... Press Ctrl+C to stop.");

    let start = Instant::now();
    let mut total_frames: u64 = 0;

    while running.load(Ordering::SeqCst) {
        // Check duration limit
        if let Some(max_secs) = duration_secs {
            if start.elapsed() >= Duration::from_secs(max_secs) {
                println!("\nDuration limit reached.");
                break;
            }
        }

        match capture.read_buffer() {
            Ok(Some(buffer)) => {
                // Write all samples to WAV
                for &sample in buffer.data() {
                    writer.write_sample(sample)?;
                }
                total_frames += buffer.num_frames() as u64;

                // Progress update every ~1 second worth of audio
                if total_frames % (sample_rate as u64) < 1024 {
                    print!(
                        "\r  Recorded: {:.1}s ({} frames)",
                        start.elapsed().as_secs_f64(),
                        total_frames
                    );
                }
            }
            Ok(None) => {
                // No data available yet — avoid busy-spinning
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => {
                eprintln!("\nRead error: {}", e);
                break;
            }
        }
    }

    // Finalize
    capture.stop()?;
    writer.finalize()?;

    println!("\n\nRecording saved to: {}", output_path);
    println!("Duration: {:.1}s", start.elapsed().as_secs_f64());
    println!("Frames: {}", total_frames);

    Ok(())
}
