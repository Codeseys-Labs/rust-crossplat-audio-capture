#!/bin/bash

# Test Orchestration Script
# Coordinates testing across Linux PipeWire, Windows, and macOS platforms

set -euo pipefail

# Configuration
WORKSPACE="/workspace"
RESULTS_DIR="/test-results"
TIMESTAMP=$(date +"%Y%m%d_%H%M%S")

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Logging functions
log_info() {
    echo -e "${BLUE}[ORCHESTRATOR]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[ORCHESTRATOR]${NC} $1"
}

log_warning() {
    echo -e "${YELLOW}[ORCHESTRATOR]${NC} $1"
}

log_error() {
    echo -e "${RED}[ORCHESTRATOR]${NC} $1"
}

print_banner() {
    echo "=============================================================================="
    echo "🎭 Cross-Platform Audio Capture Test Orchestrator"
    echo "=============================================================================="
    echo "Timestamp: $TIMESTAMP"
    echo "Results Directory: $RESULTS_DIR"
    echo "=============================================================================="
}

# Setup orchestrator environment
setup_environment() {
    log_info "Setting up orchestrator environment..."
    
    mkdir -p "$RESULTS_DIR"/{linux-pipewire,windows,macos-coreaudio,reports}
    cd "$WORKSPACE"
    
    log_success "Orchestrator environment ready"
}

# Check platform container status
check_platform_status() {
    local platform="$1"
    local container_name="$2"
    
    log_info "Checking status of $platform container: $container_name"
    
    if docker ps --format "table {{.Names}}" | grep -q "$container_name"; then
        log_success "$platform container is running"
        return 0
    else
        log_warning "$platform container is not running"
        return 1
    fi
}

# Wait for platform to be ready
wait_for_platform() {
    local platform="$1"
    local container_name="$2"
    local max_wait="$3"
    
    log_info "Waiting for $platform to be ready (max ${max_wait}s)..."
    
    local count=0
    while [ $count -lt $max_wait ]; do
        if check_platform_status "$platform" "$container_name"; then
            log_success "$platform is ready"
            return 0
        fi
        
        sleep 5
        count=$((count + 5))
        log_info "Waiting for $platform... (${count}s/${max_wait}s)"
    done
    
    log_error "$platform failed to become ready within ${max_wait}s"
    return 1
}

# Run test on specific platform
run_platform_test() {
    local platform="$1"
    local container_name="$2"
    local test_command="$3"
    
    log_info "Running test on $platform..."
    
    local test_log="$RESULTS_DIR/reports/${platform}_orchestrator_${TIMESTAMP}.log"
    
    if docker exec "$container_name" bash -c "$test_command" 2>&1 | tee "$test_log"; then
        log_success "$platform test completed successfully"
        return 0
    else
        log_error "$platform test failed"
        return 1
    fi
}

# Test Linux PipeWire platform
test_linux_pipewire() {
    log_info "🐧 Testing Linux PipeWire platform..."
    
    local container_name="rsac-linux-pipewire-test"
    
    if wait_for_platform "Linux PipeWire" "$container_name" 60; then
        run_platform_test "linux-pipewire" "$container_name" "/scripts/test-linux-pipewire.sh"
    else
        log_error "Linux PipeWire platform not available"
        return 1
    fi
}

# Test Windows platform
test_windows() {
    log_info "🪟 Testing Windows platform..."
    
    local container_name="rsac-windows-test"
    
    if wait_for_platform "Windows" "$container_name" 300; then
        log_info "Windows container is ready, but testing requires manual setup"
        log_info "Access Windows at http://localhost:8006 and run the setup script"
        log_info "Setup script location: C:\\scripts\\setup-windows-test.ps1"
        
        # Create a status file indicating Windows is ready for manual testing
        echo "Windows container ready for manual testing at $(date)" > "$RESULTS_DIR/windows/manual_test_ready_${TIMESTAMP}.txt"
        
        return 0
    else
        log_error "Windows platform not available"
        return 1
    fi
}

