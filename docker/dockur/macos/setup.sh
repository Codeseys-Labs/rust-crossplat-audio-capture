#!/bin/bash
# RSAC macOS Dev Environment Setup
# Run inside the macOS VM after first boot completes.
#
# Usage:
#   chmod +x /Volumes/shared/docker/dockur/macos/setup.sh
#   /Volumes/shared/docker/dockur/macos/setup.sh

set -e

echo "=== RSAC macOS Dev Environment Setup ==="
echo ""

# Mount shared folder from host (9p virtio filesystem)
if [ ! -d "/Volumes/shared" ] || ! mount | grep -q "/Volumes/shared"; then
    echo "Mounting shared folder..."
    sudo mkdir -p /Volumes/shared
    sudo mount_9p shared /Volumes/shared || {
        echo "ERROR: Failed to mount shared folder."
        echo "The 9p mount requires the shared volume to be configured in docker-compose."
        exit 1
    }
    echo "Shared folder mounted at /Volumes/shared"
else
    echo "Shared folder already mounted at /Volumes/shared"
fi

# Install Xcode Command Line Tools (required for compilation)
echo ""
echo "--- Installing Xcode Command Line Tools ---"
if xcode-select -p &>/dev/null; then
    echo "Xcode Command Line Tools already installed"
else
    echo "Installing Xcode Command Line Tools..."
    # Touch the file that triggers the install and wait
    touch /tmp/.com.apple.dt.CommandLineTools.installondemand.in-progress
    xcode-select --install 2>/dev/null || true
    echo ""
    echo "NOTE: If a GUI dialog appeared, click 'Install' and wait for it to complete."
    echo "Then re-run this script to continue setup."
    echo ""
    read -p "Press Enter after Xcode CLT installation completes..."
fi

# Install Homebrew
echo ""
echo "--- Installing Homebrew ---"
if command -v brew &>/dev/null; then
    echo "Homebrew already installed"
else
    echo "Installing Homebrew..."
    /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

    # Add Homebrew to PATH for Apple Silicon and Intel
    if [ -f "/opt/homebrew/bin/brew" ]; then
        eval "$(/opt/homebrew/bin/brew shellenv)"
        echo 'eval "$(/opt/homebrew/bin/brew shellenv)"' >> "$HOME/.zprofile"
    elif [ -f "/usr/local/bin/brew" ]; then
        eval "$(/usr/local/bin/brew shellenv)"
    fi
fi

# Install Rust via rustup
echo ""
echo "--- Installing Rust ---"
if command -v rustup &>/dev/null; then
    echo "Rust already installed"
    rustup show
else
    echo "Installing Rust..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
    echo "Rust installed successfully"
fi

# Verify installations
echo ""
echo "--- Verification ---"
echo "Rust:  $(rustc --version 2>/dev/null || echo 'NOT FOUND')"
echo "Cargo: $(cargo --version 2>/dev/null || echo 'NOT FOUND')"
echo "Xcode: $(xcode-select -p 2>/dev/null || echo 'NOT FOUND')"
echo "Brew:  $(brew --version 2>/dev/null | head -1 || echo 'NOT FOUND')"

echo ""
echo "=== Setup Complete ==="
echo ""
echo "Project available at: /Volumes/shared"
echo "Run tests with:"
echo "  cd /Volumes/shared && cargo test --features feat_macos"
echo ""
echo "Or use the test runner script:"
echo "  /Volumes/shared/docker/dockur/macos/test-native.sh"
