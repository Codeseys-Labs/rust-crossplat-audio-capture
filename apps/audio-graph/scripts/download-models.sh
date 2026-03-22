#!/usr/bin/env bash
# download-models.sh — Download ML model files for AudioGraph
#
# Usage:
#   ./scripts/download-models.sh              # Download Whisper model only
#   ./scripts/download-models.sh --with-sidecar  # Also download LFM2 sidecar model
#
# Models are placed in the models/ directory relative to the project root.
# The script is idempotent — existing files are skipped.

set -euo pipefail

# ---------------------------------------------------------------------------
# Color helpers
# ---------------------------------------------------------------------------
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m' # No Color

info()    { echo -e "${BLUE}ℹ${NC}  $*"; }
success() { echo -e "${GREEN}✔${NC}  $*"; }
warn()    { echo -e "${YELLOW}⚠${NC}  $*"; }
error()   { echo -e "${RED}✖${NC}  $*" >&2; }

# ---------------------------------------------------------------------------
# Resolve project root (directory containing this script's parent)
# ---------------------------------------------------------------------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
MODELS_DIR="${PROJECT_ROOT}/models"

# ---------------------------------------------------------------------------
# Parse arguments
# ---------------------------------------------------------------------------
WITH_SIDECAR=false
for arg in "$@"; do
    case "$arg" in
        --with-sidecar) WITH_SIDECAR=true ;;
        -h|--help)
            echo "Usage: $0 [--with-sidecar]"
            echo ""
            echo "Downloads ML models for AudioGraph."
            echo ""
            echo "Options:"
            echo "  --with-sidecar   Also download LFM2-350M-Extract GGUF for entity extraction"
            echo "  -h, --help       Show this help message"
            exit 0
            ;;
        *)
            error "Unknown argument: $arg"
            echo "Run '$0 --help' for usage."
            exit 1
            ;;
    esac
done

# ---------------------------------------------------------------------------
# Detect download tool
# ---------------------------------------------------------------------------
download() {
    local url="$1"
    local dest="$2"

    if command -v curl &>/dev/null; then
        curl -L --progress-bar -o "$dest" "$url"
    elif command -v wget &>/dev/null; then
        wget --show-progress -q -O "$dest" "$url"
    else
        error "Neither 'curl' nor 'wget' found. Please install one and retry."
        exit 1
    fi
}

# ---------------------------------------------------------------------------
# Verify downloaded file (exists and non-empty)
# ---------------------------------------------------------------------------
verify_file() {
    local path="$1"
    local label="$2"

    if [[ ! -f "$path" ]]; then
        error "Download failed: ${label} not found at ${path}"
        return 1
    fi

    local size
    size=$(stat --format='%s' "$path" 2>/dev/null || stat -f '%z' "$path" 2>/dev/null || echo "0")

    if [[ "$size" -eq 0 ]]; then
        error "Download failed: ${label} is empty (0 bytes)"
        rm -f "$path"
        return 1
    fi

    # Human-readable size
    local human_size
    if [[ "$size" -ge 1073741824 ]]; then
        human_size="$(awk "BEGIN {printf \"%.1f GB\", ${size}/1073741824}")"
    elif [[ "$size" -ge 1048576 ]]; then
        human_size="$(awk "BEGIN {printf \"%.1f MB\", ${size}/1048576}")"
    else
        human_size="$(awk "BEGIN {printf \"%.1f KB\", ${size}/1024}")"
    fi

    success "${label}: ${human_size}"
    return 0
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
echo ""
echo -e "${BOLD}AudioGraph Model Downloader${NC}"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Create models directory
mkdir -p "$MODELS_DIR"
info "Models directory: ${MODELS_DIR}"
echo ""

# Track what we downloaded
DOWNLOADED=()
SKIPPED=()

# --- Whisper model -----------------------------------------------------------
WHISPER_URL="https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin"
WHISPER_FILE="${MODELS_DIR}/ggml-small.en.bin"
WHISPER_LABEL="Whisper small.en (GGML)"

if [[ -f "$WHISPER_FILE" ]]; then
    warn "Skipping ${WHISPER_LABEL} — already exists"
    SKIPPED+=("$WHISPER_LABEL")
else
    info "Downloading ${WHISPER_LABEL}..."
    info "  URL: ${WHISPER_URL}"
    echo ""
    download "$WHISPER_URL" "$WHISPER_FILE"
    echo ""
    if verify_file "$WHISPER_FILE" "$WHISPER_LABEL"; then
        DOWNLOADED+=("$WHISPER_LABEL")
    else
        exit 1
    fi
fi

# --- LFM2 sidecar model (optional) ------------------------------------------
if [[ "$WITH_SIDECAR" == true ]]; then
    echo ""
    SIDECAR_URL="https://huggingface.co/QuantFactory/LFM2-350M-Extract-GGUF/resolve/main/LFM2-350M-Extract.Q8_0.gguf"
    SIDECAR_FILE="${MODELS_DIR}/LFM2-350M-Extract.Q8_0.gguf"
    SIDECAR_LABEL="LFM2-350M-Extract (GGUF Q8_0)"

    if [[ -f "$SIDECAR_FILE" ]]; then
        warn "Skipping ${SIDECAR_LABEL} — already exists"
        SKIPPED+=("$SIDECAR_LABEL")
    else
        info "Downloading ${SIDECAR_LABEL}..."
        info "  URL: ${SIDECAR_URL}"
        echo ""
        download "$SIDECAR_URL" "$SIDECAR_FILE"
        echo ""
        if verify_file "$SIDECAR_FILE" "$SIDECAR_LABEL"; then
            DOWNLOADED+=("$SIDECAR_LABEL")
        else
            exit 1
        fi
    fi
fi

# --- Summary -----------------------------------------------------------------
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo -e "${BOLD}Summary${NC}"
echo ""

if [[ ${#DOWNLOADED[@]} -gt 0 ]]; then
    for item in "${DOWNLOADED[@]}"; do
        success "Downloaded: ${item}"
    done
fi

if [[ ${#SKIPPED[@]} -gt 0 ]]; then
    for item in "${SKIPPED[@]}"; do
        warn "Skipped:    ${item}"
    done
fi

echo ""
info "Models directory contents:"
ls -lh "$MODELS_DIR"/ 2>/dev/null || info "  (empty)"
echo ""

if [[ "$WITH_SIDECAR" != true ]]; then
    info "Tip: Run with ${BOLD}--with-sidecar${NC} to also download the LFM2 entity extraction model."
fi

success "Done!"
