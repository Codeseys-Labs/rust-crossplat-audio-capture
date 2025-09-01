//! Flexible PipeWire Audio Capture Examples
//!
//! This example demonstrates various ways to use the PipeWire application capture
//! functionality with different patterns and configurations.
//!
//! Usage:
//! ```bash
//! cargo run --example flexible_pipewire_example --features feat_linux
//! ```

use rsac::audio::linux::pipewire::{PipeWireApplicationCapture, ApplicationSelector};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn main() {
    println!("🎵 Flexible PipeWire Audio Capture Examples");
    println!("==========================================");

    // Example 1: Capture for a specific duration
    println!("\n1️⃣ Example 1: Capture VLC audio for 3 seconds");
    example_duration_based_capture();

    // Example 2: Capture indefinitely with manual stop
    println!("\n2️⃣ Example 2: Capture indefinitely with manual stop");
    example_indefinite_capture();

    // Example 3: Manual stream lifecycle management
    println!("\n3️⃣ Example 3: Manual stream lifecycle management");
    example_manual_lifecycle();

    // Example 4: Multiple capture sessions
    println!("\n4️⃣ Example 4: Multiple capture sessions");
    example_multiple_sessions();

    println!("\n✅ All examples completed!");
}

/// Example 1: Simple duration-based capture
fn example_duration_based_capture() {
    let sample_count = Arc::new(AtomicUsize::new(0));
    let peak_level = Arc::new(Mutex::new(0.0f32));

    let sample_count_clone = sample_count.clone();
    let peak_level_clone = peak_level.clone();

    let mut capture = PipeWireApplicationCapture::new(ApplicationSelector::NodeId(62)); // VLC

    match capture.discover_target_node() {
        Ok(node_id) => {
            println!("✅ Found target node: {}", node_id);
            
            if let Err(e) = capture.create_monitor_stream() {
                println!("❌ Failed to create monitor stream: {}", e);
                return;
            }

            // Use the convenience method for duration-based capture
            let result = capture.start_capture_for_duration(
                move |samples| {
                    let count = sample_count_clone.fetch_add(samples.len(), Ordering::SeqCst);
                    
                    // Calculate peak level
                    let peak = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
                    if let Ok(mut current_peak) = peak_level_clone.lock() {
                        *current_peak = current_peak.max(peak);
                    }

                    // Print progress occasionally
                    if count % 50000 == 0 {
                        println!("    📊 Captured {} samples, peak: {:.3}", count + samples.len(), peak);
                    }
                },
                Duration::from_secs(3)
            );

            match result {
                Ok(()) => {
                    let total = sample_count.load(Ordering::SeqCst);
                    let peak = peak_level.lock().unwrap();
                    println!("✅ Capture completed: {} samples, peak: {:.3}", total, *peak);
                }
                Err(e) => println!("❌ Capture failed: {}", e),
            }
        }
        Err(e) => println!("❌ Failed to find target node: {}", e),
    }
}

/// Example 2: Indefinite capture with manual stop (simulated)
fn example_indefinite_capture() {
    println!("🔄 Demonstrating indefinite capture with manual stop...");

    let sample_count = Arc::new(AtomicUsize::new(0));
    let sample_count_clone = sample_count.clone();

    let mut capture = PipeWireApplicationCapture::new(ApplicationSelector::NodeId(62)); // VLC

    match capture.discover_target_node() {
        Ok(node_id) => {
            println!("✅ Found target node: {}", node_id);

            if let Err(e) = capture.create_monitor_stream() {
                println!("❌ Failed to create monitor stream: {}", e);
                return;
            }

            // Start capture with a callback that will stop after some samples
            let result = capture.start_capture(move |samples| {
                let count = sample_count_clone.fetch_add(samples.len(), Ordering::SeqCst);

                if count % 50000 == 0 {
                    println!("    📊 Indefinite capture: {} samples", count + samples.len());
                }
            });

            if result.is_err() {
                println!("❌ Failed to start capture: {:?}", result);
                return;
            }

            // Run for a short time to simulate indefinite capture
            println!("🔄 Running indefinitely for 2 seconds (demo)...");
            let _ = capture.run_main_loop_with_options(Some(Duration::from_secs(2)), true);

            // Stop the capture
            capture.stop_capture().unwrap();

            let total = sample_count.load(Ordering::SeqCst);
            println!("✅ Indefinite capture demo completed: {} samples", total);
        }
        Err(e) => println!("❌ Failed to find target node: {}", e),
    }
}

