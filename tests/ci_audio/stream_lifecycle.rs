//! Stream lifecycle integration tests.
//!
//! These tests verify that streams can be started, stopped, and dropped
//! without panics or resource leaks.

use rsac::{AudioCaptureBuilder, CaptureTarget};

#[test]
fn test_stream_start_read_stop() {
    require_system_capture!();

    let expected_sample_rate: u32 = 48000;
    let expected_channels: u16 = 2;

    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(expected_sample_rate)
        .channels(expected_channels)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ci_audio] Failed to build capture: {:?}", e);
            eprintln!("[ci_audio] SKIPPING: Build failed");
            return;
        }
    };

    // Start
    if let Err(e) = capture.start() {
        eprintln!("[ci_audio] Failed to start: {:?}", e);
        eprintln!("[ci_audio] SKIPPING: Start failed");
        return;
    }
    assert!(capture.is_running(), "Should be running after start");

    // Read at least one buffer (or timeout trying)
    let start = std::time::Instant::now();
    let timeout = std::time::Duration::from_secs(5);
    let mut read_count = 0;

    while start.elapsed() < timeout && read_count < 3 {
        match capture.read_buffer() {
            Ok(Some(buf)) => {
                read_count += 1;
                eprintln!(
                    "[ci_audio] Read buffer {}: {} frames (rate={}, channels={})",
                    read_count,
                    buf.num_frames(),
                    buf.sample_rate(),
                    buf.channels()
                );

                // Property assertions layered on top of the no-panic backbone.
                // Goal: catch silent-wrong-output regressions (bogus-but-well-
                // formed buffers) without breaking CI on heterogeneous audio
                // hardware — we only assert if a buffer actually came back.
                assert_eq!(
                    buf.sample_rate(),
                    expected_sample_rate,
                    "Buffer sample_rate must match the value configured on the builder"
                );
                assert_eq!(
                    buf.channels(),
                    expected_channels,
                    "Buffer channels must match the value configured on the builder"
                );
                assert_eq!(
                    buf.num_frames() * buf.channels() as usize,
                    buf.data().len(),
                    "Interleaved data length must equal num_frames * channels \
                     (rate={}, channels={}, frames={}, data.len={})",
                    buf.sample_rate(),
                    buf.channels(),
                    buf.num_frames(),
                    buf.data().len()
                );
            }
            Ok(None) => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => {
                eprintln!("[ci_audio] Read error: {:?}", e);
                if e.is_fatal() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }

    // Stop — important thing is no panic/crash on teardown.
    capture.stop().expect("Stop should succeed");
    assert!(!capture.is_running(), "Should not be running after stop");

    eprintln!(
        "[ci_audio] Lifecycle test passed: read {} buffers",
        read_count
    );
}

#[test]
fn test_stream_stop_idempotent() {
    require_system_capture!();

    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ci_audio] Failed to build capture: {:?}", e);
            eprintln!("[ci_audio] SKIPPING: Build failed");
            return;
        }
    };

    if let Err(e) = capture.start() {
        eprintln!("[ci_audio] Failed to start: {:?}", e);
        eprintln!("[ci_audio] SKIPPING: Start failed");
        return;
    }

    // Brief capture
    std::thread::sleep(std::time::Duration::from_millis(200));

    // First stop — should succeed
    let result1 = capture.stop();
    eprintln!("[ci_audio] First stop result: {:?}", result1);
    assert!(result1.is_ok(), "First stop should succeed");

    // Second stop — should not panic, may succeed or return error
    let result2 = capture.stop();
    eprintln!("[ci_audio] Second stop result: {:?}", result2);
    // We don't assert success on the second stop — the important thing
    // is that it doesn't panic or crash

    eprintln!("[ci_audio] Idempotent stop test passed");
}

#[test]
fn test_drop_while_running() {
    require_system_capture!();

    // This test verifies that dropping an AudioCapture while it's still
    // running doesn't panic, leak resources, or hang.

    let mut capture = match AudioCaptureBuilder::new()
        .with_target(CaptureTarget::SystemDefault)
        .sample_rate(48000)
        .channels(2)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ci_audio] Failed to build capture: {:?}", e);
            eprintln!("[ci_audio] SKIPPING: Build failed");
            return;
        }
    };

    if let Err(e) = capture.start() {
        eprintln!("[ci_audio] Failed to start: {:?}", e);
        eprintln!("[ci_audio] SKIPPING: Start failed");
        return;
    }

    assert!(capture.is_running(), "Should be running");

    // Brief capture to ensure stream is active
    std::thread::sleep(std::time::Duration::from_millis(200));

    // Drop without calling stop — the Drop impl should handle cleanup
    eprintln!("[ci_audio] Dropping capture while running...");
    drop(capture);

    // If we reach here, the drop didn't panic or hang
    eprintln!("[ci_audio] Drop-while-running test passed — no panic or hang");
}
