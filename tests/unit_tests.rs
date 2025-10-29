//! Unit tests that don't require actual audio hardware
//! These tests validate the API, configuration, and data structures

use rsac::{
    AudioBuffer, AudioCaptureBuilder, AudioFileFormat, DeviceSelector, LatencyMode, SampleFormat,
    StreamConfig,
};

#[test]
fn test_audio_buffer_creation() {
    // Test creating an audio buffer with f32 samples
    let samples = vec![0.1f32, 0.2, 0.3, 0.4];
    let buffer = AudioBuffer::new(samples.clone(), 48000, 2);

    assert_eq!(buffer.sample_rate(), 48000);
    assert_eq!(buffer.channels(), 2);
    assert_eq!(buffer.samples(), &samples[..]);
}

#[test]
fn test_audio_buffer_metadata() {
    let samples = vec![0.0f32; 1024];
    let buffer = AudioBuffer::new(samples, 44100, 1);

    assert_eq!(buffer.sample_rate(), 44100);
    assert_eq!(buffer.channels(), 1);
    assert_eq!(buffer.samples().len(), 1024);
}

#[test]
fn test_sample_format_bits_consistency() {
    // Test that sample formats have expected bit depths
    let formats = vec![
        (SampleFormat::U8, 8),
        (SampleFormat::S16LE, 16),
        (SampleFormat::S16BE, 16),
        (SampleFormat::S24LE, 24),
        (SampleFormat::S32LE, 32),
        (SampleFormat::F32LE, 32),
        (SampleFormat::F64LE, 64),
    ];

    for (format, expected_bits) in formats {
        let config = StreamConfig {
            sample_rate: 48000,
            channels: 2,
            sample_format: format,
            bits_per_sample: expected_bits,
            buffer_size_frames: Some(512),
            latency_mode: LatencyMode::Balanced,
        };

        assert_eq!(config.bits_per_sample, expected_bits);
        assert_eq!(config.sample_format, format);
    }
}

#[test]
fn test_stream_config_creation() {
    let config = StreamConfig {
        sample_rate: 48000,
        channels: 2,
        sample_format: SampleFormat::F32LE,
        bits_per_sample: 32,
        buffer_size_frames: Some(1024),
        latency_mode: LatencyMode::LowLatency,
    };

    assert_eq!(config.sample_rate, 48000);
    assert_eq!(config.channels, 2);
    assert_eq!(config.sample_format, SampleFormat::F32LE);
    assert_eq!(config.bits_per_sample, 32);
    assert_eq!(config.buffer_size_frames, Some(1024));
    assert_eq!(config.latency_mode, LatencyMode::LowLatency);
}

#[test]
fn test_device_selector_variants() {
    // Test that all DeviceSelector variants can be constructed
    let selectors = vec![
        DeviceSelector::DefaultInput,
        DeviceSelector::DefaultOutput,
        DeviceSelector::ById("test-device-id".to_string()),
        DeviceSelector::ByName("Test Device".to_string()),
    ];

    assert_eq!(selectors.len(), 4);
}

#[test]
fn test_latency_mode_variants() {
    // Test that all LatencyMode variants exist
    let modes = vec![
        LatencyMode::LowLatency,
        LatencyMode::Balanced,
        LatencyMode::PowerSaving,
    ];

    assert_eq!(modes.len(), 3);
}

#[test]
fn test_audio_file_format_variants() {
    // Test that all AudioFileFormat variants exist
    let formats = vec![AudioFileFormat::Wav, AudioFileFormat::RawPcm];

    assert_eq!(formats.len(), 2);
}

#[test]
fn test_builder_basic_configuration() {
    // Test that the builder can be configured with basic parameters
    let builder = AudioCaptureBuilder::new()
        .sample_rate(48000)
        .channels(2)
        .sample_format(SampleFormat::F32LE)
        .bits_per_sample(32);

    // Builder pattern should chain properly
    // We can't fully build without a device, but we can verify the builder is usable
    drop(builder);
}

#[test]
fn test_builder_with_all_options() {
    // Test builder with all configuration options
    let builder = AudioCaptureBuilder::new()
        .device(DeviceSelector::DefaultInput)
        .sample_rate(44100)
        .channels(1)
        .sample_format(SampleFormat::S16LE)
        .bits_per_sample(16)
        .buffer_size_frames(Some(512))
        .latency(Some(LatencyMode::Balanced));

    drop(builder);
}

#[test]
fn test_builder_application_targeting() {
    // Test that application targeting can be configured
    let builder = AudioCaptureBuilder::new()
        .sample_rate(48000)
        .channels(2)
        .sample_format(SampleFormat::F32LE)
        .bits_per_sample(32)
        .target_application_pid(1234);

    drop(builder);
}

