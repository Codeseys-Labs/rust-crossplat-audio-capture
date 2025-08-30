#!/bin/bash
set -e

# Start PipeWire (preferred audio system)
if command -v pipewire >/dev/null; then
    echo "Starting PipeWire audio system..."
    # Start PipeWire and related services
    pipewire &
    sleep 1
    pipewire-pulse &  # PulseAudio compatibility layer
    sleep 1

    # Verify PipeWire setup
    echo "Checking PipeWire setup..."
    if command -v pw-cli >/dev/null; then
        pw-cli info
    fi

    # Check PulseAudio compatibility if available
    if command -v pactl >/dev/null; then
        pactl info
        pactl list short sinks
    fi
else
    echo "PipeWire not available"
    exit 1
fi

# Run the provided command
exec "$@"