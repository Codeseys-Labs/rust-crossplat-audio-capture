#!/bin/bash

# Windows Cross-Compilation Testing Script
# Tests the Rust audio capture library and dynamic_vlc example for Windows using cargo-xwin

set -euo pipefail

# Configuration
WORKSPACE="/workspace"
RESULTS_DIR="/test-results/windows-cross"
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

print_banner() {
    echo "=============================================================================="
    echo "🪟 Windows Cross-Compilation Testing (No KVM Required)"
    echo "=============================================================================="
    echo "Timestamp: $TIMESTAMP"
    echo "Target: $WINDOWS_TARGET"
    echo "Workspace: $WORKSPACE"
    echo "Results: $RESULTS_DIR"
    echo "=============================================================================="
}

# Setup test environment
setup_environment() {
    log_info "Setting up Windows cross-compilation test environment..."
    
    mkdir -p "$RESULTS_DIR"
    cd "$WORKSPACE"
    
    log_success "Environment setup completed"
}

# Test cargo-xwin setup
test_cargo_xwin_setup() {
    log_info "Testing cargo-xwin setup..."
    
    local setup_log="$RESULTS_DIR/cargo_xwin_setup_${TIMESTAMP}.log"
    
    {
        echo "=== cargo-xwin Version ==="
        cargo xwin --version
        
        echo -e "\n=== Rust Toolchain ==="
        rustc --version
        cargo --version
        
        echo -e "\n=== Windows Targets ==="
        rustup target list --installed | grep windows
        
        echo -e "\n=== LLVM Tools ==="
        rustup component list --installed | grep llvm
        
    } > "$setup_log" 2>&1
    
    if grep -q "cargo-xwin" "$setup_log"; then
        log_success "cargo-xwin setup is working"
        return 0
    else
        log_error "cargo-xwin setup failed"
        return 1
    fi
}

# Build the Rust library for Windows
build_library() {
    log_info "Building Rust audio capture library for Windows..."
    
    local build_log="$RESULTS_DIR/library_build_${TIMESTAMP}.log"
    
    if cargo xwin build --target "$WINDOWS_TARGET" --no-default-features --features feat_windows 2>&1 | tee "$build_log"; then
        log_success "Windows library build successful"
        return 0
    else
        log_error "Windows library build failed"
        return 1
    fi
}

# Build the dynamic_vlc example for Windows
build_dynamic_vlc_example() {
    log_info "Building dynamic_vlc example for Windows..."
    
    local build_log="$RESULTS_DIR/dynamic_vlc_build_${TIMESTAMP}.log"
    
    # Check if dynamic_vlc example exists
    if [ ! -f "examples/dynamic_vlc.rs" ]; then
        log_warning "dynamic_vlc example not found, skipping..."
        return 1
    fi
    
    if cargo xwin build --example dynamic_vlc --target "$WINDOWS_TARGET" --no-default-features --features feat_windows 2>&1 | tee "$build_log"; then
        log_success "dynamic_vlc example build successful"
        return 0
    else
        log_error "dynamic_vlc example build failed"
        return 1
    fi
}

# Build Windows-specific examples
build_windows_examples() {
    log_info "Building Windows-specific examples..."
    
    local examples_log="$RESULTS_DIR/windows_examples_build_${TIMESTAMP}.log"
    local success=true
    
    # List of Windows examples to build
    local examples=(
        "windows_application_capture"
        "windows_apis"
    )
    
    for example in "${examples[@]}"; do
        if [ -f "examples/${example}.rs" ]; then
            log_info "Building ${example} example..."
            if cargo xwin build --example "$example" --target "$WINDOWS_TARGET" --no-default-features --features feat_windows 2>&1 | tee -a "$examples_log"; then
                log_success "${example} example build successful"
            else
                log_error "${example} example build failed"
                success=false
            fi
        else
            log_warning "${example} example not found"
        fi
    done
    
    return $([ "$success" = true ] && echo 0 || echo 1)
}

