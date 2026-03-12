//! # Basic Audio Capture
//!
//! Minimal example demonstrating the rsac streaming capture pipeline:
//! `AudioCaptureBuilder → AudioCapture → read_buffer() loop`
//!
//! Run with: `cargo run --example basic_capture`

use rsac::{AudioCaptureBuilder, CaptureTarget, PlatformCapabilities};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Show what this platform supports
    let caps = PlatformCapabilities::query();
    println!("Platform: {}", caps.backend_name);
    println!("System capture: {}", caps.supports_system_capture);
    println!(
        "Sample rate range: {} – {} Hz",
        caps.sample_rate_range.0, caps.sample_rate_range.1
    );
    println!("Max channels: {}", caps.max_channels);

    // Build a capture session targeting system default audio
    let mut capture = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()?;

    // Set up Ctrl+C handler for clean shutdown
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    // Start capturing
    println!("Starting capture... Press Ctrl+C to stop.");
    capture.start()?;

    let start = Instant::now();
    let mut total_frames: u64 = 0;
    let mut buffer_count: u64 = 0;

    // Main capture loop — read buffers and compute levels
    while running.load(Ordering::SeqCst) {
        match capture.read_buffer() {
            Ok(Some(buffer)) => {
                total_frames += buffer.num_frames() as u64;
                buffer_count += 1;

                // Compute RMS level
                let rms = rms_level(buffer.data());
                let db = if rms > 0.0 { 20.0 * rms.log10() } else { -60.0 };

                if buffer_count % 10 == 0 {
                    println!(
                        "[{:.1}s] Buffers: {}, Frames: {}, Level: {:.1} dB",
                        start.elapsed().as_secs_f64(),
                        buffer_count,
                        total_frames,
                        db
                    );
                }
            }
            Ok(None) => {
                // No data available yet — avoid busy-spinning
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => {
                eprintln!("Read error: {}", e);
                break;
            }
        }
    }

    capture.stop()?;
    println!("\nCapture complete!");
    println!("Duration: {:.1}s", start.elapsed().as_secs_f64());
    println!("Total frames: {}", total_frames);
    println!("Buffers read: {}", buffer_count);

    Ok(())
}

/// Compute the RMS (root mean square) level of audio samples.
fn rms_level(data: &[f32]) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    let sum: f32 = data.iter().map(|s| s * s).sum();
    (sum / data.len() as f32).sqrt()
}
