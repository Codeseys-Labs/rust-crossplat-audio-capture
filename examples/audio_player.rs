use clap::{arg, Parser};
use rsac::audio::test_utils::playback::{AudioPlayer, PlaybackError};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Audio playback utility for cross-platform testing"
)]
struct Args {
    /// Path to audio file to play
    #[arg(short, long)]
    file: Option<PathBuf>,

    /// Generate a test tone instead of playing a file
    #[arg(short, long)]
    test_tone: bool,

    /// Duration in seconds to play (default: play until completion)
    #[arg(short, long)]
    duration: Option<u64>,

    /// Volume (0.0 to 1.0)
    #[arg(short, long, default_value = "0.5")]
    volume: f32,
}

fn main() -> Result<(), PlaybackError> {
    let args = Args::parse();

    // Validate args
    if !args.test_tone && args.file.is_none() {
        eprintln!("Error: Either --file or --test-tone must be specified");
        std::process::exit(1);
    }

    // Create the appropriate audio player
    let player = if args.test_tone {
        println!("Playing test tone...");
        AudioPlayer::new_test_tone()?
    } else if let Some(file_path) = &args.file {
        println!("Playing audio file: {}", file_path.display());
        AudioPlayer::new(file_path)?
    } else {
        unreachable!();
    };

    // Set volume
    player.set_volume(args.volume);
    println!("Volume set to: {}", args.volume);

    // Set up ctrl-c handler
    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        println!("\nStopping playback...");
        r.store(false, std::sync::atomic::Ordering::SeqCst);
    })
    .expect("Error setting Ctrl-C handler");

    println!("Press Ctrl+C to stop playback");

    // Play for specified duration or until completion
    if let Some(seconds) = args.duration {
        let duration = Duration::from_secs(seconds);
        println!("Playing for {} seconds...", seconds);

        let start = std::time::Instant::now();
        while start.elapsed() < duration && running.load(std::sync::atomic::Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(100));
        }

        // Stop playback
        player.stop();
    } else {
        // Play until completion or interrupted
        while running.load(std::sync::atomic::Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(100));
        }
    }

    println!("Playback complete!");
    Ok(())
}
