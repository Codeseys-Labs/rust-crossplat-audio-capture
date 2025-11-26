# Makefile for rust-crossplat-audio-capture

.PHONY: help check check-linux check-windows check-macos check-all cross-compile test clean \
        docker-test docker-test-unified docker-test-linux docker-test-windows docker-test-macos \
        docker-dev docker-check docker-clean

# Default target
help:
	@echo "Available targets:"
	@echo ""
	@echo "Local compilation checks:"
	@echo "  check                - Check compilation for current platform"
	@echo "  check-linux          - Check Linux compilation (feat_linux)"
	@echo "  check-windows        - Check Windows compilation (feat_windows)"
	@echo "  check-macos          - Check macOS compilation (feat_macos)"
	@echo "  check-all            - Check all platform compilations"
	@echo "  check-windows-docker - Check Windows with cargo-xwin (robust)"
	@echo "  check-macos-docker   - Check macOS with Docker (robust)"
	@echo "  check-all-docker     - Check all platforms with Docker"
	@echo "  cross-compile        - Run full cross-compilation check"
	@echo ""
	@echo "Docker-based testing:"
	@echo "  docker-test          - Run unified cross-platform tests in Docker"
	@echo "  docker-test-unified  - Run all platform tests in single container"
	@echo "  docker-test-linux    - Run Linux audio tests in Docker"
	@echo "  docker-test-windows  - Run Windows cross-compilation tests"
	@echo "  docker-test-macos    - Run macOS cross-compilation tests"
	@echo "  docker-dev           - Start development container with all tools"
	@echo "  docker-check         - Quick compilation check for all platforms"
	@echo "  docker-clean         - Clean Docker volumes and containers"
	@echo ""
	@echo "Other targets:"
	@echo "  test                 - Run tests for current platform"
	@echo "  clean                - Clean build artifacts"

# Check current platform
check:
	cargo check --examples

# Check specific platforms with cross-compilation
check-linux:
	@echo "🐧 Checking Linux compilation..."
	cross check --target x86_64-unknown-linux-gnu --no-default-features --features feat_linux --examples

check-windows:
	@echo "🪟 Checking Windows compilation..."
	cross check --target x86_64-pc-windows-msvc --no-default-features --features feat_windows --examples

check-macos:
	@echo "🍎 Checking macOS compilation..."
	cross check --target x86_64-apple-darwin --no-default-features --features feat_macos --examples

check-macos-arm:
	@echo "🍎 Checking macOS ARM compilation..."
	cross check --target aarch64-apple-darwin --no-default-features --features feat_macos --examples

check-linux-arm:
	@echo "🐧 Checking Linux ARM compilation..."
	cross check --target aarch64-unknown-linux-gnu --no-default-features --features feat_linux --examples

# Check all platforms quickly
check-all: check-linux check-windows check-macos check-macos-arm check-linux-arm
	@echo "✅ All platform checks completed"

# Docker-based cross-compilation (more robust)
check-windows-docker:
	@echo "🪟 Checking Windows compilation with cargo-xwin..."
	cargo xwin check --target x86_64-pc-windows-msvc --no-default-features --features feat_windows --examples

check-macos-docker:
	@echo "🍎 Checking macOS compilation with Docker..."
	docker run --rm -v $(PWD):/workspace -w /workspace \
		--platform linux/amd64 \
		rust:1.88 \
		sh -c "rustup target add x86_64-apple-darwin && cargo check --target x86_64-apple-darwin --no-default-features --features feat_macos --examples"

# Check all platforms with Docker (most robust)
check-all-docker: check-linux check-windows-docker check-macos-docker
	@echo "✅ All Docker-based platform checks completed"

# Run full cross-compilation check with detailed output
cross-compile:
	@./scripts/cross-compile-check.sh

# Run tests
test:
	cargo test

# Clean build artifacts
clean:
	cargo clean

# =============================================================================
# Docker-based testing targets
# =============================================================================

# Run unified cross-platform tests in Docker
docker-test:
	@echo "🐳 Running unified cross-platform tests in Docker..."
	docker-compose -f docker-compose.unified.yml up --build rsac-unified

# Run all platform tests in single container
docker-test-unified:
	@echo "🐳 Running unified cross-platform compilation tests..."
	docker-compose -f docker-compose.unified.yml up --build rsac-check

# Run Linux audio tests in Docker with audio device access
docker-test-linux:
	@echo "🐧 Running Linux audio tests in Docker..."
	docker-compose -f docker-compose.unified.yml up --build rsac-linux-audio

# Run Windows cross-compilation tests using cargo-xwin
docker-test-windows:
	@echo "🪟 Running Windows cross-compilation tests..."
	docker-compose -f docker-compose.unified.yml up --build rsac-windows-cross

# Run macOS cross-compilation tests
docker-test-macos:
	@echo "🍎 Running macOS cross-compilation tests..."
	docker-compose -f docker-compose.unified.yml up --build rsac-macos-cross

