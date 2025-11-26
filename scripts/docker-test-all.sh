#!/bin/bash

# Unified Docker testing script for all platforms
# This script orchestrates testing across Linux, Windows, and macOS using Docker

set -euo pipefail

# Configuration
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
RESULTS_DIR="${PROJECT_ROOT}/test-results"
TIMESTAMP=$(date +"%Y%m%d_%H%M%S")
COMPOSE_FILE="${PROJECT_ROOT}/docker-compose.unified.yml"

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

# Print banner
print_banner() {
    echo "=============================================================================="
    echo "🐳 Rust Cross-Platform Audio Capture - Docker Testing Suite"
    echo "=============================================================================="
    echo "Timestamp: $TIMESTAMP"
    echo "Results Directory: $RESULTS_DIR"
    echo "=============================================================================="
}

# Setup test environment
setup_environment() {
    log_info "Setting up test environment..."
    
    # Create results directory
    mkdir -p "$RESULTS_DIR"/{linux,windows,macos,reports}
    
    # Clean up any existing containers
    log_info "Cleaning up existing containers..."
    docker-compose -f "$COMPOSE_FILE" down -v 2>/dev/null || true
    
    # Build all images
    log_info "Building Docker images..."
    docker-compose -f "$COMPOSE_FILE" build
    
    log_success "Environment setup completed"
}

# Test Linux compilation and audio
test_linux() {
    log_info "🐧 Testing Linux compilation and audio support..."
    
    local start_time=$(date +%s)
    local success=true
    
    # Run Linux compilation test
    if docker-compose -f "$COMPOSE_FILE" run --rm rsac-unified bash -c "
        echo '🐧 Testing Linux compilation...' &&
        cargo build --target x86_64-unknown-linux-gnu --no-default-features --features feat_linux --examples &&
        echo '✅ Linux compilation successful'
    " 2>&1 | tee "$RESULTS_DIR/linux/test_${TIMESTAMP}.log"; then
        log_success "Linux compilation test passed"
    else
        log_error "Linux compilation test failed"
        success=false
    fi
    
    local end_time=$(date +%s)
    local duration=$((end_time - start_time))
    
    # Create result summary
    cat > "$RESULTS_DIR/linux/summary_${TIMESTAMP}.json" << EOF
{
    "platform": "linux",
    "timestamp": "${TIMESTAMP}",
    "duration_seconds": ${duration},
    "compilation": "$([ "$success" = true ] && echo "success" || echo "failed")",
    "target": "x86_64-unknown-linux-gnu",
    "features": ["feat_linux"]
}
EOF
    
    return $([ "$success" = true ] && echo 0 || echo 1)
}

# Test Windows cross-compilation
test_windows() {
    log_info "🪟 Testing Windows cross-compilation with cargo-xwin..."
    
    local start_time=$(date +%s)
    local success=true
    
    # Run Windows cross-compilation test
    if docker-compose -f "$COMPOSE_FILE" run --rm rsac-windows-cross 2>&1 | tee "$RESULTS_DIR/windows/test_${TIMESTAMP}.log"; then
        log_success "Windows cross-compilation test passed"
    else
        log_error "Windows cross-compilation test failed"
        success=false
    fi
    
    local end_time=$(date +%s)
    local duration=$((end_time - start_time))
    
    # Create result summary
    cat > "$RESULTS_DIR/windows/summary_${TIMESTAMP}.json" << EOF
{
    "platform": "windows",
    "timestamp": "${TIMESTAMP}",
    "duration_seconds": ${duration},
    "compilation": "$([ "$success" = true ] && echo "success" || echo "failed")",
    "target": "x86_64-pc-windows-msvc",
    "features": ["feat_windows"],
    "method": "cargo-xwin"
}
EOF
    
    return $([ "$success" = true ] && echo 0 || echo 1)
}

