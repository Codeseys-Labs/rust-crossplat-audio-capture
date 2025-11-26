//! Real PipeWire Integration Test
//!
//! This test uses the actual PipeWire Rust crate integration
//! following the wiremix approach to capture Firefox audio.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

// Import our PipeWire implementation
#[cfg(all(target_os = "linux", feature = "feat_linux"))]
use rsac::audio::linux::pipewire::{ApplicationSelector, PipeWireApplicationCapture};

#[cfg(all(target_os = "linux", feature = "feat_linux"))]
fn main() {
    println!("🎵 Real PipeWire Integration Test");
    println!("==================================");

    // Test 1: Detect Firefox and get its node info
    println!("\n1️⃣ Detecting Firefox with real PipeWire integration...");
    let firefox_pid = detect_firefox_main_process();

    if firefox_pid.is_none() {
        println!("❌ No Firefox main process found. Please start Firefox with audio content.");
        return;
    }

    let firefox_pid = firefox_pid.unwrap();
    println!("🦊 Found Firefox main process: PID {}", firefox_pid);

    // Test 2: Create PipeWire capture for Firefox
    println!("\n2️⃣ Creating PipeWire application capture...");

    // First try by PID, then fallback to known node ID
    let mut capture = PipeWireApplicationCapture::new(ApplicationSelector::ProcessId(firefox_pid));

    // Also try by node ID if we know it from CLI tools
    println!("💡 Also testing with known Firefox Node ID 52...");
    let mut capture_by_node = PipeWireApplicationCapture::new(ApplicationSelector::NodeId(52));

    // Also test VLC which is actively playing audio
    let mut capture_vlc = PipeWireApplicationCapture::new(ApplicationSelector::NodeId(62));

    // Test 3: Discover the target node
    println!("\n3️⃣ Discovering audio node...");

    // Try VLC first since it's actively playing audio
    let mut working_capture = None;
    println!("🎵 Trying VLC Node 62 (actively playing audio)...");
    match capture_vlc.discover_target_node() {
        Ok(node_id) => {
            println!("✅ Found VLC audio node: {}", node_id);
            working_capture = Some(capture_vlc);
        }
        Err(e) => {
            println!("⚠️  Failed to discover VLC Node 62: {}", e);

            // Try Firefox by PID
            println!("🔄 Trying Firefox by PID {}...", firefox_pid);
            match capture.discover_target_node() {
                Ok(node_id) => {
                    println!("✅ Found Firefox audio node by PID: {}", node_id);
                    working_capture = Some(capture);
                }
                Err(e) => {
                    println!("⚠️  Failed to discover by PID {}: {}", firefox_pid, e);

                    // Try Firefox by known node ID
                    println!("🔄 Trying Firefox Node ID 52...");
                    match capture_by_node.discover_target_node() {
                        Ok(node_id) => {
                            println!("✅ Found Firefox audio node by Node ID: {}", node_id);
                            working_capture = Some(capture_by_node);
                        }
                        Err(e2) => {
                            println!("❌ Failed to discover any audio node: {}", e2);
                            println!("💡 This could mean:");
                            println!("   - No applications are currently playing audio");
                            println!("   - PipeWire features are not enabled");
                            println!("   - Node discovery needs improvement");
                            return;
                        }
                    }
                }
            }
        }
    }

    let mut capture = working_capture.unwrap();

    // Test 4: Create monitor stream
    println!("\n4️⃣ Creating monitor stream...");
    match capture.create_monitor_stream() {
        Ok(()) => {
            println!("✅ Monitor stream created successfully");
        }
        Err(e) => {
            println!("❌ Failed to create monitor stream: {}", e);
            return;
        }
    }

    // Test 5: Start real audio capture
    println!("\n5️⃣ Starting real PipeWire audio capture...");

    let sample_count = Arc::new(AtomicUsize::new(0));
    let sample_count_clone = sample_count.clone();
    let peak_level = Arc::new(std::sync::Mutex::new(0.0f32));
    let peak_level_clone = peak_level.clone();

    let capture_result = capture.start_capture(move |samples| {
        let count = sample_count_clone.fetch_add(samples.len(), Ordering::SeqCst);

        // Calculate peak level for this buffer
        let buffer_peak = samples.iter().map(|&s| s.abs()).fold(0.0f32, f32::max);

        // Update peak level
        if let Ok(mut peak) = peak_level_clone.lock() {
            if buffer_peak > *peak {
                *peak = buffer_peak;
            }
        }

        // Print periodic updates
        if count % 4800 == 0 {
            // More frequent updates (every ~0.1 seconds at 48kHz)
            println!(
                "    📊 Captured {} samples, buffer peak: {:.3}",
                count, buffer_peak
            );
        }
    });

    match capture_result {
        Ok(()) => {
            println!("✅ Real PipeWire capture started successfully!");

            // Run the main loop to process audio callbacks
            println!("🎧 Capturing Firefox audio for 5 seconds...");
            println!("    (Running PipeWire main loop to process audio callbacks)");

            #[cfg(feature = "pipewire")]
            {
                // Run main loop for 5 seconds to capture audio
                match capture.run_main_loop(Duration::from_secs(5)) {
                    Ok(()) => {
                        println!("✅ Main loop completed successfully");
                    }
                    Err(e) => {
                        println!("⚠️  Main loop error: {}", e);
                    }
                }
            }

            #[cfg(not(feature = "pipewire"))]
            {
                // Fallback for non-PipeWire builds
                thread::sleep(Duration::from_secs(5));
            }

            // Stop capture
            println!("\n6️⃣ Stopping capture...");
            match capture.stop_capture() {
                Ok(()) => {
                    println!("✅ Capture stopped successfully");
                }
                Err(e) => {
                    println!("⚠️  Error stopping capture: {}", e);
                }
            }

            // Report results
            let total_samples = sample_count.load(Ordering::SeqCst);
            let max_peak = peak_level.lock().unwrap_or_else(|poisoned| {
                println!("⚠️  Mutex was poisoned, recovering...");
                poisoned.into_inner()
            });

            println!("\n📈 Capture Results:");
            println!("   • Total samples captured: {}", total_samples);
            println!("   • Peak audio level: {:.3}", *max_peak);
            println!(
                "   • Estimated duration: {:.2} seconds",
                total_samples as f32 / 48000.0
            );

            if total_samples > 0 {
                println!("   ✅ Successfully captured real Firefox audio via PipeWire!");
                if *max_peak > 0.01 {
                    println!(
                        "   🎵 Audio content detected (peak level: {:.3})",
                        *max_peak
                    );
                } else {
                    println!("   🔇 Very low audio levels - Firefox may be playing quiet content");
                }
            } else {
                println!("   ⚠️  No audio samples captured");
                println!("   💡 This could mean:");
                println!("      - Firefox is not currently playing audio");
                println!("      - Main loop is not processing callbacks correctly");
                println!("      - Stream connection failed");
            }
        }
        Err(e) => {
            println!("❌ Failed to start PipeWire capture: {}", e);
            println!("💡 This could mean:");
            println!("   - PipeWire features are not compiled in");
            println!("   - PipeWire daemon is not running");
            println!("   - Insufficient permissions");
            println!("   - Target node is not available");
        }
    }

    println!("\n✅ Real PipeWire integration test completed!");
}