# Test macOS platform
test_macos() {
    log_info "🍎 Testing macOS platform..."
    
    local container_name="rsac-macos-test"
    
    if wait_for_platform "macOS" "$container_name" 600; then
        run_platform_test "macos-coreaudio" "$container_name" "/scripts/test-macos-coreaudio.sh"
    else
        log_error "macOS platform not available"
        return 1
    fi
}

# Generate comprehensive test report
generate_comprehensive_report() {
    log_info "Generating comprehensive test report..."
    
    local report_file="$RESULTS_DIR/reports/comprehensive_test_report_${TIMESTAMP}.html"
    local summary_file="$RESULTS_DIR/reports/test_summary_${TIMESTAMP}.json"
    
    # Collect platform summaries
    local linux_summary="{}"
    local windows_summary="{}"
    local macos_summary="{}"
    
    # Check for Linux results
    local linux_summary_file=$(find "$RESULTS_DIR/linux-pipewire" -name "test_summary_*.json" -type f | sort | tail -1)
    if [ -n "$linux_summary_file" ] && [ -f "$linux_summary_file" ]; then
        linux_summary=$(cat "$linux_summary_file")
    fi
    
    # Check for Windows results
    if [ -f "$RESULTS_DIR/windows/manual_test_ready_${TIMESTAMP}.txt" ]; then
        windows_summary='{"status": "ready_for_manual_testing", "timestamp": "'$TIMESTAMP'"}'
    fi
    
    # Check for macOS results
    local macos_summary_file=$(find "$RESULTS_DIR/macos-coreaudio" -name "test_summary_*.json" -type f | sort | tail -1)
    if [ -n "$macos_summary_file" ] && [ -f "$macos_summary_file" ]; then
        macos_summary=$(cat "$macos_summary_file")
    fi
    
    # Create comprehensive summary
    cat > "$summary_file" << EOF
{
    "orchestrator": {
        "timestamp": "${TIMESTAMP}",
        "version": "1.0"
    },
    "platforms": {
        "linux_pipewire": $linux_summary,
        "windows": $windows_summary,
        "macos_coreaudio": $macos_summary
    }
}
EOF
    
    # Create HTML report
    cat > "$report_file" << EOF
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Cross-Platform Audio Capture Test Report</title>
    <style>
        body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; margin: 0; padding: 20px; background-color: #f5f5f5; }
        .container { max-width: 1200px; margin: 0 auto; background-color: white; padding: 30px; border-radius: 10px; box-shadow: 0 4px 6px rgba(0,0,0,0.1); }
        .header { text-align: center; margin-bottom: 40px; }
        .header h1 { color: #2c3e50; margin-bottom: 10px; }
        .platform-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(350px, 1fr)); gap: 20px; margin-bottom: 30px; }
        .platform-card { border: 1px solid #ddd; border-radius: 8px; overflow: hidden; }
        .platform-header { padding: 15px; color: white; font-weight: bold; }
        .linux { background-color: #e74c3c; }
        .windows { background-color: #3498db; }
        .macos { background-color: #95a5a6; }
        .platform-body { padding: 20px; }
        .status-badge { display: inline-block; padding: 4px 12px; border-radius: 20px; font-size: 12px; font-weight: bold; }
        .success { background-color: #d4edda; color: #155724; }
        .warning { background-color: #fff3cd; color: #856404; }
        .info { background-color: #d1ecf1; color: #0c5460; }
        .instructions { background-color: #f8f9fa; padding: 20px; border-radius: 8px; margin-top: 20px; }
        pre { background-color: #f8f9fa; padding: 15px; border-radius: 4px; overflow-x: auto; font-size: 12px; }
    </style>
</head>
<body>
    <div class="container">
        <div class="header">
            <h1>🎭 Cross-Platform Audio Capture Test Report</h1>
            <p>Generated: ${TIMESTAMP}</p>
            <p>Testing library and dynamic_vlc example across Linux, Windows, and macOS</p>
        </div>
        
        <div class="platform-grid">
            <div class="platform-card">
                <div class="platform-header linux">🐧 Linux PipeWire</div>
                <div class="platform-body">
                    <div style="margin-bottom: 15px;">
                        <span class="status-badge success">Automated Testing</span>
                    </div>
                    <p><strong>Features:</strong> Native PipeWire, VLC integration, automated testing</p>
                    <p><strong>Status:</strong> Tests run automatically in container</p>
                    <p><strong>Results:</strong> Check linux-pipewire directory for detailed logs</p>
                </div>
            </div>
            
            <div class="platform-card">
                <div class="platform-header windows">🪟 Windows</div>
                <div class="platform-body">
                    <div style="margin-bottom: 15px;">
                        <span class="status-badge warning">Manual Testing Required</span>
                    </div>
                    <p><strong>Features:</strong> WASAPI, DirectSound, VLC integration</p>
                    <p><strong>Access:</strong> <a href="http://localhost:8006" target="_blank">http://localhost:8006</a></p>
                    <p><strong>Setup:</strong> Run C:\\scripts\\setup-windows-test.ps1</p>
                    <p><strong>Test:</strong> Run C:\\test-windows.ps1</p>
                </div>
            </div>
            
            <div class="platform-card">
                <div class="platform-header macos">🍎 macOS Core Audio</div>
                <div class="platform-body">
                    <div style="margin-bottom: 15px;">
                        <span class="status-badge info">Container Testing</span>
                    </div>
                    <p><strong>Features:</strong> Core Audio, VLC integration, BlackHole driver</p>
                    <p><strong>Status:</strong> Tests run in macOS container</p>
                    <p><strong>Results:</strong> Check macos-coreaudio directory for detailed logs</p>
                </div>
            </div>
        </div>
        
        <div class="instructions">
            <h3>📋 Testing Instructions</h3>
            <h4>Automated Platforms (Linux, macOS):</h4>
            <ul>
                <li>Tests run automatically when containers start</li>
                <li>Check the respective result directories for detailed logs</li>
                <li>Look for test_summary_*.json files for quick status</li>
            </ul>
            
            <h4>Manual Platform (Windows):</h4>
            <ol>
                <li>Access Windows at <a href="http://localhost:8006">http://localhost:8006</a></li>
                <li>Wait for Windows to fully boot (may take 5-10 minutes)</li>
                <li>Open PowerShell as Administrator</li>
                <li>Run: <code>PowerShell -ExecutionPolicy Bypass -File C:\\scripts\\setup-windows-test.ps1</code></li>
                <li>After setup, run: <code>PowerShell -ExecutionPolicy Bypass -File C:\\test-windows.ps1</code></li>
                <li>Results will be saved to C:\\test-results\\windows\\</li>
            </ol>
        </div>
        
        <div style="margin-top: 30px;">
            <h3>📊 Raw Test Data</h3>
            <pre>$(cat "$summary_file" | jq . 2>/dev/null || echo "Summary data not available")</pre>
        </div>
        
        <div style="text-align: center; margin-top: 30px; color: #7f8c8d;">
            <p>Generated by Rust Cross-Platform Audio Capture Test Orchestrator</p>
        </div>
    </div>
</body>
</html>
EOF
    
    log_success "Comprehensive test report generated: $report_file"
}

# Main orchestration function
main() {
    print_banner
    
    setup_environment
    
    # Test platforms (in parallel where possible)
    log_info "Starting platform tests..."
    
    # Linux can run immediately
    test_linux_pipewire &
    local linux_pid=$!
    
    # Windows needs manual intervention
    test_windows &
    local windows_pid=$!
    
    # macOS can run automatically but takes time
    test_macos &
    local macos_pid=$!
    
    # Wait for automated tests to complete
    log_info "Waiting for automated tests to complete..."
    
    wait $linux_pid || log_warning "Linux test process failed"
    wait $macos_pid || log_warning "macOS test process failed"
    wait $windows_pid || log_warning "Windows setup process failed"
    
    # Generate comprehensive report
    generate_comprehensive_report
    
    log_success "🎉 Test orchestration completed!"
    log_info "📊 Comprehensive report: $RESULTS_DIR/reports/comprehensive_test_report_${TIMESTAMP}.html"
    log_info "🪟 Windows testing: Access http://localhost:8006 for manual testing"
}

# Run main function
main "$@"
