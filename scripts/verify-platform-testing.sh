#!/bin/bash

# Platform Testing Setup Verification Script
# Verifies that the platform testing environment is ready

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Logging functions
log_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

log_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

print_banner() {
    echo "=============================================================================="
    echo "🔍 Platform Testing Setup Verification"
    echo "=============================================================================="
}

# Check Docker and Docker Compose
check_docker() {
    log_info "Checking Docker setup..."
    
    if ! command -v docker &> /dev/null; then
        log_error "Docker is not installed"
        return 1
    fi
    
    if ! docker info &> /dev/null; then
        log_error "Docker daemon is not running"
        return 1
    fi
    
    if ! command -v docker-compose &> /dev/null; then
        log_error "Docker Compose is not installed"
        return 1
    fi
    
    log_success "Docker and Docker Compose are available"
    return 0
}

# Check KVM support
check_kvm() {
    log_info "Checking KVM support..."
    
    if [ ! -e /dev/kvm ]; then
        log_warning "KVM device not found - Windows and macOS containers may not work"
        return 1
    fi
    
    if [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
        log_warning "KVM device not accessible - you may need to add user to kvm group"
        log_info "Run: sudo usermod -a -G kvm \$USER && newgrp kvm"
        return 1
    fi
    
    log_success "KVM support is available"
    return 0
}

# Check audio system
check_audio() {
    log_info "Checking audio system..."
    
    if [ ! -d /dev/snd ]; then
        log_warning "Audio devices not found in /dev/snd"
        return 1
    fi
    
    if ! groups | grep -q audio; then
        log_warning "User not in audio group - add with: sudo usermod -a -G audio \$USER"
        return 1
    fi
    
    log_success "Audio system appears to be available"
    return 0
}

# Check required files
check_files() {
    log_info "Checking required files..."
    
    local required_files=(
        "docker-compose.testing.yml"
        "docker/testing/Dockerfile.linux-pipewire"
        "docker/testing/Dockerfile.macos"
        "docker/testing/scripts/test-linux-pipewire.sh"
        "docker/testing/scripts/test-macos-coreaudio.sh"
        "docker/testing/scripts/setup-windows-test.ps1"
        "docker/testing/scripts/orchestrate-tests.sh"
    )
    
    local missing_files=()
    
    for file in "${required_files[@]}"; do
        if [ ! -f "$file" ]; then
            missing_files+=("$file")
        fi
    done
    
    if [ ${#missing_files[@]} -gt 0 ]; then
        log_error "Missing required files:"
        for file in "${missing_files[@]}"; do
            echo "  - $file"
        done
        return 1
    fi
    
    log_success "All required files are present"
    return 0
}

# Check Docker Compose configuration
check_compose_config() {
    log_info "Checking Docker Compose configuration..."
    
    if docker-compose -f docker-compose.testing.yml config &> /dev/null; then
        log_success "Docker Compose configuration is valid"
        return 0
    else
        log_error "Docker Compose configuration is invalid"
        return 1
    fi
}

# Check available disk space
check_disk_space() {
    log_info "Checking available disk space..."
    
    local available_gb=$(df . | awk 'NR==2 {print int($4/1024/1024)}')
    
    if [ "$available_gb" -lt 20 ]; then
        log_warning "Low disk space: ${available_gb}GB available (recommend 20GB+)"
        return 1
    fi
    
    log_success "Sufficient disk space: ${available_gb}GB available"
    return 0
}

# Check system resources
check_resources() {
    log_info "Checking system resources..."
    
    local total_ram_gb=$(free -g | awk 'NR==2{print $2}')
    local cpu_cores=$(nproc)
    
    if [ "$total_ram_gb" -lt 8 ]; then
        log_warning "Low RAM: ${total_ram_gb}GB (recommend 16GB+ for all platforms)"
    else
        log_success "RAM: ${total_ram_gb}GB available"
    fi
    
    if [ "$cpu_cores" -lt 4 ]; then
        log_warning "Few CPU cores: ${cpu_cores} (recommend 8+ for all platforms)"
    else
        log_success "CPU cores: ${cpu_cores} available"
    fi
    
    return 0
}

# Test basic container functionality
test_basic_container() {
    log_info "Testing basic container functionality..."
    
    if docker run --rm hello-world &> /dev/null; then
        log_success "Basic container functionality works"
        return 0
    else
        log_error "Basic container functionality failed"
        return 1
    fi
}

# Main verification function
main() {
    print_banner
    
    local overall_success=true
    
    # Run all checks
    check_docker || overall_success=false
    check_kvm || overall_success=false
    check_audio || overall_success=false
    check_files || overall_success=false
    check_compose_config || overall_success=false
    check_disk_space || overall_success=false
    check_resources || overall_success=false
    test_basic_container || overall_success=false
    
    echo "=============================================================================="
    
    if [ "$overall_success" = true ]; then
        log_success "🎉 Platform testing setup verification passed!"
        echo ""
        log_info "You can now run platform tests:"
        echo "  make test-all-platforms      # Test all platforms"
        echo "  make test-linux-pipewire     # Test Linux PipeWire only"
        echo "  make test-windows-manual      # Test Windows (manual setup)"
        echo "  make test-macos-coreaudio     # Test macOS Core Audio only"
        echo "  make test-orchestrate         # Run test orchestrator"
        echo ""
        log_info "For detailed documentation, see docs/PLATFORM_TESTING.md"
    else
        log_error "❌ Platform testing setup verification failed"
        echo ""
        log_info "Please fix the issues above before running platform tests"
        echo ""
        log_info "Common fixes:"
        echo "  sudo usermod -a -G docker,kvm,audio \$USER"
        echo "  newgrp docker"
        echo "  sudo systemctl start docker"
        exit 1
    fi
}

# Run main function
main "$@"
