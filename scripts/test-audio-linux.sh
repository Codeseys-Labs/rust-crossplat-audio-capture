#!/bin/bash
# =============================================================================
# Local audio capture testing for Linux (PipeWire)
# =============================================================================
#
# Tests all 3 capture tiers: system, device, process/tree
#
# Prerequisites:
#   - PipeWire running with pipewire-pulse
#   - pw-cli, pw-play, pactl available
#   - Rust toolchain installed
#
# Usage:
#   ./scripts/test-audio-linux.sh              # Run all tiers
#   ./scripts/test-audio-linux.sh --tier system # Run only system tier
#   ./scripts/test-audio-linux.sh --tier device # Run only device tier
#   ./scripts/test-audio-linux.sh --tier process # Run only process tier
#   ./scripts/test-audio-linux.sh --verbose     # Extra diagnostic output
#   ./scripts/test-audio-linux.sh --no-cleanup  # Keep virtual sinks after test
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
NO_CLEANUP=false
NULL_SINK_NAME="rsac_test_null_sink"
NULL_SINK_MODULE_ID=""
TEST_WAV=""
PLAYER_PID=""

PASS_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0
RESULTS=()

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
# Usage: run_cargo_test <tier> <label> <test-filter> [extra-args...]
run_cargo_test() {
    local tier="$1" label="$2" filter="$3"
    shift 3
    local extra_args=("$@")

    log_info "Running: cargo test --test ci_audio --features feat_linux -- $filter ${extra_args[*]:-}"

    set +e
    RSAC_CI_AUDIO_AVAILABLE=1 \
    cargo test --test ci_audio --features feat_linux \
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

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case "$1" in
        --tier)     TIER="$2"; shift 2 ;;
        --verbose)  VERBOSE=true; shift ;;
        --no-cleanup) NO_CLEANUP=true; shift ;;
        -h|--help)
            echo "Usage: $0 [--tier system|device|process|all] [--verbose] [--no-cleanup]"
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

    # Remove virtual null sink
    if [ "$NO_CLEANUP" = false ] && [ -n "$NULL_SINK_MODULE_ID" ]; then
        pactl unload-module "$NULL_SINK_MODULE_ID" 2>/dev/null || true
        log_info "Unloaded null sink module $NULL_SINK_MODULE_ID"
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

# 1. PipeWire running
if ! pw-cli ls >/dev/null 2>&1; then
    log_fail "PipeWire is not running or pw-cli is not available."
    echo ""
    echo "  Fix: systemctl --user start pipewire pipewire-pulse wireplumber"
    echo "  Install: sudo apt install pipewire pipewire-pulse libpipewire-0.3-dev"
    exit 2
fi
log_ok "PipeWire is running"

# 2. Required tools
for tool in pw-cli pw-play pactl pw-dump cargo; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        log_fail "Required tool '$tool' not found in PATH."
        exit 2
    fi
done
log_ok "All required tools present (pw-cli, pw-play, pactl, pw-dump, cargo)"

# 3. Rust project compiles
log_info "Checking compilation..."
if ! cargo check --features feat_linux 2>&1 | tail -3; then
    log_fail "cargo check --features feat_linux failed"
    exit 2
fi
log_ok "Project compiles with feat_linux"

# ---------------------------------------------------------------------------
# Setup: virtual null sink + test WAV
# ---------------------------------------------------------------------------

log_header "Test Environment Setup"

# Create virtual null sink (provides a guaranteed audio device even on headless boxes)
if pactl list sinks short 2>/dev/null | grep -q "$NULL_SINK_NAME"; then
    log_info "Null sink '$NULL_SINK_NAME' already exists — reusing"
else
    NULL_SINK_MODULE_ID=$(pactl load-module module-null-sink \
        sink_name="$NULL_SINK_NAME" \
        sink_properties=device.description="rsac-test-null-sink" \
        2>/dev/null) || true
    if [ -n "$NULL_SINK_MODULE_ID" ]; then
        log_ok "Created virtual null sink '$NULL_SINK_NAME' (module $NULL_SINK_MODULE_ID)"
    else
        log_warn "Could not create null sink — tests will use existing devices"
    fi
fi

# Generate a 5-second 440 Hz test WAV file
TEST_WAV=$(mktemp /tmp/rsac_test_tone_XXXXXX.wav)
# Use Python to generate a minimal WAV — avoids needing sox/ffmpeg
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

if [ -f "$TEST_WAV" ] && [ "$(stat -c%s "$TEST_WAV" 2>/dev/null || stat -f%z "$TEST_WAV" 2>/dev/null)" -gt 44 ]; then
    log_ok "Generated test WAV: $TEST_WAV"
else
    log_fail "Failed to generate test WAV file"
    exit 2
fi

if [ "$VERBOSE" = true ]; then
    log_info "PipeWire nodes:"
    pw-cli ls Node 2>/dev/null | head -20 || true
    echo ""
    log_info "Sinks:"
    pactl list sinks short 2>/dev/null || true
fi

# ============================================================================
#  TIER 1: System Capture
# ============================================================================

run_system_tests() {
    log_header "Tier 1: System Capture"

    # Start background audio so there's something to capture
    pw-play "$TEST_WAV" &
    PLAYER_PID=$!
    sleep 0.5
    log_info "Background player PID=$PLAYER_PID"

    local tier_failed=0

    # Integration tests from ci_audio
    run_cargo_test "system" "system_capture_receives_audio" "test_system_capture_receives_audio" --nocapture || ((tier_failed++)) || true
    run_cargo_test "system" "capture_format_correct" "test_capture_format_correct" --nocapture || ((tier_failed++)) || true

    # Stream lifecycle tests (use system capture)
    run_cargo_test "system" "stream_start_read_stop" "test_stream_start_read_stop" --nocapture || ((tier_failed++)) || true
    run_cargo_test "system" "stream_stop_idempotent" "test_stream_stop_idempotent" --nocapture || ((tier_failed++)) || true
    run_cargo_test "system" "drop_while_running" "test_drop_while_running" --nocapture || ((tier_failed++)) || true

    # Platform capabilities (no audio hardware needed)
    run_cargo_test "system" "capabilities_query" "test_capabilities_query" --nocapture || ((tier_failed++)) || true
    run_cargo_test "system" "backend_name_matches_platform" "test_backend_name_matches_platform" --nocapture || ((tier_failed++)) || true

    # Stop background player
    if kill -0 "$PLAYER_PID" 2>/dev/null; then
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
    pw-play "$TEST_WAV" &
    PLAYER_PID=$!
    sleep 0.5
    log_info "Background player PID=$PLAYER_PID"

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
    if kill -0 "$PLAYER_PID" 2>/dev/null; then
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

    local tier_failed=0

    # We do NOT start a background player here — the tests themselves spawn
    # pw-play as a child and capture from its PID.

    # Application capture tests
    run_cargo_test "process" "app_capture_by_process_id" "test_app_capture_by_process_id" --nocapture || ((tier_failed++)) || true
    run_cargo_test "process" "app_capture_by_pipewire_node_id" "test_app_capture_by_pipewire_node_id" --nocapture || ((tier_failed++)) || true
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
