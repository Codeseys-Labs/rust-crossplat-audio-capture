use audio_capture::{AudioCaptureConfig, AudioDevice, DeviceType, AudioBackend};

#[test]
fn test_config_builder_defaults() {
    let config = AudioCaptureConfig::new();
    
    assert_eq!(config.sample_rate(), 48000);
    assert_eq!(config.channels(), 2);
    assert_eq!(config.buffer_size(), 1024);
    assert!(config.device().is_none());
    assert!(config.app_name().is_none());
}

#[test]
fn test_config_builder_customization() {
    let device = AudioDevice {
        id: "test".to_string(),
        name: "Test Device".to_string(),
        device_type: DeviceType::Input,
        channels: 1,
        sample_rate: 44100,
        backend: AudioBackend::Wasapi,
    };
    
    let config = AudioCaptureConfig::new()
        .device(device.clone())
        .sample_rate(44100)
        .channels(1)
        .buffer_size(2048)
        .app_name("TestApp".to_string());
    
    assert_eq!(config.sample_rate(), 44100);
    assert_eq!(config.channels(), 1);
    assert_eq!(config.buffer_size(), 2048);
    assert_eq!(config.device().unwrap().id, "test");
    assert_eq!(config.app_name().unwrap(), "TestApp");
}

#[test]
fn test_config_validation() {
    let config = AudioCaptureConfig::new()
        .sample_rate(192000)
        .channels(8);
    
    assert!(config.validate().is_err());
}

#[test]
fn test_config_clone() {
    let original = AudioCaptureConfig::new()
        .sample_rate(44100)
        .channels(2);
    
    let cloned = original.clone();
    
    assert_eq!(original.sample_rate(), cloned.sample_rate());
    assert_eq!(original.channels(), cloned.channels());
}

#[test]
fn test_config_debug_format() {
    let config = AudioCaptureConfig::new()
        .sample_rate(44100)
        .channels(2);
    
    let debug_str = format!("{:?}", config);
    assert!(debug_str.contains("44100"));
    assert!(debug_str.contains("2"));
}