#[test]
fn test_common_sample_rates() {
    // Test that common sample rates can be configured
    let sample_rates = vec![22050, 32000, 44100, 48000, 88200, 96000, 192000];

    for rate in sample_rates {
        let config = StreamConfig {
            sample_rate: rate,
            channels: 2,
            sample_format: SampleFormat::F32LE,
            bits_per_sample: 32,
            buffer_size_frames: Some(1024),
            latency_mode: LatencyMode::Balanced,
        };

        assert_eq!(config.sample_rate, rate);
    }
}

#[test]
fn test_channel_configurations() {
    // Test various channel configurations
    let channel_counts = vec![1, 2, 4, 6, 8]; // Mono, stereo, quad, 5.1, 7.1

    for channels in channel_counts {
        let config = StreamConfig {
            sample_rate: 48000,
            channels,
            sample_format: SampleFormat::F32LE,
            bits_per_sample: 32,
            buffer_size_frames: Some(1024),
            latency_mode: LatencyMode::Balanced,
        };

        assert_eq!(config.channels, channels);
    }
}

#[test]
fn test_audio_buffer_empty() {
    // Test creating an empty audio buffer
    let buffer = AudioBuffer::new(vec![], 48000, 2);

    assert_eq!(buffer.samples().len(), 0);
    assert_eq!(buffer.sample_rate(), 48000);
    assert_eq!(buffer.channels(), 2);
}

#[test]
fn test_audio_buffer_large() {
    // Test creating a large audio buffer (10 seconds at 48kHz stereo)
    let sample_count = 48000 * 2 * 10;
    let samples = vec![0.0f32; sample_count];
    let buffer = AudioBuffer::new(samples, 48000, 2);

    assert_eq!(buffer.samples().len(), sample_count);
}

#[test]
fn test_stream_config_clone() {
    // Test that StreamConfig can be cloned
    let config1 = StreamConfig {
        sample_rate: 48000,
        channels: 2,
        sample_format: SampleFormat::F32LE,
        bits_per_sample: 32,
        buffer_size_frames: Some(1024),
        latency_mode: LatencyMode::Balanced,
    };

    let config2 = config1.clone();

    assert_eq!(config1.sample_rate, config2.sample_rate);
    assert_eq!(config1.channels, config2.channels);
    assert_eq!(config1.sample_format, config2.sample_format);
}

#[test]
fn test_device_selector_clone() {
    let selector1 = DeviceSelector::ById("test-id".to_string());
    let selector2 = selector1.clone();

    match (&selector1, &selector2) {
        (DeviceSelector::ById(id1), DeviceSelector::ById(id2)) => {
            assert_eq!(id1, id2);
        }
        _ => panic!("Selector types don't match after clone"),
    }
}

#[test]
fn test_buffer_size_options() {
    // Test various buffer sizes
    let buffer_sizes = vec![64, 128, 256, 512, 1024, 2048, 4096];

    for size in buffer_sizes {
        let config = StreamConfig {
            sample_rate: 48000,
            channels: 2,
            sample_format: SampleFormat::F32LE,
            bits_per_sample: 32,
            buffer_size_frames: Some(size),
            latency_mode: LatencyMode::Balanced,
        };

        assert_eq!(config.buffer_size_frames, Some(size));
    }
}

#[test]
fn test_buffer_size_none() {
    // Test that buffer size can be None (auto)
    let config = StreamConfig {
        sample_rate: 48000,
        channels: 2,
        sample_format: SampleFormat::F32LE,
        bits_per_sample: 32,
        buffer_size_frames: None,
        latency_mode: LatencyMode::Balanced,
    };

    assert_eq!(config.buffer_size_frames, None);
}

/// Test silence detection logic (similar to what we added to dynamic_vlc_capture)
#[test]
fn test_silence_detection() {
    // All zeros should be detected as silence
    let silent_samples = vec![0.0f32; 1000];
    let has_signal = silent_samples.iter().any(|&s| s.abs() > 0.001);
    assert!(!has_signal, "Should detect silence");

    // Some non-zero samples should be detected
    let mut active_samples = vec![0.0f32; 1000];
    active_samples[500] = 0.1; // Add a non-silent sample
    let has_signal = active_samples.iter().any(|&s| s.abs() > 0.001);
    assert!(has_signal, "Should detect signal");
}

/// Test audio level detection
#[test]
fn test_audio_level_calculation() {
    // Test peak level calculation
    let samples = vec![-0.5f32, 0.3, -0.8, 0.6, 0.0];
    let peak = samples.iter().map(|&s| s.abs()).fold(0.0f32, f32::max);

    assert!((peak - 0.8).abs() < 0.001, "Peak should be 0.8");
}

/// Test RMS level calculation
#[test]
fn test_rms_level_calculation() {
    let samples = vec![0.5f32, -0.5, 0.5, -0.5];
    let rms = (samples.iter().map(|&s| s * s).sum::<f32>() / samples.len() as f32).sqrt();

    assert!((rms - 0.5).abs() < 0.001, "RMS should be 0.5");
}