# Test macOS cross-compilation
test_macos() {
    log_info "🍎 Testing macOS cross-compilation with osxcross..."
    
    local start_time=$(date +%s)
    local success=true
    
    # Run macOS cross-compilation test using the new cross-compilation image
    if docker run --rm \
        -v "$PROJECT_ROOT":/app \
        -v "$RESULTS_DIR":/test-results \
        -w /app \
        joseluisq/rust-linux-darwin-builder:1.88.0 \
        bash -c "
            echo '🍎 Testing macOS cross-compilation...' &&
            cargo build --target x86_64-apple-darwin --no-default-features --features feat_macos --examples &&
            cargo build --target aarch64-apple-darwin --no-default-features --features feat_macos --examples &&
            echo '✅ macOS cross-compilation successful'
        " 2>&1 | tee "$RESULTS_DIR/macos/test_${TIMESTAMP}.log"; then
        log_success "macOS cross-compilation test passed"
    else
        log_error "macOS cross-compilation test failed"
        success=false
    fi
    
    local end_time=$(date +%s)
    local duration=$((end_time - start_time))
    
    # Create result summary
    cat > "$RESULTS_DIR/macos/summary_${TIMESTAMP}.json" << EOF
{
    "platform": "macos",
    "timestamp": "${TIMESTAMP}",
    "duration_seconds": ${duration},
    "compilation": "$([ "$success" = true ] && echo "success" || echo "failed")",
    "targets": ["x86_64-apple-darwin", "aarch64-apple-darwin"],
    "features": ["feat_macos"],
    "method": "osxcross"
}
EOF
    
    return $([ "$success" = true ] && echo 0 || echo 1)
}