fn detect_firefox_main_process() -> Option<u32> {
    // First, check what PID is actually associated with the Firefox audio node
    if let Some(audio_pid) = get_firefox_audio_pid_from_pipewire() {
        println!("🎯 Found Firefox audio PID from PipeWire: {}", audio_pid);
        return Some(audio_pid);
    }

    // Fallback to process detection
    if let Ok(output) = std::process::Command::new("ps")
        .args(&["-eo", "pid,comm,args"])
        .output()
    {
        if let Ok(output_str) = String::from_utf8(output.stdout) {
            let mut main_firefox = None;
            let mut rdd_process = None;

            for line in output_str.lines().skip(1) {
                let parts: Vec<&str> = line.trim().splitn(3, ' ').collect();
                if parts.len() >= 3 {
                    if let Ok(pid) = parts[0].parse::<u32>() {
                        let comm = parts[1];
                        let args = parts[2];

                        // Look for main Firefox process
                        if (comm.contains("firefox") || args.contains("firefox"))
                            && !args.contains("-contentproc")
                        {
                            main_firefox = Some(pid);
                        }
                        // Look for RDD Process (handles audio/video)
                        else if comm.contains("RDD Process") || args.contains("rdd") {
                            rdd_process = Some(pid);
                            println!("🎵 Found Firefox RDD Process (Audio/Video): PID {}", pid);
                        }
                    }
                }
            }

            // Prefer RDD process for audio capture, fallback to main process
            if let Some(rdd_pid) = rdd_process {
                println!("🎯 Using RDD Process for audio capture: PID {}", rdd_pid);
                return Some(rdd_pid);
            } else if let Some(main_pid) = main_firefox {
                println!("🎯 Using main Firefox process: PID {}", main_pid);
                return Some(main_pid);
            }
        }
    }
    None
}

fn get_firefox_audio_pid_from_pipewire() -> Option<u32> {
    // Use pw-dump to find the actual PID associated with Firefox audio
    if let Ok(output) = std::process::Command::new("pw-dump").output() {
        if let Ok(output_str) = String::from_utf8(output.stdout) {
            let lines: Vec<&str> = output_str.lines().collect();

            for (i, line) in lines.iter().enumerate() {
                // Look for Firefox application name
                if line.contains("\"application.name\": \"Firefox\"") {
                    // Look for process ID in nearby lines
                    for j in i.saturating_sub(20)..std::cmp::min(i + 20, lines.len()) {
                        if lines[j].contains("\"application.process.id\":") {
                            // Extract PID
                            if let Some(colon_pos) = lines[j].find(':') {
                                let value_part = &lines[j][colon_pos + 1..];
                                if let Some(comma_pos) = value_part.find(',') {
                                    let pid_str = value_part[..comma_pos].trim();
                                    if let Ok(pid) = pid_str.parse::<u32>() {
                                        println!(
                                            "🔍 Found Firefox audio PID from PipeWire: {}",
                                            pid
                                        );
                                        return Some(pid);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(not(all(target_os = "linux", feature = "feat_linux")))]
fn main() {
    println!("❌ Real PipeWire Integration Test");
    println!("==================================");
    println!();
    println!("This test requires PipeWire features to be enabled.");
    println!("Please compile with: cargo run --bin real_pipewire_test --features feat_linux");
    println!();
    println!("Current features:");
    #[cfg(feature = "feat_linux")]
    println!("  ✅ feat_linux enabled");
    #[cfg(not(feature = "feat_linux"))]
    println!("  ❌ feat_linux disabled");

    #[cfg(feature = "pipewire")]
    println!("  ✅ pipewire enabled");
    #[cfg(not(feature = "pipewire"))]
    println!("  ❌ pipewire disabled");

    #[cfg(feature = "libspa")]
    println!("  ✅ libspa enabled");
    #[cfg(not(feature = "libspa"))]
    println!("  ❌ libspa disabled");
}
