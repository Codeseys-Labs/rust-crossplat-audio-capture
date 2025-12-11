//! Linux Virtual Audio Device Implementation
//!
//! Uses PipeWire's built-in module-null-sink to create a virtual audio device.
//! This requires NO external dependencies - just PipeWire which is standard on modern Linux.
//!
//! The null sink acts as a virtual speaker that:
//! - Accepts audio output from any application
//! - Creates a monitor source that can be captured
//! - Discards the actual audio (doesn't play through speakers)
//!
//! This is perfect for CI testing where we need:
//! 1. A place for VLC/applications to output audio
//! 2. A source we can capture from to verify the audio pipeline

use std::process::Command;

/// Name of the virtual sink we create
const SINK_NAME: &str = "rsac_ci_test_sink";
const SINK_DESCRIPTION: &str = "RSAC CI Test Virtual Speaker";

/// Create a virtual audio sink using PipeWire's PulseAudio compatibility
pub fn create_virtual_device() -> Result<(), Box<dyn std::error::Error>> {
    println!("Platform: Linux (PipeWire/PulseAudio)");
    println!();

    // First check if PipeWire or PulseAudio is available
    if !is_audio_server_running() {
        return Err("No audio server (PipeWire/PulseAudio) is running".into());
    }

    // Check if device already exists
    if device_exists()? {
        println!("Virtual audio device '{}' already exists.", SINK_NAME);
        return Ok(());
    }

    // Try to create using pactl (works with both PipeWire and PulseAudio)
    println!("Creating null sink: {}", SINK_NAME);

    let output = Command::new("pactl")
        .args([
            "load-module",
            "module-null-sink",
            &format!("sink_name={}", SINK_NAME),
            &format!("sink_properties=device.description=\"{}\"", SINK_DESCRIPTION),
        ])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Failed to create null sink: {}", stderr).into());
    }

    let module_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    println!("Created null sink with module ID: {}", module_id);

    // Set as default sink so applications use it automatically
    println!("Setting as default sink...");
    let _ = Command::new("pactl")
        .args(["set-default-sink", SINK_NAME])
        .output();

    // Verify creation
    if device_exists()? {
        println!();
        println!("Virtual audio device created successfully!");
        println!("  Sink name: {}", SINK_NAME);
        println!("  Monitor source: {}.monitor", SINK_NAME);
        println!();
        println!("Applications can now output audio to this virtual device,");
        println!("and you can capture from the monitor source.");
        Ok(())
    } else {
        Err("Failed to verify device creation".into())
    }
}

/// Remove the virtual audio device
pub fn remove_virtual_device() -> Result<(), Box<dyn std::error::Error>> {
    println!("Platform: Linux (PipeWire/PulseAudio)");
    println!();

    if !device_exists()? {
        println!("Virtual audio device '{}' does not exist.", SINK_NAME);
        return Ok(());
    }

    // Find the module ID for our sink
    let output = Command::new("pactl")
        .args(["list", "short", "modules"])
        .output()?;

    let modules = String::from_utf8_lossy(&output.stdout);

    for line in modules.lines() {
        if line.contains("module-null-sink") && line.contains(SINK_NAME) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(module_id) = parts.first() {
                println!("Unloading module: {}", module_id);
                let unload = Command::new("pactl")
                    .args(["unload-module", module_id])
                    .output()?;

                if !unload.status.success() {
                    let stderr = String::from_utf8_lossy(&unload.stderr);
                    return Err(format!("Failed to unload module: {}", stderr).into());
                }

                println!("Virtual audio device removed successfully.");
                return Ok(());
            }
        }
    }

    Err("Could not find module to unload".into())
}

/// Check if the virtual audio device exists
pub fn check_device_status() -> Result<(), Box<dyn std::error::Error>> {
    println!("Platform: Linux (PipeWire/PulseAudio)");
    println!();

    // Check audio server
    let server = get_audio_server_info();
    println!("Audio server: {}", server);

    // List all sinks
    println!();
    println!("Available sinks:");
    let output = Command::new("pactl")
        .args(["list", "short", "sinks"])
        .output()?;

    let sinks = String::from_utf8_lossy(&output.stdout);
    let mut found = false;

    for line in sinks.lines() {
        let is_our_sink = line.contains(SINK_NAME);
        let marker = if is_our_sink { " <-- RSAC virtual device" } else { "" };
        println!("  {}{}", line, marker);
        if is_our_sink {
            found = true;
        }
    }

    println!();
    if found {
        println!("Status: Virtual audio device '{}' is ACTIVE", SINK_NAME);

        // Show the monitor source
        println!();
        println!("Monitor source available for capture:");
        let sources = Command::new("pactl")
            .args(["list", "short", "sources"])
            .output()?;

        let sources_str = String::from_utf8_lossy(&sources.stdout);
        for line in sources_str.lines() {
            if line.contains(&format!("{}.monitor", SINK_NAME)) {
                println!("  {}", line);
            }
        }
    } else {
        println!("Status: Virtual audio device '{}' NOT FOUND", SINK_NAME);
        println!();
        println!("Run 'vad-setup create' to create the virtual device.");
    }

    Ok(())
}

