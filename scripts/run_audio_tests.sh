#!/bin/bash
set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Function to clean up background processes on exit
cleanup() {
    echo -e "${YELLOW}Cleaning up processes...${NC}"
    pkill -P $$ || true
    exit
}

# Set up trap to call cleanup function on exit
trap cleanup EXIT INT TERM

# Parse command-line arguments
BACKEND=""
TEST_TYPE=""
DURATION=5
OUTPUT_DIR="./test-results"
VERBOSE=false

print_usage() {
    echo "Usage: $0 [options]"
    echo "Options:"
    echo "  -b, --backend BACKEND    Specify backend (auto, pulseaudio, pipewire, wasapi, coreaudio)"
    echo "  -t, --type TYPE          Test type (all, application, system)"
    echo "  -d, --duration SECONDS   Test duration in seconds (default: 5)"
    echo "  -o, --output-dir DIR     Output directory for test results (default: ./test-results)"
    echo "  -v, --verbose            Enable verbose output"
    echo "  -h, --help               Display this help message"
}

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        -b|--backend)
            BACKEND="$2"
            shift 2
            ;;
        -t|--type)
            TEST_TYPE="$2"
            shift 2
            ;;
        -d|--duration)
            DURATION="$2"
            shift 2
            ;;
        -o|--output-dir)
            OUTPUT_DIR="$2"
            shift 2
            ;;
        -v|--verbose)
            VERBOSE=true
            shift
            ;;
        -h|--help)
            print_usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            print_usage
            exit 1
            ;;
    esac
done

# Set defaults if not specified
if [ -z "$BACKEND" ]; then
    BACKEND="auto"
fi

if [ -z "$TEST_TYPE" ]; then
    TEST_TYPE="all"
fi

# Create output directory
mkdir -p "$OUTPUT_DIR"

echo -e "${YELLOW}=== RUST CROSS-PLATFORM AUDIO CAPTURE TESTS ===${NC}"
echo "Backend: $BACKEND"
echo "Test type: $TEST_TYPE"
echo "Duration: $DURATION seconds"
echo "Output directory: $OUTPUT_DIR"
echo

# Determine platform
PLATFORM="unknown"
if [[ "$OSTYPE" == "linux-gnu"* ]]; then
    PLATFORM="linux"
elif [[ "$OSTYPE" == "darwin"* ]]; then
    PLATFORM="macos"
elif [[ "$OSTYPE" == "msys" || "$OSTYPE" == "cygwin" || "$OSTYPE" == "win32" ]]; then
    PLATFORM="windows"
fi
echo -e "${YELLOW}Detected platform: ${PLATFORM}${NC}"

# Set up environment based on platform and backend
setup_environment() {
    if [ "$PLATFORM" == "linux" ]; then
        if [ "$BACKEND" == "pulseaudio" ] || [ "$BACKEND" == "auto" ]; then
            # Check if PulseAudio is running
            if ! pulseaudio --check > /dev/null 2>&1; then
                echo -e "${YELLOW}Starting PulseAudio...${NC}"
                pulseaudio --start
                sleep 2
            fi
        elif [ "$BACKEND" == "pipewire" ]; then
            # Check if PipeWire is running
            if ! pgrep -x "pipewire" > /dev/null; then
                echo -e "${YELLOW}Starting PipeWire...${NC}"
                pipewire &
                sleep 2
            fi
            
            # Check if PipeWire-Pulse is running
            if ! pgrep -x "pipewire-pulse" > /dev/null; then
                echo -e "${YELLOW}Starting PipeWire-Pulse...${NC}"
                pipewire-pulse &
                sleep 2
            fi
        fi
    fi
}

# Set up virtual audio devices for testing
setup_virtual_devices() {
    if [ "$PLATFORM" == "linux" ]; then
        if [ "$BACKEND" == "pulseaudio" ] || [ "$BACKEND" == "auto" ]; then
            echo -e "${YELLOW}Setting up PulseAudio virtual devices...${NC}"
            pacmd load-module module-null-sink sink_name=test_sink sink_properties=device.description="Test Sink"
            pacmd load-module module-null-sink sink_name=system_monitor sink_properties=device.description="System Monitor"
            pacmd load-module module-loopback source_dont_move=true sink_dont_move=true source=system_monitor.monitor sink=@DEFAULT_SINK@
        elif [ "$BACKEND" == "pipewire" ]; then
            echo -e "${YELLOW}Setting up PipeWire virtual devices...${NC}"
            pactl load-module module-null-sink sink_name=test_sink sink_properties=device.description=test_sink
            pactl load-module module-null-sink sink_name=system_monitor sink_properties=device.description=system_monitor
            pactl load-module module-loopback source_dont_move=true sink_dont_move=true source=system_monitor.monitor sink=@DEFAULT_SINK@
        fi
    fi
}

# Run test application
run_test_app() {
    local test_type=$1
    local backend=$2
    local duration=$3
    local output_dir=$4
    
    # Build test app example using our standardized test runner
    echo -e "${YELLOW}Building test application...${NC}"
    cargo build --bin test-runner
    
    # Run the application test
    echo -e "${YELLOW}Running $backend $test_type test...${NC}"
    TEST_RESULT_FILE="${output_dir}/${backend}_${test_type}_$(date +%Y%m%d_%H%M%S).json"
    
    cargo run --bin test-runner -- \
        --backend $backend \
        --test-type $test_type \
        --duration $duration \
        --output-dir $output_dir \
        --result-file $TEST_RESULT_FILE
        
    # Check if test succeeded
    if [ $? -eq 0 ]; then
        echo -e "${GREEN}✅ Test completed successfully!${NC}"
    else
        echo -e "${RED}❌ Test failed!${NC}"
    fi
}

# Download test audio if needed
if [ ! -f "./test_audio.wav" ]; then
    echo -e "${YELLOW}Downloading test audio...${NC}"
    ./scripts/download_test_audio.sh
fi

# Set up the test environment
setup_environment
setup_virtual_devices

# Determine tests to run based on backend and platform
if [ "$BACKEND" == "auto" ]; then
    if [ "$PLATFORM" == "linux" ]; then
        # Check for PipeWire first, fall back to PulseAudio
        if pactl info 2>/dev/null | grep -q "Server Name: PipeWire"; then
            BACKENDS=("pipewire")
        else
            BACKENDS=("pulseaudio")
        fi
    elif [ "$PLATFORM" == "macos" ]; then
        BACKENDS=("coreaudio")
    elif [ "$PLATFORM" == "windows" ]; then
        BACKENDS=("wasapi")
    else
        echo -e "${RED}Unsupported platform: $PLATFORM${NC}"
        exit 1
    fi
else
    BACKENDS=("$BACKEND")
fi

# Determine test types to run
if [ "$TEST_TYPE" == "all" ]; then
    TEST_TYPES=("application" "system")
else
    TEST_TYPES=("$TEST_TYPE")
fi

# Run tests for each backend and test type
for backend in "${BACKENDS[@]}"; do
    echo -e "${BLUE}Running tests for $backend backend${NC}"
    
    for test_type in "${TEST_TYPES[@]}"; do
        run_test_app "$test_type" "$backend" "$DURATION" "$OUTPUT_DIR"
    done
done

# Generate final report
echo -e "${YELLOW}\nGenerating test report...${NC}"
cargo run --bin test-report-generator -- --input-dir "$OUTPUT_DIR" --output-file "$OUTPUT_DIR/test_report.html"

echo -e "${GREEN}\nAll tests completed.${NC}"
echo -e "Results saved to: $OUTPUT_DIR"
echo -e "Test report: $OUTPUT_DIR/test_report.html"
