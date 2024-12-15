#!/bin/bash
set -e

# Start PulseAudio in the background
pulseaudio --start --exit-idle-time=-1 --load="module-null-sink sink_name=virtual-sink" &
sleep 2

# Start PipeWire if available
if command -v pipewire >/dev/null; then
    # Start PipeWire and related services
    pipewire &
    sleep 1
    pipewire-pulse &
    sleep 1
fi

# Verify audio setup
echo "Checking audio setup..."
if command -v pactl >/dev/null; then
    pactl info
    pactl list short sinks
fi

if command -v pw-cli >/dev/null; then
    pw-cli info
fi

# Run the provided command
exec "$@"