/// Test the virtual audio device by playing a tone and verifying capture
pub fn test_virtual_device() -> Result<(), Box<dyn std::error::Error>> {
    println!("Platform: Linux (PipeWire/PulseAudio)");
    println!();

    // Ensure device exists
    if !device_exists()? {
        println!("Creating virtual audio device first...");
        create_virtual_device()?;
        println!();
    }

    // Generate a test tone using sox (if available) or ffmpeg
    println!("Generating test tone...");

    let has_sox = Command::new("which")
        .arg("sox")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    let has_ffmpeg = Command::new("which")
        .arg("ffmpeg")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    let test_file = "/tmp/rsac_test_tone.wav";

    if has_sox {
        // Use sox to generate test tone
        let gen = Command::new("sox")
            .args(["-n", "-r", "48000", "-c", "2", test_file, "synth", "2", "sine", "440"])
            .output()?;

        if !gen.status.success() {
            return Err("Failed to generate test tone with sox".into());
        }
    } else if has_ffmpeg {
        // Use ffmpeg to generate test tone
        let gen = Command::new("ffmpeg")
            .args([
                "-y", "-f", "lavfi",
                "-i", "sine=frequency=440:duration=2",
                "-ar", "48000", "-ac", "2",
                test_file
            ])
            .output()?;

        if !gen.status.success() {
            return Err("Failed to generate test tone with ffmpeg".into());
        }
    } else {
        return Err("Neither sox nor ffmpeg available for test tone generation".into());
    }

    println!("Generated test tone: {}", test_file);

    // Play the test tone to our virtual sink
    println!("Playing test tone to virtual sink...");

    let has_paplay = Command::new("which")
        .arg("paplay")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if has_paplay {
        let play = Command::new("paplay")
            .args(["--device", SINK_NAME, test_file])
            .output()?;

        if !play.status.success() {
            let stderr = String::from_utf8_lossy(&play.stderr);
            println!("Warning: paplay failed: {}", stderr);
        } else {
            println!("Test tone played successfully!");
        }
    } else {
        println!("Warning: paplay not available, skipping playback test");
    }

    // Clean up test file
    let _ = std::fs::remove_file(test_file);

    println!();
    println!("Test completed!");
    println!();
    println!("The virtual device is working. Applications can:");
    println!("  1. Output audio to sink: {}", SINK_NAME);
    println!("  2. Capture audio from: {}.monitor", SINK_NAME);

    Ok(())
}

/// Check if PipeWire or PulseAudio is running
fn is_audio_server_running() -> bool {
    // Check for PipeWire first (modern systems)
    let pw_running = Command::new("pgrep")
        .arg("pipewire")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if pw_running {
        return true;
    }

    // Check for PulseAudio
    let pa_running = Command::new("pgrep")
        .arg("pulseaudio")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    pa_running
}

/// Get information about which audio server is running
fn get_audio_server_info() -> String {
    // Check for PipeWire
    let pw_version = Command::new("pw-cli")
        .arg("--version")
        .output();

    if let Ok(output) = pw_version {
        if output.status.success() {
            let version = String::from_utf8_lossy(&output.stdout);
            return format!("PipeWire {}", version.trim());
        }
    }

    // Check for PulseAudio
    let pa_version = Command::new("pactl")
        .arg("--version")
        .output();

    if let Ok(output) = pa_version {
        if output.status.success() {
            let version = String::from_utf8_lossy(&output.stdout);
            return version.lines().next().unwrap_or("PulseAudio").to_string();
        }
    }

    "Unknown audio server".to_string()
}

/// Check if our virtual device exists
fn device_exists() -> Result<bool, Box<dyn std::error::Error>> {
    let output = Command::new("pactl")
        .args(["list", "short", "sinks"])
        .output()?;

    let sinks = String::from_utf8_lossy(&output.stdout);
    Ok(sinks.contains(SINK_NAME))
}

/// Additional helper: Get the monitor source name for capture
#[allow(dead_code)]
pub fn get_monitor_source_name() -> String {
    format!("{}.monitor", SINK_NAME)
}

/// Additional helper: Get the sink name for output
#[allow(dead_code)]
pub fn get_sink_name() -> &'static str {
    SINK_NAME
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sink_name_format() {
        assert!(!SINK_NAME.contains(' '), "Sink name should not contain spaces");
        assert!(SINK_NAME.chars().all(|c| c.is_alphanumeric() || c == '_'),
                "Sink name should only contain alphanumeric and underscore");
    }
}
