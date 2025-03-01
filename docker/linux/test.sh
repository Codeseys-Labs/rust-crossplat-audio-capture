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

# Create test audio directory if it doesn't exist
mkdir -p /app/test-audio

# Download test audio if it doesn't exist
if [ ! -f /app/test-audio/test.wav ]; then
    echo "Downloading test audio..."
    wget -O /app/test-audio/test.wav https://www2.cs.uic.edu/~i101/SoundFiles/StarWars3.wav
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
echo "Listing audio devices..."
pw-cli list-objects | grep -E "node.name|media.class"

# Create .cargo/config.toml to set feature flag for libspa-sys
mkdir -p /app/.cargo
cat > /app/.cargo/config.toml << EOF
[build]
rustflags = ["--cfg", "feature=\"v0_3_65\""]
EOF

echo "Building Rust project..."
cd /app
cargo build --verbose

echo "Building examples..."
cargo build --verbose --example test_tone_robust
cargo build --verbose --example test_pipewire
cargo build --verbose --example test_capture_robust

echo "Running test_tone_robust example in background..."
cargo run --example test_tone_robust &
TONE_PID=$!
sleep 5

echo "Running test_pipewire example..."
cargo run --example test_pipewire
sleep 2

echo "Running test_capture_robust example..."
cargo run --example test_capture_robust
sleep 2

echo "Running cargo tests..."
cargo test --verbose

# Check if test_tone_robust is still running
if ps -p $TONE_PID > /dev/null; then
    echo "test_tone_robust is still running, killing it..."
    kill $TONE_PID
fi

echo "All tests completed successfully!" 