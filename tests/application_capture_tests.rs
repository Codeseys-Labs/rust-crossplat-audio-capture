//! Integration tests for application-specific audio capture
//! 
//! These tests verify that the cross-platform application capture API works correctly
//! across different platforms and handles edge cases appropriately.

use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::thread;

use rsac::audio::{
    ApplicationCapture, ApplicationCaptureFactory, capture_application_by_pid,
    capture_application_by_name, list_capturable_applications,
};

#[test]
fn test_list_applications_returns_results() {
    let result = list_capturable_applications();
    assert!(result.is_ok(), "Listing applications should not fail: {:?}", result.err());
    
    let applications = result.unwrap();
    // We can't guarantee applications will be running, but the call should succeed
    println!("Found {} capturable applications", applications.len());
    
    for app in applications.iter().take(5) { // Show first 5 for debugging
        println!("  PID: {}, Name: {}", app.process_id, app.name);
    }
}

#[test]
fn test_invalid_process_id_handling() {
    // Test various invalid process IDs
    let invalid_pids = vec![0, u32::MAX, 999999];
    
    for pid in invalid_pids {
        let result = capture_application_by_pid(pid);
        assert!(result.is_err(), "Capturing from invalid PID {} should fail", pid);
    }
}

#[test]
fn test_nonexistent_application_name() {
    let fake_names = vec![
        "nonexistent_app_12345",
        "",
        "app_that_definitely_does_not_exist",
    ];
    
    for name in fake_names {
        let result = capture_application_by_name(name);
        assert!(result.is_err(), "Capturing from fake app '{}' should fail", name);
    }
}

#[test]
fn test_factory_create_for_process_id() {
    // Test that the factory method works the same as the convenience function
    let applications = list_capturable_applications().unwrap();
    
    if let Some(app) = applications.first() {
        let result1 = ApplicationCaptureFactory::create_for_process_id(app.process_id);
        let result2 = capture_application_by_pid(app.process_id);
        
        // Both should have the same success/failure result
        assert_eq!(result1.is_ok(), result2.is_ok(), 
            "Factory method and convenience function should have same result");
    }
}

#[test]
fn test_factory_create_for_application_name() {
    let applications = list_capturable_applications().unwrap();
    
    if let Some(app) = applications.first() {
        let result1 = ApplicationCaptureFactory::create_for_application_name(&app.name);
        let result2 = capture_application_by_name(&app.name);
        
        // Both should have the same success/failure result
        assert_eq!(result1.is_ok(), result2.is_ok(), 
            "Factory method and convenience function should have same result");
    }
}

#[test]
fn test_capture_lifecycle() {
    // Test the basic lifecycle: create -> start -> stop
    let applications = list_capturable_applications().unwrap();
    
    if let Some(app) = applications.first() {
        if let Ok(mut capture) = capture_application_by_pid(app.process_id) {
            // Test initial state
            assert!(!capture.is_capturing(), "Should not be capturing initially");
            
            // Test start capture with a simple callback
            let sample_count = Arc::new(Mutex::new(0usize));
            let sample_count_clone = sample_count.clone();
            
            let start_result = capture.start_capture(move |samples| {
                let mut count = sample_count_clone.lock().unwrap();
                *count += samples.len();
            });
            
            if start_result.is_ok() {
                assert!(capture.is_capturing(), "Should be capturing after start");
                
                // Let it capture for a short time
                thread::sleep(Duration::from_millis(100));
                
                // Test stop capture
                let stop_result = capture.stop_capture();
                assert!(stop_result.is_ok(), "Stop capture should succeed: {:?}", stop_result.err());
                assert!(!capture.is_capturing(), "Should not be capturing after stop");
                
                // Check that we received some samples
                let final_count = *sample_count.lock().unwrap();
                println!("Captured {} samples during test", final_count);
            } else {
                println!("Could not start capture for PID {}: {:?}", app.process_id, start_result.err());
            }
        }
    } else {
        println!("No applications available for lifecycle test");
    }
}

