//! # Record Audio to WAV File
//!
//! Example showing how to capture audio and write it to a WAV file using the
//! rsac library's bundled [`WavFileSink`](rsac::sink::WavFileSink) sink driven
//! by [`RunningCapture::drain_to`](rsac::RunningCapture::drain_to) — the
//! lifecycle-correct path that pumps captured buffers into a sink on a
//! dedicated thread (never the OS audio callback thread).
//!
//! Run with: `cargo run --example record_to_file --features "cli sink-wav"`
//! Or with arguments:
//!   `cargo run --example record_to_file --features "cli sink-wav" -- --output recording.wav --duration 5`
//!
//! This example requires `cli` (target gating in `Cargo.toml`) and `sink-wav`
//! (for `WavFileSink`). When built without `sink-wav`, `main` prints a hint and exits.

#[cfg(feature = "sink-wav")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use rsac::core::config::{AudioFormat, SampleFormat};
    use rsac::sink::WavFileSink;
    use rsac::{AudioCaptureBuilder, CaptureTarget};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    // Configuration from command-line arguments.
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

    // Build AND start the capture in one call → a RunningCapture RAII guard.
    let capture = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(sample_rate)
        .channels(channels)
        .start()?;

    // Use the backend's *negotiated* delivery format for the WAV header when it
    // is available (the device may have negotiated a different rate/channel
    // count); fall back to the requested format otherwise. WavFileSink fixes
    // this format for the file's lifetime and rejects mismatched buffers.
    let format = capture.format().unwrap_or(AudioFormat {
        sample_rate,
        channels,
        sample_format: SampleFormat::F32,
    });
    println!("Capturing {}ch @ {}Hz", format.channels, format.sample_rate);

    let sink = WavFileSink::new(&output_path, &format)?;

    // Ctrl+C handler for graceful shutdown.
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    // drain_to spawns a background thread that pumps every captured buffer into
    // the sink (write → … → flush → close) until the stream ends or we stop it.
    let drain = capture.drain_to(sink)?;
    println!("Recording... Press Ctrl+C to stop.");

    let start = Instant::now();
    while running.load(Ordering::SeqCst) {
        if let Some(max_secs) = duration_secs {
            if start.elapsed() >= Duration::from_secs(max_secs) {
                println!("\nDuration limit reached.");
                break;
            }
        }
        print!("\r  Recording: {:.1}s", start.elapsed().as_secs_f64());
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
        std::thread::sleep(Duration::from_millis(200));
    }

    // Stop draining first: shutdown() joins the drain thread, flushing and
    // closing the WAV (finalizing its header) before we tear down the capture.
    drain.shutdown();
    // Then stop the capture stream (RunningCapture's Drop would also do this,
    // but doing it explicitly makes the ordering clear).
    drop(capture);

    println!("\n\nRecording saved to: {}", output_path);
    println!("Duration: {:.1}s", start.elapsed().as_secs_f64());

    Ok(())
}

#[cfg(not(feature = "sink-wav"))]
fn main() {
    eprintln!(
        "This example requires the `sink-wav` feature.\n\
         Re-run with: cargo run --example record_to_file --features \"cli sink-wav\""
    );
}
