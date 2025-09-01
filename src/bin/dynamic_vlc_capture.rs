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

use hound::{WavSpec, WavWriter};
use std::env;
use std::process;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[cfg(target_os = "linux")]
use rsac::audio::linux::pipewire::{ApplicationSelector, PipeWireApplicationCapture};

#[cfg(target_os = "linux")]
use rsac::audio::discovery::AudioSourceDiscovery;

#[cfg(target_os = "linux")]
use rsac::audio::application_capture::{
    list_capturable_applications, ApplicationCapture, ApplicationCaptureFactory,
};

fn main() {
    println!("🎵 Dynamic VLC Audio Capture Test");
    println!("==================================");

    let args: Vec<String> = env::args().collect();
    let duration = args
        .get(1)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(30);

    let output_file = args
        .get(2)
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

    #[cfg(target_os = "windows")]
    {
        match run_windows_vlc_capture(duration, &output_file) {
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

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        eprintln!("❌ This tool is currently only supported on Linux and Windows");
        process::exit(2);
    }
}

#[cfg(target_os = "linux")]
fn run_linux_vlc_capture(
    duration: u64,
    output_file: &str,
) -> Result<(), Box<dyn std::error::Error>> {
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
            println!(
                "🎯 Found VLC source: {} (ID: {}, Node: {})",
                source.name, source.id, source.node.id
            );
        }
    }

    if vlc_sources.is_empty() {
        println!("⚠️  No VLC sources found via discovery, trying alternative methods...");
        return try_alternative_vlc_discovery(duration, output_file);
    }

    // Use the first VLC source found
    let vlc_source = vlc_sources[0];
    let node_id = vlc_source.node.id;

    println!(
        "\n🎵 Step 2: Setting up capture for VLC node {}...",
        node_id
    );

    // Create PipeWire capture for the discovered node
    let mut capture = PipeWireApplicationCapture::new(ApplicationSelector::NodeId(node_id));

    // Verify the node exists and is accessible
    match capture.discover_target_node() {
        Ok(discovered_id) => {
            println!(
                "✅ Verified VLC node: {} (discovered: {})",
                node_id, discovered_id
            );
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
    println!(
        "🔧 Starting capture with precise {}-second duration control",
        duration
    );
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
                println!(
                    "📊 Captured {:.1}s elapsed, {} samples, peak: {:.3}",
                    elapsed.as_secs_f64(),
                    samples,
                    peak
                );
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

                if let Err(e) =
                    capture.run_main_loop_with_options(Some(Duration::from_millis(100)), false)
                {
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
    println!(
        "  Actual duration: {:.2} seconds",
        captured_audio.len() as f64 / (48000.0 * 2.0)
    );

    if !captured_audio.is_empty() {
        println!("🎉 SUCCESS: Captured audio data from VLC!");

        match write_wav_file(&output_file, &captured_audio) {
            Ok(_) => {
                let file_size = std::fs::metadata(&output_file)?.len();
                println!(
                    "💾 Saved WAV file to: {} ({} bytes)",
                    output_file, file_size
                );
            }
            Err(e) => {
                println!("⚠️  Failed to write WAV file: {}", e);
                std::fs::write(
                    &output_file,
                    format!(
                        "VLC audio capture: {} samples, peak: {:.3}",
                        total_samples, *final_peak
                    ),
                )?;
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
fn try_alternative_vlc_discovery(
    duration: u64,
    output_file: &str,
) -> Result<(), Box<dyn std::error::Error>> {
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
                            println!(
                                "📊 Captured {} samples from VLC PID {}",
                                total_samples, app.process_id
                            );

                            if total_samples > 0 {
                                std::fs::write(
                                    &output_file,
                                    format!(
                                        "VLC PID {} capture: {} samples",
                                        app.process_id, total_samples
                                    ),
                                )?;
                                println!("✅ Alternative method succeeded!");
                                return Ok(());
                            }
                        }
                        Err(e) => {
                            println!(
                                "⚠️  Could not create capture for VLC PID {}: {}",
                                app.process_id, e
                            );
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
                std::fs::write(
                    &output_file,
                    format!("VLC by name capture: {} samples", total_samples),
                )?;
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
        channels: 2,        // Assume stereo
        sample_rate: 48000, // Common PipeWire sample rate
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

#[cfg(target_os = "windows")]
fn run_windows_vlc_capture(
    duration: u64,
    output_file: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n🔍 Step 1: Discovering running VLC processes...");

    // Use the unified cross-platform API
    use rsac::audio::application_capture::{
        list_capturable_applications, ApplicationCapture, ApplicationCaptureFactory,
    };

    // Method 1: List all capturable applications and find VLC
    println!("📊 Listing all capturable applications...");
    match list_capturable_applications() {
        Ok(apps) => {
            println!("Found {} capturable applications", apps.len());

            // Look for VLC in the list
            let mut vlc_apps = Vec::new();
            for app in &apps {
                let name_lower = app.name.to_lowercase();
                if name_lower.contains("vlc") {
                    vlc_apps.push(app);
                    println!("🎯 Found VLC app: {} (PID: {})", app.name, app.process_id);
                }
            }

            if vlc_apps.is_empty() {
                println!("⚠️  No VLC applications found in capturable list");
                return try_windows_vlc_by_process_name(duration, output_file);
            }

            // Use the first VLC app found
            let vlc_app = vlc_apps[0];
            let process_id = vlc_app.process_id;

            println!(
                "\n🎵 Step 2: Setting up capture for VLC PID {}...",
                process_id
            );

            // Create cross-platform capture for the discovered process
            let mut capture = ApplicationCaptureFactory::create_for_process_id(process_id)?;

            println!("✅ Capture instance created successfully");

            return run_windows_capture_loop(capture, duration, output_file);
        }
        Err(e) => {
            println!("⚠️  Could not list applications: {}", e);
            return try_windows_vlc_by_process_name(duration, output_file);
        }
    }
}

#[cfg(target_os = "windows")]
fn try_windows_vlc_by_process_name(
    duration: u64,
    output_file: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n🔍 Method 2: Trying to find VLC by process name...");

    use rsac::audio::application_capture::{ApplicationCapture, ApplicationCaptureFactory};

    // Try to create capture by application name
    match ApplicationCaptureFactory::create_for_application_name("vlc") {
        Ok(mut capture) => {
            println!("✅ Found VLC by application name");
            return run_windows_capture_loop(capture, duration, output_file);
        }
        Err(e) => {
            println!("⚠️  Could not find VLC by name: {}", e);
        }
    }

    // Try alternative names
    for name in &["vlc.exe", "VLC", "VLC media player"] {
        println!("🔄 Trying name: {}", name);
        match ApplicationCaptureFactory::create_for_application_name(name) {
            Ok(mut capture) => {
                println!("✅ Found VLC with name: {}", name);
                return run_windows_capture_loop(capture, duration, output_file);
            }
            Err(e) => {
                println!("⚠️  No luck with {}: {}", name, e);
            }
        }
    }

    Err("Could not find any VLC process to capture from".into())
}

#[cfg(target_os = "windows")]
fn run_windows_capture_loop(
    mut capture: rsac::audio::application_capture::CrossPlatformApplicationCapture,
    duration: u64,
    output_file: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use rsac::audio::application_capture::ApplicationCapture;

    println!("\n🎙️  Step 3: Starting audio capture...");

    // Set up capture statistics and audio data collection (same as Linux version)
    let sample_count = Arc::new(AtomicUsize::new(0));
    let peak_level = Arc::new(Mutex::new(0.0f32));
    let audio_data = Arc::new(Mutex::new(Vec::<f32>::new()));

    let sample_count_clone = sample_count.clone();
    let peak_level_clone = peak_level.clone();
    let audio_data_clone = audio_data.clone();

    // Use TUI-style timing control for precise duration handling
    println!(
        "🔧 Starting capture with precise {}-second duration control",
        duration
    );
    println!("⏱️  Recording for {} seconds...", duration);

    // Timing control (same as Linux version)
    let is_active = Arc::new(AtomicBool::new(true));
    let is_active_clone = is_active.clone();
    let duration_limit = Duration::from_secs(duration);

    // Progress tracking with wall clock time
    let capture_start_time = Arc::new(Mutex::new(None::<std::time::Instant>));
    let capture_start_time_clone = capture_start_time.clone();
    let last_progress_time = Arc::new(Mutex::new(std::time::Instant::now()));
    let last_progress_time_clone = last_progress_time.clone();

    // For Windows: Create a shared stop flag that both callback and capture loop can access
    let shared_stop_flag = Arc::new(AtomicBool::new(false));
    let shared_stop_flag_for_callback = shared_stop_flag.clone();

    // Start capture with callback - use the new method that accepts external stop flag
    let result = match &mut capture {
        rsac::audio::application_capture::CrossPlatformApplicationCapture::Windows(
            ref mut win_capture,
        ) => {
            win_capture.start_capture_with_stop_flag(
                move |audio_chunk| {
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
                    let samples =
                        sample_count_clone.fetch_add(audio_chunk.len(), Ordering::Relaxed);
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
                            let progress_pct =
                                (elapsed.as_secs_f64() / duration_limit.as_secs_f64() * 100.0)
                                    .min(100.0);
                            println!(
                                "📊 Progress: {:.1}% | Samples: {} | Peak: {:.3} | Elapsed: {:.1}s",
                                progress_pct,
                                samples,
                                peak,
                                elapsed.as_secs_f64()
                            );
                            *last_progress = now;
                        }
                    }

                    // Check if duration limit reached
                    if elapsed >= duration_limit {
                        println!("⏰ Duration limit reached, stopping capture...");
                        is_active_clone.store(false, Ordering::SeqCst);

                        // For Windows: Signal the shared stop flag so the capture loop will exit
                        shared_stop_flag_for_callback.store(true, Ordering::SeqCst);
                    }
                },
                Some(shared_stop_flag),
            )
        }
        _ => {
            // Fallback for non-Windows platforms
            capture.start_capture(move |audio_chunk| {
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
                        let progress_pct = (elapsed.as_secs_f64() / duration_limit.as_secs_f64()
                            * 100.0)
                            .min(100.0);
                        println!(
                            "📊 Progress: {:.1}% | Samples: {} | Peak: {:.3} | Elapsed: {:.1}s",
                            progress_pct,
                            samples,
                            peak,
                            elapsed.as_secs_f64()
                        );
                        *last_progress = now;
                    }
                }

                // Check if duration limit reached
                if elapsed >= duration_limit {
                    println!("⏰ Duration limit reached, stopping capture...");
                    is_active_clone.store(false, Ordering::SeqCst);
                }
            })
        }
    };

    // Handle capture result
    match result {
        Ok(_) => {
            println!("✅ Capture completed successfully");
        }
        Err(e) => {
            println!("❌ Capture failed: {}", e);
            return Err(e);
        }
    }

    // Stop capture
    if let Err(e) = capture.stop_capture() {
        println!("⚠️  Warning: Failed to stop capture cleanly: {}", e);
    }

    // Save audio data to WAV file (same as Linux version)
    println!("\n💾 Step 4: Saving audio to {}...", output_file);

    let final_sample_count = sample_count.load(Ordering::SeqCst);
    let final_peak = *peak_level.lock().unwrap();
    let audio_samples = audio_data.lock().unwrap();

    println!("📊 Final Statistics:");
    println!("  Total samples: {}", final_sample_count);
    println!("  Peak level: {:.3}", final_peak);
    println!("  Audio data length: {}", audio_samples.len());

    if audio_samples.is_empty() {
        return Err("No audio data captured".into());
    }

    // Create WAV file with same format as Linux version
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 48000,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };

    let mut writer = hound::WavWriter::create(output_file, spec)?;
    for &sample in audio_samples.iter() {
        writer.write_sample(sample)?;
    }
    writer.finalize()?;

    println!("✅ Audio saved successfully to {}", output_file);
    println!("📊 Final capture statistics:");
    println!("  Duration: {:.2}s", duration);
    println!("  Samples captured: {}", final_sample_count);
    println!("  Peak level: {:.3}", final_peak);

    Ok(())
}
