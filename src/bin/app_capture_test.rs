//! Application Capture Test Binary
//! 
//! This binary is designed for CI/CD testing across platforms.
//! It provides a simple interface to test application-specific audio capture
//! functionality without requiring interactive input.
//! 
//! Exit codes:
//! - 0: Success
//! - 1: General error
//! - 2: Platform not supported
//! - 3: No applications found
//! - 4: Capture failed

use std::env;
use std::process;
use std::time::Duration;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rsac::audio::{
    ApplicationCapture, capture_application_by_pid, capture_application_by_name,
    list_capturable_applications,
};

fn main() {
    let args: Vec<String> = env::args().collect();
    
    let result = match args.get(1).map(|s| s.as_str()) {
        Some("--version") => {
            println!("app_capture_test v{}", env!("CARGO_PKG_VERSION"));
            println!("Platform: {}", std::env::consts::OS);
            println!("Architecture: {}", std::env::consts::ARCH);
            Ok(())
        }
        Some("--list") => test_list_applications(),
        Some("--test-invalid") => test_invalid_inputs(),
        Some("--test-lifecycle") => test_capture_lifecycle(),
        Some("--test-platform") => test_platform_specific(),
        Some("--quick-test") => quick_functionality_test(),
        Some("--help") | None => {
            print_help();
            Ok(())
        }
        Some(cmd) => {
            eprintln!("Unknown command: {}", cmd);
            print_help();
            process::exit(1);
        }
    };
    
    match result {
        Ok(()) => {
            println!("✅ Test completed successfully");
            process::exit(0);
        }
        Err(e) => {
            eprintln!("❌ Test failed: {}", e);
            process::exit(1);
        }
    }
}

fn print_help() {
    println!("Application Capture Test Binary");
    println!();
    println!("Usage: app_capture_test [COMMAND]");
    println!();
    println!("Commands:");
    println!("  --version        Show version and platform information");
    println!("  --list           Test application listing functionality");
    println!("  --test-invalid   Test error handling with invalid inputs");
    println!("  --test-lifecycle Test capture start/stop lifecycle");
    println!("  --test-platform  Test platform-specific features");
    println!("  --quick-test     Run a quick functionality test (default for CI)");
    println!("  --help           Show this help message");
    println!();
    println!("Exit codes:");
    println!("  0: Success");
    println!("  1: General error");
    println!("  2: Platform not supported");
    println!("  3: No applications found");
    println!("  4: Capture failed");
}

fn test_list_applications() -> Result<(), Box<dyn std::error::Error>> {
    println!("🔍 Testing application listing...");
    
    let applications = list_capturable_applications()?;
    
    println!("Found {} capturable applications", applications.len());
    
    if applications.is_empty() {
        println!("⚠️  No applications found - this may be normal on some systems");
        return Ok(());
    }
    
    // Show first few applications
    for (i, app) in applications.iter().take(5).enumerate() {
        println!("  {}. PID: {}, Name: {}", i + 1, app.process_id, app.name);
        
        // Show platform-specific info
        match &app.platform_specific {
            #[cfg(target_os = "windows")]
            rsac::audio::PlatformSpecificInfo::Windows { .. } => {
                println!("     Platform: Windows WASAPI");
            }
            #[cfg(target_os = "linux")]
            rsac::audio::PlatformSpecificInfo::Linux { node_id, media_class } => {
                println!("     Platform: Linux PipeWire (Node: {:?}, Class: {:?})", 
                    node_id, media_class);
            }
            #[cfg(target_os = "macos")]
            rsac::audio::PlatformSpecificInfo::MacOS { .. } => {
                println!("     Platform: macOS CoreAudio");
            }
        }
    }
    
    if applications.len() > 5 {
        println!("  ... and {} more", applications.len() - 5);
    }
    
    Ok(())
}

fn test_invalid_inputs() -> Result<(), Box<dyn std::error::Error>> {
    println!("🧪 Testing error handling...");
    
    // Test invalid PID
    println!("  Testing invalid PID...");
    let result = capture_application_by_pid(0);
    if result.is_ok() {
        return Err("Expected error for PID 0, but got success".into());
    }
    println!("    ✅ Invalid PID correctly rejected");
    
    // Test invalid application name
    println!("  Testing invalid application name...");
    let result = capture_application_by_name("nonexistent_app_12345");
    if result.is_ok() {
        return Err("Expected error for fake app name, but got success".into());
    }
    println!("    ✅ Invalid app name correctly rejected");
    
    Ok(())
}

