# Makefile for rust-crossplat-audio-capture

.PHONY: help check check-linux check-windows check-macos check-all cross-compile test clean

# Default target
help:
	@echo "Available targets:"
	@echo "  check                - Check compilation for current platform"
	@echo "  check-linux          - Check Linux compilation (feat_linux)"
	@echo "  check-windows        - Check Windows compilation (feat_windows)"
	@echo "  check-macos          - Check macOS compilation (feat_macos)"
	@echo "  check-all            - Check all platform compilations"
	@echo "  check-windows-docker - Check Windows with cargo-xwin (robust)"
	@echo "  check-macos-docker   - Check macOS with Docker (robust)"
	@echo "  check-all-docker     - Check all platforms with Docker"
	@echo "  cross-compile        - Run full cross-compilation check"
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
