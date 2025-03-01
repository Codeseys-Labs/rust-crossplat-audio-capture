#!/bin/bash

# Download test audio
echo "Downloading test audio..."
curl -L "https://ia800901.us.archive.org/23/items/gd70-02-14.early-late.sbd.cotsman.18115.sbeok.shnf/gd70-02-14d1t02.mp3" -o "/test_audio/sample.mp3"

# Build the project
echo "Building project..."
cargo build --verbose
cargo build --examples

# Start test applications
echo "Starting VLC with test audio..."
/Applications/VLC.app/Contents/MacOS/VLC --intf dummy --no-video /test_audio/sample.mp3 &
VLC_PID=$!

# Start test tone in background
cargo run --example test_tone &
PLAYER_PID=$!

# Ensure cleanup on exit
cleanup() {
    echo "Cleaning up processes..."
    if [ -n "$PLAYER_PID" ] && kill -0 $PLAYER_PID 2>/dev/null; then
        kill $PLAYER_PID
        wait $PLAYER_PID 2>/dev/null || true
    fi
    if [ -n "$VLC_PID" ] && kill -0 $VLC_PID 2>/dev/null; then
        kill $VLC_PID
        wait $VLC_PID 2>/dev/null || true
    fi
}
trap cleanup EXIT

# Wait for applications to initialize
sleep 5

# Test system-wide capture
echo "Testing system-wide capture..."
cargo run --example test_coreaudio -- --duration 5 --output system_capture.wav

# Test application-specific capture (VLC)
echo "Testing application-specific capture..."
cargo run --example test_coreaudio -- --duration 5 --output vlc_capture.wav --application "VLC"

# Test concurrent capture
echo "Testing concurrent capture..."
cargo run --example test_coreaudio -- --duration 5 --output concurrent_system.wav &
CONCURRENT_SYS_PID=$!
cargo run --example test_coreaudio -- --duration 5 --output concurrent_app.wav --application "VLC" &
CONCURRENT_APP_PID=$!

wait $CONCURRENT_SYS_PID
wait $CONCURRENT_APP_PID

# Test audio session management
echo "Testing audio session management..."
cargo run --example test_coreaudio -- --duration 5 --output session_capture.wav --test-session-management

# Verify captures
for file in system_capture.wav vlc_capture.wav concurrent_system.wav concurrent_app.wav session_capture.wav; do
    if [ ! -f "$file" ]; then
        echo "Capture failed - $file not created"
        exit 1
    fi
    FILESIZE=$(stat -f%z "$file")
    if [ "$FILESIZE" -eq 0 ]; then
        echo "Capture failed - $file is empty"
        exit 1
    fi
    echo "Successfully captured $FILESIZE bytes of audio in $file"
done 