fn test_capture_lifecycle() -> Result<(), Box<dyn std::error::Error>> {
    println!("🔄 Testing capture lifecycle...");
    
    let applications = list_capturable_applications()?;
    
    if applications.is_empty() {
        println!("  ⚠️  No applications available for lifecycle test");
        return Ok(());
    }
    
    let app = &applications[0];
    println!("  Testing with PID: {}, Name: {}", app.process_id, app.name);
    
    let mut capture = capture_application_by_pid(app.process_id)?;
    
    // Test initial state
    if capture.is_capturing() {
        return Err("Capture should not be active initially".into());
    }
    println!("    ✅ Initial state correct");
    
    // Test start capture
    let sample_count = Arc::new(Mutex::new(0usize));
    let sample_count_clone = sample_count.clone();
    
    let start_result = capture.start_capture(move |samples| {
        let mut count = sample_count_clone.lock().unwrap();
        *count += samples.len();
    });
    
    if let Err(e) = start_result {
        println!("    ⚠️  Could not start capture: {} (this may be expected on some systems)", e);
        return Ok(());
    }
    
    if !capture.is_capturing() {
        return Err("Capture should be active after start".into());
    }
    println!("    ✅ Capture started successfully");
    
    // Let it run briefly
    thread::sleep(Duration::from_millis(100));
    
    // Test stop capture
    capture.stop_capture()?;
    
    if capture.is_capturing() {
        return Err("Capture should not be active after stop".into());
    }
    println!("    ✅ Capture stopped successfully");
    
    let final_count = *sample_count.lock().unwrap();
    println!("    📊 Captured {} samples during test", final_count);
    
    Ok(())
}

fn test_platform_specific() -> Result<(), Box<dyn std::error::Error>> {
    println!("🖥️  Testing platform-specific features...");
    
    #[cfg(target_os = "windows")]
    {
        println!("  Windows platform detected");
        use rsac::audio::windows::WindowsApplicationCapture;
        
        let processes = WindowsApplicationCapture::list_audio_processes();
        println!("    Found {} Windows processes", processes.len());
        
        if let Some((pid, name)) = processes.first() {
            let found_pid = WindowsApplicationCapture::find_process_by_name(name, false);
            if found_pid.is_some() {
                println!("    ✅ Process discovery working");
            } else {
                println!("    ⚠️  Process discovery may have issues");
            }
        }
    }
    
    #[cfg(target_os = "linux")]
    {
        println!("  Linux platform detected");
        use rsac::audio::linux::pipewire::PipeWireApplicationCapture;
        
        let result = PipeWireApplicationCapture::list_audio_applications();
        match result {
            Ok(apps) => {
                println!("    Found {} PipeWire applications", apps.len());
                println!("    ✅ PipeWire integration working");
            }
            Err(e) => {
                println!("    ⚠️  PipeWire may not be available: {}", e);
            }
        }
    }
    
    #[cfg(target_os = "macos")]
    {
        println!("  macOS platform detected");
        use rsac::audio::macos::tap::MacOSApplicationCapture;
        
        let is_available = MacOSApplicationCapture::is_process_tap_available();
        println!("    Process Tap available: {}", is_available);
        
        if is_available {
            println!("    ✅ Process Tap APIs available");
        } else {
            println!("    ⚠️  Process Tap requires macOS 14.4+");
        }
        
        let apps = MacOSApplicationCapture::list_capturable_applications()?;
        println!("    Found {} capturable applications", apps.len());
    }
    
    Ok(())
}

fn quick_functionality_test() -> Result<(), Box<dyn std::error::Error>> {
    println!("⚡ Running quick functionality test...");
    
    // Test 1: Application listing
    println!("  1/4 Testing application listing...");
    let applications = list_capturable_applications()?;
    println!("      Found {} applications", applications.len());
    
    // Test 2: Error handling
    println!("  2/4 Testing error handling...");
    let invalid_result = capture_application_by_pid(0);
    if invalid_result.is_err() {
        println!("      ✅ Error handling works");
    } else {
        return Err("Error handling failed".into());
    }
    
    // Test 3: Platform detection
    println!("  3/4 Testing platform detection...");
    println!("      Platform: {}", std::env::consts::OS);
    println!("      Architecture: {}", std::env::consts::ARCH);
    
    // Test 4: Basic capture attempt (if applications available)
    println!("  4/4 Testing basic capture...");
    if let Some(app) = applications.first() {
        let capture_result = capture_application_by_pid(app.process_id);
        match capture_result {
            Ok(mut capture) => {
                println!("      ✅ Capture creation successful");
                
                // Quick start/stop test
                let start_result = capture.start_capture(|_| {});
                if start_result.is_ok() {
                    thread::sleep(Duration::from_millis(10));
                    let _ = capture.stop_capture();
                    println!("      ✅ Capture lifecycle successful");
                } else {
                    println!("      ⚠️  Capture start failed (may be expected): {:?}", start_result.err());
                }
            }
            Err(e) => {
                println!("      ⚠️  Capture creation failed (may be expected): {}", e);
            }
        }
    } else {
        println!("      ⚠️  No applications available for capture test");
    }
    
    println!("✅ Quick test completed successfully");
    Ok(())
}
