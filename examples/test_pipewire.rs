use clap::{arg, Parser};
use rsac::test_utils::playback::{AudioPlayer, PlaybackError};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(author, version, about = "Test PipeWire audio capture functionality")]
struct Args {
    /// Duration in seconds to capture audio
    #[arg(short, long, default_value = "30")]
    duration: u32,

    /// Output WAV file path
    #[arg(short, long, default_value = "pipewire_capture.wav")]
    output: PathBuf,

    /// Path to audio file to play, if not specified a test tone will be generated
    #[arg(short, long)]
    audio_file: Option<PathBuf>,

    /// Volume for audio playback (0.0 to 1.0)
    #[arg(short, long, default_value = "0.5")]
    volume: f32,
}

// Function to play audio in a separate thread
fn play_audio_in_thread(
    audio_file: Option<PathBuf>,
    volume: f32,
    duration: u32,
    running: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        println!("Starting audio playback thread...");

        // Create the audio player using the updated path
        let player = if let Some(file_path) = audio_file {
            println!("Playing audio file: {}", file_path.display());
            match rsac::test_utils::playback::AudioPlayer::new(&file_path) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("Failed to create audio player for file: {}", e);
                    // Fallback to test tone
                    match rsac::test_utils::playback::AudioPlayer::new_test_tone() {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!("Failed to create test tone player: {}", e);
                            return;
                        }
                    }
                }
            }
        } else {
            println!("Generating test tone...");
            match rsac::test_utils::playback::AudioPlayer::new_test_tone() {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("Failed to create test tone player: {}", e);
                    return;
                }
            }
        };

        // Set volume
        player.set_volume(volume);
        println!("Volume set to: {}", volume);

        // Play for specified duration or until signaled to stop
        let play_duration = Duration::from_secs(duration as u64 + 1); // Add 1 second to ensure capture completes
        let start = Instant::now();

        println!("Playing audio for {} seconds...", duration);
        while start.elapsed() < play_duration && running.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(100));
        }

        println!("Stopping audio playback...");
        player.stop();
        println!("Audio playback thread completed");
    })
}

