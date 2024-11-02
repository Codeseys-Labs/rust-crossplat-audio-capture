use hound::{SampleFormat, WavSpec, WavWriter};
use std::fs::File;
use std::io::Write;
use std::thread;
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Creating audio capture instance...");
    let mut capture = rsac::ProcessAudioCapture::new()?;

    println!("\nListing running processes:");
    let processes = rsac::ProcessAudioCapture::list_processes()?;
    for process in processes {
        if process.to_lowercase().contains("spotify") {
            println!("Found Spotify process: {}", process);
        }
    }

    println!("\nInitializing capture for Spotify.exe...");
    if let Err(e) = capture.init_for_process("Spotify.exe") {
        println!("Failed to initialize capture for Spotify.exe: {}", e);
        return Ok(());
    }

    println!("\nStarting capture...");
    if let Err(e) = capture.start() {
        println!("Failed to start capture: {}", e);
        return Ok(());
    }

    // Get audio format
    let channels = capture.channels().unwrap_or(2) as u16;
    let sample_rate = capture.sample_rate().unwrap_or(44100) as u32;
    let bits_per_sample = capture.bits_per_sample().unwrap_or(16) as u16;

    println!("Audio format:");
    println!("  Channels: {}", channels);
    println!("  Sample rate: {} Hz", sample_rate);
    println!("  Bits per sample: {}", bits_per_sample);

    // Create WAV writer
    let spec = WavSpec {
        channels,
        sample_rate,
        bits_per_sample,
        sample_format: SampleFormat::Float,
    };
    let mut wav_writer = WavWriter::create("captured_audio.wav", spec)?;
    let mut raw_file = File::create("captured_audio.raw")?;
    let mut log_file = File::create("capture_log.txt")?;

    println!("\nCapturing audio for 5 seconds...");
    let start = std::time::Instant::now();
    let mut total_bytes = 0;
    let mut packets = 0;
    let mut silent_packets = 0;

    while start.elapsed() < Duration::from_secs(5) {
        match capture.get_data() {
            Ok(data) if !data.is_empty() => {
                packets += 1;
                total_bytes += data.len();

                // Check if the packet contains any non-zero bytes
                let is_silent = data.iter().all(|&x| x == 0);
                if is_silent {
                    silent_packets += 1;
                }

                let log_msg = format!(
                    "Captured packet {}: {} bytes (total: {} bytes) {}\n",
                    packets,
                    data.len(),
                    total_bytes,
                    if is_silent { "[silent]" } else { "" }
                );
                print!("{}", log_msg);
                log_file.write_all(log_msg.as_bytes())?;

                // Print first few bytes for debugging
                if data.len() >= 4 && !is_silent {
                    let debug_msg = format!(
                        "First 4 bytes: {:02x} {:02x} {:02x} {:02x}\n",
                        data[0], data[1], data[2], data[3]
                    );
                    print!("{}", debug_msg);
                    log_file.write_all(debug_msg.as_bytes())?;
                }

                // Write audio data to files
                raw_file.write_all(&data)?;

                // Convert bytes to f32 samples and write to WAV
                for chunk in data.chunks(4) {
                    if chunk.len() == 4 {
                        let sample = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                        wav_writer.write_sample(sample)?;
                    }
                }
            }
            Ok(_) => {
                // No data available, wait a bit
                thread::sleep(Duration::from_millis(1));
            }
            Err(e) => {
                println!("Error capturing data: {}", e);
                break;
            }
        }
    }

    let summary = format!(
        "\nCapture summary:\n\
        Total packets: {}\n\
        Silent packets: {}\n\
        Total bytes: {}\n\
        Average bytes per packet: {}\n",
        packets,
        silent_packets,
        total_bytes,
        if packets > 0 {
            total_bytes / packets
        } else {
            0
        }
    );
    print!("{}", summary);
    log_file.write_all(summary.as_bytes())?;

    println!("\nStopping capture...");
    if let Err(e) = capture.stop() {
        println!("Error stopping capture: {}", e);
    }

    // Finalize WAV file
    wav_writer.finalize()?;

    println!("Audio saved to captured_audio.raw and captured_audio.wav");
    println!("Capture log saved to capture_log.txt");

    Ok(())
}
