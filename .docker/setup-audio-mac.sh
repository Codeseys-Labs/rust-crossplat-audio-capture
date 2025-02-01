#!/bin/bash
set -e

# Set up environment for CoreAudio testing
echo "Setting up CoreAudio environment..."

# Create virtual audio device (in real environment this would use BlackHole)
echo "Note: In real macOS environment, this would set up BlackHole audio device"

# Run the provided command
exec "$@"