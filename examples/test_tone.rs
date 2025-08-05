use clap::Parser;
use rsac::test_utils::playback::{AudioPlayer, PlaybackError};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "test_tone")]
#[command(about = "Generate a test tone for audio capture testing")]
struct Args {
    /// Duration in seconds to play the tone
    #[arg(short, long, default_value = "10")]
    duration: u64,
    
    /// Frequency of the test tone in Hz
    #[arg(short, long, default_value = "440.0")]
    frequency: f32,
    
    /// Volume (0.0 to 1.0)
    #[arg(short, long, default_value = "0.5")]
    volume: f32,
    
    /// Enable verbose output
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<(), PlaybackError> {
    let args = Args::parse();
    
    if args.verbose {
        println!("Starting test tone generator...");
        println!("Frequency: {} Hz", args.frequency);
        println!("Duration: {} seconds", args.duration);
        println!("Volume: {}", args.volume);
    }
    
    // Create test tone player
    let player = AudioPlayer::new_test_tone()?;
    
    if args.verbose {
        println!("Test tone started. Playing for {} seconds...", args.duration);
    }
    
    // Play for specified duration
    std::thread::sleep(Duration::from_secs(args.duration));
    
    if args.verbose {
        println!("Test tone completed.");
    }
    
    Ok(())
}
