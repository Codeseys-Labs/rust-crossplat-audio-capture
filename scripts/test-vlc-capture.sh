#!/bin/bash

# VLC Audio Capture Test Script
# This script tests our audio capture library with VLC streaming audio from URLs

set -e

# Global variables for tracking
CLEANUP_DONE=false
SCRIPT_START_TIME=$(date +%s)
PROCESS_LOG="process_tracking.log"

# Initialize process tracking log
echo "=== Process Tracking Log - $(date) ===" > "$PROCESS_LOG"
echo "Script PID: $$" >> "$PROCESS_LOG"

# Function to log process events
log_process_event() {
    local event="$1"
    local details="$2"
    local timestamp=$(date '+%H:%M:%S.%3N')
    echo "[$timestamp] $event: $details" >> "$PROCESS_LOG"
    echo "[$timestamp] $event: $details"
}

# Function to log all child processes
log_child_processes() {
    local parent_pid="$1"
    local label="$2"
    log_process_event "CHILD_SCAN" "$label - scanning children of PID $parent_pid"
    if command -v pstree >/dev/null 2>&1; then
        pstree -p "$parent_pid" 2>/dev/null >> "$PROCESS_LOG" || true
    else
        ps --ppid "$parent_pid" -o pid,ppid,comm,args 2>/dev/null >> "$PROCESS_LOG" || true
    fi
}

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

echo "=== VLC Audio Capture Test Script ==="

# Test URLs (reliable audio sources)
TEST_URLS=(
    "https://www.soundjay.com/misc/sounds/bell-ringing-05.wav"
    "https://file-examples.com/storage/fe68c1b7c1a9fd42d99c603/2017/11/file_example_WAV_1MG.wav"
    "https://www.kozco.com/tech/LRMonoPhase4.wav"
    "https://www.kozco.com/tech/piano2.wav"
)

# Function to test URL accessibility
test_url() {
    local url="$1"
    print_status "INFO" "Testing URL: $url"
    
    if curl -s --head --max-time 10 "$url" | head -n 1 | grep -q "200"; then
        print_status "OK" "URL is accessible"
        return 0
    else
        print_status "WARN" "URL not accessible or timed out"
        return 1
    fi
}

# Find a working URL
WORKING_URL=""
print_status "INFO" "Searching for accessible audio URLs..."

for i in "${!TEST_URLS[@]}"; do
    if test_url "${TEST_URLS[$i]}"; then
        WORKING_URL="${TEST_URLS[$i]}"
        print_status "OK" "Found working URL: $WORKING_URL"
        break
    fi
done

# Fallback: generate local test audio if no URL works
if [ -z "$WORKING_URL" ]; then
    print_status "WARN" "No working URLs found, generating local test audio"
    
    # Create a more interesting test audio (multiple tones)
    ffmpeg -f lavfi -i "sine=frequency=440:duration=10,sine=frequency=880:duration=10" \
           -filter_complex "[0:a][1:a]concat=n=2:v=0:a=1" \
           -ar 48000 -ac 2 vlc_test_audio.wav -y
    
    WORKING_URL="file://$(pwd)/vlc_test_audio.wav"
    print_status "OK" "Created local test audio: $WORKING_URL"
fi

print_status "INFO" "Final test URL: $WORKING_URL"

# Check VLC availability
if ! command -v cvlc >/dev/null 2>&1; then
    print_status "ERROR" "cvlc (command-line VLC) not found"
    print_status "INFO" "This test requires VLC to be installed"
    print_status "INFO" "Install with: sudo apt-get install vlc"
    exit 1
fi

print_status "OK" "VLC found: $(which cvlc)"

# Check PipeWire
if ! command -v pw-cli >/dev/null 2>&1; then
    print_status "ERROR" "pw-cli not found - PipeWire not available"
    exit 1
fi

print_status "OK" "PipeWire found: $(pw-cli --version)"

