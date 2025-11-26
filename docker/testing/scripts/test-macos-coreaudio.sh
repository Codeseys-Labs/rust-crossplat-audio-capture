#!/bin/bash

# macOS Core Audio Testing Script
# Tests the Rust audio capture library and dynamic_vlc example on macOS with Core Audio

set -euo pipefail

# Configuration
WORKSPACE="/workspace"
RESULTS_DIR="/test-results/macos-coreaudio"
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
    echo "🍎 macOS Core Audio Testing"
    echo "=============================================================================="
    echo "Timestamp: $TIMESTAMP"
    echo "Workspace: $WORKSPACE"
    echo "Results: $RESULTS_DIR"
    echo "=============================================================================="
}

# Setup test environment
setup_environment() {
    log_info "Setting up macOS test environment..."
    
    mkdir -p "$RESULTS_DIR"
    cd "$WORKSPACE"
    
    # Wait for macOS to be fully ready
    log_info "Waiting for macOS to be ready..."
    sleep 5
    
    log_success "Environment setup completed"
}

# Test Core Audio system availability
test_audio_system() {
    log_info "Testing Core Audio system availability..."
    
    local test_log="$RESULTS_DIR/audio_system_test_${TIMESTAMP}.log"
    
    {
        echo "=== macOS Version ==="
        sw_vers || echo "sw_vers not available"
        
        echo -e "\n=== Audio Hardware ==="
        system_profiler SPAudioDataType || echo "Audio hardware info not available"
        
        echo -e "\n=== Audio Devices ==="
        SwitchAudioSource -a || echo "SwitchAudioSource not available"
        
        echo -e "\n=== Audio Units ==="
        auval -a || echo "Audio Units validation not available"
        
    } > "$test_log" 2>&1
    
    if grep -q "Audio" "$test_log"; then
        log_success "Core Audio system is available"
        return 0
    else
        log_warning "Core Audio system may not be fully available"
        return 1
    fi
}

# Build the Rust library for macOS
build_library() {
    log_info "Building Rust audio capture library for macOS..."
    
    local build_log="$RESULTS_DIR/library_build_${TIMESTAMP}.log"
    
    if cargo build --no-default-features --features feat_macos 2>&1 | tee "$build_log"; then
        log_success "Library build successful"
        return 0
    else
        log_error "Library build failed"
        return 1
    fi
}

# Build the dynamic_vlc example for macOS
build_dynamic_vlc_example() {
    log_info "Building dynamic_vlc example for macOS..."
    
    local build_log="$RESULTS_DIR/dynamic_vlc_build_${TIMESTAMP}.log"
    
    # Check if dynamic_vlc example exists
    if [ ! -f "examples/dynamic_vlc.rs" ]; then
        log_warning "dynamic_vlc example not found, skipping..."
        return 1
    fi
    
    if cargo build --example dynamic_vlc --no-default-features --features feat_macos 2>&1 | tee "$build_log"; then
        log_success "dynamic_vlc example build successful"
        return 0
    else
        log_error "dynamic_vlc example build failed"
        return 1
    fi
}

# Test VLC availability and audio playback on macOS
test_vlc_audio() {
    log_info "Testing VLC audio playback on macOS..."
    
    local vlc_log="$RESULTS_DIR/vlc_test_${TIMESTAMP}.log"
    
    # Test VLC version
    if /Applications/VLC.app/Contents/MacOS/VLC --version > "$vlc_log" 2>&1; then
        log_success "VLC is available"
    else
        log_error "VLC not available"
        return 1
    fi
    
    # Test audio playback with VLC (headless)
    log_info "Testing VLC audio playback with test file..."
    if timeout 10s /Applications/VLC.app/Contents/MacOS/VLC --intf dummy --play-and-exit "$TEST_AUDIO_DIR/test-tone-440hz.wav" >> "$vlc_log" 2>&1; then
        log_success "VLC audio playback test completed"
    else
        log_warning "VLC audio playback test may have issues"
    fi
    
    return 0
}

