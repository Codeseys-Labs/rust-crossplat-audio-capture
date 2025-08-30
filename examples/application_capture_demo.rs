//! Application-Specific Audio Capture Demo
//! 
//! This example demonstrates how to use the unified cross-platform API to capture
//! audio from specific applications on Windows, Linux, and macOS.
//! 
//! Usage:
//! ```bash
//! # List all capturable applications
//! cargo run --example application_capture_demo -- list
//! 
//! # Capture audio from a specific process ID
//! cargo run --example application_capture_demo -- pid 1234
//! 
//! # Capture audio from an application by name
//! cargo run --example application_capture_demo -- name "firefox"
//! ```

use std::env;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::thread;

use rsac::audio::{
    ApplicationCapture, capture_application_by_pid, capture_application_by_name,
    list_capturable_applications,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    
    if args.len() < 2 {
        print_usage();
        return Ok(());
    }
    
    match args[1].as_str() {
        "list" => list_applications(),
        "pid" => {
            if args.len() < 3 {
                eprintln!("Error: Process ID required");
                print_usage();
                return Ok(());
            }
            let pid: u32 = args[2].parse()?;
            capture_by_pid(pid)
        }
        "name" => {
            if args.len() < 3 {
                eprintln!("Error: Application name required");
                print_usage();
                return Ok(());
            }
            capture_by_name(&args[2])
        }
        _ => {
            eprintln!("Error: Unknown command '{}'", args[1]);
            print_usage();
            Ok(())
        }
    }
}

fn print_usage() {
    println!("Application-Specific Audio Capture Demo");
    println!();
    println!("Usage:");
    println!("  cargo run --example application_capture_demo -- list");
    println!("  cargo run --example application_capture_demo -- pid <process_id>");
    println!("  cargo run --example application_capture_demo -- name <app_name>");
    println!();
    println!("Commands:");
    println!("  list        List all applications that can be captured");
    println!("  pid <id>    Capture audio from process with specified ID");
    println!("  name <name> Capture audio from application containing the specified name");
}

fn list_applications() -> Result<(), Box<dyn std::error::Error>> {
    println!("Listing capturable applications...");
    println!();
    
    let applications = list_capturable_applications()?;
    
    if applications.is_empty() {
        println!("No capturable applications found.");
        return Ok(());
    }
    
    println!("{:<8} {:<30} {}", "PID", "Application Name", "Platform Info");
    println!("{:-<8} {:-<30} {:-<20}", "", "", "");
    
    for app in applications {
        let platform_info = match app.platform_specific {
            #[cfg(target_os = "windows")]
            rsac::audio::PlatformSpecificInfo::Windows { .. } => "Windows WASAPI",
            
            #[cfg(target_os = "linux")]
            rsac::audio::PlatformSpecificInfo::Linux { node_id, media_class } => {
                &format!("PipeWire Node {} ({})", 
                    node_id.unwrap_or(0), 
                    media_class.as_deref().unwrap_or("Unknown"))
            }
            
            #[cfg(target_os = "macos")]
            rsac::audio::PlatformSpecificInfo::MacOS { .. } => "CoreAudio Process Tap",
        };
        
        println!("{:<8} {:<30} {}", app.process_id, app.name, platform_info);
    }
    
    Ok(())
}

fn capture_by_pid(pid: u32) -> Result<(), Box<dyn std::error::Error>> {
    println!("Capturing audio from process ID: {}", pid);
    
    let mut capture = capture_application_by_pid(pid)?;
    start_capture_session(capture)
}

fn capture_by_name(app_name: &str) -> Result<(), Box<dyn std::error::Error>> {
    println!("Capturing audio from application: {}", app_name);
    
    let mut capture = capture_application_by_name(app_name)?;
    start_capture_session(capture)
}

fn start_capture_session(mut capture: rsac::audio::CrossPlatformApplicationCapture) -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting audio capture...");
    println!("Press Ctrl+C to stop capture");
    
    // Set up signal handling for graceful shutdown
    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();
    
    ctrlc::set_handler(move || {
        println!("\nReceived Ctrl+C, stopping capture...");
        running_clone.store(false, Ordering::SeqCst);
    })?;
    
    // Audio processing statistics
    let mut sample_count = 0u64;
    let mut max_amplitude = 0.0f32;
    let start_time = std::time::Instant::now();
    
    // Start capturing with a callback that processes audio data
    capture.start_capture(move |samples| {
        sample_count += samples.len() as u64;
        
        // Calculate peak amplitude in this chunk
        let chunk_max = samples.iter()
            .map(|&s| s.abs())
            .fold(0.0f32, f32::max);
        
        if chunk_max > max_amplitude {
            max_amplitude = chunk_max;
        }
        
        // Print periodic statistics (every ~1 second worth of samples)
        if sample_count % 48000 == 0 {
            let elapsed = start_time.elapsed().as_secs_f32();
            let duration_mins = elapsed / 60.0;
            
            println!("Captured: {:.1} min | Samples: {} | Peak: {:.3}", 
                duration_mins, sample_count, max_amplitude);
        }
    })?;
    
    // Keep the main thread alive while capturing
    while running.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(100));
    }
    
    // Stop capture
    capture.stop_capture()?;
    
    let total_duration = start_time.elapsed().as_secs_f32();
    println!();
    println!("Capture completed:");
    println!("  Duration: {:.2} seconds", total_duration);
    println!("  Total samples: {}", sample_count);
    println!("  Peak amplitude: {:.3}", max_amplitude);
    println!("  Sample rate: ~{:.0} Hz", sample_count as f32 / total_duration);
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_list_applications() {
        // This test verifies that the application listing doesn't crash
        // The actual results will vary by platform and running applications
        let result = list_capturable_applications();
        assert!(result.is_ok(), "Application listing should not fail");
    }
    
    #[test]
    fn test_invalid_pid() {
        // Test that capturing from an invalid PID returns an error
        let result = capture_application_by_pid(0);
        assert!(result.is_err(), "Capturing from PID 0 should fail");
    }
    
    #[test]
    fn test_invalid_app_name() {
        // Test that capturing from a non-existent application returns an error
        let result = capture_application_by_name("nonexistent_application_12345");
        assert!(result.is_err(), "Capturing from non-existent app should fail");
    }
}