# Start development container with all tools
docker-dev:
	@echo "🐳 Starting development container..."
	docker-compose -f docker-compose.unified.yml run --rm rsac-dev

# Quick compilation check for all platforms in Docker
docker-check:
	@echo "🐳 Running quick compilation check for all platforms..."
	docker-compose -f docker-compose.unified.yml run --rm rsac-check

# Use standalone cargo-xwin container for Windows testing
docker-cargo-xwin:
	@echo "🪟 Starting cargo-xwin container for Windows development..."
	docker-compose -f docker-compose.unified.yml run --rm cargo-xwin

# Clean Docker volumes and containers
docker-clean:
	@echo "🐳 Cleaning Docker volumes and containers..."
	docker-compose -f docker-compose.unified.yml down -v
	docker system prune -f
	docker volume prune -f

# Build all Docker images
docker-build:
	@echo "🐳 Building all Docker images..."
	docker-compose -f docker-compose.unified.yml build

# Show Docker logs
docker-logs:
	@echo "🐳 Showing Docker logs..."
	docker-compose -f docker-compose.unified.yml logs

# Run comprehensive Docker testing suite
docker-test-all:
	@echo "🚀 Running comprehensive Docker testing suite..."
	./scripts/docker-test-all.sh

# Run quick Docker compilation check for all platforms
docker-test-quick:
	@echo "⚡ Running quick Docker compilation check..."
	./scripts/docker-test-all.sh --quick

# Aggregate test results and generate reports
docker-aggregate-results:
	@echo "📊 Aggregating test results..."
	./scripts/aggregate-test-results.sh

# Generate HTML dashboard from test results
docker-dashboard:
	@echo "📈 Generating test dashboard..."
	./scripts/aggregate-test-results.sh --format html

# Full Docker testing workflow (test + aggregate + dashboard)
docker-full-test:
	@echo "🎯 Running full Docker testing workflow..."
	./scripts/docker-test-all.sh && ./scripts/aggregate-test-results.sh

# Test specific platform with Docker
docker-test-platform:
	@echo "🎯 Testing specific platform (usage: make docker-test-platform PLATFORM=linux|windows|macos)..."
	@if [ -z "$(PLATFORM)" ]; then \
		echo "❌ Error: PLATFORM variable is required. Usage: make docker-test-platform PLATFORM=linux"; \
		exit 1; \
	fi
	./scripts/docker-test-all.sh --platform $(PLATFORM)

# =============================================================================
# Platform-specific testing targets (library + dynamic_vlc example)
# =============================================================================

# Test library and dynamic_vlc example on all platforms
test-all-platforms:
	@echo "🎭 Testing library and dynamic_vlc example on all platforms..."
	docker-compose -f docker-compose.testing.yml up --build

# Test Linux PipeWire with library and VLC
test-linux-pipewire:
	@echo "🐧 Testing Linux PipeWire with library and VLC..."
	docker-compose -f docker-compose.testing.yml up --build linux-pipewire-test

# Test Windows with library and VLC (manual)
test-windows-manual:
	@echo "🪟 Starting Windows testing environment..."
	@echo "Access Windows at http://localhost:8006 after startup"
	@echo "Run setup script: C:\\scripts\\setup-windows-test.ps1"
	docker-compose -f docker-compose.testing.yml up --build windows-test

# Test macOS with library and VLC
test-macos-coreaudio:
	@echo "🍎 Testing macOS Core Audio with library and VLC..."
	docker-compose -f docker-compose.testing.yml up --build macos-test

# Run test orchestrator
test-orchestrate:
	@echo "🎭 Running test orchestrator..."
	docker-compose -f docker-compose.testing.yml up --build test-orchestrator

# Clean testing environment
test-clean:
	@echo "🧹 Cleaning testing environment..."
	docker-compose -f docker-compose.testing.yml down -v
	docker system prune -f

# =============================================================================
# No-KVM testing targets (for VM environments)
# =============================================================================

# Test Windows cross-compilation (no KVM required)
test-windows-cross:
	@echo "🪟 Testing Windows cross-compilation (no KVM required)..."
	docker-compose -f docker-compose.testing-no-kvm.yml up --build windows-cross-test

# Test all platforms without KVM
test-no-kvm:
	@echo "🐳 Testing all platforms without KVM requirements..."
	docker-compose -f docker-compose.testing-no-kvm.yml up --build

# Test Linux only (no KVM required)
test-linux-only:
	@echo "🐧 Testing Linux only (no KVM required)..."
	docker-compose -f docker-compose.testing-no-kvm.yml up --build linux-pipewire-test

# Clean no-KVM testing environment
test-clean-no-kvm:
	@echo "🧹 Cleaning no-KVM testing environment..."
	docker-compose -f docker-compose.testing-no-kvm.yml down -v
	docker system prune -f
