//! **DEPRECATED**: This binary uses the old rsac API patterns (shell-based PipeWire
//! discovery, simulated capture). It needs to be rewritten to use the new
//! AudioCaptureBuilder → AudioCapture → CapturingStream pipeline.
//!
//! This binary is disabled in Cargo.toml. To re-enable, add its [[bin]] entry back.
//!
//! ---
//!
//! Firefox Audio Capture Test
//!
//! This test specifically targets Firefox instances and attempts to capture
//! their audio using the enhanced PipeWire integration.

use std::thread;
use std::time::Duration;

// We'll access the PipeWire functionality through the public API
// since the modules are private

fn main() {
    println!("🦊 Firefox Audio Capture Test");
    println!("==============================");

    // Test 1: Detect Firefox processes
    println!("\n1️⃣ Detecting Firefox processes...");
    let firefox_pids = detect_firefox_processes();

    if firefox_pids.is_empty() {
        println!("❌ No Firefox processes found. Please start Firefox with audio content.");
        return;
    }

    // Test 2: Test PipeWire enumeration for Firefox
    println!("\n2️⃣ Testing PipeWire enumeration...");
    test_pipewire_enumeration(&firefox_pids);

    // Test 3: Attempt to create capture for Firefox
    println!("\n3️⃣ Testing Firefox audio capture...");
    test_firefox_capture(&firefox_pids);

    println!("\n✅ Firefox capture test completed!");
}

fn detect_firefox_processes() -> Vec<u32> {
    let mut firefox_pids = Vec::new();

    if let Ok(output) = std::process::Command::new("ps")
        .args(["-eo", "pid,comm,args"])
        .output()
    {
        if let Ok(output_str) = String::from_utf8(output.stdout) {
            for line in output_str.lines().skip(1) {
                let parts: Vec<&str> = line.trim().splitn(3, ' ').collect();
                if parts.len() >= 3 {
                    if let Ok(pid) = parts[0].parse::<u32>() {
                        let comm = parts[1];
                        let args = parts[2];

                        // Look for main Firefox process (not content processes)
                        if (comm.contains("firefox") || args.contains("firefox"))
                            && !args.contains("-contentproc")
                        {
                            firefox_pids.push(pid);
                            println!("  🦊 Found Firefox main process: PID {}", pid);
                        }
                    }
                }
            }
        }
    }

    firefox_pids
}

fn test_pipewire_enumeration(firefox_pids: &[u32]) {
    println!("  Testing PipeWire node detection for Firefox...");

    // Test pw-dump for Firefox references
    if let Ok(output) = std::process::Command::new("pw-dump").output() {
        if output.status.success() {
            let output_str = String::from_utf8_lossy(&output.stdout);

            // Count Firefox references
            let firefox_count = output_str.to_lowercase().matches("firefox").count();
            println!(
                "    📊 Found {} Firefox references in PipeWire dump",
                firefox_count
            );

            // Look for specific Firefox PIDs
            for &pid in firefox_pids {
                let pid_str = pid.to_string();
                if output_str.contains(&pid_str) {
                    println!("    ✅ Firefox PID {} found in PipeWire nodes", pid);
                } else {
                    println!("    ⚠️  Firefox PID {} not found in PipeWire nodes", pid);
                }
            }
        }
    }

    // Test pw-cli for Firefox nodes
    if let Ok(output) = std::process::Command::new("pw-cli")
        .args(["list-objects"])
        .output()
    {
        if output.status.success() {
            let output_str = String::from_utf8_lossy(&output.stdout);

            // Look for Firefox application nodes
            let firefox_nodes: Vec<&str> = output_str
                .lines()
                .filter(|line| line.to_lowercase().contains("firefox"))
                .collect();

            println!(
                "    📋 Found {} Firefox-related nodes via pw-cli",
                firefox_nodes.len()
            );

            for (i, node) in firefox_nodes.iter().take(3).enumerate() {
                println!("      {}. {}", i + 1, node.trim());
            }
        }
    }
}

fn test_firefox_capture(firefox_pids: &[u32]) {
    if firefox_pids.is_empty() {
        println!("  ⚠️  No Firefox PIDs to test");
        return;
    }

    let main_firefox_pid = firefox_pids[0];
    println!("  🎯 Targeting Firefox PID: {}", main_firefox_pid);

    // Test the enhanced PipeWire functionality
    // Since we can't directly access the private modules, we'll simulate
    // what the enhanced implementation would do

    println!("  🔍 Simulating PipeWire application discovery...");

    // Simulate discovering the target node
    println!(
        "    • Looking for PipeWire nodes for PID {}",
        main_firefox_pid
    );

    // Check if we can find audio streams for this process
    if let Some(node_info) = find_audio_node_for_pid(main_firefox_pid) {
        println!("    ✅ Found audio node: {}", node_info);

        // Simulate creating a monitor stream
        println!("  🎵 Simulating monitor stream creation...");
        println!("    • Target node: {}", node_info);
        println!("    • Stream type: Monitor (non-invasive)");
        println!("    • Format: F32LE, Stereo, 48kHz");

        // Simulate audio capture
        println!("  🎧 Simulating audio capture from Firefox...");
        simulate_firefox_audio_capture();
    } else {
        println!(
            "    ❌ No audio node found for Firefox PID {}",
            main_firefox_pid
        );
        println!("    💡 This could mean:");
        println!("       - Firefox is not currently playing audio");
        println!("       - Audio is handled by a different process");
        println!("       - PipeWire node enumeration needs refinement");
    }
}

