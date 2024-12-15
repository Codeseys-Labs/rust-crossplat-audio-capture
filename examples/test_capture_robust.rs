use hound::{WavReader, WavWriter, WavSpec};
use std::time::Duration;
use std::thread;
use std::path::Path;

fn validate_audio_file(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let reader = WavReader::open(path)?;
    let spec = reader.spec();
    
    println!("Validating captured audio:");
    println!("Channels: {}", spec.channels);
    println!("Sample rate: {}", spec.sample_rate);
    println!("Bits per sample: {}", spec.bits_per_sample);
    
    // Convert samples to Vec for analysis
    let samples: Vec<i16> = reader
        .into_samples()
        .filter_map(Result::ok)
        .collect();
    
    if samples.is_empty() {
        return Err("No samples found in audio file".into());
    }
    
    // Calculate basic audio statistics
    let total_samples = samples.len();
    let max_amplitude = samples.iter().map(|&s| s.abs()).max().unwrap_or(0);
    let avg_amplitude: f32 = samples.iter().map(|&s| s.abs() as f32).sum::<f32>() / total_samples as f32;
    
    println!("Total samples: {}", total_samples);
    println!("Maximum amplitude: {}", max_amplitude);
    println!("Average amplitude: {:.2}", avg_amplitude);
    
    // Check for silence or very low volume
    if max_amplitude < 100 {
        return Err("Audio appears to be silent or very quiet".into());
    }
    
    // Check for expected duration (assuming 44.1kHz)
    let duration_secs = total_samples as f32 / (spec.sample_rate as f32 * spec.channels as f32);
    println!("Audio duration: {:.2} seconds", duration_secs);
    
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configuration
    let capture_duration_secs = 5;
    let output_file = "test_capture_robust.wav";
    
    println!("Starting audio capture test...");
    println!("Capture duration: {} seconds", capture_duration_secs);
    println!("Output file: {}", output_file);
    
    // TODO: Replace this with your actual audio capture implementation
    // This is where you'd call your library's audio capture functionality
    // For now, we'll just simulate the capture
    thread::sleep(Duration::from_secs(capture_duration_secs as u64));
    
    // After capture, validate the output
    if Path::new(output_file).exists() {
        println!("\nValidating captured audio...");
        match validate_audio_file(output_file) {
            Ok(()) => println!("Audio validation successful!"),
            Err(e) => {
                eprintln!("Audio validation failed: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        eprintln!("Error: Capture file not found!");
        std::process::exit(1);
    }
    
    Ok(())
}