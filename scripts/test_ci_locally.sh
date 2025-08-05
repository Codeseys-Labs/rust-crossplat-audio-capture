#!/bin/bash

# Script to test CI/CD setup locally
# This script simulates the GitHub Actions environment for testing

set -e

echo "=== Local CI/CD Test Script ==="
echo "This script tests the audio capture CI/CD setup locally"

# Detect platform
PLATFORM=""
if [[ "$OSTYPE" == "linux-gnu"* ]]; then
    PLATFORM="linux"
elif [[ "$OSTYPE" == "darwin"* ]]; then
    PLATFORM="macos"
elif [[ "$OSTYPE" == "msys" ]] || [[ "$OSTYPE" == "win32" ]]; then
    PLATFORM="windows"
else
    echo "Unsupported platform: $OSTYPE"
    exit 1
fi

echo "Detected platform: $PLATFORM"

# Function to check if command exists
command_exists() {
    command -v "$1" >/dev/null 2>&1
}

# Check prerequisites
echo "=== Checking Prerequisites ==="

if ! command_exists cargo; then
    echo "❌ Rust/Cargo not found. Please install Rust."
    exit 1
fi
echo "✅ Rust/Cargo found"

if ! command_exists git; then
    echo "❌ Git not found. Please install Git."
    exit 1
fi
echo "✅ Git found"

# Platform-specific checks
case $PLATFORM in
    "linux")
        echo "Checking Linux audio dependencies..."
        
        # Check for PipeWire/PulseAudio development libraries
        if ! pkg-config --exists libpipewire-0.3; then
            echo "⚠️  PipeWire development libraries not found"
            echo "   Install with: sudo apt-get install libpipewire-0.3-dev libspa-0.2-dev"
        else
            echo "✅ PipeWire development libraries found"
        fi
        
        if ! pkg-config --exists libpulse; then
            echo "⚠️  PulseAudio development libraries not found"
            echo "   Install with: sudo apt-get install libpulse-dev"
        else
            echo "✅ PulseAudio development libraries found"
        fi
        ;;
        
    "macos")
        echo "Checking macOS audio dependencies..."
        
        if ! command_exists brew; then
            echo "⚠️  Homebrew not found. Some tests may fail."
        else
            echo "✅ Homebrew found"
            
            # Check for BlackHole
            if ! brew list blackhole-2ch >/dev/null 2>&1; then
                echo "⚠️  BlackHole not installed"
                echo "   Install with: brew install blackhole-2ch"
            else
                echo "✅ BlackHole found"
            fi
        fi
        ;;
        
    "windows")
        echo "Checking Windows audio dependencies..."
        echo "✅ Windows WASAPI should be available by default"
        ;;
esac

# Build the project
echo "=== Building Project ==="
echo "Building with all features..."
cargo build --all-features --verbose

echo "Building examples..."
cargo build --examples --all-features --verbose

# Run basic tests
echo "=== Running Basic Tests ==="
cargo test --all-features --verbose

# Test audio examples
echo "=== Testing Audio Examples ==="

# Test library functionality
echo "Testing library functionality..."
cargo run --example demo_library -- --list-only --verbose || echo "Library demo completed"

# Test tone generator
echo "Testing tone generator..."
timeout 5s cargo run --example test_tone --features test-utils -- --duration 3 --verbose || echo "Tone generator test completed"

# Test platform-specific capture
case $PLATFORM in
    "linux")
        echo "Testing Linux audio capture..."
        cargo run --example test_capture --features feat_linux -- --duration 3 --output test_linux_capture.wav --verbose
        
        if [ -f "test_linux_capture.wav" ]; then
            echo "✅ Linux capture test created output file"
            cargo run --example verify_audio -- --input test_linux_capture.wav --frequency 440.0 --verbose
        else
            echo "❌ Linux capture test failed - no output file"
        fi
        ;;
        
    "macos")
        echo "Testing macOS audio capture..."
        cargo run --example test_coreaudio --features feat_macos -- --duration 3 --output test_macos_capture.wav --verbose
        
        if [ -f "test_macos_capture.wav" ]; then
            echo "✅ macOS capture test created output file"
            cargo run --example verify_audio -- --input test_macos_capture.wav --frequency 440.0 --verbose
        else
            echo "❌ macOS capture test failed - no output file"
        fi
        ;;
        
    "windows")
        echo "Testing Windows audio capture..."
        cargo run --example test_windows --features feat_windows -- --duration 3 --output test_windows_capture.wav --verbose
        
        if [ -f "test_windows_capture.wav" ]; then
            echo "✅ Windows capture test created output file"
            cargo run --example verify_audio -- --input test_windows_capture.wav --frequency 440.0 --verbose
        else
            echo "❌ Windows capture test failed - no output file"
        fi
        ;;
esac

# Cleanup
echo "=== Cleanup ==="
rm -f test_*_capture.wav

echo "=== Local CI/CD Test Complete ==="
echo "If all tests passed, the CI/CD setup should work in GitHub Actions"
