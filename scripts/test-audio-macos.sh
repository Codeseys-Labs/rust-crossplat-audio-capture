#!/bin/bash
# =============================================================================
# Local audio capture testing for macOS (CoreAudio + Process Tap)
# =============================================================================
#
# Tests all 3 capture tiers: system, device, process/tree
#
# Prerequisites:
#   - macOS 14.4+ (Sonoma) for Process Tap application/tree capture
#   - Audio output device available (built-in speakers, headphones, or BlackHole)
#   - Microphone / Screen Recording permissions granted to Terminal
#   - Rust toolchain + Xcode CLI tools installed
#
# Usage:
#   ./scripts/test-audio-macos.sh              # Run all tiers
#   ./scripts/test-audio-macos.sh --tier system # Run only system tier
#   ./scripts/test-audio-macos.sh --tier device # Run only device tier
#   ./scripts/test-audio-macos.sh --tier process # Run only process tier
#   ./scripts/test-audio-macos.sh --verbose     # Extra diagnostic output
#
# Exit codes:
#   0 — all requested tests passed
#   1 — one or more tests failed
#   2 — missing prerequisites
# =============================================================================

set -euo pipefail

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

TIER="all"
VERBOSE=false
TEST_WAV=""
PLAYER_PID=""

PASS_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0
RESULTS=()

# Minimum macOS version for Process Tap (14.4)
MIN_MAJOR=14
MIN_MINOR=4

# ---------------------------------------------------------------------------
# Colors
# ---------------------------------------------------------------------------

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

log_info()  { echo -e "${BLUE}[INFO]${NC}  $*"; }
log_ok()    { echo -e "${GREEN}[PASS]${NC}  $*"; }
log_fail()  { echo -e "${RED}[FAIL]${NC}  $*"; }
log_warn()  { echo -e "${YELLOW}[WARN]${NC}  $*"; }
log_skip()  { echo -e "${CYAN}[SKIP]${NC}  $*"; }
log_header(){ echo -e "\n${BOLD}════════════════════════════════════════════════════════════${NC}"; echo -e "${BOLD}  $*${NC}"; echo -e "${BOLD}════════════════════════════════════════════════════════════${NC}"; }

record_result() {
    local tier="$1" name="$2" status="$3" detail="${4:-}"
    case "$status" in
        PASS) ((PASS_COUNT++)) || true; log_ok "$tier :: $name" ;;
        FAIL) ((FAIL_COUNT++)) || true; log_fail "$tier :: $name${detail:+ — $detail}" ;;
        SKIP) ((SKIP_COUNT++)) || true; log_skip "$tier :: $name${detail:+ — $detail}" ;;
    esac
    RESULTS+=("$status  $tier :: $name${detail:+ — $detail}")
}

# Run a cargo test command; capture exit code.
run_cargo_test() {
    local tier="$1" label="$2" filter="$3"
    shift 3
    local extra_args=("$@")

    log_info "Running: cargo test --test ci_audio --features feat_macos -- $filter ${extra_args[*]:-}"

    set +e
    RSAC_CI_AUDIO_AVAILABLE=1 \
    cargo test --test ci_audio --features feat_macos \
        -- "$filter" "${extra_args[@]}" 2>&1 | while IFS= read -r line; do
        echo "    $line"
    done
    local rc=${PIPESTATUS[0]}
    set -e

    if [ "$rc" -eq 0 ]; then
        record_result "$tier" "$label" "PASS"
    else
        record_result "$tier" "$label" "FAIL" "exit code $rc"
    fi
    return "$rc"
}

# Parse macOS version string (e.g., "14.4.1" → major=14, minor=4)
get_macos_version() {
    local version
    version=$(sw_vers -productVersion 2>/dev/null || echo "0.0")
    MACOS_MAJOR=$(echo "$version" | cut -d. -f1)
    MACOS_MINOR=$(echo "$version" | cut -d. -f2)
    MACOS_VERSION="$version"
}

