// examples/rt_callback_simple.rs

use rust_crossplat_audio_capture::api::AudioCapture;
use rust_crossplat_audio_capture::core::buffer::AudioBuffer;
use rust_crossplat_audio_capture::core::config::{
    ApiConfig, AudioCaptureConfig, BitsPerSample, CaptureAPI, ChannelConfig, Channels,
    SampleFormat, SampleRate,
};
use rust_crossplat_audio_capture::core::error::AudioError;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// Shared data structure to count callbacks and total bytes, similar to how a real app might collect data.
#[derive(Default, Debug)]
struct CallbackStats {
    callbacks_invoked: usize,
    total_bytes_received: usize,
}

fn main() {
    println!("Real-time audio processing example with a simple callback.");
    println!("This will attempt to capture audio from the default input device.");

    // 1. Configure AudioCapture
    let audio_config = AudioCaptureConfig {
        api_config: ApiConfig::Default,
        device_id: None, // None for default device
        sample_rate: SampleRate::Rate48000,
        channels: Channels::Stereo,
        sample_format: SampleFormat::F32,
        bits_per_sample: BitsPerSample::Bits32,
        channel_config: ChannelConfig::Default,
        buffer_size_frames: None, // OS default
    };

    // 2. Initialize AudioCapture
    let mut audio_capture = match AudioCapture::new(audio_config) {
        Ok(ac) => ac,
        Err(e) => {
            eprintln!("Failed to initialize AudioCapture: {:?}", e);
            eprintln!("This example requires a working audio input device and drivers.");
            return;
        }
    };

    // 3. Prepare shared data and set the callback
    let callback_stats = Arc::new(Mutex::new(CallbackStats::default()));
    let stats_clone = Arc::clone(&callback_stats);

    let callback = move |buffer: &AudioBuffer| -> Result<(), AudioError> {
        let mut stats = stats_clone.lock().unwrap();
        stats.callbacks_invoked += 1;
        stats.total_bytes_received += buffer.data().len();

        println!(
            "Callback: Received buffer - Timestamp: {}, Length: {} bytes, Format: {:?}, Channels: {}, Sample Rate: {}",
            buffer.timestamp(),
            buffer.data().len(),
            buffer.sample_format(),
            buffer.channels(),
            buffer.sample_rate()
        );
        // Example: Convert to f32 and calculate RMS
        // if buffer.sample_format() == SampleFormat::F32 {
        //     if let Ok(samples_f32) = buffer.as_f32_slice() {
        //         if !samples_f32.is_empty() {
        //             let sum_sq: f32 = samples_f32.iter().map(|&s| s * s).sum();
        //             let rms = (sum_sq / samples_f32.len() as f32).sqrt();
        //             println!("           RMS: {:.4}", rms);
        //         }
        //     }
        // }
        Ok(())
    };

    if let Err(e) = audio_capture.set_callback(Box::new(callback)) {
        eprintln!("Failed to set callback: {:?}", e);
        return;
    }
    println!("Audio callback set.");

    // 4. Start capture (internal processing loop)
    println!("Starting audio capture for 5 seconds...");
    if let Err(e) = audio_capture.start() {
        eprintln!("Failed to start audio capture: {:?}", e);
        return;
    }
    println!("Capture started. Audio data will be processed by the callback.");

    // 5. Run for a few seconds
    thread::sleep(Duration::from_secs(5));

    // 6. Stop capture
    println!("Stopping audio capture...");
    if let Err(e) = audio_capture.stop() {
        eprintln!("Failed to stop audio capture: {:?}", e);
    } else {
        println!("Capture stopped successfully.");
    }

    // 7. Print collected stats
    let final_stats = callback_stats.lock().unwrap();
    println!("\n--- Callback Statistics ---");
    println!("Total callbacks invoked: {}", final_stats.callbacks_invoked);
    println!("Total bytes received: {}", final_stats.total_bytes_received);
    if final_stats.callbacks_invoked > 0 {
        println!(
            "Average bytes per callback: {:.2}",
            final_stats.total_bytes_received as f64 / final_stats.callbacks_invoked as f64
        );
    }
    println!("-------------------------\n");

    println!("Example finished.");
}
