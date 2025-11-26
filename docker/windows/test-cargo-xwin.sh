#!/bin/bash

# Test script for Windows cross-compilation using cargo-xwin
# This script tests compilation and basic functionality for Windows targets

set -euo pipefail

# Configuration
RESULTS_DIR="/test-results"
TIMESTAMP=$(date +"%Y%m%d_%H%M%S")
WINDOWS_TARGET="x86_64-pc-windows-msvc"

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
    mkdir -p "${RESULTS_DIR}/windows"
    log_info "Results will be saved to: ${RESULTS_DIR}/windows"
}

# Test cargo-xwin installation and setup
test_cargo_xwin_setup() {
    log_info "Testing cargo-xwin setup..."
    
    # Check cargo-xwin version
    if cargo xwin --version > "${RESULTS_DIR}/windows/cargo-xwin-version_${TIMESTAMP}.txt" 2>&1; then
        log_success "cargo-xwin is installed: $(cargo xwin --version)"
    else
        log_error "cargo-xwin is not properly installed"
        return 1
    fi
    
    # Check Windows target
    if rustup target list --installed | grep -q "$WINDOWS_TARGET"; then
        log_success "Windows target $WINDOWS_TARGET is installed"
    else
        log_error "Windows target $WINDOWS_TARGET is not installed"
        return 1
    fi
    
    return 0
}

# Test basic Windows compilation
test_windows_compilation() {
    log_info "Testing Windows compilation with cargo-xwin..."
    
    local build_log="${RESULTS_DIR}/windows/build_${TIMESTAMP}.log"
    local result_file="${RESULTS_DIR}/windows/compilation_${TIMESTAMP}.json"
    
    # Test basic library compilation
    log_info "Building library for Windows..."
    if cargo xwin build --target "$WINDOWS_TARGET" --no-default-features --features feat_windows 2>&1 | tee "$build_log"; then
        log_success "Windows library compilation successful"
        echo '{"library_compilation": "success"}' > "$result_file"
    else
        log_error "Windows library compilation failed"
        echo '{"library_compilation": "failed"}' > "$result_file"
        return 1
    fi
    
    # Test examples compilation
    log_info "Building examples for Windows..."
    if cargo xwin build --target "$WINDOWS_TARGET" --no-default-features --features feat_windows --examples 2>&1 | tee -a "$build_log"; then
        log_success "Windows examples compilation successful"
        jq '. + {"examples_compilation": "success"}' "$result_file" > "${result_file}.tmp" && mv "${result_file}.tmp" "$result_file"
    else
        log_error "Windows examples compilation failed"
        jq '. + {"examples_compilation": "failed"}' "$result_file" > "${result_file}.tmp" && mv "${result_file}.tmp" "$result_file"
        return 1
    fi
    
    return 0
}

# Test specific Windows examples
test_windows_examples() {
    log_info "Testing specific Windows examples..."
    
    local examples_log="${RESULTS_DIR}/windows/examples_${TIMESTAMP}.log"
    local result_file="${RESULTS_DIR}/windows/compilation_${TIMESTAMP}.json"
    
    # Test Windows application capture example
    log_info "Building windows_application_capture example..."
    if cargo xwin build --example windows_application_capture --target "$WINDOWS_TARGET" --no-default-features --features feat_windows 2>&1 | tee "$examples_log"; then
        log_success "windows_application_capture example built successfully"
        jq '. + {"windows_application_capture": "success"}' "$result_file" > "${result_file}.tmp" && mv "${result_file}.tmp" "$result_file"
    else
        log_warning "windows_application_capture example failed to build"
        jq '. + {"windows_application_capture": "failed"}' "$result_file" > "${result_file}.tmp" && mv "${result_file}.tmp" "$result_file"
    fi
    
    # Test Windows APIs example
    log_info "Building windows_apis example..."
    if cargo xwin build --example windows_apis --target "$WINDOWS_TARGET" --no-default-features --features feat_windows 2>&1 | tee -a "$examples_log"; then
        log_success "windows_apis example built successfully"
        jq '. + {"windows_apis": "success"}' "$result_file" > "${result_file}.tmp" && mv "${result_file}.tmp" "$result_file"
    else
        log_warning "windows_apis example failed to build"
        jq '. + {"windows_apis": "failed"}' "$result_file" > "${result_file}.tmp" && mv "${result_file}.tmp" "$result_file"
    fi
    
    return 0
}

