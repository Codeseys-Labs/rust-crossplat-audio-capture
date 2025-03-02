#!/bin/bash
set -e

# Function to clean up background processes on exit
cleanup() {
    echo "Cleaning up processes..."
    pkill -P $$ || true
    exit
}

# Set up trap to call cleanup function on exit
trap cleanup EXIT INT TERM

# Test variables
TEST_NAME="pulseaudio_system_capture"
TEST_AUDIO="test_audio.wav"
TEST_RESULT_DIR="/test-results"
TEST_OUTPUT="${TEST_RESULT_DIR}/${TEST_NAME}_$(date +%Y%m%d_%H%M%S).wav"

echo "=== STARTING PULSEAUDIO SYSTEM CAPTURE TEST ==="
echo "Test output: $TEST_OUTPUT"

# Create test results directory if it doesn't exist
mkdir -p $TEST_RESULT_DIR

# Download test audio if it doesn't exist
if [ ! -f $TEST_AUDIO ]; then
    echo "Downloading test audio..."
    wget -O $TEST_AUDIO https://www2.cs.uic.edu/~i101/SoundFiles/StarWars3.wav
fi

# Check if PulseAudio is running
if ! pulseaudio --check; then
    echo "Starting PulseAudio..."
    pulseaudio --start
    sleep 2
fi

# Create monitor sink for system audio
echo "Creating virtual sink for system audio capture..."
pacmd load-module module-null-sink sink_name=system_monitor sink_properties=device.description="System Monitor"
pacmd load-module module-loopback source_dont_move=true sink_dont_move=true source=system_monitor.monitor sink=@DEFAULT_SINK@

# List audio devices
echo "Listing PulseAudio sources and sinks..."
pacmd list-sources | grep -E "name:|state"
pacmd list-sinks | grep -E "name:|state"

echo "Building Rust project..."
cd /app
cargo build --verbose

# Create a system capture example for PulseAudio
echo "Creating system capture example..."
cat > /app/examples/test_pulseaudio_system.rs << EOF
use clap::Parser;
use hound::{WavSpec, WavWriter};
use rsac::{get_audio_backend, AudioConfig, AudioFormat, DeviceInfo};
use std::time::{Duration, Instant};

const SAMPLE_RATE: u32 = 48000;
const CHANNELS: u16 = 2;

#[derive(Parser, Debug)]
#[command(about = "Test PulseAudio system audio capture")]
struct Args {
    /// Duration in seconds to capture
    #[arg(long, default_value = "5")]
    duration: u64,

    /// Output WAV file path
    #[arg(long, default_value = "test_capture.wav")]
    output: String,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    println!("Starting PulseAudio system audio capture test...");

    let backend = get_audio_backend()?;
    println!("Using audio backend: {}", backend.name());

    // List available devices
    println!("Available audio devices:");
    let devices = backend.list_devices()?;
    for (i, device) in devices.iter().enumerate() {
        println!("{}: {} (ID: {})", i, device.name, device.id);
    }

    // Find monitor device
    let device = devices
        .iter()
        .find(|d| d.name.contains("monitor"))
        .ok_or("Could not find system monitor device")?;

    println!("Capturing from system device: {}", device.name);

    let config = AudioConfig {
        sample_rate: SAMPLE_RATE,
        channels: CHANNELS,
        format: AudioFormat::F32LE,
    };

    let mut stream = backend.capture_device(device, config)?;
    stream.start()?;
    println!("Started capturing...");

    let spec = WavSpec {
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut wav_writer = WavWriter::create(&args.output, spec)?;

    let mut buffer = vec![0u8; 4096];
    let start = Instant::now();
    let duration = Duration::from_secs(args.duration);
    let mut total_bytes = 0u32;

    while start.elapsed() < duration {
        let bytes_read = stream.read(&mut buffer)?;
        if bytes_read > 0 {
            let samples = unsafe {
                std::slice::from_raw_parts(
                    buffer[..bytes_read].as_ptr() as *const f32,
                    bytes_read / 4,
                )
            };
            for &sample in samples {
                wav_writer.write_sample(sample)?;
            }
            total_bytes += bytes_read as u32;
            print!("\rCaptured {} bytes", total_bytes);
        }
    }
    println!();

    stream.stop()?;
    wav_writer.finalize()?;

    println!("Capture complete! Saved to {}", args.output);
    println!("Total bytes captured: {}", total_bytes);

    Ok(())
}
EOF

# Compile the system capture example
echo "Compiling system capture example..."
cargo build --example test_pulseaudio_system

# Play the test audio through the system in background
echo "Playing test audio through system..."
paplay --device=system_monitor $TEST_AUDIO &
AUDIO_PID=$!
sleep 1

# Run the system capture example
echo "Running system capture with PulseAudio..."
cargo run --example test_pulseaudio_system -- --output $TEST_OUTPUT

# Check capture result
if [ -f "$TEST_OUTPUT" ]; then
    echo "Capture successful, audio file saved to: $TEST_OUTPUT"
    
    # Get file info
    SIZE=$(stat -c%s "$TEST_OUTPUT")
    echo "Output file size: $SIZE bytes"
    
    # Validate it's not empty or too small
    if [ $SIZE -lt 1000 ]; then
        echo "WARNING: Output file is suspiciously small!"
        exit 1
    fi
else
    echo "ERROR: Capture failed, no output file found!"
    exit 1
fi

# Kill the audio playback process if still running
if ps -p $AUDIO_PID > /dev/null; then
    echo "Stopping audio playback..."
    kill $AUDIO_PID
fi

echo "=== PULSEAUDIO SYSTEM CAPTURE TEST COMPLETED SUCCESSFULLY ===" 