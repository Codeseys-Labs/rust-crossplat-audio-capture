# Makefile for rust-crossplat-audio-capture

.PHONY: help check check-linux check-linux-arm check-all cross-compile test clean
# rsac-0645: check-windows/check-macos were advertised in help with no recipes —
# cross-rs has no MSVC/Darwin images, so those checks need CI or real hardware.

# Default target
help:
	@echo "Available targets:"
	@echo ""
	@echo "Local compilation checks:"
	@echo "  check                - Check compilation for current platform"
	@echo "  check-linux          - Check Linux compilation (feat_linux)"
	@echo "  check-all            - Check all platform compilations"
	@echo "  cross-compile        - Run full cross-compilation check"
	@echo ""
	@echo "Other targets:"
	@echo "  test                 - Run tests for current platform"
	@echo "  clean                - Clean build artifacts"

# Check current platform
check:
	cargo check --examples

# Check specific platforms with cross-compilation.
# NOTE (rsac-a3c4): cross-rs has no MSVC or Darwin images, so Windows/macOS
# cross-checks via `cross` are impossible — use CI or real hardware.
check-linux:
	@echo "🐧 Checking Linux compilation..."
	cross check --target x86_64-unknown-linux-gnu --no-default-features --features feat_linux --examples

check-linux-arm:
	@echo "🐧 Checking Linux ARM compilation..."
	cross check --target aarch64-unknown-linux-gnu --no-default-features --features feat_linux --examples

# Check all cross-able platforms quickly
check-all: check-linux check-linux-arm
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
