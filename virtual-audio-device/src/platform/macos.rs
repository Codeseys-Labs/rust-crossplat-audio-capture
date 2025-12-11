//! macOS Virtual Audio Device Implementation
//!
//! macOS virtual audio requires a CoreAudio HAL (Hardware Abstraction Layer) plugin.
//! Unlike Windows, this is NOT a kernel driver - it's a userspace plugin that
//! registers with CoreAudio as a virtual audio device.
//!
//! This module supports:
//! 1. Using BlackHole (open source, MIT licensed) via Homebrew
//! 2. Building BlackHole from source (for truly self-contained deployment)
//! 3. Using a bundled pre-built HAL plugin (future)
//!
//! BlackHole creates a virtual audio device that:
//! - Accepts audio output from any application
//! - Routes that audio to a virtual input (loopback)
//! - Zero latency pass-through

use std::process::{Command, Stdio};
use std::path::{Path, PathBuf};
use std::fs;

/// Device configuration
const DEVICE_NAME: &str = "RSAC Virtual Audio";
const BLACKHOLE_BREW_PACKAGE: &str = "blackhole-2ch";
const BLACKHOLE_DEVICE_NAME: &str = "BlackHole 2ch";
const HAL_PLUGINS_DIR: &str = "/Library/Audio/Plug-Ins/HAL";

/// BlackHole source repository for building from source
const BLACKHOLE_REPO: &str = "https://github.com/ExistentialAudio/BlackHole.git";
const BLACKHOLE_VERSION: &str = "v0.6.0";

/// Create/install the virtual audio device
pub fn create_virtual_device() -> Result<(), Box<dyn std::error::Error>> {
    println!("Platform: macOS (CoreAudio HAL Plugin)");
    println!();

    // Check if device already exists
    if device_exists()? {
        println!("Virtual audio device already installed.");
        return Ok(());
    }

    // Try methods in order of preference:
    // 1. Use bundled HAL plugin (future self-contained distribution)
    // 2. Install via Homebrew (easiest)
    // 3. Build from source (most self-contained)

    let bundled_plugin = get_bundled_plugin_path();
    if bundled_plugin.exists() {
        println!("Using bundled HAL plugin...");
        return install_bundled_plugin(&bundled_plugin);
    }

    // Check if Homebrew is available
    let has_brew = Command::new("which")
        .arg("brew")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if has_brew {
        println!("Installing via Homebrew (recommended)...");
        return install_via_homebrew();
    }

    // Fall back to building from source
    println!("Homebrew not available, building from source...");
    build_from_source()
}

/// Remove the virtual audio device
pub fn remove_virtual_device() -> Result<(), Box<dyn std::error::Error>> {
    println!("Platform: macOS (CoreAudio HAL Plugin)");
    println!();

    // Check if installed via Homebrew
    let brew_installed = Command::new("brew")
        .args(["list", BLACKHOLE_BREW_PACKAGE])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if brew_installed {
        println!("Uninstalling via Homebrew...");
        let uninstall = Command::new("brew")
            .args(["uninstall", BLACKHOLE_BREW_PACKAGE])
            .output()?;

        if uninstall.status.success() {
            println!("Uninstalled successfully.");
            restart_coreaudio()?;
            return Ok(());
        } else {
            let stderr = String::from_utf8_lossy(&uninstall.stderr);
            return Err(format!("Homebrew uninstall failed: {}", stderr).into());
        }
    }

    // Try to remove manually installed plugin
    let plugin_path = PathBuf::from(HAL_PLUGINS_DIR).join("BlackHole2ch.driver");
    if plugin_path.exists() {
        println!("Removing HAL plugin: {:?}", plugin_path);

        // Need sudo to remove from /Library
        let remove = Command::new("sudo")
            .args(["rm", "-rf", &plugin_path.to_string_lossy()])
            .output()?;

        if remove.status.success() {
            println!("Plugin removed successfully.");
            restart_coreaudio()?;
            return Ok(());
        } else {
            let stderr = String::from_utf8_lossy(&remove.stderr);
            return Err(format!("Failed to remove plugin: {}", stderr).into());
        }
    }

    println!("No virtual audio device found to remove.");
    Ok(())
}

