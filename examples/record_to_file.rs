//! # Record Audio to WAV File
//!
//! Example showing how to capture audio and write it to a WAV file using the
//! rsac library's bundled [`WavFileSink`](rsac::sink::WavFileSink) sink.
//!
//! This example owns the write loop (read → `sink.write` → check) instead of
//! handing the sink to [`RunningCapture::drain_to`](rsac::RunningCapture::drain_to):
//! the drain thread can only *log* a sink error, while an owned loop checks
//! every read and every write, so a failed or shortened recording is reported
//! as such instead of claimed to be saved.
//!
//! The WAV header comes from the first *delivered* buffer, not from
//! `capture.format()` right after `start()`, which can still be the requested
//! format (when it switches to the negotiated one is backend timing).
//!
//! Run with: `cargo run --example record_to_file --features "cli sink-wav"`
//! Or with arguments:
//!   `cargo run --example record_to_file --features "cli sink-wav" -- --output recording.wav --duration 5`
//!
//! This example requires `cli` (target gating in `Cargo.toml`) and `sink-wav`
//! (for `WavFileSink`). When built without `sink-wav`, `main` prints a hint and exits.

#[cfg(feature = "sink-wav")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use rsac::sink::{AudioSink, WavFileSink};
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

    // ONE clock for the whole recording: `--duration` bounds waiting for the
    // first buffer AND writing the rest alike, so `--duration 0` (or any
    // duration that expires first) ends normally without creating a file.
    let started = Instant::now();
    let deadline = duration_secs.map(|d| started + Duration::from_secs(d));

    // Ctrl+C handler for graceful shutdown.
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        r.store(false, Ordering::SeqCst);
    })?;

    // A wait that cannot overshoot `--duration`.
    let wait = |slice: Duration| {
        let nap = deadline
            .map(|dl| dl.saturating_duration_since(Instant::now()))
            .unwrap_or(slice)
            .min(slice);
        std::thread::sleep(nap);
    };

    // Build AND start the capture in one call → a RunningCapture RAII guard.
    let capture = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(sample_rate)
        .channels(channels)
        .start()?;

    // Take the WAV header from the first DELIVERED buffer, not from
    // `capture.format()` right after `start()`, which can still be the
    // requested format: WavFileSink fixes this format for the file's lifetime
    // and rejects mismatched buffers.
    println!("Waiting for the first audio buffer...");
    let first = loop {
        if !running.load(Ordering::SeqCst) {
            println!("\nStopped before any audio was delivered; no file written.");
            return Ok(());
        }
        if deadline.is_some_and(|dl| Instant::now() >= dl) {
            println!("\nDuration limit reached before any audio was delivered; no file written.");
            return Ok(());
        }
        // Same policy as the write loop below, not `?`: a non-fatal read error
        // while waiting is a hiccup (log, short sleep, retry within the same
        // deadline and Ctrl+C checks); only a fatal error aborts here. And
        // `read_chunk_nonblocking` (not `read_buffer`) so a dead stream ends
        // with its real terminal error instead of a downgraded recoverable one.
        match capture.read_chunk_nonblocking() {
            Ok(Some(buffer)) => break buffer,
            Ok(None) => wait(Duration::from_millis(5)),
            Err(e) if !e.is_fatal() => {
                eprintln!("⚠  Transient read error while waiting: {e}");
                wait(Duration::from_millis(5));
            }
            Err(e) => return Err(e.into()),
        }
    };
    let format = first.format().clone();
    println!("Capturing {}ch @ {}Hz", format.channels, format.sample_rate);

    let mut sink = WavFileSink::new(&output_path, &format)?;

    // Own the write loop: the first buffer (which taught us the format) and
    // then every buffer read afterwards, each write checked here.
    let mut first_buf = Some(first);
    let mut total_frames = 0u64;
    let mut buffer_count = 0u64;
    let mut partial: Option<String> = None;
    println!("Recording... Press Ctrl+C to stop.");

    while partial.is_none() {
        if !running.load(Ordering::SeqCst) {
            break;
        }
        if deadline.is_some_and(|dl| Instant::now() >= dl) {
            println!("\nDuration limit reached.");
            break;
        }
        let buffer = match first_buf.take() {
            Some(b) => b,
            None => match capture.read_chunk_nonblocking() {
                Ok(Some(b)) => b,
                Ok(None) => {
                    wait(Duration::from_millis(5));
                    continue;
                }
                Err(e) if e.is_fatal() => {
                    partial = Some(format!("read failed: {e}"));
                    break;
                }
                // A recoverable read error is a hiccup: log and keep going.
                Err(e) => {
                    eprintln!("\n⚠  Transient read error: {e}");
                    wait(Duration::from_millis(5));
                    continue;
                }
            },
        };
        // A WAV header is fixed for the file's lifetime; a mid-recording format
        // change ends the recording rather than corrupting the file.
        if buffer.format() != &format {
            partial = Some(format!(
                "delivered format changed mid-recording ({}ch@{}Hz -> {}ch@{}Hz)",
                format.channels,
                format.sample_rate,
                buffer.channels(),
                buffer.sample_rate()
            ));
            break;
        }
        if let Err(e) = sink.write(&buffer) {
            partial = Some(format!("write failed: {e}"));
            break;
        }
        total_frames += buffer.num_frames() as u64;
        buffer_count += 1;
        print!("\r  Recording: {:.1}s", started.elapsed().as_secs_f64());
        use std::io::Write as _;
        let _ = std::io::stdout().flush();
    }

    // Finalise the sink on EVERY exit path so the header covers whatever was
    // written, then tear the capture down (RunningCapture's Drop would too).
    if let Err(e) = sink.close() {
        return Err(format!("failed to finalize the WAV file: {e}").into());
    }
    drop(capture);

    match partial {
        Some(reason) => {
            eprintln!("\n⚠  Recording stopped early: {reason}");
            println!(
                "\nPartial recording saved to: {} ({} buffers, {} frames)",
                output_path, buffer_count, total_frames
            );
            Err(reason.into())
        }
        None => {
            println!("\n\nRecording saved to: {}", output_path);
            println!("Duration: {:.1}s", started.elapsed().as_secs_f64());
            println!("Buffers: {}, Frames: {}", buffer_count, total_frames);
            Ok(())
        }
    }
}

#[cfg(not(feature = "sink-wav"))]
fn main() {
    eprintln!(
        "This example requires the `sink-wav` feature.\n\
         Re-run with: cargo run --example record_to_file --features \"cli sink-wav\""
    );
}
