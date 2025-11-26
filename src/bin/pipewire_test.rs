//! PipeWire Integration Test
//!
//! This binary tests the enhanced PipeWire functionality that combines
//! the simplified implementation with concepts from the original pipwire-1.rs

use std::thread;
use std::time::Duration;

fn main() {
    println!("🔧 PipeWire Integration Test");
    println!("============================");

    // Test 1: Check PipeWire availability
    println!("\n1️⃣ Testing PipeWire availability...");
    test_pipewire_availability();

    // Test 2: List audio applications
    println!("\n2️⃣ Testing application enumeration...");
    test_application_enumeration();

    // Test 3: Test application capture simulation
    println!("\n3️⃣ Testing application capture...");
    test_application_capture();

    println!("\n✅ All PipeWire tests completed!");
}

fn test_pipewire_availability() {
    // Check if PipeWire binary exists
    let pipewire_binary = std::process::Command::new("which")
        .arg("pipewire")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if pipewire_binary {
        println!("  ✅ PipeWire binary found");
    } else {
        println!("  ⚠️  PipeWire binary not found");
    }

    // Check if PipeWire is running
    let pipewire_running = std::process::Command::new("pgrep")
        .arg("pipewire")
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if pipewire_running {
        println!("  ✅ PipeWire daemon is running");
    } else {
        println!("  ⚠️  PipeWire daemon not running");
    }

    // Check for development files
    let dev_files = std::process::Command::new("pkg-config")
        .args(["--exists", "libpipewire-0.3"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if dev_files {
        println!("  ✅ PipeWire development files found");
    } else {
        println!("  ⚠️  PipeWire development files not found");
    }
}

fn test_application_enumeration() {
    println!("  Testing application discovery methods...");

    // Enhanced process-based enumeration with more details
    if let Ok(output) = std::process::Command::new("ps")
        .args(["-eo", "pid,comm,args"])
        .output()
    {
        if let Ok(output_str) = String::from_utf8(output.stdout) {
            let mut audio_apps = Vec::new();
            let mut firefox_processes = Vec::new();

            for line in output_str.lines().skip(1) {
                let parts: Vec<&str> = line.trim().splitn(3, ' ').collect();
                if parts.len() >= 3 {
                    if let Ok(pid) = parts[0].parse::<u32>() {
                        let comm = parts[1];
                        let args = parts[2];

                        // Special handling for Firefox
                        if comm.contains("firefox") || args.contains("firefox") {
                            let process_type = if args.contains("-contentproc") {
                                if args.contains("socket") {
                                    "Socket Process"
                                } else if args.contains("rdd") {
                                    "RDD Process (Audio/Video)"
                                } else if args.contains("tab") {
                                    "Tab Process"
                                } else {
                                    "Content Process"
                                }
                            } else {
                                "Main Process"
                            };

                            firefox_processes.push((pid, process_type));
                        }

                        // Check for likely audio applications
                        if is_likely_audio_app(comm) || is_likely_audio_app(args) {
                            audio_apps.push((pid, comm, args));
                        }
                    }
                }
            }

            // Report Firefox processes specifically
            if !firefox_processes.is_empty() {
                println!("  🦊 Firefox Detection:");
                for (pid, process_type) in &firefox_processes {
                    println!("    • {} (PID: {})", process_type, pid);
                }

                // Check if any Firefox process might be handling audio
                let main_firefox = firefox_processes
                    .iter()
                    .find(|(_, ptype)| ptype.contains("Main"))
                    .or_else(|| firefox_processes.first());

                if let Some((main_pid, _)) = main_firefox {
                    println!("    🎵 Main Firefox PID for audio targeting: {}", main_pid);
                }
            } else {
                println!("  ⚠️  No Firefox processes detected");
            }

            // Report other audio applications
            println!("  🎵 Other Audio Applications:");
            if audio_apps.is_empty() {
                println!("    • No other audio applications detected");
            } else {
                for (i, (pid, comm, _)) in audio_apps.iter().take(5).enumerate() {
                    println!("    {}. {} (PID: {})", i + 1, comm, pid);
                }

                if audio_apps.len() > 5 {
                    println!("    ... and {} more", audio_apps.len() - 5);
                }
            }
        }
    }

    // Test PipeWire command-line tools if available
    println!("  🔧 PipeWire Node Detection:");
    if let Ok(output) = std::process::Command::new("pw-cli")
        .args(["list-objects"])
        .output()
    {
        if output.status.success() {
            let output_str = String::from_utf8_lossy(&output.stdout);

            // Look for audio stream nodes
            let audio_nodes: Vec<&str> = output_str
                .lines()
                .filter(|line| {
                    line.contains("Stream/Output/Audio") || line.contains("Stream/Input/Audio")
                })
                .collect();

            // Look for Firefox-related nodes
            let firefox_nodes: Vec<&str> = output_str
                .lines()
                .filter(|line| line.to_lowercase().contains("firefox"))
                .collect();

            println!("    • Total audio stream nodes: {}", audio_nodes.len());
            println!("    • Firefox-related nodes: {}", firefox_nodes.len());

            if !firefox_nodes.is_empty() {
                println!("    🦊 Firefox PipeWire nodes found:");
                for (i, node) in firefox_nodes.iter().take(3).enumerate() {
                    println!("      {}. {}", i + 1, node.trim());
                }
            }
        } else {
            println!("    ⚠️  pw-cli command failed");
        }
    } else {
        println!("    ⚠️  pw-cli not available for node enumeration");
    }

    // Alternative: Try pw-dump for more detailed info
    if let Ok(output) = std::process::Command::new("pw-dump").output() {
        if output.status.success() {
            let output_str = String::from_utf8_lossy(&output.stdout);
            let _firefox_mentions = output_str.matches("firefox").count();
            let firefox_mentions_ci = output_str.to_lowercase().matches("firefox").count();

            if firefox_mentions_ci > 0 {
                println!(
                    "    🦊 pw-dump found {} Firefox references",
                    firefox_mentions_ci
                );
            }
        }
    }
}

fn test_application_capture() {
    println!("  Testing application capture simulation...");

    // Simulate the enhanced capture functionality
    let mut sample_count = 0;
    let max_samples = 100; // Capture for a short time

    println!("  🎵 Starting simulated audio capture...");

    // Simulate the callback-based capture
    let start_time = std::time::Instant::now();

    while sample_count < max_samples {
        // Simulate audio buffer (stereo, 1024 samples)
        let mut buffer = vec![0.0f32; 1024];
        let phase = sample_count as f32 * 0.01;

        // Generate test audio (sine waves)
        for (i, sample) in buffer.iter_mut().enumerate() {
            if i % 2 == 0 {
                // Left channel - 440 Hz
                *sample = (phase * 440.0 * 2.0 * std::f32::consts::PI).sin() * 0.1;
            } else {
                // Right channel - 880 Hz
                *sample = (phase * 880.0 * 2.0 * std::f32::consts::PI).sin() * 0.1;
            }
        }

        // Simulate processing the buffer
        let rms = calculate_rms(&buffer);
        if sample_count % 20 == 0 {
            println!("    📊 Sample {}: RMS level = {:.4}", sample_count, rms);
        }

        sample_count += 1;
        thread::sleep(Duration::from_millis(10));
    }

    let elapsed = start_time.elapsed();
    println!(
        "  ✅ Captured {} audio buffers in {:?}",
        sample_count, elapsed
    );
    println!(
        "  📈 Average processing rate: {:.1} buffers/sec",
        sample_count as f64 / elapsed.as_secs_f64()
    );
}

fn is_likely_audio_app(app_name: &str) -> bool {
    let audio_keywords = [
        "audio",
        "music",
        "video",
        "chrome",
        "vlc",
        "spotify",
        "mpv",
        "mplayer",
        "audacity",
        "pulseaudio",
        "pipewire",
        "jack",
        "alsa",
        "youtube",
        "discord",
        "zoom",
        "obs",
        "steam",
        "wine",
        "rhythmbox",
        "banshee",
        "amarok",
        "clementine",
        "deadbeef",
        "qmmp",
        "audacious",
        "totem",
        "parole",
        "smplayer",
        "kaffeine",
        "dragon",
        "phonon",
        "gstreamer",
        "ffmpeg",
        "mplayer2",
        "xine",
        "vlc-bin",
    ];

    let app_lower = app_name.to_lowercase();

    // Don't include firefox in general audio apps since we handle it specially
    if app_lower.contains("firefox") {
        return false;
    }

    audio_keywords
        .iter()
        .any(|keyword| app_lower.contains(keyword))
}

fn calculate_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }

    let sum_squares: f32 = samples.iter().map(|&x| x * x).sum();
    (sum_squares / samples.len() as f32).sqrt()
}