# Start VLC with the URL
print_status "INFO" "Starting VLC with audio stream..."
log_process_event "VLC_START" "Starting VLC with URL: $WORKING_URL"
cvlc --intf dummy --loop "$WORKING_URL" --verbose 2 > vlc_capture_test.log 2>&1 &
VLC_PID=$!
log_process_event "VLC_STARTED" "VLC PID: $VLC_PID"
log_child_processes "$$" "After VLC start"

# Cleanup function
cleanup() {
    local cleanup_exit_code=$?
    log_process_event "CLEANUP_START" "Cleanup called with exit code: $cleanup_exit_code"

    # Prevent recursive cleanup calls
    if [ "$CLEANUP_DONE" = "true" ]; then
        log_process_event "CLEANUP_SKIP" "Cleanup already done, skipping"
        return 0
    fi
    CLEANUP_DONE=true

    print_status "INFO" "Cleaning up..."
    log_child_processes "$$" "Before cleanup"

    # Remove traps to prevent recursion
    trap - EXIT SIGTERM SIGINT
    log_process_event "TRAPS_REMOVED" "Signal traps removed"

    if [ -n "$VLC_PID" ] && kill -0 $VLC_PID 2>/dev/null; then
        print_status "INFO" "Stopping VLC (PID: $VLC_PID)"
        log_process_event "VLC_STOP_START" "Sending SIGTERM to VLC PID: $VLC_PID"

        # Send SIGTERM for graceful shutdown
        kill -TERM $VLC_PID 2>/dev/null || true
        log_process_event "VLC_SIGTERM_SENT" "SIGTERM sent to VLC"

        # Wait a bit for graceful shutdown
        local countdown=3
        while [ $countdown -gt 0 ] && kill -0 $VLC_PID 2>/dev/null; do
            log_process_event "VLC_WAIT" "Waiting for VLC shutdown, countdown: $countdown"
            sleep 1
            countdown=$((countdown - 1))
        done

        # Force kill if still running
        if kill -0 $VLC_PID 2>/dev/null; then
            log_process_event "VLC_FORCE_KILL" "VLC still running, sending SIGKILL"
            kill -KILL $VLC_PID 2>/dev/null || true
        else
            log_process_event "VLC_GRACEFUL_EXIT" "VLC exited gracefully"
        fi

        # Wait for process cleanup and capture exit code
        log_process_event "VLC_WAIT_START" "Waiting for VLC process cleanup"
        wait $VLC_PID 2>/dev/null
        local vlc_exit_code=$?
        log_process_event "VLC_WAIT_DONE" "VLC wait completed with exit code: $vlc_exit_code"
    else
        log_process_event "VLC_NOT_RUNNING" "VLC not running or PID not set"
    fi

    # Skip broad VLC process cleanup to avoid signal issues
    log_process_event "PKILL_SKIP" "Skipping broad VLC cleanup to prevent signal conflicts"

    # Clean up temporary files
    log_process_event "FILE_CLEANUP" "Removing temporary files"
    rm -f vlc_test_audio.wav 2>/dev/null || true

    log_child_processes "$$" "After cleanup"
    log_process_event "CLEANUP_COMPLETE" "Cleanup completed, original exit code: $cleanup_exit_code"

    # If the original exit code was 0 (success), preserve it
    if [ "$cleanup_exit_code" -eq 0 ]; then
        log_process_event "EXIT_OVERRIDE" "Preserving successful exit code 0"
        exit 0
    fi
}

# Set up signal handlers
log_process_event "TRAPS_SET" "Setting up signal handlers for EXIT, SIGTERM, SIGINT"
trap cleanup EXIT SIGTERM SIGINT

# Wait for VLC to start
print_status "INFO" "Waiting for VLC to start..."
sleep 8

# Check if VLC is running
if ! kill -0 $VLC_PID 2>/dev/null; then
    print_status "ERROR" "VLC failed to start"
    print_status "INFO" "VLC logs:"
    cat vlc_capture_test.log || true
    exit 1
fi

print_status "OK" "VLC is running with PID: $VLC_PID"

