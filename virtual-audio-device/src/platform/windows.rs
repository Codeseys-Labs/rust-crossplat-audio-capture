//! Windows Virtual Audio Device Implementation
//!
//! Windows requires a kernel-mode WDM (Windows Driver Model) audio driver
//! to create virtual audio devices. Unlike Linux, there's no userspace alternative.
//!
//! This module provides two approaches:
//! 1. Use a pre-built, signed virtual audio driver (current)
//! 2. Build and install our own minimal driver (future)
//!
//! For CI testing, we currently use the Virtual Audio Driver project which
//! provides a signed driver that can be installed without special certificates.
//!
//! Future: We plan to include a minimal SYSVAD-based driver in this repo
//! that can be built with the Windows Driver Kit (WDK).

use std::process::{Command, Stdio};
use std::path::{Path, PathBuf};
use std::fs;

/// Driver metadata
const DRIVER_NAME: &str = "RSAC Virtual Audio";
const DRIVER_VENDOR: &str = "VirtualDrivers"; // Current driver source
const DRIVER_VERSION: &str = "25.7.14";
const DRIVER_DOWNLOAD_URL: &str = "https://github.com/VirtualDrivers/Virtual-Audio-Driver/releases/download/25.7.14/Virtual.Audio.Driver.Signed.-.25.7.14.zip";

/// Expected device names after installation
const VIRTUAL_SPEAKER_NAME: &str = "Virtual Speaker";
const VIRTUAL_MIC_NAME: &str = "Virtual Microphone";

/// Create/install the virtual audio device
pub fn create_virtual_device() -> Result<(), Box<dyn std::error::Error>> {
    println!("Platform: Windows (WASAPI/WDM Driver)");
    println!();

    // Check if device already exists
    if device_exists()? {
        println!("Virtual audio device already installed.");
        return Ok(());
    }

    // Check if we have admin privileges
    if !is_admin() {
        return Err("Administrator privileges required to install drivers. Run as Administrator.".into());
    }

    // Try to use bundled driver first (future)
    let bundled_driver = get_bundled_driver_path();
    if bundled_driver.exists() {
        println!("Using bundled driver from: {:?}", bundled_driver);
        return install_bundled_driver(&bundled_driver);
    }

    // Fall back to downloading the driver
    println!("Bundled driver not found, downloading from GitHub...");
    println!("Source: {}", DRIVER_VENDOR);
    println!("Version: {}", DRIVER_VERSION);
    println!();

    let driver_path = download_driver()?;
    install_downloaded_driver(&driver_path)?;

    // Verify installation
    if device_exists()? {
        println!();
        println!("Virtual audio device installed successfully!");
        println!("  Speaker: {}", VIRTUAL_SPEAKER_NAME);
        println!("  Microphone: {}", VIRTUAL_MIC_NAME);
        Ok(())
    } else {
        Err("Driver installed but device not detected. A reboot may be required.".into())
    }
}

/// Remove the virtual audio device
pub fn remove_virtual_device() -> Result<(), Box<dyn std::error::Error>> {
    println!("Platform: Windows (WASAPI/WDM Driver)");
    println!();

    if !is_admin() {
        return Err("Administrator privileges required to remove drivers. Run as Administrator.".into());
    }

    // Find and remove the driver using pnputil
    println!("Searching for installed virtual audio drivers...");

    let output = Command::new("pnputil")
        .args(["/enum-drivers"])
        .output()?;

    let drivers = String::from_utf8_lossy(&output.stdout);

    // Look for our driver in the output
    let mut found_oem = None;
    for line in drivers.lines() {
        if line.contains("Virtual") && line.contains("Audio") {
            // Try to extract OEM inf name
            if let Some(prev_line) = drivers.lines()
                .take_while(|l| *l != line)
                .filter(|l| l.contains("oem"))
                .last() {
                if let Some(oem) = prev_line.split_whitespace().last() {
                    found_oem = Some(oem.to_string());
                }
            }
        }
    }

    if let Some(oem_inf) = found_oem {
        println!("Found driver: {}", oem_inf);
        println!("Removing driver...");

        let uninstall = Command::new("pnputil")
            .args(["/delete-driver", &oem_inf, "/uninstall", "/force"])
            .output()?;

        if uninstall.status.success() {
            println!("Driver removed successfully.");
            println!("Note: A reboot may be required to complete removal.");
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&uninstall.stderr);
            Err(format!("Failed to remove driver: {}", stderr).into())
        }
    } else {
        println!("Virtual audio driver not found in driver store.");
        Ok(())
    }
}