# Test Windows binary analysis
analyze_windows_binaries() {
    log_info "Analyzing Windows binaries..."
    
    local analysis_log="$RESULTS_DIR/binary_analysis_${TIMESTAMP}.log"
    
    {
        echo "=== Windows Binary Analysis ==="
        
        # Find Windows binaries
        local target_dir="target/$WINDOWS_TARGET/debug"
        
        if [ -d "$target_dir" ]; then
            echo "Windows binaries found in: $target_dir"
            
            # List library files
            echo -e "\n=== Library Files ==="
            find "$target_dir" -name "*.dll" -o -name "*.lib" -o -name "*.exe" | head -10
            
            # Check example binaries
            echo -e "\n=== Example Binaries ==="
            if [ -d "$target_dir/examples" ]; then
                ls -la "$target_dir/examples/" | grep "\.exe$" || echo "No .exe files found"
            fi
            
            # Check file sizes
            echo -e "\n=== Binary Sizes ==="
            find "$target_dir" -name "*.exe" -exec ls -lh {} \; | head -5
            
        else
            echo "No Windows target directory found"
        fi
        
    } > "$analysis_log" 2>&1
    
    log_success "Binary analysis completed"
    return 0
}

# Test dependency analysis
test_dependencies() {
    log_info "Testing Windows dependencies..."
    
    local deps_log="$RESULTS_DIR/dependencies_${TIMESTAMP}.log"
    
    {
        echo "=== Cargo Dependencies ==="
        cargo tree --target "$WINDOWS_TARGET" --features feat_windows || echo "Dependency tree not available"
        
        echo -e "\n=== Windows-specific Dependencies ==="
        cargo tree --target "$WINDOWS_TARGET" --features feat_windows | grep -i "windows\|win32\|wasapi\|directsound" || echo "No Windows-specific dependencies found"
        
    } > "$deps_log" 2>&1
    
    log_success "Dependency analysis completed"
    return 0
}

# Simulate Windows testing (since we can't run Windows binaries)
simulate_windows_testing() {
    log_info "Simulating Windows testing scenarios..."
    
    local simulation_log="$RESULTS_DIR/windows_simulation_${TIMESTAMP}.log"
    
    {
        echo "=== Windows Testing Simulation ==="
        echo "Since we're cross-compiling, we can't run Windows binaries directly."
        echo "However, we can verify that the binaries were built correctly."
        
        # Check if key Windows binaries exist
        local target_dir="target/$WINDOWS_TARGET/debug"
        
        echo -e "\n=== Binary Verification ==="
        
        # Check main library
        if [ -f "$target_dir/librust_crossplat_audio_capture.rlib" ] || [ -f "$target_dir/rust_crossplat_audio_capture.lib" ]; then
            echo "✅ Main library binary exists"
        else
            echo "❌ Main library binary not found"
        fi
        
        # Check dynamic_vlc example
        if [ -f "$target_dir/examples/dynamic_vlc.exe" ]; then
            echo "✅ dynamic_vlc.exe exists"
            echo "File size: $(ls -lh "$target_dir/examples/dynamic_vlc.exe" | awk '{print $5}')"
        else
            echo "❌ dynamic_vlc.exe not found"
        fi
        
        # Check Windows examples
        for example in "windows_application_capture" "windows_apis"; do
            if [ -f "$target_dir/examples/${example}.exe" ]; then
                echo "✅ ${example}.exe exists"
                echo "File size: $(ls -lh "$target_dir/examples/${example}.exe" | awk '{print $5}')"
            else
                echo "❌ ${example}.exe not found"
            fi
        done
        
        echo -e "\n=== Next Steps for Real Testing ==="
        echo "To test these binaries on actual Windows:"
        echo "1. Copy the .exe files to a Windows machine"
        echo "2. Install VLC on Windows"
        echo "3. Run the examples with VLC playing audio"
        echo "4. Verify audio capture functionality"
        
    } > "$simulation_log" 2>&1
    
    log_success "Windows testing simulation completed"
    return 0
}

