use clap::Parser;
use hound::WavReader;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "verify_audio")]
#[command(about = "Verify that captured audio contains expected frequencies")]
struct Args {
    /// Input WAV file to analyze
    #[arg(short, long)]
    input: PathBuf,

    /// Expected frequency in Hz
    #[arg(short, long, default_value = "440.0")]
    frequency: f32,

    /// Tolerance for frequency detection (Hz)
    #[arg(short, long, default_value = "10.0")]
    tolerance: f32,

    /// Minimum amplitude threshold (0.0 to 1.0)
    #[arg(short, long, default_value = "0.01")]
    amplitude_threshold: f32,

    /// Enable verbose output
    #[arg(short, long)]
    verbose: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    if args.verbose {
        println!("Analyzing audio file: {}", args.input.display());
        println!(
            "Looking for frequency: {} Hz (±{} Hz)",
            args.frequency, args.tolerance
        );
        println!("Amplitude threshold: {}", args.amplitude_threshold);
    }

    // Read the WAV file
    let mut reader = WavReader::open(&args.input)?;
    let spec = reader.spec();

    if args.verbose {
        println!("Audio format:");
        println!("  Sample rate: {} Hz", spec.sample_rate);
        println!("  Channels: {}", spec.channels);
        println!("  Bits per sample: {}", spec.bits_per_sample);
    }

    // Read all samples
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<Vec<_>, _>>()?,
        hound::SampleFormat::Int => {
            let int_samples: Vec<i32> = reader.samples::<i32>().collect::<Result<Vec<_>, _>>()?;
            let max_val = (1 << (spec.bits_per_sample - 1)) as f32;
            int_samples
                .into_iter()
                .map(|s| s as f32 / max_val)
                .collect()
        }
    };

    if args.verbose {
        println!("Read {} samples", samples.len());
    }

    // Convert to mono if stereo (take left channel)
    let mono_samples: Vec<f32> = if spec.channels == 2 {
        samples.iter().step_by(2).cloned().collect()
    } else {
        samples
    };

    // Perform simple frequency analysis
    let sample_rate = spec.sample_rate as f32;
    let analysis_result =
        analyze_frequency(&mono_samples, sample_rate, args.frequency, args.tolerance);

    match analysis_result {
        Some(detected_freq) => {
            println!(
                "✅ SUCCESS: Detected frequency {} Hz (expected {} Hz)",
                detected_freq, args.frequency
            );

            // Check amplitude
            let max_amplitude = mono_samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
            if max_amplitude >= args.amplitude_threshold {
                println!(
                    "✅ SUCCESS: Amplitude {} >= threshold {}",
                    max_amplitude, args.amplitude_threshold
                );
            } else {
                println!(
                    "⚠️  WARNING: Low amplitude {} < threshold {}",
                    max_amplitude, args.amplitude_threshold
                );
            }
        }
        None => {
            println!(
                "❌ FAILURE: Expected frequency {} Hz not detected",
                args.frequency
            );

            // Provide some diagnostic info
            let max_amplitude = mono_samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
            println!("   Max amplitude: {}", max_amplitude);

            if max_amplitude < args.amplitude_threshold {
                println!("   Possible cause: Audio too quiet or silent");
            }

            std::process::exit(1);
        }
    }

    Ok(())
}

/// Simple frequency detection using autocorrelation
fn analyze_frequency(
    samples: &[f32],
    sample_rate: f32,
    target_freq: f32,
    tolerance: f32,
) -> Option<f32> {
    if samples.len() < 1024 {
        return None;
    }

    // Calculate expected period in samples
    let _expected_period = sample_rate / target_freq;
    let min_period = sample_rate / (target_freq + tolerance);
    let max_period = sample_rate / (target_freq - tolerance);

    // Use a subset of samples for analysis (first 2 seconds or available samples)
    let analysis_samples = std::cmp::min(samples.len(), (sample_rate * 2.0) as usize);
    let analysis_data = &samples[0..analysis_samples];

    // Simple autocorrelation to find dominant period
    let mut best_correlation = 0.0f32;
    let mut best_period = 0.0f32;

    let min_lag = min_period as usize;
    let max_lag = std::cmp::min(max_period as usize, analysis_data.len() / 2);

    for lag in min_lag..=max_lag {
        let mut correlation = 0.0f32;
        let mut count = 0;

        for i in 0..(analysis_data.len() - lag) {
            correlation += analysis_data[i] * analysis_data[i + lag];
            count += 1;
        }

        if count > 0 {
            correlation /= count as f32;

            if correlation > best_correlation {
                best_correlation = correlation;
                best_period = lag as f32;
            }
        }
    }

    // Convert period back to frequency
    if best_period > 0.0 && best_correlation > 0.01 {
        let detected_freq = sample_rate / best_period;

        // Check if it's within tolerance
        if (detected_freq - target_freq).abs() <= tolerance {
            Some(detected_freq)
        } else {
            None
        }
    } else {
        None
    }
}