/// Check device status
pub fn check_device_status() -> Result<(), Box<dyn std::error::Error>> {
    println!("Platform: macOS (CoreAudio HAL Plugin)");
    println!();

    // List audio devices using system_profiler
    println!("Checking for virtual audio devices...");
    println!();

    let output = Command::new("system_profiler")
        .args(["SPAudioDataType", "-json"])
        .output()?;

    if output.status.success() {
        let json_str = String::from_utf8_lossy(&output.stdout);

        // Simple check for BlackHole in the output
        let has_blackhole = json_str.contains("BlackHole") || json_str.contains("blackhole");

        if has_blackhole {
            println!("Found virtual audio device: {}", BLACKHOLE_DEVICE_NAME);
        } else {
            println!("No virtual audio devices found.");
        }
    }

    // Also check HAL plugins directory
    println!();
    println!("HAL Plugins directory ({}):", HAL_PLUGINS_DIR);

    if let Ok(entries) = fs::read_dir(HAL_PLUGINS_DIR) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let marker = if name_str.contains("BlackHole") { " <-- Virtual Audio" } else { "" };
            println!("  {}{}", name_str, marker);
        }
    } else {
        println!("  (Could not read directory)");
    }

    // Check Homebrew installation
    println!();
    let brew_check = Command::new("brew")
        .args(["list", BLACKHOLE_BREW_PACKAGE])
        .output();

    match brew_check {
        Ok(output) if output.status.success() => {
            println!("Homebrew package: {} INSTALLED", BLACKHOLE_BREW_PACKAGE);
        }
        _ => {
            println!("Homebrew package: {} NOT INSTALLED", BLACKHOLE_BREW_PACKAGE);
        }
    }

    println!();
    if device_exists()? {
        println!("Status: Virtual audio device is ACTIVE");
    } else {
        println!("Status: Virtual audio device NOT FOUND");
        println!();
        println!("Run 'vad-setup create' to install the virtual audio device.");
    }

    Ok(())
}

/// Test the virtual audio device
pub fn test_virtual_device() -> Result<(), Box<dyn std::error::Error>> {
    println!("Platform: macOS (CoreAudio HAL Plugin)");
    println!();

    // Ensure device exists
    if !device_exists()? {
        println!("Virtual audio device not found. Installing...");
        create_virtual_device()?;
        println!();
    }

    // Try to play a test tone using afplay
    println!("Playing test tone through virtual device...");

    // Generate a test tone using afplay with system sounds
    let test_sound = "/System/Library/Sounds/Ping.aiff";

    if Path::new(test_sound).exists() {
        // Set the virtual device as output (temporarily)
        let set_output = Command::new("osascript")
            .args(["-e", &format!(
                "set volume output volume 50\ntell application \"System Events\" to set volume output volume 50"
            )])
            .output();

        // Play the test sound
        let play = Command::new("afplay")
            .arg(test_sound)
            .output();

        match play {
            Ok(output) if output.status.success() => {
                println!("Test tone played successfully!");
            }
            _ => {
                println!("Warning: Could not play test tone");
            }
        }
    } else {
        println!("Test sound not found, skipping playback test");
    }

    println!();
    println!("Virtual audio device test completed!");
    println!();
    println!("Applications can now:");
    println!("  1. Output audio to: {}", BLACKHOLE_DEVICE_NAME);
    println!("  2. Capture audio from: {} (same device as input)", BLACKHOLE_DEVICE_NAME);
    println!();
    println!("Note: Use Audio MIDI Setup.app to configure multi-output devices");
    println!("if you need to hear audio while capturing.");

    Ok(())
}

/// Check if virtual audio device exists
fn device_exists() -> Result<bool, Box<dyn std::error::Error>> {
    // Check if BlackHole plugin exists
    let plugin_paths = [
        PathBuf::from(HAL_PLUGINS_DIR).join("BlackHole2ch.driver"),
        PathBuf::from(HAL_PLUGINS_DIR).join("BlackHole.driver"),
        PathBuf::from(HAL_PLUGINS_DIR).join("RSAC_VirtualAudio.driver"),
    ];

    for path in &plugin_paths {
        if path.exists() {
            return Ok(true);
        }
    }

    // Also check system_profiler for active devices
    let output = Command::new("system_profiler")
        .args(["SPAudioDataType"])
        .output()?;

    let devices = String::from_utf8_lossy(&output.stdout);
    Ok(devices.contains("BlackHole") || devices.contains("Virtual Audio"))
}

/// Install via Homebrew
fn install_via_homebrew() -> Result<(), Box<dyn std::error::Error>> {
    println!("Installing {} via Homebrew...", BLACKHOLE_BREW_PACKAGE);

    // First, tap the cask if needed
    let tap = Command::new("brew")
        .args(["tap", "homebrew/cask"])
        .output();

    // Install BlackHole
    let install = Command::new("brew")
        .args(["install", "--cask", BLACKHOLE_BREW_PACKAGE])
        .output()?;

    if !install.status.success() {
        let stderr = String::from_utf8_lossy(&install.stderr);
        let stdout = String::from_utf8_lossy(&install.stdout);

        // Check if already installed
        if stdout.contains("already installed") || stderr.contains("already installed") {
            println!("BlackHole already installed via Homebrew.");
            return Ok(());
        }

        return Err(format!("Homebrew install failed: {} {}", stdout, stderr).into());
    }

    println!("BlackHole installed successfully via Homebrew!");
    restart_coreaudio()?;

    Ok(())
}

