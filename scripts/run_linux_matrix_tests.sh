#!/bin/bash
set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

echo -e "${YELLOW}=== RUST CROSS-PLATFORM AUDIO CAPTURE - LINUX TEST MATRIX ===${NC}"
echo "This script will run tests for all combinations of:"
echo "  - Audio servers: PulseAudio, PipeWire"
echo "  - Capture types: Application, System"
echo

# Create results directory
RESULTS_DIR="./test-results"
mkdir -p $RESULTS_DIR

# Run PipeWire tests
echo -e "${BLUE}Running PipeWire tests...${NC}"
docker compose up --build rsac-linux-pipewire

# Run PulseAudio tests
echo -e "${BLUE}Running PulseAudio tests...${NC}"
docker compose up --build rsac-linux-pulseaudio

# Generate consolidated report
echo -e "${BLUE}Generating test report...${NC}"
docker compose up --build rsac-report

# Display report location
echo -e "${GREEN}\nTest matrix completed!${NC}"
echo "Results are available in: ${RESULTS_DIR}/test_report.html"

# Check if test report exists
if [ -f "${RESULTS_DIR}/test_report.html" ]; then
    echo -e "${GREEN}Test report generated successfully!${NC}"
    if command -v xdg-open > /dev/null; then
        echo "Opening test report in browser..."
        xdg-open "${RESULTS_DIR}/test_report.html" || true
    fi
    exit 0
else
    echo -e "${RED}Failed to generate test report.${NC}"
    exit 1
fi
