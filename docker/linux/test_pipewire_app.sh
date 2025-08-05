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
TEST_NAME="pipewire_app_capture"
TEST_AUDIO="test_audio.wav"
TEST_RESULT_DIR="/test-results"
TEST_OUTPUT="${TEST_RESULT_DIR}/${TEST_NAME}_$(date +%Y%m%d_%H%M%S).wav"

echo "=== STARTING PIPEWIRE APPLICATION CAPTURE TEST ==="
echo "Test output: $TEST_OUTPUT"

# Create test results directory if it doesn't exist
mkdir -p $TEST_RESULT_DIR

# Download test audio if it doesn't exist
if [ ! -f $TEST_AUDIO ]; then
    echo "Downloading test audio from Internet Archive..."
    curl -L "https://ia800901.us.archive.org/23/items/gd70-02-14.early-late.sbd.cotsman.18115.sbeok.shnf/gd70-02-14d1t02.mp3" -o "test_audio.mp3"
    
    # Check if ffmpeg is installed, if not install it
    if ! command -v ffmpeg &> /dev/null; then
        echo "Installing ffmpeg..."
        apt-get update && apt-get install -y ffmpeg
    fi
    
    # Convert to WAV format
    echo "Converting to WAV format..."
    ffmpeg -i "test_audio.mp3" -ar 48000 -ac 2 -acodec pcm_f32le "$TEST_AUDIO"
fi

# Check if Pipewire is running
if ! pgrep -x "pipewire" > /dev/null; then
    echo "Starting Pipewire..."
    pipewire &
    sleep 2
fi

# Check if Pipewire-Pulse is running
if ! pgrep -x "pipewire-pulse" > /dev/null; then
    echo "Starting Pipewire-Pulse..."
    pipewire-pulse &
    sleep 2
fi

# List audio devices
echo "Listing PipeWire audio devices..."
pw-cli list-objects | grep -E "node.name|media.class"

# Build just our test example
echo "Building simplified PipeWire test..."
cd /app
cargo build --example test_pipewire

# Run our simplified test
echo "Running simplified PipeWire test..."
cargo run --example test_pipewire -- --output $TEST_OUTPUT

# Check if the test succeeded
if [ $? -eq 0 ]; then
    echo "=== PIPEWIRE APPLICATION CAPTURE TEST COMPLETED SUCCESSFULLY ==="
else
    echo "=== PIPEWIRE APPLICATION CAPTURE TEST FAILED ==="
    exit 1
fi 