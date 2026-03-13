//! Async audio capture example
//!
//! Demonstrates using `AsyncAudioStream` to capture audio data asynchronously.
//!
//! # Requirements
//! - The `async-stream` feature must be enabled
//! - Requires `tokio` runtime (for this example only — the library is runtime-agnostic)
//!
//! # Usage
//! ```sh
//! cargo run --example async_capture --features async-stream
//! ```

#[cfg(feature = "async-stream")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use futures_util::StreamExt;
    use rsac::{AudioCaptureBuilder, CaptureTarget};

    println!("=== Async Audio Capture Example ===\n");

    // Build capture configuration
    let mut capture = AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()?;

    println!("Starting capture...");
    capture.start()?;

    // Get async audio stream
    let mut stream = capture.audio_data_stream()?;

    println!("Streaming audio data asynchronously...\n");

    // Read up to 10 buffers
    let mut count = 0;
    while let Some(result) = stream.next().await {
        match result {
            Ok(buffer) => {
                println!(
                    "Buffer {}: {} frames, {} channels, {} Hz, peak amplitude: {:.4}",
                    count + 1,
                    buffer.num_frames(),
                    buffer.format().channels,
                    buffer.format().sample_rate,
                    buffer
                        .data()
                        .iter()
                        .fold(0.0f32, |max, &s| max.max(s.abs())),
                );
                count += 1;
                if count >= 10 {
                    println!("\nCaptured 10 buffers. Stopping.");
                    break;
                }
            }
            Err(e) => {
                eprintln!("Error reading audio: {}", e);
                break;
            }
        }
    }

    capture.stop()?;
    println!("\nCapture stopped. Done!");
    Ok(())
}

#[cfg(not(feature = "async-stream"))]
fn main() {
    eprintln!("This example requires the 'async-stream' feature.");
    eprintln!("Run with: cargo run --example async_capture --features async-stream");
    std::process::exit(1);
}
