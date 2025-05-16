# macOS Application-Level Audio Capture

This document describes how to use the `rust-crossplat-audio-capture` library to capture audio from specific applications on macOS. This feature leverages Core Audio Taps.

## Overview

Application-level audio capture on macOS allows you to record or process the audio output of a single, targeted application, rather than the entire system audio mix. This is achieved by creating a "tap" on the audio stream of the target application process.

## Prerequisites

To use application-level audio capture on macOS, the following conditions must be met:

1.  **Operating System:** macOS 14.4 or newer is required. Earlier versions of macOS do not support the necessary Core Audio features for reliable application-level tapping by third-party applications.
2.  **Info.plist Configuration:** The application that _uses this library_ (i.e., your application) **must** include the `NSAudioCaptureUsageDescription` key in its `Info.plist` file. This key provides a string that is displayed to the user when the system requests permission for audio capture.

    Example `Info.plist` snippet:

    ```xml
    <?xml version="1.0" encoding="UTF-8"?>
    <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
    <plist version="1.0">
    <dict>
        <!-- ... other keys ... -->
        <key>NSAudioCaptureUsageDescription</key>
        <string>This application needs to capture audio from other applications to [explain your feature, e.g., 'record its sound output', 'visualize its audio', 'provide audio analysis'].</string>
        <!-- ... other keys ... -->
    </dict>
    </plist>
    ```

    Failure to include this key will result in permission errors or silent failure when attempting to capture audio.

## Usage Workflow

The typical workflow for capturing audio from a specific application is as follows:

1.  **Enumerate Running Applications:**
    Call [`rust_crossplat_audio_capture::audio::macos::enumerate_audio_applications()`](../src/audio/macos.rs) to get a list of currently running applications. This function returns a `Vec<ApplicationInfo>`, where each [`ApplicationInfo`](../src/audio/macos.rs) struct contains the process ID (PID), name, and bundle ID of an application.

    ```rust
    use rust_crossplat_audio_capture::audio::macos::enumerate_audio_applications;

    match enumerate_audio_applications() {
        Ok(apps) => {
            if apps.is_empty() {
                println!("No running applications found.");
            } else {
                println!("Available applications:");
                for (i, app_info) in apps.iter().enumerate() {
                    println!(
                        "{}: PID: {}, Name: {}, Bundle ID: {:?}",
                        i, app_info.process_id, app_info.name, app_info.bundle_id
                    );
                }
                // Store 'apps' for selection
            }
        }
        Err(e) => {
            eprintln!("Error enumerating applications: {:?}", e);
        }
    }
    ```

2.  **Select Target Application:**
    Present the list of applications to the user (or use other logic) to select a target application. Obtain the `process_id` (PID) of the chosen application.

3.  **Build AudioCapture Instance:**
    Use [`AudioCaptureBuilder`](../src/api.rs) to configure and build an [`AudioCapture`](../src/api.rs) instance. Crucially, call the [`target_application_pid()`](../src/api.rs) method with the PID of the selected application.

    ```rust
    use rust_crossplat_audio_capture::api::AudioCaptureBuilder;
    use rust_crossplat_audio_capture::audio::{StreamConfig, AudioFormat, SampleFormat, ChannelCount};

    let target_pid = /* PID of the selected application */;
    let stream_config = StreamConfig {
        format: AudioFormat {
            sample_format: SampleFormat::F32LE, // CoreAudio taps typically provide F32 non-interleaved
            sample_rate: 48000, // Or match the tap's native sample rate if known
            channels: ChannelCount::Stereo, // Or match the tap's native channel count
        },
        buffer_size_frames: None, // Use default
    };

    let mut capturer = match AudioCaptureBuilder::new()
        .target_application_pid(target_pid)
        .stream_config(stream_config)
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to build application audio capturer: {:?}", e);
            return; // Or handle error
        }
    };
    ```

4.  **Start and Manage Stream:**
    Once the `AudioCapture` instance is built, you can start, read/stream data, and stop the stream using its methods (`start_capture()`, `read_chunk()`, `stop_capture()`, etc.) as you would for regular system audio capture.

    ```rust
    # use rust_crossplat_audio_capture::api::AudioCapture;
    # use rust_crossplat_audio_capture::core::AudioBuffer;
    # fn do_capture(mut capturer: AudioCapture<impl rust_crossplat_audio_capture::audio::AudioDevice + 'static>) -> anyhow::Result<()> {
    capturer.start_capture()?;
    println!("Capturing audio from target application...");

    // Example: Capture for a few seconds
    let start_time = std::time::Instant::now();
    while start_time.elapsed().as_secs() < 5 {
        match capturer.read_chunk(Some(100)) { // Non-blocking read with timeout
            Ok(Some(buffer)) => {
                println!("Read chunk: {} frames", buffer.num_frames());
                // Process audio data in buffer.as_slice()
            }
            Ok(None) => {
                // No data available, timeout occurred
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(e) => {
                eprintln!("Error reading chunk: {:?}", e);
                break;
            }
        }
    }
    capturer.stop_capture()?;
    println!("Capture stopped.");
    # Ok(())
    # }
    ```

## Limitations

- **macOS Version:** This feature is strictly limited to macOS 14.4 and newer.
- **PID Stability:** Targeting is based on Process IDs (PIDs). If a target application restarts, its PID will change, and the existing capture stream will likely fail or stop producing data. The user may need to re-select the application.
- **Enumeration vs. Audio Activity:** [`enumerate_audio_applications()`](../src/audio/macos.rs) lists all _running_ applications, not necessarily only those currently playing or producing audio. The tap will attach regardless, but might not yield data if the application is silent.
- **Permissions:** The user must grant screen and audio recording permission to your application when prompted by macOS. If permission is denied, capture will fail.

## Troubleshooting

- **Permission Errors:**
  - Ensure the `NSAudioCaptureUsageDescription` key is correctly set in your application's `Info.plist`.
  - Guide the user to grant permission in System Settings > Privacy & Security > Screen Recording (and potentially Microphone, though the tap is for application output). The system prompt for application audio capture usually covers this.
- **Target Application Quits:** If the targeted application quits or crashes, the audio stream will likely stop producing data. Calls to `read_chunk()` may return errors (e.g., `AudioError::DeviceDisconnected` or a backend-specific error indicating the tap is no longer valid). Your application should handle this gracefully, possibly by notifying the user and allowing them to select a new target.
- **No Audio Data:**
  - Verify the target application is actually producing audio.
  - Check for any errors returned by `build()` or `start_capture()`.
  - Ensure the correct PID was used.

## Best Practices

- **Re-enumerate on Failure:** If a capture stream for a previously working PID fails (e.g., due to `AudioError::DeviceDisconnected`), consider prompting the user to re-select the application. Call [`enumerate_audio_applications()`](../src/audio/macos.rs) again to get an updated list, as the application might have restarted with a new PID.
- **User Feedback:** Provide clear feedback to the user about which application is being targeted and the status of the capture.
- **Error Handling:** Robustly handle potential errors from all library calls, especially those related to device interaction and stream management.