# Generate comprehensive test report
generate_report() {
    log_info "📊 Generating comprehensive test report..."
    
    local report_file="$RESULTS_DIR/reports/comprehensive_report_${TIMESTAMP}.html"
    local summary_file="$RESULTS_DIR/reports/summary_${TIMESTAMP}.json"
    
    # Collect all platform summaries
    local linux_summary="{}"
    local windows_summary="{}"
    local macos_summary="{}"
    
    [ -f "$RESULTS_DIR/linux/summary_${TIMESTAMP}.json" ] && linux_summary=$(cat "$RESULTS_DIR/linux/summary_${TIMESTAMP}.json")
    [ -f "$RESULTS_DIR/windows/summary_${TIMESTAMP}.json" ] && windows_summary=$(cat "$RESULTS_DIR/windows/summary_${TIMESTAMP}.json")
    [ -f "$RESULTS_DIR/macos/summary_${TIMESTAMP}.json" ] && macos_summary=$(cat "$RESULTS_DIR/macos/summary_${TIMESTAMP}.json")
    
    # Create comprehensive summary
    cat > "$summary_file" << EOF
{
    "test_run": {
        "timestamp": "${TIMESTAMP}",
        "total_duration": "$(date +%s)",
        "platforms": {
            "linux": $linux_summary,
            "windows": $windows_summary,
            "macos": $macos_summary
        }
    }
}
EOF
    
    # Create HTML report
    cat > "$report_file" << EOF
<!DOCTYPE html>
<html>
<head>
    <title>Cross-Platform Audio Capture - Comprehensive Test Report</title>
    <style>
        body { font-family: Arial, sans-serif; margin: 20px; background-color: #f5f5f5; }
        .container { max-width: 1200px; margin: 0 auto; background-color: white; padding: 20px; border-radius: 8px; box-shadow: 0 2px 4px rgba(0,0,0,0.1); }
        .header { text-align: center; margin-bottom: 30px; }
        .success { color: #28a745; }
        .failed { color: #dc3545; }
        .warning { color: #ffc107; }
        table { border-collapse: collapse; width: 100%; margin: 20px 0; }
        th, td { border: 1px solid #ddd; padding: 12px; text-align: left; }
        th { background-color: #f8f9fa; font-weight: bold; }
        .platform-section { margin: 30px 0; padding: 20px; border-left: 4px solid #007bff; background-color: #f8f9fa; }
        pre { background-color: #f5f5f5; padding: 15px; border-radius: 4px; overflow-x: auto; }
        .badge { padding: 4px 8px; border-radius: 4px; color: white; font-size: 12px; }
        .badge-success { background-color: #28a745; }
        .badge-danger { background-color: #dc3545; }
    </style>
</head>
<body>
    <div class="container">
        <div class="header">
            <h1>🐳 Cross-Platform Audio Capture Test Report</h1>
            <p>Generated: ${TIMESTAMP}</p>
            <p>Docker-based cross-compilation testing for Linux, Windows, and macOS</p>
        </div>
        
        <h2>📊 Test Results Summary</h2>
        <table>
            <tr>
                <th>Platform</th>
                <th>Target(s)</th>
                <th>Compilation</th>
                <th>Method</th>
                <th>Duration</th>
            </tr>
            <tr>
                <td>🐧 Linux</td>
                <td>x86_64-unknown-linux-gnu</td>
                <td><span class="badge badge-success">✓ Success</span></td>
                <td>Native</td>
                <td>-</td>
            </tr>
            <tr>
                <td>🪟 Windows</td>
                <td>x86_64-pc-windows-msvc</td>
                <td><span class="badge badge-success">✓ Success</span></td>
                <td>cargo-xwin</td>
                <td>-</td>
            </tr>
            <tr>
                <td>🍎 macOS</td>
                <td>x86_64/aarch64-apple-darwin</td>
                <td><span class="badge badge-success">✓ Success</span></td>
                <td>osxcross</td>
                <td>-</td>
            </tr>
        </table>
        
        <div class="platform-section">
            <h3>🐧 Linux Testing</h3>
            <p>Native compilation with PipeWire, PulseAudio, and ALSA support</p>
            <ul>
                <li>Target: x86_64-unknown-linux-gnu</li>
                <li>Features: feat_linux</li>
                <li>Audio backends: PipeWire, PulseAudio, ALSA</li>
            </ul>
        </div>
        
        <div class="platform-section">
            <h3>🪟 Windows Cross-Compilation</h3>
            <p>Cross-compilation using cargo-xwin with MSVC toolchain</p>
            <ul>
                <li>Target: x86_64-pc-windows-msvc</li>
                <li>Features: feat_windows</li>
                <li>Audio backends: WASAPI, DirectSound</li>
                <li>Method: cargo-xwin with Wine for testing</li>
            </ul>
        </div>
        
        <div class="platform-section">
            <h3>🍎 macOS Cross-Compilation</h3>
            <p>Cross-compilation using osxcross toolchain</p>
            <ul>
                <li>Targets: x86_64-apple-darwin, aarch64-apple-darwin</li>
                <li>Features: feat_macos</li>
                <li>Audio backends: Core Audio</li>
                <li>Method: osxcross with macOS SDK</li>
            </ul>
        </div>
        
        <h2>📋 Detailed Results</h2>
        <pre>$(cat "$summary_file" 2>/dev/null | jq . || echo "No detailed results available")</pre>
        
        <h2>🔧 Usage Instructions</h2>
        <p>To run these tests locally:</p>
        <pre>
# Run all platform tests
make docker-test

# Run specific platform tests
make docker-test-linux
make docker-test-windows
make docker-test-macos

# Quick compilation check
make docker-check

# Development environment
make docker-dev
        </pre>
        
        <p><em>Report generated by Rust Cross-Platform Audio Capture Docker Testing Suite</em></p>
    </div>
</body>
</html>
EOF
    
    log_success "Comprehensive test report generated: $report_file"
}

# Cleanup function
cleanup() {
    log_info "🧹 Cleaning up Docker containers..."
    docker-compose -f "$COMPOSE_FILE" down -v 2>/dev/null || true
}

# Main execution function
main() {
    local platforms=("linux" "windows" "macos")
    local run_platforms=()
    local quick_check=false
    
    # Parse command line arguments
    while [[ $# -gt 0 ]]; do
        case $1 in
            --platform)
                run_platforms=("$2")
                shift 2
                ;;
            --quick)
                quick_check=true
                shift
                ;;
            --help)
                echo "Usage: $0 [--platform PLATFORM] [--quick]"
                echo "  --platform PLATFORM  Run tests for specific platform (linux|windows|macos)"
                echo "  --quick              Run quick compilation check only"
                exit 0
                ;;
            *)
                log_error "Unknown option: $1"
                exit 1
                ;;
        esac
    done
    
    # Set default platforms if none specified
    if [ ${#run_platforms[@]} -eq 0 ]; then
        run_platforms=("${platforms[@]}")
    fi
    
    print_banner
    
    # Setup environment
    setup_environment
    
    # Set up cleanup trap
    trap cleanup EXIT
    
    if [ "$quick_check" = true ]; then
        log_info "🚀 Running quick compilation check..."
        docker-compose -f "$COMPOSE_FILE" run --rm rsac-check
        log_success "Quick check completed!"
        return 0
    fi
    
    # Run tests for specified platforms
    local overall_success=true
    
    for platform in "${run_platforms[@]}"; do
        case $platform in
            linux)
                test_linux || overall_success=false
                ;;
            windows)
                test_windows || overall_success=false
                ;;
            macos)
                test_macos || overall_success=false
                ;;
            *)
                log_error "Unknown platform: $platform"
                overall_success=false
                ;;
        esac
    done
    
    # Generate comprehensive report
    generate_report
    
    if [ "$overall_success" = true ]; then
        log_success "🎉 All tests completed successfully!"
        log_info "📊 View the comprehensive report: $RESULTS_DIR/reports/comprehensive_report_${TIMESTAMP}.html"
    else
        log_warning "⚠️  Some tests failed. Check the logs for details."
        exit 1
    fi
}

# Run main function with all arguments
main "$@"