# Check if macOS version supports Process Tap (>= 14.4)
supports_process_tap() {
    get_macos_version
    if [ "$MACOS_MAJOR" -gt "$MIN_MAJOR" ]; then
        return 0
    elif [ "$MACOS_MAJOR" -eq "$MIN_MAJOR" ] && [ "$MACOS_MINOR" -ge "$MIN_MINOR" ]; then
        return 0
    fi
    return 1
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case "$1" in
        --tier)     TIER="$2"; shift 2 ;;
        --verbose)  VERBOSE=true; shift ;;
        -h|--help)
            echo "Usage: $0 [--tier system|device|process|all] [--verbose]"
            exit 0 ;;
        *) echo "Unknown option: $1"; exit 2 ;;
    esac
done

# ---------------------------------------------------------------------------
# Cleanup handler
# ---------------------------------------------------------------------------

cleanup() {
    log_info "Cleaning up..."

    # Kill test audio player if still running
    if [ -n "$PLAYER_PID" ] && kill -0 "$PLAYER_PID" 2>/dev/null; then
        kill "$PLAYER_PID" 2>/dev/null || true
        wait "$PLAYER_PID" 2>/dev/null || true
        log_info "Stopped test audio player (PID $PLAYER_PID)"
    fi

    # Remove temp WAV
    if [ -n "$TEST_WAV" ] && [ -f "$TEST_WAV" ]; then
        rm -f "$TEST_WAV"
        log_info "Removed temp WAV $TEST_WAV"
    fi
}
trap cleanup EXIT INT TERM

# ---------------------------------------------------------------------------
# Prerequisite checks
# ---------------------------------------------------------------------------

log_header "Prerequisite Checks"

# 1. Confirm we're on macOS
if [ "$(uname -s)" != "Darwin" ]; then
    log_fail "This script is for macOS only (detected: $(uname -s))"
    exit 2
fi
log_ok "Running on macOS"

# 2. macOS version
get_macos_version
log_info "macOS version: $MACOS_VERSION (major=$MACOS_MAJOR, minor=$MACOS_MINOR)"

if supports_process_tap; then
    log_ok "macOS $MACOS_VERSION supports Process Tap (>= $MIN_MAJOR.$MIN_MINOR)"
    PROCESS_TAP_AVAILABLE=true
else
    log_warn "macOS $MACOS_VERSION does NOT support Process Tap (need >= $MIN_MAJOR.$MIN_MINOR)"
    log_warn "Process/application capture tests will be skipped"
    PROCESS_TAP_AVAILABLE=false
fi

# 3. Audio devices
log_info "Checking audio devices..."
if system_profiler SPAudioDataType >/dev/null 2>&1; then
    DEVICE_COUNT=$(system_profiler SPAudioDataType 2>/dev/null | grep -c "Default Output Device: Yes" || echo "0")
    if [ "$DEVICE_COUNT" -gt 0 ]; then
        log_ok "Default output audio device found"
    else
        log_warn "No default output device detected — tests may fail"
    fi

    if [ "$VERBOSE" = true ]; then
        log_info "Audio device details:"
        system_profiler SPAudioDataType 2>/dev/null | head -40
        echo ""
    fi
else
    log_warn "system_profiler not available or failed"
fi

# Check for BlackHole virtual audio driver (common for CI/testing)
if system_profiler SPAudioDataType 2>/dev/null | grep -qi "BlackHole"; then
    log_ok "BlackHole virtual audio driver detected (good for headless testing)"
fi

# 4. Required tools
for tool in cargo afplay sw_vers; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        log_fail "Required tool '$tool' not found in PATH."
        exit 2
    fi
done
log_ok "All required tools present (cargo, afplay, sw_vers)"

# 5. Xcode CLI tools
if ! xcode-select -p >/dev/null 2>&1; then
    log_fail "Xcode Command Line Tools not installed."
    echo "  Fix: xcode-select --install"
    exit 2
fi
log_ok "Xcode Command Line Tools installed"

