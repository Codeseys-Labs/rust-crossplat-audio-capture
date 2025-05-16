// examples/macos_application_capture.rs
use rust_crossplat_audio_capture::{
    api::{AudioCapture, AudioCaptureBuilder},
    audio::{
        enumerate_audio_applications, ApplicationInfo, AudioFormat, ChannelCount, SampleFormat,
        StreamConfig,
    },
    core::AudioBuffer,
};
use std::io::{self, Write}; // For user input

fn main() -> anyhow::Result<()> {
    println!("Available running applications on macOS:");
    let apps = enumerate_audio_applications()?;
    if apps.is_empty() {
        println!("No running applications found to capture from.");
        return Ok(());
    }

    for (i, app_info) in apps.iter().enumerate() {
        println!(
            "{}: PID: {}, Name: {}, Bundle ID: {:?}",
            i, app_info.process_id, app_info.name, app_info.bundle_id
        );
    }

    print!("Enter the number of the application to capture: ");
    io::stdout().flush()?;
    let mut choice_str = String::new();
    io::stdin().read_line(&mut choice_str)?;
    let choice: usize = choice_str.trim().parse()?;

    if choice >= apps.len() {
        println!("Invalid choice.");
        return Ok(());
    }

    let target_app = &apps[choice];
    println!(
        "Attempting to capture audio from: {} (PID: {})",
        target_app.name, target_app.process_id
    );

    let stream_config = StreamConfig {
        format: AudioFormat {
            sample_format: SampleFormat::F32LE, // CoreAudio typically provides F32
            sample_rate: 48000,                 // Or get from device/tap if possible
            channels: ChannelCount::Stereo,
        },
        buffer_size_frames: None, // Use default
    };

    let mut capturer = AudioCaptureBuilder::new()
        .target_application_pid(target_app.process_id)
        .stream_config(stream_config)
        .build()?;

    println!("Starting capture for 5 seconds...");
    capturer.start_capture()?;

    let start_time = std::time::Instant::now();
    let mut total_frames = 0;
    while start_time.elapsed().as_secs() < 5 {
        match capturer.read_chunk(None) {
            // Non-blocking read
            Ok(Some(buffer)) => {
                total_frames += buffer.num_frames();
                // println!("Read chunk: {} frames, {} channels", buffer.num_frames(), buffer.num_channels());
            }
            Ok(None) => {
                // No data currently available, sleep briefly
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => {
                eprintln!("Error reading chunk: {:?}", e);
                break;
            }
        }
    }

    capturer.stop_capture()?;
    println!("Capture stopped. Total frames captured: {}", total_frames);

    Ok(())
}