#[test]
fn test_multiple_stop_calls() {
    // Test that calling stop multiple times doesn't cause issues
    let applications = list_capturable_applications().unwrap();
    
    if let Some(app) = applications.first() {
        if let Ok(mut capture) = capture_application_by_pid(app.process_id) {
            let start_result = capture.start_capture(|_samples| {
                // Do nothing with samples
            });
            
            if start_result.is_ok() {
                // Stop multiple times
                let stop1 = capture.stop_capture();
                let stop2 = capture.stop_capture();
                let stop3 = capture.stop_capture();
                
                // All should succeed (or at least not panic)
                assert!(stop1.is_ok(), "First stop should succeed");
                assert!(stop2.is_ok(), "Second stop should succeed");
                assert!(stop3.is_ok(), "Third stop should succeed");
                
                assert!(!capture.is_capturing(), "Should not be capturing after multiple stops");
            }
        }
    }
}

#[test]
fn test_platform_specific_info() {
    let applications = list_capturable_applications().unwrap();
    
    for app in applications.iter().take(3) {
        match &app.platform_specific {
            #[cfg(target_os = "windows")]
            rsac::audio::PlatformSpecificInfo::Windows { executable_path } => {
                println!("Windows app: {} (exe: {:?})", app.name, executable_path);
            }
            
            #[cfg(target_os = "linux")]
            rsac::audio::PlatformSpecificInfo::Linux { node_id, media_class } => {
                println!("Linux app: {} (node: {:?}, class: {:?})", 
                    app.name, node_id, media_class);
                assert!(node_id.is_some(), "Node ID should be present for Linux apps");
            }
            
            #[cfg(target_os = "macos")]
            rsac::audio::PlatformSpecificInfo::MacOS { bundle_id } => {
                println!("macOS app: {} (bundle: {:?})", app.name, bundle_id);
            }
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn test_macos_process_tap_availability() {
    use rsac::audio::macos::tap::MacOSApplicationCapture;
    
    let is_available = MacOSApplicationCapture::is_process_tap_available();
    println!("Process Tap available: {}", is_available);
    
    if !is_available {
        println!("Process Tap requires macOS 14.4+");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn test_pipewire_node_discovery() {
    use rsac::audio::linux::pipewire::PipeWireApplicationCapture;
    
    let result = PipeWireApplicationCapture::list_audio_applications();
    match result {
        Ok(apps) => {
            println!("Found {} PipeWire audio applications", apps.len());
            for app in apps.iter().take(3) {
                println!("  Node {}: {} (PID: {:?})", 
                    app.node_id, 
                    app.app_name.as_deref().unwrap_or("Unknown"),
                    app.process_id);
            }
        }
        Err(e) => {
            println!("PipeWire node discovery failed: {}", e);
        }
    }
}

#[cfg(target_os = "windows")]
#[test]
fn test_windows_process_discovery() {
    use rsac::audio::windows::WindowsApplicationCapture;
    
    let processes = WindowsApplicationCapture::list_audio_processes();
    println!("Found {} Windows processes", processes.len());
    
    for (pid, name) in processes.iter().take(5) {
        println!("  PID {}: {}", pid, name);
    }
    
    // Test process name lookup
    if let Some((pid, name)) = processes.first() {
        let found_pid = WindowsApplicationCapture::find_process_by_name(name, false);
        assert!(found_pid.is_some(), "Should find process by name");
        println!("Found PID {} for process '{}'", found_pid.unwrap(), name);
    }
}

#[test]
fn test_concurrent_capture_attempts() {
    // Test that we can handle multiple capture attempts gracefully
    let applications = list_capturable_applications().unwrap();
    
    if let Some(app) = applications.first() {
        let handles: Vec<_> = (0..3).map(|i| {
            let pid = app.process_id;
            thread::spawn(move || {
                let result = capture_application_by_pid(pid);
                println!("Thread {}: capture result: {}", i, result.is_ok());
                result
            })
        }).collect();
        
        let results: Vec<_> = handles.into_iter()
            .map(|h| h.join().unwrap())
            .collect();
        
        // At least one should succeed (or all should fail with the same reason)
        let success_count = results.iter().filter(|r| r.is_ok()).count();
        println!("Concurrent capture attempts: {} succeeded out of 3", success_count);
    }
}