fn main() {
    let args = Args::parse();
    println!("Testing PipeWire audio capture...");

    // Shared flag for thread coordination
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    // Start audio playback in a separate thread
    println!("Starting audio playback thread...");
    let playback_thread = play_audio_in_thread(
        args.audio_file.clone(),
        args.volume,
        args.duration,
        running_clone,
    );

    // Give the playback thread a moment to start
    thread::sleep(Duration::from_millis(3000));

    // Get the backend
    let backend = match rsac::audio::get_audio_backend() {
        Ok(backend) => {
            println!(
                "✅ Successfully connected to audio backend: {}",
                backend.name()
            );
            backend
        }
        Err(e) => {
            eprintln!("❌ Failed to connect to audio backend: {}", e);
            running.store(false, Ordering::SeqCst);
            std::process::exit(1);
        }
    };

    // List available audio applications
    println!("Listing available audio applications...");
    let apps = match backend.list_applications() {
        Ok(apps) => {
            println!("✅ Found {} audio applications", apps.len());
            apps
        }
        Err(e) => {
            eprintln!("❌ Failed to list audio applications: {}", e);
            running.store(false, Ordering::SeqCst);
            std::process::exit(1);
        }
    };

    // Print the list of applications
    println!("Available audio applications:");
    for (i, app) in apps.iter().enumerate() {
        println!(
            "  {}. {} (pid: {}, id: {})",
            i + 1,
            app.name,
            app.pid,
            app.id
        );
    }

    // Choose the system audio application
    let app = apps
        .iter()
        .find(|app| app.name == "System" || app.name.to_lowercase().contains("system"))
        .unwrap_or(&apps[0]);

    println!("Capturing audio from: {} (id: {})", app.name, app.id);

    // Setup audio config
    let config = rsac::audio::AudioConfig {
        sample_rate: 48000,
        channels: 2,
        format: rsac::audio::AudioFormat::F32LE,
    };

    // Create a WAV writer
    let spec = hound::WavSpec {
        channels: config.channels,
        sample_rate: config.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };

    let mut wav_writer = match hound::WavWriter::create(&args.output, spec) {
        Ok(writer) => {
            println!("✅ Created output WAV file: {}", args.output.display());
            writer
        }
        Err(e) => {
            eprintln!("❌ Failed to create WAV file: {}", e);
            running.store(false, Ordering::SeqCst);
            std::process::exit(1);
        }
    };

    // Create an audio capture stream
    let mut stream = match backend.capture_application(app, config) {
        Ok(stream) => {
            println!("✅ Created audio capture stream");
            stream
        }
        Err(e) => {
            eprintln!("❌ Failed to create audio capture stream: {}", e);
            running.store(false, Ordering::SeqCst);
            std::process::exit(1);
        }
    };

    // Start capturing
    if let Err(e) = stream.start() {
        eprintln!("❌ Failed to start audio capture: {}", e);
        running.store(false, Ordering::SeqCst);
        std::process::exit(1);
    }
    println!("✅ Started audio capture");

    // Capture for the specified duration
    let start_time = Instant::now();
    let capture_duration = Duration::from_secs(args.duration as u64);
    let mut buffer = vec![0u8; 4096];
    let mut total_frames = 0;

    println!("Capturing audio for {} seconds...", args.duration);

    while start_time.elapsed() < capture_duration {
        match stream.read(&mut buffer) {
            Ok(bytes_read) => {
                if bytes_read > 0 {
                    // Convert bytes to f32 samples
                    for i in 0..(bytes_read / 4) {
                        let sample_bytes = [
                            buffer[i * 4],
                            buffer[i * 4 + 1],
                            buffer[i * 4 + 2],
                            buffer[i * 4 + 3],
                        ];
                        let sample = f32::from_le_bytes(sample_bytes);
                        wav_writer.write_sample(sample).unwrap();
                    }

                    total_frames += bytes_read / 4 / config.channels as usize;
                }
            }
            Err(e) => {
                eprintln!("❌ Error reading audio data: {}", e);
                break;
            }
        }

        // Sleep a bit to avoid busy-waiting
        thread::sleep(Duration::from_millis(10));
    }

    // Stop capturing
    if let Err(e) = stream.stop() {
        eprintln!("❌ Failed to stop audio capture: {}", e);
    }
    println!("✅ Stopped audio capture");

    // Signal playback thread to stop
    running.store(false, Ordering::SeqCst);

    // Finalize the WAV file
    match wav_writer.finalize() {
        Ok(_) => {
            println!(
                "✅ Successfully wrote {} frames to {}",
                total_frames,
                args.output.display()
            );
            println!(
                "✅ Audio file duration: {:.2} seconds",
                total_frames as f64 / config.sample_rate as f64
            );
        }
        Err(e) => {
            eprintln!("❌ Failed to finalize WAV file: {}", e);
        }
    };

    // Wait for playback thread to finish
    if let Err(e) = playback_thread.join() {
        eprintln!("Error joining playback thread: {:?}", e);
    }

    // Analyze the audio file to check if it's not just static/single tone
    println!("Analyzing captured audio...");
    let mut min_sample: f32 = 0.0;
    let mut max_sample: f32 = 0.0;
    let mut sum_squares = 0.0;
    let mut count = 0;

    if let Ok(reader) = hound::WavReader::open(&args.output) {
        for sample in reader.into_samples::<f32>() {
            if let Ok(value) = sample {
                min_sample = min_sample.min(value);
                max_sample = max_sample.max(value);
                sum_squares += value * value;
                count += 1;
            }
        }
    }

    if count > 0 {
        let rms = (sum_squares / count as f32).sqrt();
        println!("Audio analysis results:");
        println!("  - Minimum sample value: {:.6}", min_sample);
        println!("  - Maximum sample value: {:.6}", max_sample);
        println!("  - Dynamic range: {:.6}", max_sample - min_sample);
        println!("  - RMS value: {:.6}", rms);

        if max_sample - min_sample < 0.01 {
            println!("❌ Warning: Audio may be static. Very small dynamic range detected.");
        } else if rms < 0.001 {
            println!("❌ Warning: Audio may be silent. Very low RMS value detected.");
        } else {
            println!(
                "✅ Audio seems to have good variance and is likely not just static/single tone"
            );
        }
    } else {
        println!("❌ Could not analyze the captured audio file");
    }

    println!(
        "Test completed. Captured audio saved to: {}",
        args.output.display()
    );
}
