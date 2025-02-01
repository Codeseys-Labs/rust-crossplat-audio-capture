use rsac::{AudioConfig, AudioFormat};

#[test]
fn test_config_defaults() {
    let config = AudioConfig::default();

    assert_eq!(config.sample_rate, 48000);
    assert_eq!(config.channels, 2);
    assert_eq!(config.format, AudioFormat::F32LE);
}

#[test]
fn test_config_custom() {
    let config = AudioConfig {
        sample_rate: 44100,
        channels: 1,
        format: AudioFormat::S16LE,
    };

    assert_eq!(config.sample_rate, 44100);
    assert_eq!(config.channels, 1);
    assert_eq!(config.format, AudioFormat::S16LE);
}

#[test]
fn test_config_clone() {
    let original = AudioConfig {
        sample_rate: 44100,
        channels: 2,
        format: AudioFormat::S32LE,
    };

    let cloned = original.clone();

    assert_eq!(original.sample_rate, cloned.sample_rate);
    assert_eq!(original.channels, cloned.channels);
    assert_eq!(original.format, cloned.format);
}

#[test]
fn test_config_debug() {
    let config = AudioConfig {
        sample_rate: 44100,
        channels: 2,
        format: AudioFormat::F32LE,
    };

    let debug_str = format!("{:?}", config);
    assert!(debug_str.contains("44100"));
    assert!(debug_str.contains("2"));
    assert!(debug_str.contains("F32LE"));
}