# Generate test report
generate_report() {
    log_info "Generating Windows cross-compilation test report..."
    
    local report_file="$RESULTS_DIR/test_report_${TIMESTAMP}.html"
    local summary_file="$RESULTS_DIR/test_summary_${TIMESTAMP}.json"
    
    # Create JSON summary
    cat > "$summary_file" << EOF
{
    "platform": "windows-cross-compilation",
    "target": "${WINDOWS_TARGET}",
    "timestamp": "${TIMESTAMP}",
    "method": "cargo-xwin",
    "tests": {
        "cargo_xwin_setup": "$([ -f "$RESULTS_DIR/cargo_xwin_setup_${TIMESTAMP}.log" ] && echo "completed" || echo "skipped")",
        "library_build": "$([ -f "$RESULTS_DIR/library_build_${TIMESTAMP}.log" ] && echo "completed" || echo "failed")",
        "dynamic_vlc_build": "$([ -f "$RESULTS_DIR/dynamic_vlc_build_${TIMESTAMP}.log" ] && echo "completed" || echo "skipped")",
        "windows_examples_build": "$([ -f "$RESULTS_DIR/windows_examples_build_${TIMESTAMP}.log" ] && echo "completed" || echo "skipped")",
        "binary_analysis": "$([ -f "$RESULTS_DIR/binary_analysis_${TIMESTAMP}.log" ] && echo "completed" || echo "skipped")",
        "dependencies": "$([ -f "$RESULTS_DIR/dependencies_${TIMESTAMP}.log" ] && echo "completed" || echo "skipped")",
        "simulation": "$([ -f "$RESULTS_DIR/windows_simulation_${TIMESTAMP}.log" ] && echo "completed" || echo "skipped")"
    },
    "note": "Cross-compilation only - binaries need to be tested on actual Windows"
}
EOF
    
    # Create HTML report
    cat > "$report_file" << EOF
<!DOCTYPE html>
<html>
<head>
    <title>Windows Cross-Compilation Test Report</title>
    <style>
        body { font-family: Arial, sans-serif; margin: 20px; }
        .header { background-color: #f0f0f0; padding: 20px; border-radius: 5px; }
        .test-section { margin: 20px 0; padding: 15px; border-left: 4px solid #007bff; }
        .success { border-left-color: #28a745; }
        .warning { border-left-color: #ffc107; }
        .info { border-left-color: #17a2b8; }
        pre { background-color: #f8f9fa; padding: 10px; border-radius: 3px; overflow-x: auto; }
        .note { background-color: #fff3cd; padding: 15px; border-radius: 5px; margin: 20px 0; }
    </style>
</head>
<body>
    <div class="header">
        <h1>🪟 Windows Cross-Compilation Test Report</h1>
        <p>Generated: ${TIMESTAMP}</p>
        <p>Target: ${WINDOWS_TARGET}</p>
        <p>Method: cargo-xwin cross-compilation</p>
    </div>
    
    <div class="note">
        <h3>⚠️ Important Note</h3>
        <p>This report covers cross-compilation testing only. The generated Windows binaries (.exe files) 
        need to be tested on an actual Windows machine with VLC installed for complete validation.</p>
    </div>
    
    <div class="test-section success">
        <h2>Test Summary</h2>
        <pre>$(cat "$summary_file" | jq . 2>/dev/null || echo "Summary not available")</pre>
    </div>
    
    <div class="test-section info">
        <h2>Generated Windows Binaries</h2>
        <p>The following Windows binaries should be available for testing:</p>
        <ul>
            <li><strong>dynamic_vlc.exe</strong> - Main VLC integration example</li>
            <li><strong>windows_application_capture.exe</strong> - Windows-specific audio capture</li>
            <li><strong>windows_apis.exe</strong> - Windows audio API examples</li>
            <li><strong>Library files</strong> - Core audio capture library</li>
        </ul>
    </div>
    
    <div class="test-section">
        <h2>Test Logs</h2>
        <p>Detailed logs are available in the results directory:</p>
        <ul>
            <li>cargo-xwin Setup: cargo_xwin_setup_${TIMESTAMP}.log</li>
            <li>Library Build: library_build_${TIMESTAMP}.log</li>
            <li>Dynamic VLC Build: dynamic_vlc_build_${TIMESTAMP}.log</li>
            <li>Windows Examples: windows_examples_build_${TIMESTAMP}.log</li>
            <li>Binary Analysis: binary_analysis_${TIMESTAMP}.log</li>
            <li>Dependencies: dependencies_${TIMESTAMP}.log</li>
            <li>Testing Simulation: windows_simulation_${TIMESTAMP}.log</li>
        </ul>
    </div>
    
    <div class="test-section warning">
        <h2>Next Steps for Real Windows Testing</h2>
        <ol>
            <li>Copy the generated .exe files to a Windows machine</li>
            <li>Install VLC media player on Windows</li>
            <li>Run VLC with some audio content</li>
            <li>Execute the dynamic_vlc.exe and other examples</li>
            <li>Verify that audio capture works correctly</li>
        </ol>
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
    test_cargo_xwin_setup
    build_library
    build_dynamic_vlc_example
    build_windows_examples
    analyze_windows_binaries
    test_dependencies
    simulate_windows_testing
    
    # Generate report
    generate_report
    
    log_success "🎉 Windows cross-compilation testing completed!"
    log_info "📊 Results available in: $RESULTS_DIR"
    log_warning "⚠️  Remember: Binaries need to be tested on actual Windows for full validation"
}

# Run main function
main "$@"