fn find_audio_node_for_pid(pid: u32) -> Option<String> {
    println!("    🔍 Searching for audio nodes for PID {}...", pid);

    // First try pw-cli for more reliable parsing
    if let Some(node_info) = find_audio_node_via_pw_cli(pid) {
        return Some(node_info);
    }

    // Fallback to pw-dump
    if let Some(node_info) = find_audio_node_via_pw_dump(pid) {
        return Some(node_info);
    }

    None
}

fn find_audio_node_via_pw_cli(pid: u32) -> Option<String> {
    if let Ok(output) = std::process::Command::new("pw-cli")
        .args(["list-objects"])
        .output()
    {
        if output.status.success() {
            let output_str = String::from_utf8_lossy(&output.stdout);
            let pid_str = pid.to_string();

            let lines: Vec<&str> = output_str.lines().collect();
            let mut current_node_id = None;
            let mut current_app_name = None;
            let mut current_media_class = None;
            let mut found_target_pid = false;

            for line in lines {
                let line = line.trim();

                // Start of a new node
                if line.contains("type PipeWire:Interface:Node") {
                    // Reset for new node
                    current_node_id = None;
                    current_app_name = None;
                    current_media_class = None;
                    found_target_pid = false;

                    // Extract node ID
                    if let Some(id_start) = line.find("id ") {
                        if let Some(comma) = line[id_start + 3..].find(',') {
                            if let Ok(id) = line[id_start + 3..id_start + 3 + comma]
                                .trim()
                                .parse::<u32>()
                            {
                                current_node_id = Some(id);
                            }
                        }
                    }
                }
                // Check for application name
                else if line.contains("application.name") && line.contains("Firefox") {
                    current_app_name = Some("Firefox".to_string());
                    println!(
                        "      📍 Found Firefox application.name in node {:?}",
                        current_node_id
                    );
                }
                // Check for process ID
                else if line.contains("application.process.id") && line.contains(&pid_str) {
                    found_target_pid = true;
                    println!(
                        "      📍 Found target PID {} in node {:?}",
                        pid, current_node_id
                    );
                }
                // Check for media class
                else if line.contains("media.class") && line.contains("Stream/Output/Audio") {
                    current_media_class = Some("Stream/Output/Audio".to_string());
                    println!(
                        "      📍 Found audio output stream in node {:?}",
                        current_node_id
                    );
                }

                // If we have all the pieces for this node, check if it matches
                if let (Some(node_id), Some(_), Some(_)) =
                    (&current_node_id, &current_app_name, &current_media_class)
                {
                    if found_target_pid {
                        println!("      ✅ Found matching audio node: ID {}", node_id);
                        return Some(format!("Node {} (Firefox Audio Stream)", node_id));
                    }
                }
            }
        }
    }

    None
}

fn find_audio_node_via_pw_dump(pid: u32) -> Option<String> {
    if let Ok(output) = std::process::Command::new("pw-dump").output() {
        if output.status.success() {
            let output_str = String::from_utf8_lossy(&output.stdout);
            let pid_str = format!("\"application.process.id\": {}", pid);

            println!("      🔍 Looking for: {}", pid_str);

            // Look for nodes with this exact PID format
            let lines: Vec<&str> = output_str.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                if line.contains(&pid_str) {
                    println!("      📍 Found PID {} at line {}", pid, i);

                    // Look for media.class in nearby lines (within the same object)
                    let search_start = i.saturating_sub(50);
                    let search_end = std::cmp::min(i + 50, lines.len());

                    for line_j in lines.iter().take(search_end).skip(search_start) {
                        if line_j.contains("\"media.class\": \"Stream/Output/Audio\"") {
                            println!("      ✅ Found audio output stream for PID {}", pid);

                            // Try to find the node ID
                            for line_k in lines.iter().take(search_end).skip(search_start) {
                                if line_k.contains("\"id\":") {
                                    if let Some(id_str) = extract_json_number_value(line_k) {
                                        if let Ok(node_id) = id_str.parse::<u32>() {
                                            return Some(format!(
                                                "Node {} (Firefox Audio Stream via pw-dump)",
                                                node_id
                                            ));
                                        }
                                    }
                                }
                            }

                            return Some(format!("Firefox Audio Stream (PID {})", pid));
                        }
                    }
                }
            }
        }
    }

    None
}

fn extract_json_number_value(line: &str) -> Option<String> {
    if let Some(colon) = line.find(':') {
        let value_part = line[colon + 1..].trim();
        if let Some(comma) = value_part.find(',') {
            return Some(value_part[..comma].trim().to_string());
        } else {
            return Some(
                value_part
                    .trim_end_matches(['}', ' ', '\n', '\r'])
                    .to_string(),
            );
        }
    }
    None
}

fn simulate_firefox_audio_capture() {
    println!("    🎶 Starting simulated Firefox audio capture...");

    let mut sample_count = 0;
    let max_samples = 50;

    while sample_count < max_samples {
        // Simulate receiving audio data from Firefox
        let rms = simulate_firefox_audio_level(sample_count);

        if sample_count % 10 == 0 {
            println!(
                "      📊 Sample {}: Firefox audio level = {:.3}",
                sample_count, rms
            );
        }

        sample_count += 1;
        thread::sleep(Duration::from_millis(20));
    }

    println!("    ✅ Simulated capture completed");
    println!(
        "    📈 Captured {} audio buffers from Firefox",
        sample_count
    );
}

fn simulate_firefox_audio_level(sample_num: usize) -> f32 {
    // Simulate varying audio levels that might come from Firefox
    let base_level = 0.1;
    let variation = (sample_num as f32 * 0.1).sin() * 0.05;
    let music_simulation = (sample_num as f32 * 0.05).sin() * 0.3;

    (base_level + variation + music_simulation.abs()).min(1.0)
}
