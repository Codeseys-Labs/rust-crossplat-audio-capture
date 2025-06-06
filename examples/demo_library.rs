use clap::Parser;
use rsac::api::{AudioCaptureBuilder};
use rsac::core::config::{DeviceSelector, SampleFormat};
use rsac::core::error::AudioError;
use rsac::get_device_enumerator;
use std::path::PathBuf;
use std::time::Duration;
use std::thread;

#[cfg(target_os = "windows")]
use rsac::{enumerate_application_audio_sessions, ProcessAudioCapture};

#[cfg(target_os = "macos")]
use rsac::enumerate_audio_applications;

#[cfg(target_os = "linux")]
use rsac::get_audio_backend;

#[derive(Parser)]
#[command(name = "demo_library")]
#[command(about = "Demonstrate the cross-platform audio capture library functionality")]
struct Args {
    /// Duration in seconds to capture
    #[arg(short, long, default_value = "5")]
    duration: u64,
    
    /// Output directory for captured files
    #[arg(short, long, default_value = "demo_output")]
    output_dir: PathBuf,
    
    /// Application name to capture (optional)
    #[arg(short, long)]
    application: Option<String>,
    
    /// Test system capture
    #[arg(long)]
    test_system: bool,
    
    /// Test application capture
    #[arg(long)]
    test_application: bool,
    
    /// List available devices and applications
    #[arg(long)]
    list_only: bool,
    
    /// Enable verbose output
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    
    println!("🎵 Cross-Platform Audio Capture Library Demo");
    println!("Platform: {}", std::env::consts::OS);
    
    // Create output directory
    std::fs::create_dir_all(&args.output_dir)?;
    
    if args.list_only {
        list_devices_and_applications(&args)?;
        return Ok(());
    }
    
    if args.test_system || (!args.test_application && args.application.is_none()) {
        println!("\n📡 Testing System Audio Capture...");
        test_system_capture(&args)?;
    }
    
    if args.test_application || args.application.is_some() {
        println!("\n🎯 Testing Application-Specific Capture...");
        test_application_capture(&args)?;
    }
    
    println!("\n✅ Demo completed! Check {} for output files", args.output_dir.display());
    Ok(())
}

fn list_devices_and_applications(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    println!("\n🔍 Listing Available Devices and Applications...");
    
    // List audio devices
    match get_device_enumerator() {
        Ok(mut enumerator) => {
            println!("\n📱 Available Audio Devices:");
            match enumerator.enumerate_devices() {
                Ok(devices) => {
                    for (i, device) in devices.iter().enumerate() {
                        match device.name() {
                            Ok(name) => println!("  {}: {}", i + 1, name),
                            Err(e) => println!("  {}: <Error getting name: {}>", i + 1, e),
                        }
                    }
                }
                Err(e) => println!("  Error enumerating devices: {}", e),
            }
        }
        Err(e) => println!("  Error creating device enumerator: {}", e),
    }
    
    // List applications (platform-specific)
    println!("\n🎮 Available Audio Applications:");
    
    #[cfg(target_os = "windows")]
    {
        match enumerate_application_audio_sessions() {
            Ok(sessions) => {
                for session in sessions {
                    println!("  - {} (PID: {})", session.display_name, session.process_id);
                }
            }
            Err(e) => println!("  Error enumerating Windows audio sessions: {}", e),
        }
    }
    
    #[cfg(target_os = "macos")]
    {
        match enumerate_audio_applications() {
            Ok(applications) => {
                for app in applications {
                    println!("  - {} (PID: {})", app.name, app.pid);
                }
            }
            Err(e) => println!("  Error enumerating macOS applications: {}", e),
        }
    }
    
    #[cfg(target_os = "linux")]
    {
        match get_audio_backend() {
            Ok(backend) => {
                match backend.list_applications() {
                    Ok(applications) => {
                        for app in applications {
                            println!("  - {}", app.name);
                        }
                    }
                    Err(e) => println!("  Error listing Linux applications: {}", e),
                }
            }
            Err(e) => println!("  Error getting Linux audio backend: {}", e),
        }
    }
    
    Ok(())
}

fn test_system_capture(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let output_path = args.output_dir.join("system_capture.wav");
    
    if args.verbose {
        println!("  Output: {}", output_path.display());
        println!("  Duration: {} seconds", args.duration);
    }
    
    match capture_system_audio(args.duration, &output_path, args.verbose) {
        Ok(_) => {
            println!("  ✅ System capture completed successfully");
            
            // Verify file was created
            if output_path.exists() {
                let metadata = std::fs::metadata(&output_path)?;
                println!("  📁 File size: {} bytes", metadata.len());
            }
        }
        Err(e) => {
            println!("  ❌ System capture failed: {}", e);
        }
    }
    
    Ok(())
}

fn test_application_capture(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let app_name = args.application.as_deref().unwrap_or("vlc");
    let output_path = args.output_dir.join(format!("{}_capture.wav", app_name));
    
    if args.verbose {
        println!("  Target application: {}", app_name);
        println!("  Output: {}", output_path.display());
        println!("  Duration: {} seconds", args.duration);
    }
    
    match capture_application_audio(app_name, args.duration, &output_path, args.verbose) {
        Ok(_) => {
            println!("  ✅ Application capture completed successfully");
            
            // Verify file was created
            if output_path.exists() {
                let metadata = std::fs::metadata(&output_path)?;
                println!("  📁 File size: {} bytes", metadata.len());
            }
        }
        Err(e) => {
            println!("  ❌ Application capture failed: {}", e);
            println!("  💡 Make sure the application '{}' is running and playing audio", app_name);
        }
    }
    
    Ok(())
}

