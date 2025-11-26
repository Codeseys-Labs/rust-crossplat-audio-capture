#!/usr/bin/env cargo
//! PipeWire Diagnostics Tool
//!
//! This tool helps diagnose PipeWire issues in CI environments
//! by testing various stream creation scenarios.

use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🔍 PipeWire Diagnostics Tool");
    println!("============================");

    // Test 1: Check PipeWire daemon status
    println!("\n📊 Test 1: PipeWire Daemon Status");
    match Command::new("pw-cli").arg("info").arg("0").output() {
        Ok(output) => {
            if output.status.success() {
                println!("✅ PipeWire daemon is accessible");
                println!("Output: {}", String::from_utf8_lossy(&output.stdout));
            } else {
                println!("❌ PipeWire daemon not accessible");
                println!("Error: {}", String::from_utf8_lossy(&output.stderr));
            }
        }
        Err(e) => println!("❌ Failed to run pw-cli: {}", e),
    }

    // Test 2: List all nodes
    println!("\n📊 Test 2: Available Nodes");
    match Command::new("pw-cli")
        .arg("list-objects")
        .arg("Node")
        .output()
    {
        Ok(output) => {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let lines: Vec<&str> = stdout.lines().collect();
                println!("✅ Found {} lines of node information", lines.len());

                // Show VLC-related nodes
                let vlc_lines: Vec<&str> = lines
                    .iter()
                    .filter(|line| {
                        line.to_lowercase().contains("vlc") || line.contains("Dynamic_vlc_cap")
                    })
                    .cloned()
                    .collect();

                if !vlc_lines.is_empty() {
                    println!("🎯 VLC-related nodes:");
                    for line in vlc_lines {
                        println!("  {}", line);
                    }
                } else {
                    println!("⚠️  No VLC-related nodes found");
                }

                // Show audio nodes
                let audio_lines: Vec<&str> = lines
                    .iter()
                    .filter(|line| line.contains("Audio/Sink") || line.contains("Audio/Source"))
                    .cloned()
                    .collect();

                if !audio_lines.is_empty() {
                    println!("🔊 Audio nodes:");
                    for line in audio_lines {
                        println!("  {}", line);
                    }
                } else {
                    println!("⚠️  No audio sink/source nodes found");
                }
            } else {
                println!("❌ Failed to list nodes");
                println!("Error: {}", String::from_utf8_lossy(&output.stderr));
            }
        }
        Err(e) => println!("❌ Failed to run pw-cli list-objects: {}", e),
    }

    // Test 3: Check environment variables
    println!("\n📊 Test 3: Environment Variables");
    let env_vars = [
        "PIPEWIRE_RUNTIME_DIR",
        "PULSE_RUNTIME_PATH",
        "XDG_RUNTIME_DIR",
        "PIPEWIRE_LATENCY",
    ];

    for var in &env_vars {
        match std::env::var(var) {
            Ok(value) => println!("✅ {}: {}", var, value),
            Err(_) => println!("⚠️  {} not set", var),
        }
    }

    // Test 4: Check user permissions
    println!("\n📊 Test 4: User and Permissions");
    println!("User ID: {}", unsafe { libc::getuid() });
    println!("Group ID: {}", unsafe { libc::getgid() });

    // Check if we're in audio group
    match Command::new("groups").output() {
        Ok(output) => {
            let groups = String::from_utf8_lossy(&output.stdout);
            println!("Groups: {}", groups.trim());
            if groups.contains("audio") {
                println!("✅ User is in audio group");
            } else {
                println!("⚠️  User is not in audio group");
            }
        }
        Err(e) => println!("❌ Failed to check groups: {}", e),
    }

    // Test 5: Try to create a simple stream (this will help us understand the exact error)
    println!("\n📊 Test 5: Stream Creation Test");
    println!("This test will help identify why monitor stream creation fails...");

    // We'll use a simple pw-cat command to test stream creation
    match Command::new("pw-cat")
        .args([
            "--record",
            "--format",
            "s16",
            "--rate",
            "48000",
            "--channels",
            "2",
        ])
        .args(["--volume", "0.0", "/dev/null"])
        .arg("--help")
        .output()
    {
        Ok(output) => {
            if output.status.success() {
                println!("✅ pw-cat is available for stream testing");
            } else {
                println!("⚠️  pw-cat available but returned error");
            }
        }
        Err(e) => println!("⚠️  pw-cat not available: {}", e),
    }

    println!("\n🎯 Diagnostics Complete");
    println!("This information will help debug the monitor stream creation issue.");

    Ok(())
}
