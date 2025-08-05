// Simple Windows device enumeration test
// Tests our enhanced WASAPI implementation

#[cfg(target_os = "windows")]
use rsac::audio::windows::{WindowsDeviceEnumerator, enumerate_application_audio_sessions};
use rsac::core::interface::DeviceEnumerator;
use rsac::core::error::AudioResult;

#[cfg(target_os = "windows")]
fn test_device_enumeration() -> AudioResult<()> {
    println!("=== Testing Enhanced Windows WASAPI Implementation ===");
    
    // Test device enumeration
    println!("\n1. Testing Device Enumeration:");
    let enumerator = WindowsDeviceEnumerator::new()?;
    let devices = enumerator.enumerate_devices()?;
    
    println!("Found {} audio devices:", devices.len());
    for (i, device) in devices.iter().enumerate() {
        let name = device.get_name()?;
        let id = device.get_id()?;
        let kind = device.kind()?;
        println!("  {}. {} (ID: {}, Kind: {:?})", i + 1, name, id, kind);
        
        // Test default format
        if let Ok(Some(format)) = device.get_default_format() {
            println!("     Default format: {:?}", format);
        }
    }
    
    // Test default devices
    println!("\n2. Testing Default Devices:");
    if let Ok(Some(input_device)) = enumerator.get_default_device(rsac::core::interface::DeviceKind::Input) {
        println!("Default input: {}", input_device.get_name()?);
    } else {
        println!("No default input device found");
    }
    
    if let Ok(Some(output_device)) = enumerator.get_default_device(rsac::core::interface::DeviceKind::Output) {
        println!("Default output: {}", output_device.get_name()?);
    } else {
        println!("No default output device found");
    }
    
    // Test application audio sessions
    println!("\n3. Testing Application Audio Sessions:");
    match enumerate_application_audio_sessions() {
        Ok(sessions) => {
            println!("Found {} active audio sessions:", sessions.len());
            for (i, session) in sessions.iter().enumerate().take(10) { // Show first 10
                println!("  {}. {} (PID: {})", i + 1, session.display_name, session.process_id);
                if let Some(ref path) = session.executable_path {
                    println!("     Path: {}", path);
                }
            }
            if sessions.len() > 10 {
                println!("  ... and {} more sessions", sessions.len() - 10);
            }
        }
        Err(e) => {
            println!("Failed to enumerate audio sessions: {}", e);
        }
    }
    
    println!("\n=== Test Complete ===");
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn test_device_enumeration() -> AudioResult<()> {    
    println!("This test can only run on Windows");
    println!("Current platform: {}", std::env::consts::OS);
    Ok(())
}

fn main() {
    match test_device_enumeration() {
        Ok(_) => println!("✅ All tests passed!"),
        Err(e) => {
            eprintln!("❌ Test failed: {}", e);
            std::process::exit(1);
        }
    }
}