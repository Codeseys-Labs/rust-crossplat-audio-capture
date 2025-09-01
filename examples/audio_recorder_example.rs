//! Audio Recording Examples
//!
//! This example demonstrates how to record audio from PipeWire applications
//! and save it to WAV files with various configurations.
//!
//! Usage:
//! ```bash
//! cargo run --example audio_recorder_example --features feat_linux
//! ```

use rsac::audio::linux::pipewire::{PipeWireApplicationCapture, ApplicationSelector};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::path::Path;
use std::fs::File;
use std::io::BufWriter;
use hound::{WavWriter, WavSpec, SampleFormat};

/// Audio recording configuration
#[derive(Debug, Clone)]
pub struct RecordingConfig {
    pub output_path: String,
    pub duration: Duration,
    pub sample_rate: u32,
    pub channels: u16,
    pub format: AudioFormat,
}

/// Supported audio formats for recording
#[derive(Debug, Clone)]
pub enum AudioFormat {
    Wav,
    // Future: Flac, Mp3, Ogg, etc.
}

/// Audio recorder that captures from PipeWire and saves to file
pub struct AudioRecorder {
    config: RecordingConfig,
    samples_captured: Arc<AtomicUsize>,
    is_recording: Arc<AtomicBool>,
    start_time: Option<Instant>,
}

impl AudioRecorder {
    pub fn new(config: RecordingConfig) -> Self {
        Self {
            config,
            samples_captured: Arc::new(AtomicUsize::new(0)),
            is_recording: Arc::new(AtomicBool::new(false)),
            start_time: None,
        }
    }

    /// Record audio from a specific application to file
    pub fn record_application(&mut self, app_selector: ApplicationSelector) -> Result<(), Box<dyn std::error::Error>> {
        println!("🎙️  Starting audio recording...");
        println!("    📁 Output: {}", self.config.output_path);
        println!("    ⏱️  Duration: {:?}", self.config.duration);
        println!("    🎵 Format: {:?}", self.config.format);

        // Create the audio writer based on format
        let audio_writer = self.create_audio_writer()?;
        let audio_writer = Arc::new(Mutex::new(Some(audio_writer)));

        // Set up PipeWire capture
        let mut capture = PipeWireApplicationCapture::new(app_selector);
        
        // Discover and create stream
        match capture.discover_target_node() {
            Ok(node_id) => {
                println!("✅ Found target application node: {}", node_id);
            }
            Err(e) => {
                println!("❌ Failed to find target application: {}", e);
                return Err(e);
            }
        }

        if let Err(e) = capture.create_monitor_stream() {
            println!("❌ Failed to create monitor stream: {}", e);
            return Err(e);
        }

        // Set up recording state
        self.is_recording.store(true, Ordering::SeqCst);
        self.start_time = Some(Instant::now());
        
        let samples_captured = self.samples_captured.clone();
        let is_recording = self.is_recording.clone();
        let audio_writer_clone = audio_writer.clone();
        let start_time = self.start_time.unwrap();

        // Create the audio processing callback
        let audio_callback = move |samples: &[f32]| {
            if !is_recording.load(Ordering::SeqCst) {
                return; // Stop processing if recording stopped
            }

            let count = samples_captured.fetch_add(samples.len(), Ordering::SeqCst);
            
            // Write samples to file
            if let Ok(mut writer_option) = audio_writer_clone.lock() {
                if let Some(ref mut writer) = writer_option.as_mut() {
                    for &sample in samples {
                        // Convert f32 to i16 for WAV format (16-bit PCM)
                        let sample_i16 = (sample * i16::MAX as f32) as i16;
                        if let Err(e) = writer.write_sample(sample_i16) {
                            eprintln!("⚠️  Error writing sample: {}", e);
                            break;
                        }
                    }
                }
            }

            // Print progress every second worth of samples (48000 * 2 channels = 96000 samples)
            if count % 96000 == 0 {
                let elapsed = start_time.elapsed();
                let samples_per_second = count as f64 / elapsed.as_secs_f64();
                println!("    📊 {:.1}s: {} samples ({:.0} samples/sec)", 
                         elapsed.as_secs_f32(), count + samples.len(), samples_per_second);
            }
        };

        // Start recording
        println!("🔴 Recording started...");
        let result = capture.start_capture_for_duration(audio_callback, self.config.duration);

        // Stop recording
        self.is_recording.store(false, Ordering::SeqCst);
        
        // Finalize the audio file
        if let Ok(mut writer_option) = audio_writer.lock() {
            if let Some(writer) = writer_option.take() {
                if let Err(e) = writer.finalize() {
                    eprintln!("⚠️  Error finalizing audio file: {}", e);
                } else {
                    println!("✅ Audio file finalized successfully");
                }
            }
        }

        match result {
            Ok(()) => {
                let total_samples = self.samples_captured.load(Ordering::SeqCst);
                let duration_actual = self.start_time.unwrap().elapsed();
                let estimated_duration = total_samples as f64 / (self.config.sample_rate as f64 * self.config.channels as f64);
                
                println!("✅ Recording completed successfully!");
                println!("    📊 Total samples: {}", total_samples);
                println!("    ⏱️  Actual duration: {:.2}s", duration_actual.as_secs_f32());
                println!("    🎵 Estimated audio duration: {:.2}s", estimated_duration);
                println!("    📁 Saved to: {}", self.config.output_path);
                
                // Verify file was created
                if Path::new(&self.config.output_path).exists() {
                    let file_size = std::fs::metadata(&self.config.output_path)?.len();
                    println!("    💾 File size: {} bytes ({:.2} MB)", file_size, file_size as f64 / 1_048_576.0);
                } else {
                    println!("⚠️  Warning: Output file not found!");
                }
                
                Ok(())
            }
            Err(e) => {
                println!("❌ Recording failed: {}", e);
                Err(e)
            }
        }
    }

