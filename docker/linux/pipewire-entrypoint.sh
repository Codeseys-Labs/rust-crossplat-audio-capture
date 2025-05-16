#!/bin/bash
set -e

echo "Starting PipeWire services..."

# Start D-Bus daemon
echo "Starting D-Bus daemon..."
mkdir -p /run/dbus
dbus-daemon --system --nofork --nopidfile &
DBUS_PID=$!
sleep 1

# Start Xvfb
echo "Starting Xvfb..."
Xvfb :99 -screen 0 1024x768x24 &
XVFB_PID=$!
sleep 1

# Start PipeWire
echo "Starting PipeWire..."
# Create runtime directories
mkdir -p $XDG_RUNTIME_DIR
chmod 700 $XDG_RUNTIME_DIR

# Start the PipeWire daemon
pipewire &
PIPEWIRE_PID=$!
sleep 1

# Start PipeWire Media Session (or Wireplumber if available)
if command -v wireplumber >/dev/null 2>&1; then
    echo "Starting Wireplumber..."
    wireplumber &
    SESSION_PID=$!
else
    echo "Starting PipeWire Media Session..."
    pipewire-media-session &
    SESSION_PID=$!
fi
sleep 1

# Start PipeWire PulseAudio compatibility layer
echo "Starting PipeWire-PulseAudio..."
pipewire-pulse &
PULSE_PID=$!
sleep 2

# Verify PipeWire is running correctly
echo "Verifying PipeWire setup..."
/usr/local/bin/verify-pipewire.sh

if [ $? -ne 0 ]; then
    echo "ERROR: PipeWire setup verification failed!"
    exit 1
fi

echo "PipeWire setup completed successfully!"

# Save PIDs to file for potential cleanup
echo $DBUS_PID > /tmp/dbus.pid
echo $XVFB_PID > /tmp/xvfb.pid
echo $PIPEWIRE_PID > /tmp/pipewire.pid
echo $SESSION_PID > /tmp/session.pid
echo $PULSE_PID > /tmp/pipewire-pulse.pid

# Define cleanup function
cleanup() {
    echo "Cleaning up PipeWire services..."
    kill -TERM $PULSE_PID 2>/dev/null || true
    kill -TERM $SESSION_PID 2>/dev/null || true
    kill -TERM $PIPEWIRE_PID 2>/dev/null || true
    kill -TERM $XVFB_PID 2>/dev/null || true
    kill -TERM $DBUS_PID 2>/dev/null || true
}

# Set trap to clean up services on exit
trap cleanup EXIT

# Execute the command passed to the script
exec "$@" 