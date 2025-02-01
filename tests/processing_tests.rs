mod common;

use audio_capture::processing::{AudioProcessor, ResampleOptions, AudioFormat};
use common::{create_test_signal, verify_audio_similarity};

#[test]
fn test_audio_resampling() {
    let original_rate = 48000;
    let target_rate = 44100;
    let test_signal = create_test_signal(440.0, 1000, original_rate);
    
    let options = ResampleOptions {
        original_rate,
        target_rate,
        channels: 1,
    };
    
    let resampled = AudioProcessor::resample(&test_signal, options)
        .expect("Failed to resample audio");
    
    // Check that the resampled length is proportional to the rate change
    let expected_length = (test_signal.len() as f32 * target_rate as f32 / original_rate as f32) as usize;
    assert!((resampled.len() as i32 - expected_length as i32).abs() <= 1);
}

#[test]
fn test_format_conversion() {
    let samples = create_test_signal(440.0, 100, 48000);
    
    // Convert to S16LE
    let s16_data = AudioProcessor::convert_format(&samples, AudioFormat::S16LE)
        .expect("Failed to convert to S16LE");
    assert_eq!(s16_data.len(), samples.len() * 2); // 16-bit = 2 bytes per sample
    
    // Convert to S32LE
    let s32_data = AudioProcessor::convert_format(&samples, AudioFormat::S32LE)
        .expect("Failed to convert to S32LE");
    assert_eq!(s32_data.len(), samples.len() * 4); // 32-bit = 4 bytes per sample
}

#[test]
fn test_channel_conversion() {
    let mono_signal = create_test_signal(440.0, 100, 48000);
    
    // Mono to stereo
    let stereo = AudioProcessor::mono_to_stereo(&mono_signal)
        .expect("Failed to convert mono to stereo");
    assert_eq!(stereo.len(), mono_signal.len() * 2);
    
    // Verify left and right channels are identical
    for i in 0..mono_signal.len() {
        assert_eq!(stereo[i * 2], stereo[i * 2 + 1]);
        assert_eq!(stereo[i * 2], mono_signal[i]);
    }
    
    // Stereo to mono
    let mono = AudioProcessor::stereo_to_mono(&stereo)
        .expect("Failed to convert stereo to mono");
    assert_eq!(mono.len(), mono_signal.len());
    
    // Verify the conversion is reversible
    assert!(verify_audio_similarity(&mono_signal, &mono, 0.001));
}

#[test]
fn test_volume_adjustment() {
    let signal = create_test_signal(440.0, 100, 48000);
    
    // Amplify by 2.0
    let amplified = AudioProcessor::adjust_volume(&signal, 2.0)
        .expect("Failed to amplify signal");
    
    for (original, adjusted) in signal.iter().zip(amplified.iter()) {
        assert!((adjusted / 2.0 - original).abs() < 0.001);
    }
    
    // Reduce volume by 0.5
    let reduced = AudioProcessor::adjust_volume(&signal, 0.5)
        .expect("Failed to reduce signal volume");
    
    for (original, adjusted) in signal.iter().zip(reduced.iter()) {
        assert!((adjusted * 2.0 - original).abs() < 0.001);
    }
}

#[test]
fn test_dc_offset_removal() {
    let mut signal = create_test_signal(440.0, 100, 48000);
    let offset = 0.5;
    
    // Add DC offset
    for sample in signal.iter_mut() {
        *sample += offset;
    }
    
    let processed = AudioProcessor::remove_dc_offset(&signal)
        .expect("Failed to remove DC offset");
    
    // Calculate mean of processed signal
    let mean: f32 = processed.iter().sum::<f32>() / processed.len() as f32;
    assert!(mean.abs() < 0.001);
}

#[test]
fn test_normalization() {
    let mut signal = create_test_signal(440.0, 100, 48000);
    
    // Reduce the signal amplitude
    for sample in signal.iter_mut() {
        *sample *= 0.1;
    }
    
    let normalized = AudioProcessor::normalize(&signal)
        .expect("Failed to normalize signal");
    
    // Check that the maximum amplitude is close to 1.0
    let max_amplitude = normalized.iter()
        .map(|&x| x.abs())
        .fold(0.0f32, f32::max);
    
    assert!((max_amplitude - 1.0).abs() < 0.001);
}

#[test]
fn test_error_handling() {
    // Test invalid resampling parameters
    let signal = create_test_signal(440.0, 100, 48000);
    let invalid_options = ResampleOptions {
        original_rate: 0,
        target_rate: 44100,
        channels: 1,
    };
    
    assert!(AudioProcessor::resample(&signal, invalid_options).is_err());
    
    // Test invalid volume adjustment
    assert!(AudioProcessor::adjust_volume(&signal, -1.0).is_err());
    
    // Test stereo to mono with invalid input
    let invalid_stereo = vec![1.0, 2.0, 3.0]; // Invalid length for stereo
    assert!(AudioProcessor::stereo_to_mono(&invalid_stereo).is_err());
}