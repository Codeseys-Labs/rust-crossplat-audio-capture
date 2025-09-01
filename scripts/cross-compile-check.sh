#!/bin/bash

# Cross-compilation check script for rust-crossplat-audio-capture
# This script tests compilation for all supported platforms and feature combinations

set -e

echo "🔧 Cross-compilation check for rust-crossplat-audio-capture"
echo "============================================================"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Function to run cargo check with error handling
check_target() {
    local target=$1
    local features=$2
    local description=$3
    
    echo -e "\n${YELLOW}📦 Checking: $description${NC}"
    echo "Target: $target"
    echo "Features: $features"
    echo "----------------------------------------"
    
    if cross check --target "$target" --no-default-features --features "$features" --examples; then
        echo -e "${GREEN}✅ SUCCESS: $description${NC}"
        return 0
    else
        echo -e "${RED}❌ FAILED: $description${NC}"
        return 1
    fi
}

# Track results
declare -a results=()

echo -e "\n🐧 Testing Linux builds..."
# Linux with PipeWire
if check_target "x86_64-unknown-linux-gnu" "feat_linux" "Linux x86_64 with PipeWire"; then
    results+=("✅ Linux x86_64 + PipeWire")
else
    results+=("❌ Linux x86_64 + PipeWire")
fi

echo -e "\n🪟 Testing Windows builds..."
# Windows with WASAPI
if check_target "x86_64-pc-windows-msvc" "feat_windows" "Windows x86_64 with WASAPI"; then
    results+=("✅ Windows x86_64 + WASAPI")
else
    results+=("❌ Windows x86_64 + WASAPI")
fi

echo -e "\n🍎 Testing macOS builds..."
# macOS Intel with CoreAudio
if check_target "x86_64-apple-darwin" "feat_macos" "macOS Intel x86_64 with CoreAudio"; then
    results+=("✅ macOS Intel x86_64 + CoreAudio")
else
    results+=("❌ macOS Intel x86_64 + CoreAudio")
fi

# macOS Apple Silicon with CoreAudio
if check_target "aarch64-apple-darwin" "feat_macos" "macOS Apple Silicon ARM64 with CoreAudio"; then
    results+=("✅ macOS Apple Silicon ARM64 + CoreAudio")
else
    results+=("❌ macOS Apple Silicon ARM64 + CoreAudio")
fi

echo -e "\n🔄 Testing multi-platform builds..."
# Test with all features enabled (should work on any platform but only compile relevant code)
if check_target "x86_64-unknown-linux-gnu" "feat_linux,feat_windows,feat_macos" "All features on Linux"; then
    results+=("✅ All features on Linux")
else
    results+=("❌ All features on Linux")
fi

echo -e "\n📊 SUMMARY"
echo "=========="
for result in "${results[@]}"; do
    echo -e "$result"
done

# Count failures
failed_count=$(printf '%s\n' "${results[@]}" | grep -c "❌" || true)

if [ "$failed_count" -eq 0 ]; then
    echo -e "\n${GREEN}🎉 All cross-compilation checks passed!${NC}"
    exit 0
else
    echo -e "\n${RED}💥 $failed_count cross-compilation check(s) failed!${NC}"
    exit 1
fi