# Test Windows binary execution with wine (if available)
test_windows_execution() {
    log_info "Testing Windows binary execution with wine..."
    
    local exec_log="${RESULTS_DIR}/windows/execution_${TIMESTAMP}.log"
    local result_file="${RESULTS_DIR}/windows/compilation_${TIMESTAMP}.json"
    
    # Check if wine is available
    if ! command -v wine &> /dev/null; then
        log_warning "Wine not available, skipping execution tests"
        jq '. + {"execution_test": "skipped"}' "$result_file" > "${result_file}.tmp" && mv "${result_file}.tmp" "$result_file"
        return 0
    fi
    
    # Try to run a simple test
    log_info "Attempting to run Windows binary with wine..."
    if timeout 30s wine target/"$WINDOWS_TARGET"/debug/examples/windows_apis.exe --help 2>&1 | tee "$exec_log"; then
        log_success "Windows binary executed successfully with wine"
        jq '. + {"execution_test": "success"}' "$result_file" > "${result_file}.tmp" && mv "${result_file}.tmp" "$result_file"
    else
        log_warning "Windows binary execution failed or timed out"
        jq '. + {"execution_test": "failed"}' "$result_file" > "${result_file}.tmp" && mv "${result_file}.tmp" "$result_file"
    fi
    
    return 0
}

# Generate test report
generate_report() {
    log_info "Generating Windows test report..."
    
    local report_file="${RESULTS_DIR}/windows/report_${TIMESTAMP}.html"
    local summary_file="${RESULTS_DIR}/windows/summary_${TIMESTAMP}.json"
    
    # Create summary
    if [ -f "${RESULTS_DIR}/windows/compilation_${TIMESTAMP}.json" ]; then
        cp "${RESULTS_DIR}/windows/compilation_${TIMESTAMP}.json" "$summary_file"
    else
        echo '{"error": "No compilation results found"}' > "$summary_file"
    fi
    
    # Create HTML report
    cat > "$report_file" << EOF
<!DOCTYPE html>
<html>
<head>
    <title>Windows Cross-Compilation Test Report</title>
    <style>
        body { font-family: Arial, sans-serif; margin: 20px; }
        .success { color: green; }
        .failed { color: red; }
        .skipped { color: orange; }
        pre { background-color: #f5f5f5; padding: 10px; overflow-x: auto; }
    </style>
</head>
<body>
    <h1>Windows Cross-Compilation Test Report</h1>
    <p>Generated: ${TIMESTAMP}</p>
    <p>Target: ${WINDOWS_TARGET}</p>
    
    <h2>Test Results</h2>
    <pre>$(cat "$summary_file" 2>/dev/null || echo "No results available")</pre>
    
    <h2>Build Logs</h2>
    <p>Check the log files in the results directory for detailed output.</p>
</body>
</html>
EOF
    
    log_success "Windows test report generated: $report_file"
}

# Main execution
main() {
    log_info "Starting Windows cross-compilation tests with cargo-xwin..."
    
    setup_results
    
    # Run tests
    if test_cargo_xwin_setup; then
        test_windows_compilation
        test_windows_examples
        test_windows_execution
    else
        log_error "cargo-xwin setup failed, skipping compilation tests"
    fi
    
    # Generate report
    generate_report
    
    log_success "Windows cross-compilation tests completed!"
    log_info "Results available in: ${RESULTS_DIR}/windows"
}

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --results-dir)
            RESULTS_DIR="$2"
            shift 2
            ;;
        --target)
            WINDOWS_TARGET="$2"
            shift 2
            ;;
        --help)
            echo "Usage: $0 [--results-dir DIR] [--target TARGET]"
            echo "  --results-dir DIR    Directory to save test results (default: /test-results)"
            echo "  --target TARGET      Windows target (default: x86_64-pc-windows-msvc)"
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
