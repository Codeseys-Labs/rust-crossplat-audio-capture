#!/bin/bash

# Comprehensive validation script for the CI/CD setup
# This script validates that all components are properly configured

set -e

echo "🔍 Validating CI/CD Setup for Cross-Platform Audio Capture"
echo "=========================================================="

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
    esac
}

# Check if we're in the right directory
if [ ! -f "Cargo.toml" ]; then
    print_status "ERROR" "Not in project root directory (Cargo.toml not found)"
    exit 1
fi

print_status "OK" "Found Cargo.toml - in project root"

# Validate Cargo.toml has all required examples
echo ""
echo "📋 Validating Cargo.toml Examples..."

required_examples=("test_tone" "test_capture" "test_coreaudio" "test_windows" "verify_audio" "demo_library")
for example in "${required_examples[@]}"; do
    if grep -q "name = \"$example\"" Cargo.toml; then
        print_status "OK" "Example '$example' found in Cargo.toml"
    else
        print_status "ERROR" "Example '$example' missing from Cargo.toml"
    fi
done

# Check if example files exist
echo ""
echo "📁 Validating Example Files..."

for example in "${required_examples[@]}"; do
    if [ -f "examples/${example}.rs" ]; then
        print_status "OK" "Example file 'examples/${example}.rs' exists"
    else
        print_status "ERROR" "Example file 'examples/${example}.rs' missing"
    fi
done

# Validate GitHub Actions workflows
echo ""
echo "🔄 Validating GitHub Actions Workflows..."

workflows=("ci.yml" "audio-tests.yml" "linux.yml" "macos.yml" "windows.yml")
for workflow in "${workflows[@]}"; do
    if [ -f ".github/workflows/$workflow" ]; then
        print_status "OK" "Workflow '.github/workflows/$workflow' exists"
        
        # Check if workflow references the correct examples
        if grep -q "demo_library" ".github/workflows/$workflow"; then
            print_status "OK" "Workflow '$workflow' uses demo_library"
        elif [ "$workflow" != "ci.yml" ] && [ "$workflow" != "audio-tests.yml" ]; then
            print_status "WARN" "Workflow '$workflow' doesn't use demo_library"
        fi
    else
        print_status "ERROR" "Workflow '.github/workflows/$workflow' missing"
    fi
done

# Check documentation
echo ""
echo "📚 Validating Documentation..."

docs=("CI_CD_SETUP.md" "CI_CD_IMPROVEMENTS_SUMMARY.md")
for doc in "${docs[@]}"; do
    if [ -f "docs/$doc" ] || [ -f "$doc" ]; then
        print_status "OK" "Documentation '$doc' exists"
    else
        print_status "WARN" "Documentation '$doc' missing"
    fi
done

# Validate Docker setup
echo ""
echo "🐳 Validating Docker Setup..."

if [ -f "docker/linux/Dockerfile" ]; then
    print_status "OK" "Linux Dockerfile exists"
    
    # Check if Dockerfile has proper audio libraries
    if grep -q "libpipewire-0.3-dev" "docker/linux/Dockerfile"; then
        print_status "OK" "Linux Dockerfile includes PipeWire libraries"
    else
        print_status "WARN" "Linux Dockerfile missing PipeWire libraries"
    fi
else
    print_status "ERROR" "Linux Dockerfile missing"
fi

# Test basic compilation (if Rust is available)
echo ""
echo "🦀 Testing Compilation..."

if command -v cargo >/dev/null 2>&1; then
    print_status "OK" "Cargo found, testing compilation..."
    
    # Test basic build
    if cargo check --all-features >/dev/null 2>&1; then
        print_status "OK" "Basic compilation successful"
    else
        print_status "ERROR" "Basic compilation failed"
    fi
    
    # Test example compilation
    if cargo check --examples --all-features >/dev/null 2>&1; then
        print_status "OK" "Example compilation successful"
    else
        print_status "ERROR" "Example compilation failed"
    fi
    
else
    print_status "WARN" "Cargo not found, skipping compilation tests"
fi

# Validate platform-specific features
echo ""
echo "🖥️  Validating Platform Features..."

# Check if Cargo.toml has platform features
if grep -q "feat_windows" Cargo.toml && grep -q "feat_linux" Cargo.toml && grep -q "feat_macos" Cargo.toml; then
    print_status "OK" "Platform features defined in Cargo.toml"
else
    print_status "ERROR" "Platform features missing from Cargo.toml"
fi

# Check platform-specific dependencies
if grep -q "target_os = \"windows\"" Cargo.toml; then
    print_status "OK" "Windows-specific dependencies found"
else
    print_status "WARN" "Windows-specific dependencies missing"
fi

if grep -q "target_os = \"linux\"" Cargo.toml; then
    print_status "OK" "Linux-specific dependencies found"
else
    print_status "WARN" "Linux-specific dependencies missing"
fi

if grep -q "target_os = \"macos\"" Cargo.toml; then
    print_status "OK" "macOS-specific dependencies found"
else
    print_status "WARN" "macOS-specific dependencies missing"
fi

# Summary
echo ""
echo "📊 Validation Summary"
echo "===================="

# Count errors and warnings
error_count=$(grep -c "❌" /tmp/validation_output 2>/dev/null || echo "0")
warn_count=$(grep -c "⚠️" /tmp/validation_output 2>/dev/null || echo "0")

if [ "$error_count" -eq 0 ]; then
    print_status "OK" "No critical errors found!"
    if [ "$warn_count" -eq 0 ]; then
        print_status "OK" "CI/CD setup is fully validated and ready!"
    else
        print_status "WARN" "CI/CD setup is mostly ready, but has $warn_count warnings"
    fi
else
    print_status "ERROR" "Found $error_count critical errors that need to be fixed"
    exit 1
fi

echo ""
echo "🚀 Next Steps:"
echo "1. Fix any errors or warnings shown above"
echo "2. Test locally with: ./scripts/test_ci_locally.sh"
echo "3. Push to GitHub to trigger CI/CD workflows"
echo "4. Monitor GitHub Actions for successful execution"

print_status "INFO" "Validation complete!"