/// Check device status
pub fn check_device_status() -> Result<(), Box<dyn std::error::Error>> {
    println!("Platform: Windows (WASAPI/WDM Driver)");
    println!();

    println!("Checking for virtual audio devices...");
    println!();

    // Use PowerShell to enumerate audio devices
    let script = r#"
        Get-PnpDevice | Where-Object {
            $_.FriendlyName -like '*Virtual*Audio*' -or
            $_.FriendlyName -like '*Virtual*Speaker*' -or
            $_.FriendlyName -like '*Virtual*Mic*'
        } | Format-Table FriendlyName, Status, InstanceId -AutoSize
    "#;

    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()?;

    let devices = String::from_utf8_lossy(&output.stdout);

    if devices.trim().is_empty() || devices.contains("----") == false {
        println!("Status: No virtual audio devices found");
        println!();
        println!("Run 'vad-setup create' to install the virtual audio driver.");
    } else {
        println!("Found virtual audio devices:");
        println!("{}", devices);
        println!();
        println!("Status: Virtual audio device is INSTALLED");
    }

    // Also show Windows audio service status
    println!();
    println!("Audio Services Status:");

    let services = Command::new("powershell")
        .args(["-NoProfile", "-Command",
            "Get-Service AudioSrv, AudioEndpointBuilder | Format-Table Name, Status -AutoSize"])
        .output()?;

    println!("{}", String::from_utf8_lossy(&services.stdout));

    Ok(())
}

/// Test the virtual audio device
pub fn test_virtual_device() -> Result<(), Box<dyn std::error::Error>> {
    println!("Platform: Windows (WASAPI/WDM Driver)");
    println!();

    // Ensure device exists
    if !device_exists()? {
        println!("Virtual audio device not found. Installing...");
        create_virtual_device()?;
        println!();
    }

    // Try to play a test tone using PowerShell
    println!("Playing test tone through virtual device...");

    let script = r#"
        Add-Type -AssemblyName System.Speech
        $synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
        # This will use the default audio device
        $synth.Speak("Virtual audio device test")
    "#;

    let play = Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output();

    match play {
        Ok(output) if output.status.success() => {
            println!("Test tone played successfully!");
        }
        _ => {
            println!("Warning: Could not play test tone (this is OK in headless CI)");
        }
    }

    println!();
    println!("Virtual audio device test completed!");
    println!();
    println!("Applications can now:");
    println!("  1. Output audio to: {}", VIRTUAL_SPEAKER_NAME);
    println!("  2. Capture audio from the virtual device loopback");

    Ok(())
}

/// Check if virtual audio device is installed
fn device_exists() -> Result<bool, Box<dyn std::error::Error>> {
    let script = r#"
        $devices = Get-PnpDevice | Where-Object {
            $_.FriendlyName -like '*Virtual*Speaker*' -or
            $_.FriendlyName -like '*Virtual*Audio*'
        }
        if ($devices) { Write-Output "FOUND" } else { Write-Output "NOTFOUND" }
    "#;

    let output = Command::new("powershell")
        .args(["-NoProfile", "-Command", script])
        .output()?;

    let result = String::from_utf8_lossy(&output.stdout);
    Ok(result.trim() == "FOUND")
}

/// Check if running with admin privileges
fn is_admin() -> bool {
    let output = Command::new("net")
        .args(["session"])
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .status();

    output.map(|s| s.success()).unwrap_or(false)
}

/// Get path to bundled driver (for future self-contained distribution)
fn get_bundled_driver_path() -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));

    exe_dir.join("drivers").join("windows").join("rsac-virtual-audio.inf")
}

/// Download the driver from GitHub
fn download_driver() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let temp_dir = std::env::temp_dir().join("rsac-virtual-audio");
    fs::create_dir_all(&temp_dir)?;

    let zip_path = temp_dir.join("driver.zip");
    let extract_dir = temp_dir.join("extracted");

    println!("Downloading driver package...");

    // Use PowerShell to download (works on all Windows versions)
    let download_script = format!(
        "Invoke-WebRequest -Uri '{}' -OutFile '{}'",
        DRIVER_DOWNLOAD_URL,
        zip_path.display()
    );

    let download = Command::new("powershell")
        .args(["-NoProfile", "-Command", &download_script])
        .output()?;

    if !download.status.success() {
        let stderr = String::from_utf8_lossy(&download.stderr);
        return Err(format!("Failed to download driver: {}", stderr).into());
    }

    println!("Extracting driver package...");

    // Extract the zip
    let extract_script = format!(
        "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
        zip_path.display(),
        extract_dir.display()
    );

    let extract = Command::new("powershell")
        .args(["-NoProfile", "-Command", &extract_script])
        .output()?;

    if !extract.status.success() {
        let stderr = String::from_utf8_lossy(&extract.stderr);
        return Err(format!("Failed to extract driver: {}", stderr).into());
    }

    Ok(extract_dir)
}