/// Example 3: Manual stream lifecycle management
fn example_manual_lifecycle() {
    let mut capture = PipeWireApplicationCapture::new(ApplicationSelector::NodeId(62)); // VLC

    println!("🔍 Stream ready: {}", capture.is_stream_ready());
    println!("🔍 Application selector: {:?}", capture.get_application_selector());

    // Step 1: Discover target
    match capture.discover_target_node() {
        Ok(node_id) => {
            println!("✅ Discovered node: {} (serial: {:?})",
                     capture.get_node_id().unwrap(),
                     capture.get_discovered_node_serial());
        }
        Err(e) => {
            println!("❌ Discovery failed: {}", e);
            return;
        }
    }

    // Step 2: Create stream
    if let Err(e) = capture.create_monitor_stream() {
        println!("❌ Stream creation failed: {}", e);
        return;
    }

    println!("🔍 Stream ready: {}", capture.is_stream_ready());

    // Step 3: Start capture
    let sample_count = Arc::new(AtomicUsize::new(0));
    let sample_count_clone = sample_count.clone();

    if let Err(e) = capture.start_capture(move |samples| {
        sample_count_clone.fetch_add(samples.len(), Ordering::SeqCst);
    }) {
        println!("❌ Start capture failed: {}", e);
        return;
    }

    println!("🔍 Is capturing: {}", capture.is_capturing());

    // Step 4: Run main loop with verbose output
    if let Err(e) = capture.run_main_loop_with_options(Some(Duration::from_secs(2)), true) {
        println!("❌ Main loop failed: {}", e);
        return;
    }

    // Step 5: Stop capture
    capture.stop_capture().unwrap();
    
    println!("🔍 Is capturing: {}", capture.is_capturing());
    println!("✅ Manual lifecycle completed: {} samples", sample_count.load(Ordering::SeqCst));
}

/// Example 4: Multiple capture sessions
fn example_multiple_sessions() {
    let mut capture = PipeWireApplicationCapture::new(ApplicationSelector::NodeId(62)); // VLC

    // Discover once, use multiple times
    if capture.discover_target_node().is_err() {
        println!("❌ Failed to discover target");
        return;
    }

    if capture.create_monitor_stream().is_err() {
        println!("❌ Failed to create stream");
        return;
    }

    // Session 1: Short capture
    println!("📡 Session 1: 1 second capture");
    let count1 = Arc::new(AtomicUsize::new(0));
    let count1_clone = count1.clone();
    
    let _ = capture.start_capture_for_duration(
        move |samples| { count1_clone.fetch_add(samples.len(), Ordering::SeqCst); },
        Duration::from_secs(1)
    );
    
    println!("✅ Session 1: {} samples", count1.load(Ordering::SeqCst));

    // Session 2: Another short capture
    println!("📡 Session 2: 1 second capture");
    let count2 = Arc::new(AtomicUsize::new(0));
    let count2_clone = count2.clone();
    
    let _ = capture.start_capture_for_duration(
        move |samples| { count2_clone.fetch_add(samples.len(), Ordering::SeqCst); },
        Duration::from_secs(1)
    );
    
    println!("✅ Session 2: {} samples", count2.load(Ordering::SeqCst));
    println!("✅ Multiple sessions completed!");
}