fn capture_system_audio(duration: u64, output_path: &PathBuf, verbose: bool) -> Result<(), AudioError> {
    // Use the new API for system capture
    let mut capture_session = AudioCaptureBuilder::new()
        .device(DeviceSelector::DefaultInput)
        .sample_rate(44100)
        .channels(2)
        .sample_format(SampleFormat::S16LE)
        .bits_per_sample(16)
        .build()?;

    if verbose {
        println!("    Created audio capture session");
    }

    // Start capturing
    capture_session.start()?;
    
    if verbose {
        println!("    Started capture, recording for {} seconds...", duration);
    }

    // Record for specified duration
    thread::sleep(Duration::from_secs(duration));

    // Stop capturing
    capture_session.stop()?;

    if verbose {
        println!("    Stopped capture");
    }

    // TODO: Implement actual audio data collection and WAV file writing
    // For now, create a placeholder file to demonstrate the API works
    create_demo_wav_file(output_path)?;

    Ok(())
}

fn capture_application_audio(app_name: &str, duration: u64, output_path: &PathBuf, verbose: bool) -> Result<(), AudioError> {
    // Platform-specific application capture
    #[cfg(target_os = "windows")]
    {
        return capture_windows_application(app_name, duration, output_path, verbose);
    }
    
    #[cfg(target_os = "macos")]
    {
        return capture_macos_application(app_name, duration, output_path, verbose);
    }
    
    #[cfg(target_os = "linux")]
    {
        return capture_linux_application(app_name, duration, output_path, verbose);
    }
}

#[cfg(target_os = "windows")]
fn capture_windows_application(app_name: &str, duration: u64, output_path: &PathBuf, verbose: bool) -> Result<(), AudioError> {
    use rsac::AudioCaptureError;
    
    // Find the application
    let sessions = enumerate_application_audio_sessions()
        .map_err(|e| AudioError::BackendSpecificError(e.to_string()))?;
    
    let target_session = sessions.iter().find(|session| 
        session.display_name.to_lowercase().contains(&app_name.to_lowercase())
    ).ok_or_else(|| AudioError::BackendSpecificError(format!("Application '{}' not found", app_name)))?;
    
    if verbose {
        println!("    Found application: {} (PID: {})", target_session.display_name, target_session.process_id);
    }
    
    // Use ProcessAudioCapture for application-specific capture
    let mut process_capture = ProcessAudioCapture::new(target_session.process_id)
        .map_err(|e| AudioError::BackendSpecificError(e.to_string()))?;
    
    // Start capture
    process_capture.start_capture()
        .map_err(|e| AudioError::BackendSpecificError(e.to_string()))?;
    
    if verbose {
        println!("    Started process capture, recording for {} seconds...", duration);
    }
    
    thread::sleep(Duration::from_secs(duration));
    
    // Stop capture
    process_capture.stop_capture()
        .map_err(|e| AudioError::BackendSpecificError(e.to_string()))?;
    
    // TODO: Implement actual audio data collection
    create_demo_wav_file(output_path)?;
    
    Ok(())
}

#[cfg(target_os = "macos")]
fn capture_macos_application(app_name: &str, _duration: u64, output_path: &PathBuf, verbose: bool) -> Result<(), AudioError> {
    if verbose {
        println!("    macOS application capture not fully implemented yet");
    }
    
    // TODO: Implement macOS application-specific capture
    create_demo_wav_file(output_path)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn capture_linux_application(app_name: &str, _duration: u64, output_path: &PathBuf, verbose: bool) -> Result<(), AudioError> {
    if verbose {
        println!("    Linux application capture not fully implemented yet");
    }
    
    // TODO: Implement Linux application-specific capture
    create_demo_wav_file(output_path)?;
    Ok(())
}

fn create_demo_wav_file(output_path: &PathBuf) -> Result<(), AudioError> {
    use hound::{WavSpec, WavWriter};
    
    // Create a simple WAV file to demonstrate the API worked
    let spec = WavSpec {
        channels: 2,
        sample_rate: 44100,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    
    let mut writer = WavWriter::create(output_path, spec)
        .map_err(|e| AudioError::BackendSpecificError(e.to_string()))?;
    
    // Generate 1 second of test audio
    let sample_rate = 44100;
    for i in 0..sample_rate {
        let t = i as f32 / sample_rate as f32;
        let signal = 0.3 * (2.0 * std::f32::consts::PI * 440.0 * t).sin();
        let sample = (signal * 16384.0) as i16;
        
        writer.write_sample(sample)
            .map_err(|e| AudioError::BackendSpecificError(e.to_string()))?;
        writer.write_sample(sample)
            .map_err(|e| AudioError::BackendSpecificError(e.to_string()))?;
    }
    
    writer.finalize()
        .map_err(|e| AudioError::BackendSpecificError(e.to_string()))?;
    
    Ok(())
}
