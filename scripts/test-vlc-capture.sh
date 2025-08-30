#!/bin/bash

# VLC Audio Capture Test Script
# This script tests our audio capture library with VLC streaming audio from URLs

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Function to print colored output
print_status() {
    local status=$1
    local message=$2
    case $status in
        "OK")
            echo -e "${GREEN}✅ $message${NC}"
            ;;
        "WARN")
            echo -e "${YELLOW}⚠️  $message${NC}"
            ;;
        "ERROR")
            echo -e "${RED}❌ $message${NC}"
            ;;
        "INFO")
            echo -e "${BLUE}ℹ️  $message${NC}"
            ;;
        *)
            echo "$message"
            ;;
    esac
}

echo "=== VLC Audio Capture Test Script ==="

# Test URLs (reliable audio sources)
TEST_URLS=(
    "https://www.soundjay.com/misc/sounds/bell-ringing-05.wav"
    "https://file-examples.com/storage/fe68c1b7c1a9fd42d99c603/2017/11/file_example_WAV_1MG.wav"
    "https://www.kozco.com/tech/LRMonoPhase4.wav"
    "https://www.kozco.com/tech/piano2.wav"
)

# Function to test URL accessibility
test_url() {
    local url="$1"
    print_status "INFO" "Testing URL: $url"
    
    if curl -s --head --max-time 10 "$url" | head -n 1 | grep -q "200"; then
        print_status "OK" "URL is accessible"
        return 0
    else
        print_status "WARN" "URL not accessible or timed out"
        return 1
    fi
}

# Find a working URL
WORKING_URL=""
print_status "INFO" "Searching for accessible audio URLs..."

for i in "${!TEST_URLS[@]}"; do
    if test_url "${TEST_URLS[$i]}"; then
        WORKING_URL="${TEST_URLS[$i]}"
        print_status "OK" "Found working URL: $WORKING_URL"
        break
    fi
done

# Fallback: generate local test audio if no URL works
if [ -z "$WORKING_URL" ]; then
    print_status "WARN" "No working URLs found, generating local test audio"
    
    # Create a more interesting test audio (multiple tones)
    ffmpeg -f lavfi -i "sine=frequency=440:duration=10,sine=frequency=880:duration=10" \
           -filter_complex "[0:a][1:a]concat=n=2:v=0:a=1" \
           -ar 48000 -ac 2 vlc_test_audio.wav -y
    
    WORKING_URL="file://$(pwd)/vlc_test_audio.wav"
    print_status "OK" "Created local test audio: $WORKING_URL"
fi

print_status "INFO" "Final test URL: $WORKING_URL"

# Check VLC availability
if ! command -v cvlc >/dev/null 2>&1; then
    print_status "ERROR" "cvlc (command-line VLC) not found"
    print_status "INFO" "This test requires VLC to be installed"
    print_status "INFO" "Install with: sudo apt-get install vlc"
    exit 1
fi

print_status "OK" "VLC found: $(which cvlc)"

# Check PipeWire
if ! command -v pw-cli >/dev/null 2>&1; then
    print_status "ERROR" "pw-cli not found - PipeWire not available"
    exit 1
fi

print_status "OK" "PipeWire found: $(pw-cli --version)"

# Start VLC with the URL
print_status "INFO" "Starting VLC with audio stream..."
cvlc --intf dummy --loop "$WORKING_URL" --verbose 2 > vlc_capture_test.log 2>&1 &
VLC_PID=$!

# Cleanup function
cleanup() {
    print_status "INFO" "Cleaning up..."
    if [ -n "$VLC_PID" ] && kill -0 $VLC_PID 2>/dev/null; then
        print_status "INFO" "Stopping VLC (PID: $VLC_PID)"
        kill $VLC_PID
        wait $VLC_PID 2>/dev/null || true
    fi
    pkill -f vlc || true
}
trap cleanup EXIT

# Wait for VLC to start
print_status "INFO" "Waiting for VLC to start..."
sleep 8

# Check if VLC is running
if ! kill -0 $VLC_PID 2>/dev/null; then
    print_status "ERROR" "VLC failed to start"
    print_status "INFO" "VLC logs:"
    cat vlc_capture_test.log || true
    exit 1
fi

print_status "OK" "VLC is running with PID: $VLC_PID"

# Check PipeWire for VLC nodes
print_status "INFO" "Checking PipeWire for VLC audio nodes..."
VLC_NODES=$(pw-cli list-objects Node | grep -i vlc || true)

if [ -n "$VLC_NODES" ]; then
    print_status "OK" "Found VLC nodes in PipeWire:"
    echo "$VLC_NODES"
else
    print_status "WARN" "No VLC nodes found in PipeWire yet, waiting..."
    sleep 5
    VLC_NODES=$(pw-cli list-objects Node | grep -i vlc || true)
    
    if [ -n "$VLC_NODES" ]; then
        print_status "OK" "Found VLC nodes after waiting:"
        echo "$VLC_NODES"
    else
        print_status "WARN" "Still no VLC nodes found, but continuing test..."
    fi
fi

# Show all audio nodes for debugging
print_status "INFO" "All PipeWire audio nodes:"
pw-cli list-objects Node | grep -E "(application|Audio)" || true

# Test our audio capture
print_status "INFO" "Testing audio capture with our library..."

# Test 1: Flexible PipeWire example
print_status "INFO" "Running flexible PipeWire example..."
timeout 20s cargo run --bin flexible_pipewire_example --features feat_linux > flexible_test.log 2>&1 || true

# Test 2: Try to capture system audio
print_status "INFO" "Attempting system audio capture..."
cargo run --example test_capture --features feat_linux -- \
    --duration 5 \
    --output vlc_system_capture.wav \
    --verbose > capture_test.log 2>&1 || true

# Check results
if [ -f "vlc_system_capture.wav" ]; then
    FILESIZE=$(stat -c%s "vlc_system_capture.wav")
    if [ "$FILESIZE" -gt 1000 ]; then
        print_status "OK" "Audio capture successful: $FILESIZE bytes"
        
        # Verify it's a valid WAV file
        if file vlc_system_capture.wav | grep -q "WAVE"; then
            print_status "OK" "Valid WAV file created"
        else
            print_status "WARN" "File created but may not be valid WAV"
        fi
    else
        print_status "WARN" "Capture file is very small: $FILESIZE bytes"
    fi
else
    print_status "WARN" "No capture file created"
fi

# Show logs for debugging
print_status "INFO" "=== VLC Logs (first 20 lines) ==="
head -20 vlc_capture_test.log || true

print_status "INFO" "=== Flexible Example Logs ==="
cat flexible_test.log || true

print_status "INFO" "=== Capture Test Logs ==="
cat capture_test.log || true

print_status "OK" "VLC Audio Capture Test Complete"

# Summary
echo ""
echo "=== Test Summary ==="
echo "VLC URL: $WORKING_URL"
echo "VLC PID: $VLC_PID"
echo "VLC Nodes Found: $([ -n "$VLC_NODES" ] && echo "Yes" || echo "No")"
echo "Capture File: $([ -f "vlc_system_capture.wav" ] && echo "Created ($(stat -c%s vlc_system_capture.wav) bytes)" || echo "Not created")"