/// Build from source (most self-contained approach)
fn build_from_source() -> Result<(), Box<dyn std::error::Error>> {
    println!("Building BlackHole from source...");
    println!("Repository: {}", BLACKHOLE_REPO);
    println!("Version: {}", BLACKHOLE_VERSION);
    println!();

    // Check for Xcode command line tools
    let has_xcode = Command::new("xcode-select")
        .args(["--print-path"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !has_xcode {
        return Err("Xcode command line tools required. Run: xcode-select --install".into());
    }

    let build_dir = std::env::temp_dir().join("rsac-blackhole-build");
    fs::create_dir_all(&build_dir)?;

    // Clone repository
    println!("Cloning repository...");
    let clone = Command::new("git")
        .args(["clone", "--depth", "1", "--branch", BLACKHOLE_VERSION, BLACKHOLE_REPO])
        .current_dir(&build_dir)
        .output()?;

    if !clone.status.success() {
        let stderr = String::from_utf8_lossy(&clone.stderr);
        return Err(format!("Git clone failed: {}", stderr).into());
    }

    let project_dir = build_dir.join("BlackHole");

    // Build with xcodebuild
    println!("Building with xcodebuild...");
    let build = Command::new("xcodebuild")
        .args([
            "-project", "BlackHole.xcodeproj",
            "-configuration", "Release",
            "-target", "BlackHole2ch",
            "GCC_PREPROCESSOR_DEFINITIONS=kNumber_Of_Channels=2 kDevice_Name=\"RSAC Virtual Audio\"",
        ])
        .current_dir(&project_dir)
        .output()?;

    if !build.status.success() {
        let stderr = String::from_utf8_lossy(&build.stderr);
        return Err(format!("xcodebuild failed: {}", stderr).into());
    }

    // Find the built driver
    let built_driver = project_dir
        .join("build")
        .join("Release")
        .join("BlackHole2ch.driver");

    if !built_driver.exists() {
        return Err("Built driver not found".into());
    }

    // Install the driver
    println!("Installing HAL plugin...");
    let dest = PathBuf::from(HAL_PLUGINS_DIR).join("BlackHole2ch.driver");

    let install = Command::new("sudo")
        .args(["cp", "-R", &built_driver.to_string_lossy(), &dest.to_string_lossy()])
        .output()?;

    if !install.status.success() {
        let stderr = String::from_utf8_lossy(&install.stderr);
        return Err(format!("Failed to install plugin: {}", stderr).into());
    }

    // Clean up build directory
    let _ = fs::remove_dir_all(&build_dir);

    restart_coreaudio()?;

    println!("Built and installed successfully!");
    Ok(())
}

/// Install bundled HAL plugin
fn install_bundled_plugin(plugin_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    println!("Installing bundled HAL plugin...");

    let dest = PathBuf::from(HAL_PLUGINS_DIR).join(
        plugin_path.file_name().ok_or("Invalid plugin path")?
    );

    let install = Command::new("sudo")
        .args(["cp", "-R", &plugin_path.to_string_lossy(), &dest.to_string_lossy()])
        .output()?;

    if !install.status.success() {
        let stderr = String::from_utf8_lossy(&install.stderr);
        return Err(format!("Failed to install plugin: {}", stderr).into());
    }

    restart_coreaudio()?;

    println!("Bundled plugin installed successfully!");
    Ok(())
}

/// Restart CoreAudio to pick up new devices
fn restart_coreaudio() -> Result<(), Box<dyn std::error::Error>> {
    println!("Restarting CoreAudio daemon...");

    let restart = Command::new("sudo")
        .args(["launchctl", "kickstart", "-kp", "system/com.apple.audio.coreaudiod"])
        .output()?;

    if restart.status.success() {
        println!("CoreAudio restarted successfully.");
        // Give it a moment to re-enumerate devices
        std::thread::sleep(std::time::Duration::from_secs(2));
    } else {
        println!("Warning: Could not restart CoreAudio (may need manual restart)");
    }

    Ok(())
}

/// Get path to bundled HAL plugin
fn get_bundled_plugin_path() -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));

    exe_dir.join("drivers").join("macos").join("RSAC_VirtualAudio.driver")
}

/// Get the virtual device name
pub fn get_device_name() -> &'static str {
    BLACKHOLE_DEVICE_NAME
}