    /// Create an audio writer based on the configured format
    fn create_audio_writer(&self) -> Result<WavWriter<BufWriter<File>>, Box<dyn std::error::Error>> {
        match self.config.format {
            AudioFormat::Wav => {
                let spec = WavSpec {
                    channels: self.config.channels,
                    sample_rate: self.config.sample_rate,
                    bits_per_sample: 16, // 16-bit PCM
                    sample_format: SampleFormat::Int,
                };

                let file = File::create(&self.config.output_path)?;
                let writer = WavWriter::new(BufWriter::new(file), spec)?;
                Ok(writer)
            }
        }
    }

    /// Get recording statistics
    pub fn get_stats(&self) -> RecordingStats {
        let samples = self.samples_captured.load(Ordering::SeqCst);
        let duration = self.start_time.map(|start| start.elapsed()).unwrap_or_default();
        
        RecordingStats {
            samples_captured: samples,
            duration_elapsed: duration,
            estimated_audio_duration: samples as f64 / (self.config.sample_rate as f64 * self.config.channels as f64),
            is_recording: self.is_recording.load(Ordering::SeqCst),
        }
    }
}

/// Recording statistics
#[derive(Debug)]
pub struct RecordingStats {
    pub samples_captured: usize,
    pub duration_elapsed: Duration,
    pub estimated_audio_duration: f64,
    pub is_recording: bool,
}

fn main() {
    println!("🎙️  PipeWire Audio Recorder Examples");
    println!("====================================");

    // Example 1: Record VLC audio to WAV file
    println!("\n1️⃣ Example 1: Record VLC audio for 5 seconds");
    example_record_vlc_to_wav();

    // Example 2: Record with custom configuration
    println!("\n2️⃣ Example 2: Record with custom configuration");
    example_custom_recording();

    // Example 3: Multiple short recordings
    println!("\n3️⃣ Example 3: Multiple short recordings");
    example_multiple_recordings();

    println!("\n✅ All recording examples completed!");
}

/// Example 1: Simple VLC recording to WAV
fn example_record_vlc_to_wav() {
    let config = RecordingConfig {
        output_path: "vlc_recording.wav".to_string(),
        duration: Duration::from_secs(5),
        sample_rate: 48000,
        channels: 2,
        format: AudioFormat::Wav,
    };

    let mut recorder = AudioRecorder::new(config);
    
    match recorder.record_application(ApplicationSelector::NodeId(62)) {
        Ok(()) => {
            let stats = recorder.get_stats();
            println!("📈 Final stats: {:?}", stats);
        }
        Err(e) => {
            println!("❌ Recording failed: {}", e);
        }
    }
}

/// Example 2: Custom configuration recording
fn example_custom_recording() {
    let config = RecordingConfig {
        output_path: "custom_recording.wav".to_string(),
        duration: Duration::from_secs(3),
        sample_rate: 48000,
        channels: 2,
        format: AudioFormat::Wav,
    };

    let mut recorder = AudioRecorder::new(config);
    
    println!("🔧 Recording with custom configuration...");
    match recorder.record_application(ApplicationSelector::NodeId(62)) {
        Ok(()) => {
            println!("✅ Custom recording completed");
        }
        Err(e) => {
            println!("❌ Custom recording failed: {}", e);
        }
    }
}

/// Example 3: Multiple short recordings
fn example_multiple_recordings() {
    for i in 1..=3 {
        println!("🎬 Recording session {}/3", i);
        
        let config = RecordingConfig {
            output_path: format!("recording_{}.wav", i),
            duration: Duration::from_secs(2),
            sample_rate: 48000,
            channels: 2,
            format: AudioFormat::Wav,
        };

        let mut recorder = AudioRecorder::new(config);
        
        match recorder.record_application(ApplicationSelector::NodeId(62)) {
            Ok(()) => {
                println!("✅ Session {} completed", i);
            }
            Err(e) => {
                println!("❌ Session {} failed: {}", i, e);
                break;
            }
        }
        
        // Small delay between recordings
        if i < 3 {
            println!("⏸️  Pausing 1 second between recordings...");
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}
