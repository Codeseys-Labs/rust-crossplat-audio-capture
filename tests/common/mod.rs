use std::sync::Arc;
use std::time::Duration;

/// Creates a test sine wave for audio validation
pub fn create_test_signal(frequency: f32, duration_ms: u32, sample_rate: u32) -> Vec<f32> {
    let num_samples = (duration_ms as f32 * sample_rate as f32 / 1000.0) as usize;
    let mut samples = Vec::with_capacity(num_samples);
    
    for i in 0..num_samples {
        let t = i as f32 / sample_rate as f32;
        let sample = (2.0 * std::f32::consts::PI * frequency * t).sin();
        samples.push(sample);
    }
    
    samples
}

/// Verifies that two audio signals are similar within a tolerance
pub fn verify_audio_similarity(signal1: &[f32], signal2: &[f32], tolerance: f32) -> bool {
    if signal1.len() != signal2.len() {
        return false;
    }
    
    signal1.iter()
        .zip(signal2.iter())
        .all(|(s1, s2)| (s1 - s2).abs() <= tolerance)
}

/// Mock audio device for testing
#[derive(Debug, Clone)]
pub struct MockAudioDevice {
    pub id: String,
    pub name: String,
    pub channels: u32,
    pub sample_rate: u32,
}

/// Mock audio capture implementation for testing
pub struct MockAudioCapture {
    is_capturing: bool,
    devices: Vec<MockAudioDevice>,
    current_device: Option<MockAudioDevice>,
    test_data: Arc<Vec<f32>>,
}

impl MockAudioCapture {
    pub fn new() -> Self {
        Self {
            is_capturing: false,
            devices: vec![
                MockAudioDevice {
                    id: "device1".to_string(),
                    name: "Test Device 1".to_string(),
                    channels: 2,
                    sample_rate: 48000,
                },
                MockAudioDevice {
                    id: "device2".to_string(),
                    name: "Test Device 2".to_string(),
                    channels: 1,
                    sample_rate: 44100,
                },
            ],
            current_device: None,
            test_data: Arc::new(create_test_signal(440.0, 1000, 48000)),
        }
    }
    
    pub fn with_test_data(test_data: Vec<f32>) -> Self {
        Self {
            is_capturing: false,
            devices: vec![],
            current_device: None,
            test_data: Arc::new(test_data),
        }
    }
}

/// Helper function to create a temporary WAV file for testing
pub fn create_test_wav_file(path: &str, samples: &[f32], channels: u16, sample_rate: u32) -> std::io::Result<()> {
    use hound::{WavWriter, WavSpec};
    
    let spec = WavSpec {
        channels,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    
    let mut writer = WavWriter::create(path, spec)?;
    for sample in samples {
        writer.write_sample(*sample)?;
    }
    writer.finalize()?;
    
    Ok(())
}

/// Helper function to read a WAV file for testing
pub fn read_wav_file(path: &str) -> std::io::Result<(Vec<f32>, hound::WavSpec)> {
    use hound::WavReader;
    
    let mut reader = WavReader::open(path)?;
    let spec = reader.spec();
    let samples: Vec<f32> = reader.samples::<f32>()
        .filter_map(Result::ok)
        .collect();
    
    Ok((samples, spec))
}