#!/bin/bash

# Linux Audio System Debug Script
# This script checks for PulseAudio and PipeWire availability and tests audio devices

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Function to print colored output
print_status() {
    local status=$1
    local message=$2
    case $status in
        "OK")
            echo -e "${GREEN}✅ $message${NC}"
            ;;
        "WARN")
            echo -e "${YELLOW}⚠️  $message${NC}"
            ;;
        "ERROR")
            echo -e "${RED}❌ $message${NC}"
            ;;
        "INFO")
            echo -e "${BLUE}ℹ️  $message${NC}"
            ;;
        *)
            echo "$message"
            ;;
    esac
}

echo "=== Linux Audio System Debug Script ==="
echo "Generated on: $(date)"
echo ""

# System Information
echo "=== System Information ==="
print_status "INFO" "OS: $(lsb_release -d 2>/dev/null | cut -f2 || cat /etc/os-release | grep PRETTY_NAME | cut -d= -f2 | tr -d '\"')"
print_status "INFO" "Kernel: $(uname -r)"
print_status "INFO" "User: $(whoami) (UID: $(id -u), GID: $(id -g))"
print_status "INFO" "XDG_RUNTIME_DIR: ${XDG_RUNTIME_DIR:-not set}"
echo ""

# Check for audio system binaries
echo "=== Audio System Detection ==="

# PulseAudio detection
echo "--- PulseAudio Detection ---"
PULSEAUDIO_AVAILABLE=false
PACTL_AVAILABLE=false

if command -v pulseaudio >/dev/null 2>&1; then
    print_status "OK" "PulseAudio binary found: $(which pulseaudio)"
    VERSION=$(pulseaudio --version 2>/dev/null || echo 'Version check failed')
    print_status "INFO" "Version: $VERSION"
    PULSEAUDIO_AVAILABLE=true
else
    print_status "ERROR" "PulseAudio binary not found"
fi

if command -v pactl >/dev/null 2>&1; then
    print_status "OK" "pactl found: $(which pactl)"
    PACTL_AVAILABLE=true
else
    print_status "ERROR" "pactl not found"
fi

echo ""

# PipeWire detection
echo "--- PipeWire Detection ---"
PIPEWIRE_AVAILABLE=false
PWCLI_AVAILABLE=false
PWDUMP_AVAILABLE=false

if command -v pipewire >/dev/null 2>&1; then
    print_status "OK" "PipeWire binary found: $(which pipewire)"
    VERSION=$(pipewire --version 2>/dev/null || echo 'Version check failed')
    print_status "INFO" "Version: $VERSION"
    PIPEWIRE_AVAILABLE=true
else
    print_status "ERROR" "PipeWire binary not found"
fi

if command -v pw-cli >/dev/null 2>&1; then
    print_status "OK" "pw-cli found: $(which pw-cli)"
    VERSION=$(pw-cli --version 2>/dev/null || echo 'Version check failed')
    print_status "INFO" "Version: $VERSION"
    PWCLI_AVAILABLE=true
else
    print_status "ERROR" "pw-cli not found"
fi

if command -v pw-dump >/dev/null 2>&1; then
    print_status "OK" "pw-dump found: $(which pw-dump)"
    PWDUMP_AVAILABLE=true
else
    print_status "WARN" "pw-dump not found (optional)"
fi

echo ""

# Check running services
echo "=== Running Audio Services ==="

echo "--- Process Check ---"
print_status "INFO" "PulseAudio processes:"
if pgrep -f pulseaudio >/dev/null 2>&1; then
    pgrep -f pulseaudio | while read pid; do
        print_status "OK" "PID $pid: $(ps -p $pid -o comm= 2>/dev/null || echo 'unknown')"
    done
else
    print_status "WARN" "No PulseAudio processes found"
fi

echo ""
print_status "INFO" "PipeWire processes:"
if pgrep -f pipewire >/dev/null 2>&1; then
    pgrep -f pipewire | while read pid; do
        print_status "OK" "PID $pid: $(ps -p $pid -o comm= 2>/dev/null || echo 'unknown')"
    done
else
    print_status "WARN" "No PipeWire processes found"
fi

echo ""
print_status "INFO" "WirePlumber processes:"
if pgrep -f wireplumber >/dev/null 2>&1; then
    pgrep -f wireplumber | while read pid; do
        print_status "OK" "PID $pid: $(ps -p $pid -o comm= 2>/dev/null || echo 'unknown')"
    done
else
    print_status "WARN" "No WirePlumber processes found"
fi

echo ""

# Check systemd services
echo "--- Systemd User Services ---"
if systemctl --user list-units --type=service 2>/dev/null | grep -E "(pulse|pipewire|wireplumber)" >/dev/null; then
    print_status "OK" "Found audio-related systemd services:"
    systemctl --user list-units --type=service 2>/dev/null | grep -E "(pulse|pipewire|wireplumber)" | while read line; do
        print_status "INFO" "$line"
    done
else
    print_status "WARN" "No audio-related systemd services found"
fi

echo ""

# Test audio devices
echo "=== Audio Device Testing ==="