# 6. Compilation check
log_info "Checking compilation..."
if ! cargo check --features feat_macos 2>&1 | tail -3; then
    log_fail "cargo check --features feat_macos failed"
    exit 2
fi
log_ok "Project compiles with feat_macos"

# ---------------------------------------------------------------------------
# Setup: test WAV file
# ---------------------------------------------------------------------------

log_header "Test Environment Setup"

# Generate a 5-second 440 Hz test WAV file using Python (available on all macOS)
TEST_WAV=$(mktemp /tmp/rsac_test_tone_XXXXXX.wav)
python3 -c "
import struct, math, wave
sr, dur, freq = 48000, 5.0, 440.0
n = int(sr * dur)
with wave.open('$TEST_WAV', 'w') as w:
    w.setnchannels(2)
    w.setsampwidth(2)
    w.setframerate(sr)
    for i in range(n):
        s = int(32767 * 0.8 * math.sin(2 * math.pi * freq * i / sr))
        w.writeframesraw(struct.pack('<hh', s, s))
" 2>/dev/null

if [ -f "$TEST_WAV" ] && [ "$(stat -f%z "$TEST_WAV" 2>/dev/null || wc -c < "$TEST_WAV")" -gt 44 ]; then
    log_ok "Generated test WAV: $TEST_WAV"
else
    log_fail "Failed to generate test WAV file"
    exit 2
fi

# ============================================================================
#  TIER 1: System Capture
# ============================================================================

run_system_tests() {
    log_header "Tier 1: System Capture"

    # Start background audio so there's something to capture
    afplay "$TEST_WAV" &
    PLAYER_PID=$!
    sleep 0.5
    log_info "Background player PID=$PLAYER_PID (afplay)"

    local tier_failed=0

    # System capture integration tests
    run_cargo_test "system" "system_capture_receives_audio" "test_system_capture_receives_audio" --nocapture || ((tier_failed++)) || true
    run_cargo_test "system" "capture_format_correct" "test_capture_format_correct" --nocapture || ((tier_failed++)) || true

    # Stream lifecycle tests
    run_cargo_test "system" "stream_start_read_stop" "test_stream_start_read_stop" --nocapture || ((tier_failed++)) || true
    run_cargo_test "system" "stream_stop_idempotent" "test_stream_stop_idempotent" --nocapture || ((tier_failed++)) || true
    run_cargo_test "system" "drop_while_running" "test_drop_while_running" --nocapture || ((tier_failed++)) || true

    # Platform capabilities
    run_cargo_test "system" "capabilities_query" "test_capabilities_query" --nocapture || ((tier_failed++)) || true
    run_cargo_test "system" "backend_name_matches_platform" "test_backend_name_matches_platform" --nocapture || ((tier_failed++)) || true

    # Stop background player
    if [ -n "$PLAYER_PID" ] && kill -0 "$PLAYER_PID" 2>/dev/null; then
        kill "$PLAYER_PID" 2>/dev/null || true
        wait "$PLAYER_PID" 2>/dev/null || true
    fi
    PLAYER_PID=""

    return "$tier_failed"
}

# ============================================================================
#  TIER 2: Device Capture
# ============================================================================

run_device_tests() {
    log_header "Tier 2: Device Capture"

    # Start background audio
    afplay "$TEST_WAV" &
    PLAYER_PID=$!
    sleep 0.5
    log_info "Background player PID=$PLAYER_PID (afplay)"

    local tier_failed=0

    # Device enumeration
    run_cargo_test "device" "enumerate_devices_finds_at_least_one" "test_enumerate_devices_finds_at_least_one" --nocapture || ((tier_failed++)) || true
    run_cargo_test "device" "default_device_exists" "test_default_device_exists" --nocapture || ((tier_failed++)) || true
    run_cargo_test "device" "platform_capabilities_reasonable" "test_platform_capabilities_reasonable" --nocapture || ((tier_failed++)) || true

    # Device capture tests
    run_cargo_test "device" "capture_from_selected_device" "test_capture_from_selected_device" --nocapture || ((tier_failed++)) || true
    run_cargo_test "device" "all_enumerated_devices_have_valid_ids" "test_all_enumerated_devices_have_valid_ids" --nocapture || ((tier_failed++)) || true
    run_cargo_test "device" "capture_nonexistent_device" "test_capture_nonexistent_device" --nocapture || ((tier_failed++)) || true

    # Stop background player
    if [ -n "$PLAYER_PID" ] && kill -0 "$PLAYER_PID" 2>/dev/null; then
        kill "$PLAYER_PID" 2>/dev/null || true
        wait "$PLAYER_PID" 2>/dev/null || true
    fi
    PLAYER_PID=""

    return "$tier_failed"
}

