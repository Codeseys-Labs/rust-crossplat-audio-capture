// examples/rt_basic_processor.rs

use rust_crossplat_audio_capture::api::AudioCapture;
use rust_crossplat_audio_capture::core::buffer::AudioBuffer;
use rust_crossplat_audio_capture::core::config::{
    ApiConfig, AudioCaptureConfig, BitsPerSample, CaptureAPI, ChannelConfig, Channels,
    SampleFormat, SampleRate,
};
use rust_crossplat_audio_capture::core::error::AudioError;
use rust_crossplat_audio_capture::core::interface::AudioProcessor;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// A simple audio processor that prints buffer statistics
struct SimpleStatsProcessor {
    total_buffers_processed: usize,
    total_bytes_processed: usize,
}

impl SimpleStatsProcessor {
    fn new() -> Self {
        SimpleStatsProcessor {
            total_buffers_processed: 0,
            total_bytes_processed: 0,
        }
    }
}

impl AudioProcessor for SimpleStatsProcessor {
    fn process(&mut self, buffer: &AudioBuffer) -> Result<(), AudioError> {
        self.total_buffers_processed += 1;
        self.total_bytes_processed += buffer.data().len();

        println!(
            "Processor: Received buffer - Timestamp: {}, Length: {} bytes, Format: {:?}, Channels: {}, Sample Rate: {}",
            buffer.timestamp(),
            buffer.data().len(),
            buffer.sample_format(),
            buffer.channels(),
            buffer.sample_rate()
        );
        // For more detailed analysis, one could calculate RMS, average, etc.
        // let samples_f32: Vec<f32> = buffer.as_f32_vec().unwrap_or_default();
        // if !samples_f32.is_empty() {
        //     let sum_sq: f32 = samples_f32.iter().map(|&s| s * s).sum();
        //     let rms = (sum_sq / samples_f32.len() as f32).sqrt();
        //     println!("           RMS: {:.4}", rms);
        // }
        Ok(())
    }
}

fn main() {
    println!("Real-time audio processing example with a basic processor.");
    println!("This will attempt to capture audio from the default input device.");

    // 1. Configure AudioCapture
    // Using default configuration for simplicity.
    // Users can customize this based on their needs (specific device, sample rate, etc.)
    let audio_config = AudioCaptureConfig {
        api_config: ApiConfig::Default, // Or specify e.g., ApiConfig::Wasapi, ApiConfig::Alsa { device_name: "default".to_string() }
        device_id: None,                // None for default device
        sample_rate: SampleRate::Rate48000,
        channels: Channels::Stereo,             // Or Channels::Mono
        sample_format: SampleFormat::F32,       // Or SampleFormat::S16
        bits_per_sample: BitsPerSample::Bits32, // Only relevant for integer formats
        channel_config: ChannelConfig::Default,
        buffer_size_frames: None, // OS default
    };

    // 2. Initialize AudioCapture
    // This will try to use a real audio device.
    // If this fails in a headless CI environment, the example might need adjustment
    // or be excluded from CI runs that lack audio hardware/drivers.
    let mut audio_capture = match AudioCapture::new(audio_config) {
        Ok(ac) => ac,
        Err(e) => {
            eprintln!("Failed to initialize AudioCapture: {:?}", e);
            eprintln!("This example requires a working audio input device and drivers.");
            eprintln!("If running in a headless environment, this error is expected if no mock/virtual device is available.");
            return;
        }
    };

    // 3. Add the audio processor
    let processor = SimpleStatsProcessor::new();
    if let Err(e) = audio_capture.add_processor(Box::new(processor)) {
        eprintln!("Failed to add processor: {:?}", e);
        return;
    }
    println!("Audio processor added.");

    // 4. Start capture (internal processing loop)
    println!("Starting audio capture for 5 seconds...");
    if let Err(e) = audio_capture.start() {
        eprintln!("Failed to start audio capture: {:?}", e);
        return;
    }
    println!("Capture started. Audio data will be processed internally.");

    // 5. Run for a few seconds
    thread::sleep(Duration::from_secs(5));

    // 6. Stop capture
    println!("Stopping audio capture...");
    if let Err(e) = audio_capture.stop() {
        eprintln!("Failed to stop audio capture: {:?}", e);
    } else {
        println!("Capture stopped successfully.");
    }

    // The processor instance is owned by AudioCapture.
    // To get results back, the processor would typically use an Arc<Mutex<...>>
    // to share data with the main thread, as shown in the test cases.
    // For this example, we just print from within the processor.
    println!("Example finished.");
}
