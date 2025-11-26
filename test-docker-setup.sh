#!/bin/bash

# Quick test script to verify Docker setup is working
# This script runs a minimal test of the Docker-based cross-compilation setup

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
    echo "🐳 Docker Setup Verification Test"
    echo "=============================================================================="
}

# Test Docker and Docker Compose availability
test_docker_availability() {
    log_info "Testing Docker availability..."
    
    if ! command -v docker &> /dev/null; then
        log_error "Docker is not installed or not in PATH"
        return 1
    fi
    
    if ! docker info &> /dev/null; then
        log_error "Docker daemon is not running or not accessible"
        return 1
    fi
    
    if ! command -v docker-compose &> /dev/null; then
        log_error "Docker Compose is not installed or not in PATH"
        return 1
    fi
    
    log_success "Docker and Docker Compose are available"
    return 0
}

# Test quick compilation check
test_quick_compilation() {
    log_info "Running quick compilation check..."
    
    if make docker-test-quick 2>&1 | tee /tmp/docker-test-quick.log; then
        log_success "Quick compilation check passed"
        return 0
    else
        log_error "Quick compilation check failed"
        log_info "Check /tmp/docker-test-quick.log for details"
        return 1
    fi
}

# Test individual platform containers
test_platform_containers() {
    log_info "Testing individual platform containers..."
    
    local success=true
    
    # Test cargo-xwin container
    log_info "Testing cargo-xwin container..."
    if docker run --rm messense/cargo-xwin:latest cargo xwin --version &> /dev/null; then
        log_success "cargo-xwin container is working"
    else
        log_warning "cargo-xwin container test failed"
        success=false
    fi
    
    # Test rust-linux-darwin-builder container
    log_info "Testing rust-linux-darwin-builder container..."
    if docker run --rm joseluisq/rust-linux-darwin-builder:1.88.0 rustc --version &> /dev/null; then
        log_success "rust-linux-darwin-builder container is working"
    else
        log_warning "rust-linux-darwin-builder container test failed"
        success=false
    fi
    
    return $([ "$success" = true ] && echo 0 || echo 1)
}

# Test Docker Compose configuration
test_docker_compose_config() {
    log_info "Testing Docker Compose configuration..."
    
    if docker-compose -f docker-compose.unified.yml config &> /dev/null; then
        log_success "Docker Compose configuration is valid"
        return 0
    else
        log_error "Docker Compose configuration is invalid"
        return 1
    fi
}

# Test script permissions
test_script_permissions() {
    log_info "Testing script permissions..."
    
    local scripts=(
        "scripts/docker-test-all.sh"
        "scripts/aggregate-test-results.sh"
    )
    
    local success=true
    
    for script in "${scripts[@]}"; do
        if [ -x "$script" ]; then
            log_success "$script is executable"
        else
            log_error "$script is not executable"
            success=false
        fi
    done
    
    return $([ "$success" = true ] && echo 0 || echo 1)
}

# Main test function
main() {
    print_banner
    
    local overall_success=true
    
    # Run all tests
    test_docker_availability || overall_success=false
    test_docker_compose_config || overall_success=false
    test_script_permissions || overall_success=false
    test_platform_containers || overall_success=false
    
    # Only run compilation test if basic tests pass
    if [ "$overall_success" = true ]; then
        test_quick_compilation || overall_success=false
    else
        log_warning "Skipping compilation test due to previous failures"
    fi
    
    echo "=============================================================================="
    
    if [ "$overall_success" = true ]; then
        log_success "🎉 All Docker setup tests passed!"
        log_info "You can now run:"
        echo "  make docker-test-all      # Full cross-platform testing"
        echo "  make docker-test-quick    # Quick compilation check"
        echo "  make docker-dev           # Development environment"
        echo "  make docker-dashboard     # Generate test dashboard"
    else
        log_error "❌ Some Docker setup tests failed"
        log_info "Please check the errors above and fix the issues"
        exit 1
    fi
}

# Run main function
main "$@"