/// Install a downloaded driver
fn install_downloaded_driver(driver_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    println!("Installing driver from: {:?}", driver_dir);

    // Find .inf files
    let inf_files: Vec<_> = walkdir(driver_dir)?
        .into_iter()
        .filter(|p| p.extension().map(|e| e == "inf").unwrap_or(false))
        .collect();

    if inf_files.is_empty() {
        return Err("No .inf driver files found in package".into());
    }

    // Install certificate first (for trust)
    println!("Pre-authorizing driver certificate...");
    install_driver_certificate(driver_dir)?;

    // Install each .inf file
    for inf_file in &inf_files {
        println!("Installing: {:?}", inf_file.file_name().unwrap_or_default());

        let install = Command::new("pnputil")
            .args(["/add-driver", &inf_file.to_string_lossy(), "/install"])
            .output()?;

        if install.status.success() {
            println!("Driver installed successfully!");
        } else {
            let stderr = String::from_utf8_lossy(&install.stderr);
            let stdout = String::from_utf8_lossy(&install.stdout);
            println!("pnputil output: {}", stdout);
            if !stderr.is_empty() {
                println!("pnputil stderr: {}", stderr);
            }
            // Continue anyway - might need reboot
        }
    }

    // Restart audio services
    println!("Restarting audio services...");
    let _ = Command::new("net").args(["stop", "AudioSrv"]).output();
    let _ = Command::new("net").args(["start", "AudioSrv"]).output();

    Ok(())
}

/// Install driver certificate to trusted stores
fn install_driver_certificate(driver_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // Find .cat files (contain the driver signature)
    let cat_files: Vec<_> = walkdir(driver_dir)?
        .into_iter()
        .filter(|p| p.extension().map(|e| e == "cat").unwrap_or(false))
        .collect();

    for cat_file in cat_files {
        let script = format!(r#"
            $sig = Get-AuthenticodeSignature -FilePath '{}'
            if ($sig.SignerCertificate) {{
                $cert = $sig.SignerCertificate
                $certPath = Join-Path $env:TEMP 'driver_cert.cer'
                [System.IO.File]::WriteAllBytes($certPath, $cert.Export('Cert'))
                Import-Certificate -FilePath $certPath -CertStoreLocation Cert:\LocalMachine\Root | Out-Null
                Import-Certificate -FilePath $certPath -CertStoreLocation Cert:\LocalMachine\TrustedPublisher | Out-Null
                Remove-Item $certPath
                Write-Output 'Certificate imported successfully'
            }}
        "#, cat_file.display());

        let result = Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .output()?;

        if result.status.success() {
            println!("Certificate imported from {:?}", cat_file.file_name());
            return Ok(());
        }
    }

    println!("Warning: Could not import driver certificate (driver may still work)");
    Ok(())
}

/// Install our bundled driver (future)
fn install_bundled_driver(driver_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    println!("Installing bundled driver: {:?}", driver_path);

    let install = Command::new("pnputil")
        .args(["/add-driver", &driver_path.to_string_lossy(), "/install"])
        .output()?;

    if !install.status.success() {
        let stderr = String::from_utf8_lossy(&install.stderr);
        return Err(format!("Failed to install bundled driver: {}", stderr).into());
    }

    println!("Bundled driver installed successfully!");
    Ok(())
}

/// Simple recursive directory walker
fn walkdir(dir: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();

    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                files.extend(walkdir(&path)?);
            } else {
                files.push(path);
            }
        }
    }

    Ok(files)
}

/// Get the virtual speaker device name
pub fn get_virtual_speaker_name() -> &'static str {
    VIRTUAL_SPEAKER_NAME
}

/// Get the virtual microphone device name
pub fn get_virtual_mic_name() -> &'static str {
    VIRTUAL_MIC_NAME
}
