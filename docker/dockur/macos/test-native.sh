#!/bin/bash
# RSAC Native macOS Test Runner
# Run inside the macOS VM after setup.sh has completed.
#
# Usage:
#   chmod +x /Volumes/shared/docker/dockur/macos/test-native.sh
#   /Volumes/shared/docker/dockur/macos/test-native.sh

set -e

echo "=== RSAC macOS Native Test Runner ==="
echo ""

# Ensure shared folder is mounted
if [ ! -d "/Volumes/shared" ] || ! mount | grep -q "/Volumes/shared"; then
    echo "Mounting shared folder..."
    sudo mkdir -p /Volumes/shared
    sudo mount_9p shared /Volumes/shared || {
        echo "ERROR: Failed to mount shared folder."
        echo "Run setup.sh first or mount manually: sudo mount_9p shared /Volumes/shared"
        exit 1
    }
fi

cd /Volumes/shared

# Source Rust environment
if [ -f "$HOME/.cargo/env" ]; then
    source "$HOME/.cargo/env"
fi

# Verify Rust is available
echo "--- Rust Version ---"
if ! command -v cargo &>/dev/null; then
    echo "ERROR: Rust not found. Run setup.sh first."
    exit 1
fi
rustup show
cargo --version

# Check audio devices
echo ""
echo "--- Audio Devices ---"
system_profiler SPAudioDataType 2>/dev/null || echo "No audio profiler available (expected in VM)"

# Create test results directory
mkdir -p test-results

# Run compilation check
echo ""
echo "--- Cargo Check (macOS features) ---"
cargo check --features feat_macos 2>&1 | tee test-results/cargo-check-macos.log
check_exit=${PIPESTATUS[0]}

if [ "$check_exit" -ne 0 ]; then
    echo ""
    echo "ERROR: cargo check failed (exit code $check_exit)"
    echo "See test-results/cargo-check-macos.log for details"
    exit "$check_exit"
fi

echo ""
echo "Compilation check passed!"

# Run tests (single-threaded to avoid audio device contention)
echo ""
echo "--- Cargo Test (macOS features) ---"
cargo test --features feat_macos -- --test-threads=1 2>&1 | tee test-results/cargo-test-macos.log
test_exit=${PIPESTATUS[0]}

echo ""
echo "=== Tests Complete ==="
echo "Results saved to test-results/"
echo "  - cargo-check-macos.log"
echo "  - cargo-test-macos.log"

if [ "$test_exit" -ne 0 ]; then
    echo ""
    echo "Some tests failed (exit code $test_exit)"
fi

exit "$test_exit"
