//! Windows Application-Specific Audio Capture Example
//!
//! This example demonstrates how to capture audio from specific Windows applications
//! using WASAPI Process Loopback functionality. It targets applications by process ID
//! and can capture from the entire process tree.
//!
//! Usage:
//! ```bash
//! cargo run --example windows_application_capture --features feat_windows
//! ```
//!
//! Requirements:
//! - Windows 10 version 2004 (20H1) or later
//! - Target application must be running and producing audio

use std::collections::VecDeque;
use std::error::Error;
use std::ffi::OsStr;
use std::fs::File;
use std::io::Write;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
use {
    sysinfo::{ProcessRefreshKind, RefreshKind, System},
    wasapi::*,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[cfg(target_os = "windows")]
fn find_process_by_name(process_name: &str) -> Option<u32> {
    let refreshes = RefreshKind::nothing().with_processes(ProcessRefreshKind::everything());
    let system = System::new_with_specifics(refreshes);
    let process_ids = system.processes_by_name(OsStr::new(process_name));
    
    for process in process_ids {
        // Use parent process ID if available for better capture coverage
        let pid = process.parent().unwrap_or(process.pid()).as_u32();
        println!("Found {} with PID: {}", process_name, pid);
        return Some(pid);
    }
    None
}

#[cfg(target_os = "windows")]
fn capture_application_audio(
    process_id: u32,
    duration_secs: u64,
    output_file: &str,
) -> Result<()> {
    println!("🎵 Starting Windows application audio capture");
    println!("Target Process ID: {}", process_id);
    println!("Duration: {} seconds", duration_secs);
    println!("Output file: {}", output_file);

    // Initialize COM for WASAPI
    initialize_mta()?;

    // Configure audio format - 48kHz, 32-bit float, stereo
    let desired_format = WaveFormat::new(32, 32, &SampleType::Float, 48000, 2, None);
    let blockalign = desired_format.get_blockalign();
    println!("Audio format: {:?}", desired_format);

    // Create application loopback client
    let include_tree = true; // Capture entire process tree
    let mut audio_client = AudioClient::new_application_loopback_client(process_id, include_tree)?;
    
    // Initialize client with event-driven shared mode
    let autoconvert = true;
    let mode = StreamMode::EventsShared {
        autoconvert,
        buffer_duration_hns: 0, // Use default buffer size
    };
    
    audio_client.initialize_client(&desired_format, &Direction::Capture, &mode)?;
    println!("✅ Audio client initialized");

    // Get event handle for synchronization
    let h_event = audio_client.set_get_eventhandle()?;
    let capture_client = audio_client.get_audiocaptureclient()?;

    // Create output file
    let mut outfile = File::create(output_file)?;
    println!("✅ Output file created: {}", output_file);

    // Sample buffer management
    let mut sample_queue: VecDeque<u8> = VecDeque::new();
    let chunksize = 4096; // Process in chunks of 4096 frames
    let mut total_samples = 0u64;
    let mut peak_level = 0.0f32;

    // Start audio stream
    audio_client.start_stream()?;
    println!("🔴 Recording started...");

    let start_time = Instant::now();
    let target_duration = Duration::from_secs(duration_secs);

    // Main capture loop
    while start_time.elapsed() < target_duration {
        // Process queued samples in chunks
        while sample_queue.len() >= (blockalign as usize * chunksize) {
            let mut chunk = vec![0u8; blockalign as usize * chunksize];
            for element in chunk.iter_mut() {
                *element = sample_queue.pop_front().unwrap();
            }
            
            // Calculate peak level for monitoring
            if desired_format.get_sampletype() == SampleType::Float {
                let float_samples: &[f32] = unsafe {
                    std::slice::from_raw_parts(
                        chunk.as_ptr() as *const f32,
                        chunk.len() / 4,
                    )
                };
                let chunk_peak = float_samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
                peak_level = peak_level.max(chunk_peak);
            }
            
            outfile.write_all(&chunk)?;
            total_samples += chunksize as u64;
            
            // Progress update every 50k samples (~1 second at 48kHz)
            if total_samples % 50000 == 0 {
                let elapsed = start_time.elapsed().as_secs_f32();
                println!("📊 {:.1}s - {} samples, peak: {:.3}", elapsed, total_samples, peak_level);
            }
        }

        // Capture new audio data
        let new_frames = capture_client.get_next_packet_size()?.unwrap_or(0);
        if new_frames > 0 {
            let additional = (new_frames as usize * blockalign as usize)
                .saturating_sub(sample_queue.capacity() - sample_queue.len());
            sample_queue.reserve(additional);
            capture_client.read_from_device_to_deque(&mut sample_queue)?;
        }

        // Wait for next audio event (3 second timeout)
        if h_event.wait_for_event(3000).is_err() {
            println!("⚠️ Timeout waiting for audio event");
            break;
        }
    }

    // Stop recording
    audio_client.stop_stream()?;
    
    // Flush remaining samples
    while !sample_queue.is_empty() {
        let chunk_size = std::cmp::min(sample_queue.len(), blockalign as usize * chunksize);
        let mut chunk = vec![0u8; chunk_size];
        for element in chunk.iter_mut() {
            *element = sample_queue.pop_front().unwrap();
        }
        outfile.write_all(&chunk)?;
        total_samples += (chunk_size / blockalign as usize) as u64;
    }

    println!("✅ Recording completed!");
    println!("Total samples captured: {}", total_samples);
    println!("Peak level: {:.3}", peak_level);
    println!("Duration: {:.2}s", start_time.elapsed().as_secs_f32());
    
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn main() {
    println!("❌ This example only works on Windows");
    println!("Use --features feat_windows when building on Windows");
}

#[cfg(target_os = "windows")]
fn main() -> Result<()> {
    println!("🎵 Windows Application Audio Capture Example");
    println!("===========================================");

    // Example 1: Capture from Firefox
    println!("\n1️⃣ Attempting to capture from Firefox...");
    if let Some(firefox_pid) = find_process_by_name("firefox.exe") {
        match capture_application_audio(firefox_pid, 5, "firefox_capture.raw") {
            Ok(()) => println!("✅ Firefox capture completed successfully"),
            Err(e) => println!("❌ Firefox capture failed: {}", e),
        }
    } else {
        println!("⚠️ Firefox not found - please start Firefox and try again");
    }

    // Example 2: Capture from VLC
    println!("\n2️⃣ Attempting to capture from VLC...");
    if let Some(vlc_pid) = find_process_by_name("vlc.exe") {
        match capture_application_audio(vlc_pid, 5, "vlc_capture.raw") {
            Ok(()) => println!("✅ VLC capture completed successfully"),
            Err(e) => println!("❌ VLC capture failed: {}", e),
        }
    } else {
        println!("⚠️ VLC not found - please start VLC and try again");
    }

    // Example 3: Capture from Chrome
    println!("\n3️⃣ Attempting to capture from Chrome...");
    if let Some(chrome_pid) = find_process_by_name("chrome.exe") {
        match capture_application_audio(chrome_pid, 5, "chrome_capture.raw") {
            Ok(()) => println!("✅ Chrome capture completed successfully"),
            Err(e) => println!("❌ Chrome capture failed: {}", e),
        }
    } else {
        println!("⚠️ Chrome not found - please start Chrome and try again");
    }

    println!("\n🎯 Windows application capture examples completed!");
    println!("Raw audio files saved - use audio software to convert to WAV/MP3");
    
    Ok(())
}