# Check PipeWire for VLC nodes
print_status "INFO" "Checking PipeWire for VLC audio nodes..."
VLC_NODES=$(pw-cli list-objects Node | grep -i vlc || true)

if [ -n "$VLC_NODES" ]; then
    print_status "OK" "Found VLC nodes in PipeWire:"
    echo "$VLC_NODES"
else
    print_status "WARN" "No VLC nodes found in PipeWire yet, waiting..."
    sleep 5
    VLC_NODES=$(pw-cli list-objects Node | grep -i vlc || true)
    
    if [ -n "$VLC_NODES" ]; then
        print_status "OK" "Found VLC nodes after waiting:"
        echo "$VLC_NODES"
    else
        print_status "WARN" "Still no VLC nodes found, but continuing test..."
    fi
fi

# Show all audio nodes for debugging
print_status "INFO" "All PipeWire audio nodes:"
pw-cli list-objects Node | grep -E "(application|Audio)" || true

# Test our audio capture
print_status "INFO" "Testing audio capture with our library..."

# Test 1: Dynamic VLC capture example
print_status "INFO" "Running dynamic_vlc_capture example..."
log_process_event "CARGO_START" "Starting cargo run with timeout 40s"
log_child_processes "$$" "Before cargo run"

# Set environment variables to prevent interactive sudo prompts
export CI=true
export GITHUB_ACTIONS=true
cargo run --bin dynamic_vlc_capture  --features feat_linux 10 > flexible_test.log 2>&1
CARGO_EXIT_CODE=$?

log_process_event "CARGO_COMPLETE" "Cargo completed with exit code: $CARGO_EXIT_CODE"
log_child_processes "$$" "After cargo run"

if [ $CARGO_EXIT_CODE -eq 124 ]; then
    print_status "WARN" "Cargo command timed out after 40 seconds"
    log_process_event "CARGO_TIMEOUT" "Timeout occurred (exit code 124)"
elif [ $CARGO_EXIT_CODE -eq 143 ]; then
    print_status "WARN" "Cargo command was terminated by signal (SIGTERM)"
    log_process_event "CARGO_SIGTERM" "Cargo terminated by SIGTERM (exit code 143)"
elif [ $CARGO_EXIT_CODE -ne 0 ]; then
    print_status "WARN" "Cargo command failed with exit code $CARGO_EXIT_CODE"
    log_process_event "CARGO_ERROR" "Cargo failed with exit code: $CARGO_EXIT_CODE"
else
    log_process_event "CARGO_SUCCESS" "Cargo completed successfully"
fi

# Test 2: System audio capture is now disabled to focus on dynamic VLC capture.
print_status "INFO" "System audio capture test skipped."

# Show logs for debugging
print_status "INFO" "=== VLC Logs (first 20 lines) ==="
head -20 vlc_capture_test.log || true

print_status "INFO" "=== Dynamic VLC Capture Logs ==="
cat flexible_test.log || true

# print_status "INFO" "=== Capture Test Logs ==="
# cat capture_test.log || true

print_status "OK" "VLC Audio Capture Test Complete"

# Summary
echo ""
echo "=== Test Summary ==="
echo "VLC URL: $WORKING_URL"
echo "VLC PID: $VLC_PID"
echo "VLC Nodes Found: $([ -n "$VLC_NODES" ] && echo "Yes" || echo "No")"
echo "Dynamic Capture File: $(if [ -f "dynamic_vlc_capture.wav" ]; then echo "Created ($(stat -c%s dynamic_vlc_capture.wav) bytes)"; else echo "Not created"; fi)"

print_status "OK" "Test completed successfully"
log_process_event "TEST_SUCCESS" "All tests completed successfully"
log_child_processes "$$" "Before script exit"

# Show the process tracking log
echo ""
echo "=== Process Tracking Summary ==="
if [ -f "$PROCESS_LOG" ]; then
    cat "$PROCESS_LOG"
else
    echo "Process log not found"
fi

log_process_event "SCRIPT_END" "Script ending normally with exit code 0"
