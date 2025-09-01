# Makefile for rust-crossplat-audio-capture

.PHONY: help check check-linux check-windows check-macos check-all cross-compile test clean

# Default target
help:
	@echo "Available targets:"
	@echo "  check         - Check compilation for current platform"
	@echo "  check-linux   - Check Linux compilation (feat_linux)"
	@echo "  check-windows - Check Windows compilation (feat_windows)"
	@echo "  check-macos   - Check macOS compilation (feat_macos)"
	@echo "  check-all     - Check all platform compilations"
	@echo "  cross-compile - Run full cross-compilation check"
	@echo "  test          - Run tests for current platform"
	@echo "  clean         - Clean build artifacts"

# Check current platform
check:
	cargo check --examples

# Check specific platforms with cross-compilation
check-linux:
	@echo "🐧 Checking Linux compilation..."
	cargo check --target x86_64-unknown-linux-gnu --no-default-features --features feat_linux --examples

check-windows:
	@echo "🪟 Checking Windows compilation..."
	cargo check --target x86_64-pc-windows-msvc --no-default-features --features feat_windows --examples

check-macos:
	@echo "🍎 Checking macOS compilation..."
	cargo check --target x86_64-apple-darwin --no-default-features --features feat_macos --examples

# Check all platforms quickly
check-all: check-linux check-windows check-macos
	@echo "✅ All platform checks completed"

# Run full cross-compilation check with detailed output
cross-compile:
	@./scripts/cross-compile-check.sh

# Run tests
test:
	cargo test

# Clean build artifacts
clean:
	cargo clean
