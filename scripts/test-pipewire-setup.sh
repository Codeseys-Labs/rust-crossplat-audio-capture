#!/bin/bash

# Test script to verify PipeWire setup and CLI tools
# This script can be run locally or in CI to verify PipeWire functionality

set -e

echo "=== PipeWire Setup Test Script ==="

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
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
        *)
            echo "$message"
            ;;
    esac
}

# Check if running in CI
if [ "$CI" = "true" ]; then
    print_status "OK" "Running in CI environment"
else
    print_status "OK" "Running locally"
fi

# Check for PipeWire CLI tools
echo ""
echo "=== Checking PipeWire CLI Tools ==="

# Check pw-cli
if command -v pw-cli >/dev/null 2>&1; then
    print_status "OK" "pw-cli found: $(which pw-cli)"
    echo "Version: $(pw-cli --version 2>/dev/null || echo 'Version not available')"
else
    print_status "ERROR" "pw-cli not found"
    echo "To install: sudo apt-get install pipewire-tools"
    exit 1
fi

# Check pw-dump
if command -v pw-dump >/dev/null 2>&1; then
    print_status "OK" "pw-dump found: $(which pw-dump)"
else
    print_status "WARN" "pw-dump not found - may need newer PipeWire version"
fi

# Check pw-link
if command -v pw-link >/dev/null 2>&1; then
    print_status "OK" "pw-link found: $(which pw-link)"
else
    print_status "WARN" "pw-link not found"
fi

# Check pactl (PulseAudio compatibility)
if command -v pactl >/dev/null 2>&1; then
    print_status "OK" "pactl found: $(which pactl)"
else
    print_status "WARN" "pactl not found"
fi

# Test PipeWire functionality
echo ""
echo "=== Testing PipeWire Functionality ==="

# Test pw-cli info
echo "Testing pw-cli info..."
if pw-cli info >/dev/null 2>&1; then
    print_status "OK" "pw-cli info works"
else
    print_status "ERROR" "pw-cli info failed - PipeWire may not be running"
    echo "To start PipeWire:"
    echo "  systemctl --user --now enable pipewire.service"
    echo "  systemctl --user --now enable wireplumber.service"
    exit 1
fi

# Test pw-cli list-objects
echo "Testing pw-cli list-objects..."
if pw-cli list-objects Node >/dev/null 2>&1; then
    print_status "OK" "pw-cli list-objects works"
    NODE_COUNT=$(pw-cli list-objects Node | grep -c "Node" || echo "0")
    echo "Found $NODE_COUNT PipeWire nodes"
else
    print_status "WARN" "pw-cli list-objects failed"
fi

# Test pactl if available
if command -v pactl >/dev/null 2>&1; then
    echo "Testing pactl info..."
    if pactl info >/dev/null 2>&1; then
        print_status "OK" "pactl info works"
    else
        print_status "WARN" "pactl info failed"
    fi
fi

# Test pw-dump if available
if command -v pw-dump >/dev/null 2>&1; then
    echo "Testing pw-dump..."
    if pw-dump >/dev/null 2>&1; then
        print_status "OK" "pw-dump works"
    else
        print_status "WARN" "pw-dump failed"
    fi
fi

# Check for audio applications
echo ""
echo "=== Checking Audio Applications ==="

# Check VLC
if command -v vlc >/dev/null 2>&1; then
    print_status "OK" "VLC found: $(which vlc)"
else
    print_status "WARN" "VLC not found - install with: sudo apt-get install vlc"
fi

# Check cvlc (command-line VLC)
if command -v cvlc >/dev/null 2>&1; then
    print_status "OK" "cvlc found: $(which cvlc)"
else
    print_status "WARN" "cvlc not found"
fi

# Check ffmpeg
if command -v ffmpeg >/dev/null 2>&1; then
    print_status "OK" "ffmpeg found: $(which ffmpeg)"
else
    print_status "WARN" "ffmpeg not found - install with: sudo apt-get install ffmpeg"
fi

# Summary
echo ""
echo "=== Summary ==="

# Check if we have the minimum requirements
REQUIREMENTS_MET=true

if ! command -v pw-cli >/dev/null 2>&1; then
    print_status "ERROR" "Missing required tool: pw-cli"
    REQUIREMENTS_MET=false
fi

if ! pw-cli info >/dev/null 2>&1; then
    print_status "ERROR" "PipeWire not running or not accessible"
    REQUIREMENTS_MET=false
fi

if [ "$REQUIREMENTS_MET" = true ]; then
    print_status "OK" "All minimum requirements met for PipeWire testing"
    echo ""
    echo "You can now run:"
    echo "  cargo build --features feat_linux"
    echo "  cargo run --example test_capture --features feat_linux"
    echo "  cargo run --bin audio_recorder_tui --features feat_linux"
else
    print_status "ERROR" "Some requirements not met - see messages above"
    exit 1
fi

echo ""
echo "=== PipeWire Setup Test Complete ==="