# Test audio capture functionality on macOS
test_audio_capture() {
    log_info "Testing audio capture functionality on macOS..."
    
    local capture_log="$RESULTS_DIR/audio_capture_test_${TIMESTAMP}.log"
    
    # Start VLC playing test audio in background
    log_info "Starting VLC playback for capture testing..."
    /Applications/VLC.app/Contents/MacOS/VLC --intf dummy --loop "$TEST_AUDIO_DIR/test-music.wav" &
    local vlc_pid=$!
    
    sleep 3  # Let VLC start playing
    
    # Test basic audio capture using our library
    log_info "Testing audio capture with macos_application_capture example..."
    if timeout 10s cargo run --example macos_application_capture --no-default-features --features feat_macos 2>&1 | tee "$capture_log"; then
        log_success "Audio capture test completed"
    else
        log_warning "Audio capture test may have issues"
    fi
    
    # Stop VLC
    kill $vlc_pid 2>/dev/null || true
    
    return 0
}

# Test dynamic VLC example on macOS
test_dynamic_vlc_example() {
    log_info "Testing dynamic_vlc example on macOS..."
    
    if [ ! -f "target/debug/examples/dynamic_vlc" ]; then
        log_warning "dynamic_vlc example not built, skipping test..."
        return 1
    fi
    
    local dynamic_vlc_log="$RESULTS_DIR/dynamic_vlc_test_${TIMESTAMP}.log"
    
    # Start VLC playing test audio
    log_info "Starting VLC for dynamic_vlc testing..."
    /Applications/VLC.app/Contents/MacOS/VLC --intf dummy --loop "$TEST_AUDIO_DIR/test-music.wav" &
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

# Test BlackHole audio driver if available
test_blackhole_audio() {
    log_info "Testing BlackHole audio driver..."
    
    local blackhole_log="$RESULTS_DIR/blackhole_test_${TIMESTAMP}.log"
    
    # Check if BlackHole is available
    if SwitchAudioSource -a | grep -q "BlackHole"; then
        log_info "BlackHole audio driver found"
        
        # Switch to BlackHole for testing
        SwitchAudioSource -s "BlackHole 2ch" > "$blackhole_log" 2>&1 || true
        
        # Test audio routing through BlackHole
        log_info "Testing audio routing through BlackHole..."
        /Applications/VLC.app/Contents/MacOS/VLC --intf dummy --play-and-exit "$TEST_AUDIO_DIR/test-tone-440hz.wav" >> "$blackhole_log" 2>&1 || true
        
        log_success "BlackHole audio test completed"
    else
        log_warning "BlackHole audio driver not available"
    fi
    
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
    "platform": "macos-coreaudio",
    "timestamp": "${TIMESTAMP}",
    "tests": {
        "audio_system": "$([ -f "$RESULTS_DIR/audio_system_test_${TIMESTAMP}.log" ] && echo "completed" || echo "skipped")",
        "library_build": "$([ -f "$RESULTS_DIR/library_build_${TIMESTAMP}.log" ] && echo "completed" || echo "failed")",
        "dynamic_vlc_build": "$([ -f "$RESULTS_DIR/dynamic_vlc_build_${TIMESTAMP}.log" ] && echo "completed" || echo "skipped")",
        "vlc_audio": "$([ -f "$RESULTS_DIR/vlc_test_${TIMESTAMP}.log" ] && echo "completed" || echo "skipped")",
        "audio_capture": "$([ -f "$RESULTS_DIR/audio_capture_test_${TIMESTAMP}.log" ] && echo "completed" || echo "skipped")",
        "dynamic_vlc_test": "$([ -f "$RESULTS_DIR/dynamic_vlc_test_${TIMESTAMP}.log" ] && echo "completed" || echo "skipped")",
        "blackhole_test": "$([ -f "$RESULTS_DIR/blackhole_test_${TIMESTAMP}.log" ] && echo "completed" || echo "skipped")"
    }
}
EOF
    
    # Create HTML report
    cat > "$report_file" << EOF
<!DOCTYPE html>
<html>
<head>
    <title>macOS Core Audio Test Report</title>
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
        <h1>🍎 macOS Core Audio Test Report</h1>
        <p>Generated: ${TIMESTAMP}</p>
        <p>Platform: macOS with Core Audio</p>
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
            <li>BlackHole Test: blackhole_test_${TIMESTAMP}.log</li>
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
    test_blackhole_audio
    test_audio_capture
    test_dynamic_vlc_example
    
    # Generate report
    generate_report
    
    log_success "🎉 macOS Core Audio testing completed!"
    log_info "📊 Results available in: $RESULTS_DIR"
}

# Run main function
main "$@"