# ============================================================================
#  TIER 3: Process / Application / Tree Capture
# ============================================================================

run_process_tests() {
    log_header "Tier 3: Process / Application / Tree Capture"

    if [ "$PROCESS_TAP_AVAILABLE" = false ]; then
        record_result "process" "all process tests" "SKIP" "requires macOS >= $MIN_MAJOR.$MIN_MINOR for Process Tap"
        return 0
    fi

    log_info "Process Tap available — running application/tree capture tests"
    log_warn "Note: First run may prompt for Microphone/Screen Recording permission."
    log_warn "Grant permission in System Settings > Privacy & Security if prompted."

    local tier_failed=0

    # Tests spawn their own audio players (afplay), so no background player needed.

    # Application capture tests
    run_cargo_test "process" "app_capture_by_process_id" "test_app_capture_by_process_id" --nocapture || ((tier_failed++)) || true
    run_cargo_test "process" "app_capture_nonexistent_target" "test_app_capture_nonexistent_target" --nocapture || ((tier_failed++)) || true

    # Process tree capture tests
    run_cargo_test "process" "process_tree_capture_receives_audio" "test_process_tree_capture_receives_audio" --nocapture || ((tier_failed++)) || true
    run_cargo_test "process" "process_tree_capture_nonexistent_pid" "test_process_tree_capture_nonexistent_pid" --nocapture || ((tier_failed++)) || true
    run_cargo_test "process" "process_tree_capture_lifecycle" "test_process_tree_capture_lifecycle" --nocapture || ((tier_failed++)) || true

    return "$tier_failed"
}

# ============================================================================
#  Main
# ============================================================================

TOTAL_FAILED=0

case "$TIER" in
    system)
        run_system_tests  || ((TOTAL_FAILED+=$?)) || true ;;
    device)
        run_device_tests  || ((TOTAL_FAILED+=$?)) || true ;;
    process)
        run_process_tests || ((TOTAL_FAILED+=$?)) || true ;;
    all)
        run_system_tests  || ((TOTAL_FAILED+=$?)) || true
        run_device_tests  || ((TOTAL_FAILED+=$?)) || true
        run_process_tests || ((TOTAL_FAILED+=$?)) || true ;;
    *)
        echo "Unknown tier: $TIER (expected: system, device, process, all)"
        exit 2 ;;
esac

# ============================================================================
#  Summary
# ============================================================================

log_header "Test Summary"

echo ""
for r in "${RESULTS[@]}"; do
    case "${r:0:4}" in
        PASS) echo -e "  ${GREEN}$r${NC}" ;;
        FAIL) echo -e "  ${RED}$r${NC}" ;;
        SKIP) echo -e "  ${CYAN}$r${NC}" ;;
    esac
done
echo ""

TOTAL=$((PASS_COUNT + FAIL_COUNT + SKIP_COUNT))
echo -e "  ${BOLD}Total: $TOTAL${NC}  |  ${GREEN}Passed: $PASS_COUNT${NC}  |  ${RED}Failed: $FAIL_COUNT${NC}  |  ${CYAN}Skipped: $SKIP_COUNT${NC}"
echo ""

if [ "$FAIL_COUNT" -gt 0 ]; then
    log_fail "$FAIL_COUNT test(s) failed."
    exit 1
else
    log_ok "All tests passed."
    exit 0
fi
