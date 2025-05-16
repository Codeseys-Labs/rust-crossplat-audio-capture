#!/bin/bash
set -e

echo "=== PIPEWIRE VERIFICATION ==="

# Function to check if a command exists
check_command() {
    if command -v "$1" >/dev/null 2>&1; then
        echo "✅ $1 command is available"
        return 0
    else
        echo "❌ $1 command not found"
        return 1
    fi
}

# Function to check if a process is running
check_process() {
    if pgrep -x "$1" >/dev/null; then
        echo "✅ $1 process is running"
        return 0
    else
        echo "❌ $1 process is not running"
        return 1
    fi
}

# Function to check if a library is installed
check_library() {
    if ldconfig -p | grep -q "$1"; then
        echo "✅ $1 library is installed"
        return 0
    else
        echo "❌ $1 library not found"
        return 1
    fi
}

# Check if required commands exist
echo "Checking PipeWire commands..."
check_command "pipewire" || exit 1
check_command "pw-cli" || exit 1
check_command "pw-top" || exit 1

# Check if required libraries exist
echo "Checking PipeWire libraries..."
check_library "libpipewire-0.3" || exit 1

# Check if processes are running
echo "Checking PipeWire processes..."
check_process "pipewire" || exit 1
check_process "pipewire-pulse" || exit 1
# Use a more flexible check for the session manager since the name might vary
pgrep -f "pipewire-media-session\|wireplumber" >/dev/null || {
    echo "❌ No session manager running (pipewire-media-session or wireplumber)"
    exit 1
}
echo "✅ Session manager is running"

# Check if PipeWire socket exists
echo "Checking PipeWire socket..."
if [ -S "$XDG_RUNTIME_DIR/pipewire-0" ]; then
    echo "✅ PipeWire socket exists at $XDG_RUNTIME_DIR/pipewire-0"
else
    echo "❌ PipeWire socket not found at $XDG_RUNTIME_DIR/pipewire-0"
    exit 1
fi

# List all PipeWire nodes and objects
echo "Listing PipeWire objects:"
pw-cli list-objects | grep -E "node.name|media.class" || {
    echo "❌ Failed to list PipeWire objects"
    exit 1
}

# Test playing a sound through PipeWire
echo "Testing PipeWire audio playback..."
# Create a test WAV file
if [ ! -f "/tmp/test-tone.wav" ]; then
    echo "Generating test audio file..."
    dd if=/dev/urandom bs=1k count=10 | ffmpeg -i - -f wav -acodec pcm_s16le -ac 2 -ar 48000 -t 1 /tmp/test-tone.wav
fi

# Try to play using ALSA with PipeWire device
if command -v speaker-test >/dev/null 2>&1; then
    echo "Testing with speaker-test..."
    (speaker-test -Dpipewire -c2 -twav -l1 >/dev/null 2>&1) &
    TEST_PID=$!
    sleep 2
    kill $TEST_PID 2>/dev/null || true
fi

# Try to play using pw-play
if command -v pw-play >/dev/null 2>&1; then
    echo "Testing with pw-play..."
    (pw-play /tmp/test-tone.wav >/dev/null 2>&1) &
    TEST_PID=$!
    sleep 2
    kill $TEST_PID 2>/dev/null || true
fi

echo "=== PIPEWIRE VERIFICATION COMPLETE ==="
echo "✅ All PipeWire checks passed"
exit 0 