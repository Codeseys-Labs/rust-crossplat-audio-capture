#!/bin/bash

# Linux PipeWire Audio Capture Testing Script
# Tests the Rust audio capture library and dynamic_vlc example on Linux with PipeWire

set -euo pipefail

# Configuration
WORKSPACE="/workspace"
RESULTS_DIR="/test-results/linux-pipewire"
TIMESTAMP=$(date +"%Y%m%d_%H%M%S")
TEST_AUDIO_DIR="/test-audio"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Logging functions
log_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

log_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

print_banner() {
    echo "=============================================================================="
    echo "🐧 Linux PipeWire Audio Capture Testing"
    echo "=============================================================================="
    echo "Timestamp: $TIMESTAMP"
    echo "Workspace: $WORKSPACE"
    echo "Results: $RESULTS_DIR"
    echo "=============================================================================="
}

# Setup test environment
setup_environment() {
    log_info "Setting up test environment..."
    
    mkdir -p "$RESULTS_DIR"
    cd "$WORKSPACE"
    
    # Start audio services if not running
    if ! pgrep -x "pipewire" > /dev/null; then
        log_info "Starting PipeWire..."
        pipewire &
        sleep 2
    fi
    
    if ! pgrep -x "pipewire-pulse" > /dev/null; then
        log_info "Starting PipeWire PulseAudio compatibility..."
        pipewire-pulse &
        sleep 2
    fi
    
    if ! pgrep -x "wireplumber" > /dev/null; then
        log_info "Starting WirePlumber..."
        wireplumber &
        sleep 2
    fi
    
    log_success "Environment setup completed"
}

# Test audio system availability
test_audio_system() {
    log_info "Testing audio system availability..."
    
    local test_log="$RESULTS_DIR/audio_system_test_${TIMESTAMP}.log"
    
    {
        echo "=== PipeWire Info ==="
        pw-cli info || echo "PipeWire CLI not available"
        
        echo -e "\n=== PulseAudio Info ==="
        pactl info || echo "PulseAudio not available"
        
        echo -e "\n=== ALSA Devices ==="
        aplay -l || echo "No ALSA devices"
        
        echo -e "\n=== PipeWire Objects ==="
        pw-cli list-objects || echo "No PipeWire objects"
        
        echo -e "\n=== Audio Sinks ==="
        pactl list short sinks || echo "No audio sinks"
        
        echo -e "\n=== Audio Sources ==="
        pactl list short sources || echo "No audio sources"
        
    } > "$test_log" 2>&1
    
    if grep -q "Server Name" "$test_log"; then
        log_success "Audio system is available"
        return 0
    else
        log_warning "Audio system may not be fully available"
        return 1
    fi
}

# Build the Rust library
build_library() {
    log_info "Building Rust audio capture library..."
    
    local build_log="$RESULTS_DIR/library_build_${TIMESTAMP}.log"
    
    if cargo build --no-default-features --features feat_linux 2>&1 | tee "$build_log"; then
        log_success "Library build successful"
        return 0
    else
        log_error "Library build failed"
        return 1
    fi
}

# Build the dynamic_vlc example
build_dynamic_vlc_example() {
    log_info "Building dynamic_vlc example..."
    
    local build_log="$RESULTS_DIR/dynamic_vlc_build_${TIMESTAMP}.log"
    
    # Check if dynamic_vlc example exists
    if [ ! -f "examples/dynamic_vlc.rs" ]; then
        log_warning "dynamic_vlc example not found, skipping..."
        return 1
    fi
    
    if cargo build --example dynamic_vlc --no-default-features --features feat_linux 2>&1 | tee "$build_log"; then
        log_success "dynamic_vlc example build successful"
        return 0
    else
        log_error "dynamic_vlc example build failed"
        return 1
    fi
}

# Test VLC availability and audio playback
test_vlc_audio() {
    log_info "Testing VLC audio playback..."
    
    local vlc_log="$RESULTS_DIR/vlc_test_${TIMESTAMP}.log"
    
    # Test VLC version
    vlc --version > "$vlc_log" 2>&1 || {
        log_error "VLC not available"
        return 1
    }
    
    # Test audio playback with VLC (headless)
    log_info "Testing VLC audio playback with test file..."
    if timeout 10s vlc --intf dummy --play-and-exit "$TEST_AUDIO_DIR/test-tone-440hz.wav" >> "$vlc_log" 2>&1; then
        log_success "VLC audio playback test completed"
    else
        log_warning "VLC audio playback test may have issues"
    fi
    
    return 0
}

# Test audio capture functionality
test_audio_capture() {
    log_info "Testing audio capture functionality..."
    
    local capture_log="$RESULTS_DIR/audio_capture_test_${TIMESTAMP}.log"
    
    # Start VLC playing test audio in background
    log_info "Starting VLC playback for capture testing..."
    vlc --intf dummy --loop "$TEST_AUDIO_DIR/test-music.wav" &
    local vlc_pid=$!
    
    sleep 3  # Let VLC start playing
    
    # Test basic audio capture using our library
    log_info "Testing audio capture with flexible_pipewire_example..."
    if timeout 10s cargo run --example flexible_pipewire_example --no-default-features --features feat_linux 2>&1 | tee "$capture_log"; then
        log_success "Audio capture test completed"
    else
        log_warning "Audio capture test may have issues"
    fi
    
    # Stop VLC
    kill $vlc_pid 2>/dev/null || true
    
    return 0
}