# Test PulseAudio if available
if [ "$PACTL_AVAILABLE" = true ]; then
    echo "--- PulseAudio Device Check ---"
    
    if pactl info >/dev/null 2>&1; then
        print_status "OK" "PulseAudio server is running"
        
        print_status "INFO" "Server info:"
        pactl info 2>/dev/null | head -10
        
        echo ""
        print_status "INFO" "Available sinks (output devices):"
        if pactl list short sinks 2>/dev/null | grep -q .; then
            pactl list short sinks | while read line; do
                print_status "OK" "$line"
            done
        else
            print_status "WARN" "No sinks available"
        fi
        
        echo ""
        print_status "INFO" "Available sources (input devices):"
        if pactl list short sources 2>/dev/null | grep -q .; then
            pactl list short sources | while read line; do
                print_status "OK" "$line"
            done
        else
            print_status "WARN" "No sources available"
        fi
        
        echo ""
        DEFAULT_SINK=$(pactl get-default-sink 2>/dev/null || echo "none")
        print_status "INFO" "Default sink: $DEFAULT_SINK"
        
        DEFAULT_SOURCE=$(pactl get-default-source 2>/dev/null || echo "none")
        print_status "INFO" "Default source: $DEFAULT_SOURCE"
        
    else
        print_status "ERROR" "PulseAudio server is not running or not accessible"
    fi
    
    echo ""
fi

# Test PipeWire if available
if [ "$PWCLI_AVAILABLE" = true ]; then
    echo "--- PipeWire Device Check ---"
    
    if pw-cli info 0 >/dev/null 2>&1; then
        print_status "OK" "PipeWire server is running"
        
        print_status "INFO" "PipeWire info:"
        pw-cli info 0 2>/dev/null | head -10
        
        echo ""
        print_status "INFO" "PipeWire nodes (first 20):"
        if pw-cli list-objects Node 2>/dev/null | head -40 | grep -q .; then
            pw-cli list-objects Node 2>/dev/null | head -40 | grep -E "(id|node\.name|media\.class)" | head -20
        else
            print_status "WARN" "No PipeWire nodes found"
        fi
        
        if [ "$PWDUMP_AVAILABLE" = true ]; then
            echo ""
            print_status "INFO" "Audio nodes (via pw-dump):"
            if command -v jq >/dev/null 2>&1; then
                pw-dump 2>/dev/null | jq -r '.[] | select(.info.props."media.class" // "" | test("Audio")) | "\(.info.props."node.name" // "unknown"): \(.info.props."media.class" // "unknown")"' 2>/dev/null | head -10 || print_status "WARN" "pw-dump parsing failed"
            else
                print_status "WARN" "jq not available for pw-dump parsing"
            fi
        fi
        
    else
        print_status "ERROR" "PipeWire server is not running or not accessible"
    fi
    
    echo ""
fi

# Check ALSA devices
echo "--- ALSA Device Check ---"
if command -v aplay >/dev/null 2>&1; then
    print_status "INFO" "ALSA playback devices:"
    if aplay -l 2>/dev/null | grep -q "card"; then
        aplay -l 2>/dev/null | grep -E "(card|device)" | head -10
    else
        print_status "WARN" "No ALSA playback devices found"
    fi
    
    echo ""
    print_status "INFO" "ALSA capture devices:"
    if arecord -l 2>/dev/null | grep -q "card"; then
        arecord -l 2>/dev/null | grep -E "(card|device)" | head -10
    else
        print_status "WARN" "No ALSA capture devices found"
    fi
else
    print_status "WARN" "ALSA tools not available"
fi

echo ""

# Generate summary and recommendations
echo "=== Summary and Recommendations ==="

echo "## Audio System Availability"
print_status "INFO" "PulseAudio: $PULSEAUDIO_AVAILABLE"
print_status "INFO" "pactl: $PACTL_AVAILABLE"
print_status "INFO" "PipeWire: $PIPEWIRE_AVAILABLE"
print_status "INFO" "pw-cli: $PWCLI_AVAILABLE"
print_status "INFO" "pw-dump: $PWDUMP_AVAILABLE"

echo ""
echo "## Recommendations"

if [ "$PIPEWIRE_AVAILABLE" = true ] && [ "$PWCLI_AVAILABLE" = true ]; then
    if pw-cli info 0 >/dev/null 2>&1; then
        print_status "OK" "PipeWire is available and running - should work for audio capture"
    else
        print_status "WARN" "PipeWire is installed but not running - try starting it with:"
        echo "  systemctl --user --now enable pipewire.service"
        echo "  systemctl --user --now enable wireplumber.service"
    fi
elif [ "$PULSEAUDIO_AVAILABLE" = true ] && [ "$PACTL_AVAILABLE" = true ]; then
    if pactl info >/dev/null 2>&1; then
        print_status "OK" "PulseAudio is available and running - should work for audio capture"
    else
        print_status "WARN" "PulseAudio is installed but not running - try starting it with:"
        echo "  pulseaudio --start"
    fi
else
    print_status "ERROR" "Neither PipeWire nor PulseAudio appear to be fully available"
    echo ""
    print_status "INFO" "To install PipeWire:"
    echo "  sudo apt-get install pipewire pipewire-audio-client-libraries wireplumber"
    echo ""
    print_status "INFO" "To install PulseAudio:"
    echo "  sudo apt-get install pulseaudio pulseaudio-utils"
fi

echo ""
print_status "INFO" "Debug script completed"
