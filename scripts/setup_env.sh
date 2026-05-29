#!/usr/bin/env bash
# Basic environment initialization for rsac development
# This script installs Linux audio dependencies when possible and
# exports useful environment variables. It can be sourced or run at
# container startup.

set -euo pipefail

# Detect platform
OS=$(uname -s)

if [[ "$OS" == "Linux" ]]; then
    if command -v apt-get >/dev/null; then
        echo "Updating package lists and installing dependencies..."
        sudo apt-get update && sudo apt-get install -y \
            build-essential pkg-config clang libclang-dev \
            libasound2-dev libpulse-dev libpipewire-0.3-dev
    else
        echo "apt-get not found. Please install development libraries manually." >&2
    fi
fi

# Source cargo environment if installed via rustup
if [ -f "$HOME/.cargo/env" ]; then
    source "$HOME/.cargo/env"
fi

# Enable build.rs auto-install logic on supported systems
export RSAC_AUTO_INSTALL=1

# Adjust PKG_CONFIG_PATH if you installed libraries in a custom location
# export PKG_CONFIG_PATH=/usr/local/lib/pkgconfig:$PKG_CONFIG_PATH

# Verify dependencies using the helper script
if [ -x scripts/check_deps.sh ]; then
    scripts/check_deps.sh || true
fi

