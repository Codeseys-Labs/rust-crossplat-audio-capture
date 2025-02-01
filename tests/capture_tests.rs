mod common;

use std::time::Duration;
use audio_capture::{AudioCapture, AudioCaptureConfig, AudioError};
use common::{MockAudioCapture, create_test_signal, verify_audio_similarity};

#[test]
fn test_basic_capture_lifecycle() {
    let mut capture = MockAudioCapture::new();
    
    assert!(!capture.is_capturing());
    
    capture.start().expect("Failed to start capture");
    assert!(capture.is_capturing());
    
    capture.stop().expect("Failed to stop capture");
    assert!(!capture.is_capturing());
}

#[test]
fn test_capture_with_custom_config() {
    let config = AudioCaptureConfig::new()
        .sample_rate(44100)
        .channels(1);
    
    let test_data = create_test_signal(440.0, 1000, 44100);
    let mut capture = MockAudioCapture::with_test_data(test_data.clone());
    
    capture.start().expect("Failed to start capture");
    
    // Simulate capture for 1 second
    std::thread::sleep(Duration::from_secs(1));
    
    let captured_data = capture.get_captured_data().expect("Failed to get captured data");
    assert!(verify_audio_similarity(&test_data, &captured_data, 0.001));
    
    capture.stop().expect("Failed to stop capture");
}

#[test]
fn test_device_enumeration() {
    let capture = MockAudioCapture::new();
    let devices = capture.get_available_devices().expect("Failed to get devices");
    
    assert!(!devices.is_empty());
    assert_eq!(devices[0].name, "Test Device 1");
    assert_eq!(devices[0].channels, 2);
    assert_eq!(devices[0].sample_rate, 48000);
}

#[test]
fn test_device_selection() {
    let mut capture = MockAudioCapture::new();
    let devices = capture.get_available_devices().expect("Failed to get devices");
    
    capture.set_device(&devices[1]).expect("Failed to set device");
    let current_device = capture.get_current_device().expect("Failed to get current device");
    
    assert_eq!(current_device.id, "device2");
    assert_eq!(current_device.sample_rate, 44100);
}

#[test]
fn test_invalid_device_selection() {
    let mut capture = MockAudioCapture::new();
    let invalid_device = MockAudioDevice {
        id: "invalid".to_string(),
        name: "Invalid Device".to_string(),
        channels: 2,
        sample_rate: 48000,
    };
    
    let result = capture.set_device(&invalid_device);
    assert!(matches!(result, Err(AudioError::DeviceNotFound(_))));
}

#[test]
fn test_capture_error_handling() {
    let mut capture = MockAudioCapture::new();
    
    // Try to stop before starting
    let result = capture.stop();
    assert!(matches!(result, Err(AudioError::CaptureError(_))));
    
    // Try to start twice
    capture.start().expect("Failed to start capture");
    let result = capture.start();
    assert!(matches!(result, Err(AudioError::CaptureError(_))));
}

#[test]
fn test_capture_with_zero_data() {
    let test_data = vec![0.0f32; 48000];
    let mut capture = MockAudioCapture::with_test_data(test_data);
    
    capture.start().expect("Failed to start capture");
    std::thread::sleep(Duration::from_secs(1));
    
    let result = capture.get_captured_data();
    assert!(matches!(result, Err(AudioError::CaptureError(_))));
    
    capture.stop().expect("Failed to stop capture");
}