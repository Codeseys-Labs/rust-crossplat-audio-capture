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
TEST_NAME="pulseaudio_app_capture"
TEST_AUDIO="test_audio.wav"
TEST_RESULT_DIR="/test-results"
TEST_OUTPUT="${TEST_RESULT_DIR}/${TEST_NAME}_$(date +%Y%m%d_%H%M%S).wav"

echo "=== STARTING PULSEAUDIO APPLICATION CAPTURE TEST ==="
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

# List audio devices
echo "Listing PulseAudio sources and sinks..."
pacmd list-sources | grep -E "name:|state"
pacmd list-sinks | grep -E "name:|state"

echo "Building Rust project..."
cd /app
cargo build --verbose

# Start a test tone in the background
echo "Starting test tone in background..."
cargo run --example test_tone --release &
TONE_PID=$!
sleep 3

# Run capture with PulseAudio example
echo "Running application capture with PulseAudio..."
cargo run --example test_pulseaudio -- --output $TEST_OUTPUT

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

# Kill the test tone process
if ps -p $TONE_PID > /dev/null; then
    echo "Stopping test tone..."
    kill $TONE_PID
fi

echo "=== PULSEAUDIO APPLICATION CAPTURE TEST COMPLETED SUCCESSFULLY ===" 