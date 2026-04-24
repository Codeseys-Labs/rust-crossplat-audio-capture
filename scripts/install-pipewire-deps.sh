#!/bin/bash
set -e

# Script to install PipeWire dependencies for the audio capture library

echo "Installing PipeWire dependencies..."

# Check if running as root
if [ "$EUID" -ne 0 ]; then
    echo "Please run with sudo or as root"
    exit 1
fi

# Detect the Linux distribution
if [ -f /etc/debian_version ]; then
    echo "Debian/Ubuntu detected"
    apt update
    # Runtime dependencies
    apt install -y libpipewire-0.3-0
    # Development dependencies (only needed for building)
    apt install -y libpipewire-0.3-dev pkg-config build-essential clang libclang-dev llvm-dev
elif [ -f /etc/fedora-release ]; then
    echo "Fedora detected"
    dnf check-update || true
    # Runtime dependencies
    dnf install -y pipewire-libs
    # Development dependencies
    dnf install -y pipewire-devel pkg-config gcc clang clang-devel llvm-devel
elif [ -f /etc/arch-release ]; then
    echo "Arch Linux detected"
    pacman -Sy
    # Runtime and development dependencies
    pacman -S --needed pipewire pkgconf base-devel clang llvm
else
    echo "Unsupported Linux distribution"
    echo "Please manually install PipeWire:"
    echo "- For Debian/Ubuntu: sudo apt install libpipewire-0.3-0"
    echo "- For Fedora: sudo dnf install pipewire-libs"
    echo "- For Arch Linux: sudo pacman -S pipewire"
    exit 1
fi

echo "Checking if PipeWire daemon is running..."
if pgrep -x "pipewire" > /dev/null; then
    echo "✅ PipeWire daemon is running"
else
    echo "⚠️ PipeWire daemon not detected"
    echo "Starting PipeWire..."
    # Try to start the PipeWire service for the current user
    systemctl --user start pipewire.service || true
    systemctl --user start pipewire.socket || true
    
    # Check again
    sleep 2
    if pgrep -x "pipewire" > /dev/null; then
        echo "✅ PipeWire daemon started successfully"
    else
        echo "❌ Failed to start PipeWire daemon"
        echo "Please make sure PipeWire is properly configured on your system"
        echo "You might need to reboot or manually start the PipeWire service"
    fi
fi

echo "Dependencies installation completed" 