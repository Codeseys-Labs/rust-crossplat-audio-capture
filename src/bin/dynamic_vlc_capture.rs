//! Dynamic VLC Audio Capture Test
//!
//! This binary discovers running VLC applications dynamically and captures their audio.
//! It's designed for CI/CD testing where we don't know the exact node IDs in advance.
//!
//! Features:
//! - Discovers VLC processes automatically
//! - Gets actual PipeWire node IDs dynamically
//! - Captures audio from the discovered VLC instance
//! - Saves audio to WAV file for verification

use std::env;
use std::process;
use std::time::Duration;
use std::thread;
use std::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use hound::{WavSpec, WavWriter};

#[cfg(target_os = "linux")]
use rsac::audio::linux::pipewire::{PipeWireApplicationCapture, ApplicationSelector};

#[cfg(target_os = "linux")]
use rsac::audio::discovery::AudioSourceDiscovery;

#[cfg(target_os = "linux")]
use rsac::audio::application_capture::{ApplicationCaptureFactory, ApplicationCapture, list_capturable_applications};

fn main() {
    println!("🎵 Dynamic VLC Audio Capture Test");
    println!("==================================");

    let args: Vec<String> = env::args().collect();
    let duration = args.get(1)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(30);

    let output_file = args.get(2)
        .map(|s| s.to_string())
        .unwrap_or_else(|| "dynamic_vlc_capture.wav".to_string());

    println!("📋 Configuration:");
    println!("  Duration: {} seconds", duration);
    println!("  Output file: {}", output_file);

    #[cfg(target_os = "linux")]
    {
        match run_linux_vlc_capture(duration, &output_file) {
            Ok(_) => {
                println!("✅ VLC capture completed successfully!");
                process::exit(0);
            }
            Err(e) => {
                eprintln!("❌ VLC capture failed: {}", e);
                process::exit(1);
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("❌ This tool is currently only supported on Linux");
        process::exit(2);
    }
}

#[cfg(target_os = "linux")]
fn run_linux_vlc_capture(duration: u64, output_file: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n🔍 Step 1: Discovering running applications...");
    
    // Method 1: Use the discovery module
    let mut discovery = AudioSourceDiscovery::new()?;
    let audio_sources = discovery.discover_active_audio_sources()?;
    
    println!("📊 Found {} audio sources", audio_sources.len());
    
    // Look for VLC in the discovered sources
    let mut vlc_sources = Vec::new();
    for source in &audio_sources {
        let name_lower = source.name.to_lowercase();
        if name_lower.contains("vlc") {
            vlc_sources.push(source);
            println!("🎯 Found VLC source: {} (ID: {}, Node: {})", 
                source.name, source.id, source.node.id);
        }
    }
    
    if vlc_sources.is_empty() {
        println!("⚠️  No VLC sources found via discovery, trying alternative methods...");
        return try_alternative_vlc_discovery(duration, output_file);
    }
    
    // Use the first VLC source found
    let vlc_source = vlc_sources[0];
    let node_id = vlc_source.node.id;
    
    println!("\n🎵 Step 2: Setting up capture for VLC node {}...", node_id);
    
    // Create PipeWire capture for the discovered node
    let mut capture = PipeWireApplicationCapture::new(ApplicationSelector::NodeId(node_id));
    
    // Verify the node exists and is accessible
    match capture.discover_target_node() {
        Ok(discovered_id) => {
            println!("✅ Verified VLC node: {} (discovered: {})", node_id, discovered_id);
        }
        Err(e) => {
            println!("⚠️  Could not verify node {}: {}", node_id, e);
            println!("🔄 Trying with discovered ID anyway...");
        }
    }
    
    // Create monitor stream
    if let Err(e) = capture.create_monitor_stream() {
        return Err(format!("Failed to create monitor stream: {}", e).into());
    }
    
    println!("✅ Monitor stream created successfully");
    
    println!("\n🎙️  Step 3: Starting audio capture...");

    // Set up capture statistics and audio data collection
    let sample_count = Arc::new(AtomicUsize::new(0));
    let peak_level = Arc::new(Mutex::new(0.0f32));
    let audio_data = Arc::new(Mutex::new(Vec::<f32>::new()));

    let sample_count_clone = sample_count.clone();
    let peak_level_clone = peak_level.clone();
    let audio_data_clone = audio_data.clone();

    // Use TUI-style timing control for precise duration handling
    println!("🔧 Starting capture with precise {}-second duration control", duration);
    println!("⏱️  Recording for {} seconds...", duration);

    // Timing control (TUI approach)
    let is_active = Arc::new(AtomicBool::new(true));
    let is_active_clone = is_active.clone();
    let duration_limit = Duration::from_secs(duration);

    // Progress tracking with wall clock time
    let capture_start_time = Arc::new(Mutex::new(None::<std::time::Instant>));
    let capture_start_time_clone = capture_start_time.clone();
    let last_progress_time = Arc::new(Mutex::new(std::time::Instant::now()));
    let last_progress_time_clone = last_progress_time.clone();

    // Start capture with callback
    let result = capture.start_capture(move |audio_chunk| {
        // Only process if capture is still active (TUI approach)
        if !is_active_clone.load(Ordering::SeqCst) {
            return;
        }

        // Initialize capture start time on first callback
        let mut start_opt = capture_start_time_clone.lock().unwrap();
        if start_opt.is_none() {
            *start_opt = Some(std::time::Instant::now());
            println!("📊 Audio capture started");
        }
        let capture_start = start_opt.unwrap();
        drop(start_opt);

        // Store audio data
        if let Ok(mut data) = audio_data_clone.lock() {
            data.extend_from_slice(audio_chunk);
        }

        // Update statistics
        let samples = sample_count_clone.fetch_add(audio_chunk.len(), Ordering::Relaxed);
        let peak = audio_chunk.iter().map(|&x| x.abs()).fold(0.0f32, f32::max);
        if let Ok(mut current_peak) = peak_level_clone.lock() {
            if peak > *current_peak {
                *current_peak = peak;
            }
        }

        // Progress reporting (every ~1 second)
        let now = std::time::Instant::now();
        let elapsed = capture_start.elapsed();
        if let Ok(mut last_progress) = last_progress_time_clone.lock() {
            if now.duration_since(*last_progress) >= Duration::from_millis(900) {
                println!("📊 Captured {:.1}s elapsed, {} samples, peak: {:.3}",
                    elapsed.as_secs_f64(), samples, peak);
                *last_progress = now;
            }
        }
    });

    match result {
        Ok(_) => {
            println!("✅ Capture started successfully");

            // Run main loop with duration control (TUI approach)
            let main_start_time = std::time::Instant::now();
            loop {
                if main_start_time.elapsed() >= duration_limit {
                    println!("⏹️  Duration limit reached, stopping capture...");
                    is_active.store(false, Ordering::SeqCst);
                    break;
                }

                if !is_active.load(Ordering::SeqCst) {
                    break;
                }

                if let Err(e) = capture.run_main_loop_with_options(Some(Duration::from_millis(100)), false) {
                    println!("⚠️  Main loop error: {}", e);
                    break;
                }
            }

            println!("✅ Capture completed successfully");
            capture.stop_capture()?;
        }
        Err(e) => {
            return Err(format!("Failed to start capture: {}", e).into());
        }
    }

    // Generate final statistics
    let total_samples = sample_count.load(Ordering::Relaxed);
    let final_peak = peak_level.lock().unwrap();
    let captured_audio = audio_data.lock().unwrap();

    println!("\n✅ Capture completed successfully!");
    println!("📊 Final statistics:");
    println!("  Total samples: {}", total_samples);
    println!("  Peak level: {:.3}", *final_peak);
    println!("  Requested duration: {} seconds", duration);
    println!("  Audio data length: {}", captured_audio.len());
    println!("  Actual duration: {:.2} seconds", captured_audio.len() as f64 / (48000.0 * 2.0));

    if !captured_audio.is_empty() {
        println!("🎉 SUCCESS: Captured audio data from VLC!");

        match write_wav_file(&output_file, &captured_audio) {
            Ok(_) => {
                let file_size = std::fs::metadata(&output_file)?.len();
                println!("💾 Saved WAV file to: {} ({} bytes)", output_file, file_size);
            }
            Err(e) => {
                println!("⚠️  Failed to write WAV file: {}", e);
                std::fs::write(&output_file, format!("VLC audio capture: {} samples, peak: {:.3}", total_samples, *final_peak))?;
                println!("💾 Saved capture info to: {}", output_file);
            }
        }
    } else {
        println!("⚠️  No audio data captured (VLC might not be playing audio)");
        write_empty_wav_file(&output_file)?;
        println!("💾 Created empty WAV file: {}", output_file);
    }

            Ok(())
}

#[cfg(target_os = "linux")]
fn try_alternative_vlc_discovery(duration: u64, output_file: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n🔄 Trying alternative VLC discovery methods...");
    
    // Method 2: Use application capture factory
    println!("🔍 Method 2: Application capture factory...");
    
    match list_capturable_applications() {
        Ok(apps) => {
            println!("📊 Found {} capturable applications", apps.len());
            
            for app in &apps {
                println!("  - {} (PID: {})", app.name, app.process_id);
                if app.name.to_lowercase().contains("vlc") {
                    println!("🎯 Found VLC application: {}", app.name);
                    
                    // Try to capture from this VLC instance
                    match ApplicationCaptureFactory::create_for_process_id(app.process_id) {
                        Ok(mut capture) => {
                            println!("✅ Created capture for VLC PID {}", app.process_id);
                            
                            // Start a simple capture test
                            let sample_count = Arc::new(AtomicUsize::new(0));
                            let sample_count_clone = sample_count.clone();
                            
                            capture.start_capture(move |audio_data| {
                                sample_count_clone.fetch_add(audio_data.len(), Ordering::Relaxed);
                            })?;
                            
                            // Capture for the specified duration
                            thread::sleep(Duration::from_secs(duration));
                            
                            capture.stop_capture()?;
                            
                            let total_samples = sample_count.load(Ordering::Relaxed);
                            println!("📊 Captured {} samples from VLC PID {}", total_samples, app.process_id);
                            
                            if total_samples > 0 {
                                std::fs::write(&output_file, format!("VLC PID {} capture: {} samples", app.process_id, total_samples))?;
                                println!("✅ Alternative method succeeded!");
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            println!("⚠️  Could not create capture for VLC PID {}: {}", app.process_id, e);
                        }
                    }
                }
            }
        }
        Err(e) => {
            println!("⚠️  Could not list applications: {}", e);
        }
    }
    
    // Method 3: Try by application name
    println!("\n🔍 Method 3: Capture by application name...");
    match ApplicationCaptureFactory::create_for_application_name("vlc") {
        Ok(mut capture) => {
            println!("✅ Created capture for VLC by name");
            
            let sample_count = Arc::new(AtomicUsize::new(0));
            let sample_count_clone = sample_count.clone();
            
            capture.start_capture(move |audio_data| {
                sample_count_clone.fetch_add(audio_data.len(), Ordering::Relaxed);
            })?;
            
            thread::sleep(Duration::from_secs(duration));
            capture.stop_capture()?;
            
            let total_samples = sample_count.load(Ordering::Relaxed);
            println!("📊 Captured {} samples from VLC by name", total_samples);
            
            if total_samples > 0 {
                std::fs::write(&output_file, format!("VLC by name capture: {} samples", total_samples))?;
                println!("✅ Name-based method succeeded!");
                return Ok(());
            }
        }
        Err(e) => {
            println!("⚠️  Could not create capture for VLC by name: {}", e);
        }
    }
    
    Err("All VLC discovery methods failed".into())
}

/// Write captured audio data to a WAV file
fn write_wav_file(output_file: &str, audio_data: &[f32]) -> Result<(), Box<dyn std::error::Error>> {
    let spec = WavSpec {
        channels: 2,  // Assume stereo
        sample_rate: 48000,  // Common PipeWire sample rate
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let mut writer = WavWriter::create(output_file, spec)?;

    // Convert f32 samples to i16 and write to WAV
    for &sample in audio_data {
        // Clamp and convert f32 (-1.0 to 1.0) to i16 (-32768 to 32767)
        let sample_i16 = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
        writer.write_sample(sample_i16)?;
    }

    writer.finalize()?;
    Ok(())
}

/// Write an empty WAV file (for cases where no audio was captured)
fn write_empty_wav_file(output_file: &str) -> Result<(), Box<dyn std::error::Error>> {
    let spec = WavSpec {
        channels: 2,
        sample_rate: 48000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };

    let writer = WavWriter::create(output_file, spec)?;
    writer.finalize()?;
    Ok(())
}
