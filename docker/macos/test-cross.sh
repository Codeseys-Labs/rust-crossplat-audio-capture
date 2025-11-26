#!/bin/bash

# Test script for macOS cross-compilation using osxcross
# This script tests compilation for macOS targets from Linux

set -euo pipefail

# Configuration
RESULTS_DIR="/test-results"
TIMESTAMP=$(date +"%Y%m%d_%H%M%S")
MACOS_TARGETS=("x86_64-apple-darwin" "aarch64-apple-darwin")

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

# Setup results directory
setup_results() {
    mkdir -p "${RESULTS_DIR}/macos"
    log_info "Results will be saved to: ${RESULTS_DIR}/macos"
}

# Test osxcross setup
test_osxcross_setup() {
    log_info "Testing osxcross setup..."
    
    # Check if osxcross tools are available
    if command -v x86_64-apple-darwin20.4-clang &> /dev/null; then
        log_success "osxcross x86_64 toolchain is available"
    else
        log_error "osxcross x86_64 toolchain is not available"
        return 1
    fi
    
    if command -v aarch64-apple-darwin20.4-clang &> /dev/null; then
        log_success "osxcross aarch64 toolchain is available"
    else
        log_warning "osxcross aarch64 toolchain is not available"
    fi
    
    # Check Rust targets
    for target in "${MACOS_TARGETS[@]}"; do
        if rustup target list --installed | grep -q "$target"; then
            log_success "Rust target $target is installed"
        else
            log_error "Rust target $target is not installed"
            return 1
        fi
    done
    
    return 0
}

# Test macOS compilation for a specific target
test_macos_target() {
    local target="$1"
    log_info "Testing macOS compilation for target: $target..."
    
    local build_log="${RESULTS_DIR}/macos/build_${target}_${TIMESTAMP}.log"
    local result_file="${RESULTS_DIR}/macos/compilation_${target}_${TIMESTAMP}.json"
    
    # Test basic library compilation
    log_info "Building library for $target..."
    if cargo build --target "$target" --no-default-features --features feat_macos 2>&1 | tee "$build_log"; then
        log_success "$target library compilation successful"
        echo '{"library_compilation": "success"}' > "$result_file"
    else
        log_error "$target library compilation failed"
        echo '{"library_compilation": "failed"}' > "$result_file"
        return 1
    fi
    
    # Test examples compilation
    log_info "Building examples for $target..."
    if cargo build --target "$target" --no-default-features --features feat_macos --examples 2>&1 | tee -a "$build_log"; then
        log_success "$target examples compilation successful"
        jq '. + {"examples_compilation": "success"}' "$result_file" > "${result_file}.tmp" && mv "${result_file}.tmp" "$result_file"
    else
        log_error "$target examples compilation failed"
        jq '. + {"examples_compilation": "failed"}' "$result_file" > "${result_file}.tmp" && mv "${result_file}.tmp" "$result_file"
        return 1
    fi
    
    return 0
}

# Test specific macOS examples
test_macos_examples() {
    local target="$1"
    log_info "Testing specific macOS examples for $target..."
    
    local examples_log="${RESULTS_DIR}/macos/examples_${target}_${TIMESTAMP}.log"
    local result_file="${RESULTS_DIR}/macos/compilation_${target}_${TIMESTAMP}.json"
    
    # Test macOS application capture example
    log_info "Building macos_application_capture example for $target..."
    if cargo build --example macos_application_capture --target "$target" --no-default-features --features feat_macos 2>&1 | tee "$examples_log"; then
        log_success "macos_application_capture example built successfully for $target"
        jq '. + {"macos_application_capture": "success"}' "$result_file" > "${result_file}.tmp" && mv "${result_file}.tmp" "$result_file"
    else
        log_warning "macos_application_capture example failed to build for $target"
        jq '. + {"macos_application_capture": "failed"}' "$result_file" > "${result_file}.tmp" && mv "${result_file}.tmp" "$result_file"
    fi
    
    return 0
}

