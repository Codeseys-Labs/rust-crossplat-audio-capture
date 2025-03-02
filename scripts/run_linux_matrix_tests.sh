#!/bin/bash
set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${YELLOW}=== RUST CROSS-PLATFORM AUDIO CAPTURE - LINUX TEST MATRIX ===${NC}"
echo "This script will run tests for all combinations of:"
echo "  - Audio servers: PulseAudio, PipeWire"
echo "  - Capture types: Application, System"
echo

# Create results directory
RESULTS_DIR="./test-results"
mkdir -p $RESULTS_DIR

# Run all tests in the matrix
echo -e "${YELLOW}[1/4] Running PipeWire Application Capture Test${NC}"
docker-compose up --build rsac-linux-pipewire-app

echo -e "${YELLOW}[2/4] Running PipeWire System Capture Test${NC}"
docker-compose up --build rsac-linux-pipewire-system

echo -e "${YELLOW}[3/4] Running PulseAudio Application Capture Test${NC}"
docker-compose up --build rsac-linux-pulseaudio-app

echo -e "${YELLOW}[4/4] Running PulseAudio System Capture Test${NC}"
docker-compose up --build rsac-linux-pulseaudio-system

# Check test results
echo -e "${YELLOW}\nVerifying test results...${NC}"
SUCCESS=true

# Check for result files
EXPECTED_FILES=(
    "pipewire_app_capture"
    "pipewire_system_capture"
    "pulseaudio_app_capture"
    "pulseaudio_system_capture"
)

for TEST in "${EXPECTED_FILES[@]}"; do
    COUNT=$(find $RESULTS_DIR -name "${TEST}*" | wc -l)
    
    if [ $COUNT -gt 0 ]; then
        echo -e "✅ ${TEST}: ${GREEN}PASSED${NC} ($COUNT result files found)"
    else
        echo -e "❌ ${TEST}: ${RED}FAILED${NC} (No result files found)"
        SUCCESS=false
    fi
done

echo
if $SUCCESS; then
    echo -e "${GREEN}All tests completed successfully!${NC}"
    exit 0
else
    echo -e "${RED}Some tests failed. Check logs for details.${NC}"
    exit 1
fi 