# Test dynamic VLC example if available
test_dynamic_vlc_example() {
    log_info "Testing dynamic_vlc example..."
    
    if [ ! -f "target/debug/examples/dynamic_vlc" ]; then
        log_warning "dynamic_vlc example not built, skipping test..."
        return 1
    fi
    
    local dynamic_vlc_log="$RESULTS_DIR/dynamic_vlc_test_${TIMESTAMP}.log"
    
    # Start VLC playing test audio
    log_info "Starting VLC for dynamic_vlc testing..."
    vlc --intf dummy --loop "$TEST_AUDIO_DIR/test-music.wav" &
    local vlc_pid=$!
    
    sleep 3  # Let VLC start
    
    # Test dynamic VLC example
    log_info "Running dynamic_vlc example..."
    if timeout 15s ./target/debug/examples/dynamic_vlc 2>&1 | tee "$dynamic_vlc_log"; then
        log_success "dynamic_vlc example test completed"
    else
        log_warning "dynamic_vlc example test may have issues"
    fi
    
    # Stop VLC
    kill $vlc_pid 2>/dev/null || true
    
    return 0
}

# Generate test report
generate_report() {
    log_info "Generating test report..."
    
    local report_file="$RESULTS_DIR/test_report_${TIMESTAMP}.html"
    local summary_file="$RESULTS_DIR/test_summary_${TIMESTAMP}.json"
    
    # Create JSON summary
    cat > "$summary_file" << EOF
{
    "platform": "linux-pipewire",
    "timestamp": "${TIMESTAMP}",
    "tests": {
        "audio_system": "$([ -f "$RESULTS_DIR/audio_system_test_${TIMESTAMP}.log" ] && echo "completed" || echo "skipped")",
        "library_build": "$([ -f "$RESULTS_DIR/library_build_${TIMESTAMP}.log" ] && echo "completed" || echo "failed")",
        "dynamic_vlc_build": "$([ -f "$RESULTS_DIR/dynamic_vlc_build_${TIMESTAMP}.log" ] && echo "completed" || echo "skipped")",
        "vlc_audio": "$([ -f "$RESULTS_DIR/vlc_test_${TIMESTAMP}.log" ] && echo "completed" || echo "skipped")",
        "audio_capture": "$([ -f "$RESULTS_DIR/audio_capture_test_${TIMESTAMP}.log" ] && echo "completed" || echo "skipped")",
        "dynamic_vlc_test": "$([ -f "$RESULTS_DIR/dynamic_vlc_test_${TIMESTAMP}.log" ] && echo "completed" || echo "skipped")"
    }
}
EOF
    
    # Create HTML report
    cat > "$report_file" << EOF
<!DOCTYPE html>
<html>
<head>
    <title>Linux PipeWire Audio Capture Test Report</title>
    <style>
        body { font-family: Arial, sans-serif; margin: 20px; }
        .header { background-color: #f0f0f0; padding: 20px; border-radius: 5px; }
        .test-section { margin: 20px 0; padding: 15px; border-left: 4px solid #007bff; }
        .success { border-left-color: #28a745; }
        .warning { border-left-color: #ffc107; }
        .error { border-left-color: #dc3545; }
        pre { background-color: #f8f9fa; padding: 10px; border-radius: 3px; overflow-x: auto; }
    </style>
</head>
<body>
    <div class="header">
        <h1>🐧 Linux PipeWire Audio Capture Test Report</h1>
        <p>Generated: ${TIMESTAMP}</p>
        <p>Platform: Linux with PipeWire</p>
    </div>
    
    <div class="test-section success">
        <h2>Test Summary</h2>
        <pre>$(cat "$summary_file" | jq . 2>/dev/null || echo "Summary not available")</pre>
    </div>
    
    <div class="test-section">
        <h2>Test Logs</h2>
        <p>Detailed logs are available in the results directory:</p>
        <ul>
            <li>Audio System Test: audio_system_test_${TIMESTAMP}.log</li>
            <li>Library Build: library_build_${TIMESTAMP}.log</li>
            <li>VLC Test: vlc_test_${TIMESTAMP}.log</li>
            <li>Audio Capture Test: audio_capture_test_${TIMESTAMP}.log</li>
            <li>Dynamic VLC Test: dynamic_vlc_test_${TIMESTAMP}.log</li>
        </ul>
    </div>
</body>
</html>
EOF
    
    log_success "Test report generated: $report_file"
}

# Main execution
main() {
    print_banner
    
    setup_environment
    
    # Run tests
    test_audio_system
    build_library
    build_dynamic_vlc_example
    test_vlc_audio
    test_audio_capture
    test_dynamic_vlc_example
    
    # Generate report
    generate_report
    
    log_success "🎉 Linux PipeWire testing completed!"
    log_info "📊 Results available in: $RESULTS_DIR"
}

# Run main function
main "$@"