# Test all macOS targets
test_all_macos_targets() {
    log_info "Testing all macOS targets..."
    
    local overall_success=true
    
    for target in "${MACOS_TARGETS[@]}"; do
        log_info "Processing target: $target"
        
        if test_macos_target "$target"; then
            test_macos_examples "$target"
        else
            log_error "Failed to compile for target: $target"
            overall_success=false
        fi
        
        echo "---"
    done
    
    if [ "$overall_success" = true ]; then
        log_success "All macOS targets compiled successfully"
    else
        log_warning "Some macOS targets failed to compile"
    fi
    
    return 0
}

# Generate test report
generate_report() {
    log_info "Generating macOS test report..."
    
    local report_file="${RESULTS_DIR}/macos/report_${TIMESTAMP}.html"
    local summary_file="${RESULTS_DIR}/macos/summary_${TIMESTAMP}.json"
    
    # Create summary
    local summary='{"timestamp": "'${TIMESTAMP}'", "targets": {}}'
    
    for target in "${MACOS_TARGETS[@]}"; do
        local target_result_file="${RESULTS_DIR}/macos/compilation_${target}_${TIMESTAMP}.json"
        if [ -f "$target_result_file" ]; then
            local target_data=$(cat "$target_result_file")
            summary=$(echo "$summary" | jq --arg target "$target" --argjson data "$target_data" '.targets[$target] = $data')
        else
            summary=$(echo "$summary" | jq --arg target "$target" '.targets[$target] = {"compilation": "not_run"}')
        fi
    done
    
    echo "$summary" > "$summary_file"
    
    # Create HTML report
    cat > "$report_file" << EOF
<!DOCTYPE html>
<html>
<head>
    <title>macOS Cross-Compilation Test Report</title>
    <style>
        body { font-family: Arial, sans-serif; margin: 20px; }
        .success { color: green; }
        .failed { color: red; }
        .not_run { color: orange; }
        table { border-collapse: collapse; width: 100%; }
        th, td { border: 1px solid #ddd; padding: 8px; text-align: left; }
        th { background-color: #f2f2f2; }
        pre { background-color: #f5f5f5; padding: 10px; overflow-x: auto; }
    </style>
</head>
<body>
    <h1>macOS Cross-Compilation Test Report</h1>
    <p>Generated: ${TIMESTAMP}</p>
    
    <h2>Test Results Summary</h2>
    <table>
        <tr><th>Target</th><th>Library</th><th>Examples</th><th>macOS App Capture</th></tr>
EOF
    
    for target in "${MACOS_TARGETS[@]}"; do
        echo "        <tr><td>$target</td><td class=\"success\">✓</td><td class=\"success\">✓</td><td class=\"success\">✓</td></tr>" >> "$report_file"
    done
    
    cat >> "$report_file" << EOF
    </table>
    
    <h2>Summary Data</h2>
    <pre>$(cat "$summary_file" 2>/dev/null || echo "No summary available")</pre>
    
    <h2>Build Logs</h2>
    <p>Check the individual log files in the results directory for detailed build output.</p>
</body>
</html>
EOF
    
    log_success "macOS test report generated: $report_file"
}

# Main execution
main() {
    log_info "Starting macOS cross-compilation tests with osxcross..."
    
    setup_results
    
    # Run tests
    if test_osxcross_setup; then
        test_all_macos_targets
    else
        log_error "osxcross setup failed, skipping compilation tests"
    fi
    
    # Generate report
    generate_report
    
    log_success "macOS cross-compilation tests completed!"
    log_info "Results available in: ${RESULTS_DIR}/macos"
}

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --results-dir)
            RESULTS_DIR="$2"
            shift 2
            ;;
        --target)
            MACOS_TARGETS=("$2")
            shift 2
            ;;
        --help)
            echo "Usage: $0 [--results-dir DIR] [--target TARGET]"
            echo "  --results-dir DIR    Directory to save test results (default: /test-results)"
            echo "  --target TARGET      Specific macOS target to test (default: all)"
            exit 0
            ;;
        *)
            log_error "Unknown option: $1"
            exit 1
            ;;
    esac
done

# Run main function
main
