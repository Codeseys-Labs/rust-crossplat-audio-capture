#!/bin/bash

# Cross-compilation check script for rsac.
# Checks the target/feature combinations that are actually possible with
# cross-rs from a Linux/WSL host.
#
# Scope note (2026-07-05 cleanup, rsac-a3c4): earlier versions also tried
# x86_64-pc-windows-msvc and {x86_64,aarch64}-apple-darwin via `cross` —
# cross-rs ships no MSVC or Darwin images, so those legs could never
# succeed. For Windows use `make check-windows-docker` (cargo-xwin); for
# macOS use `make check-macos-docker` or real hardware/CI.

set -e

echo "🔧 Cross-compilation check for rsac"
echo "===================================="

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
if check_target "x86_64-unknown-linux-gnu" "feat_linux" "Linux x86_64 with PipeWire"; then
    results+=("✅ Linux x86_64 + PipeWire")
else
    results+=("❌ Linux x86_64 + PipeWire")
fi

if check_target "aarch64-unknown-linux-gnu" "feat_linux" "Linux ARM64 with PipeWire"; then
    results+=("✅ Linux ARM64 + PipeWire")
else
    results+=("❌ Linux ARM64 + PipeWire")
fi

echo -e "\n🔄 Testing multi-feature build..."
# All backend features enabled on one target — target_os gating means only
# the Linux backend actually compiles in; this catches feature-unification
# breakage.
if check_target "x86_64-unknown-linux-gnu" "feat_linux,feat_windows,feat_macos" "All features on Linux"; then
    results+=("✅ All features on Linux")
else
    results+=("❌ All features on Linux")
fi

echo -e "\n📊 SUMMARY"
echo "=========="
failures=0
for r in "${results[@]}"; do
    echo -e "$r"
    case "$r" in ❌*) failures=$((failures + 1)) ;; esac
done

if [ "$failures" -gt 0 ]; then
    echo -e "\n${RED}$failures check(s) failed${NC}"
    exit 1
fi
echo -e "\n${GREEN}All cross-compilation checks passed${NC}"
