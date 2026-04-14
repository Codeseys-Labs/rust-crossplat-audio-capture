# Makefile for rsac-go — Go bindings for rsac
#
# Prerequisites:
#   - Go 1.22+
#   - Rust toolchain (cargo, rustc)
#   - CGo enabled (CGO_ENABLED=1, default on most systems)
#
# Directory layout:
#   bindings/rsac-ffi/     — Rust FFI crate (staticlib)
#   bindings/rsac-go/      — This Go package
#   bindings/rsac-go/lib/  — Built static library placed here for linking

SHELL := /bin/bash

# ── Paths ────────────────────────────────────────────────────────────────

# Root of the rsac repository (two levels up from bindings/rsac-go/).
RSAC_ROOT := $(shell cd ../.. && pwd)
FFI_DIR   := $(RSAC_ROOT)/bindings/rsac-ffi
GO_DIR    := $(shell pwd)
LIB_DIR   := $(GO_DIR)/lib

# Detect OS for platform-specific library names and linker flags.
UNAME_S := $(shell uname -s)

ifeq ($(UNAME_S),Darwin)
    STATIC_LIB := librsac_ffi.a
    # macOS needs frameworks for CoreAudio, Security, etc.
    EXTRA_LDFLAGS := -framework CoreAudio -framework AudioToolbox \
                     -framework CoreFoundation -framework Security \
                     -framework SystemConfiguration
else ifeq ($(UNAME_S),Linux)
    STATIC_LIB := librsac_ffi.a
    EXTRA_LDFLAGS := -lpipewire-0.3 -lspa-0.2 -lpthread -ldl -lm
else
    # Windows (MSYS2/MinGW)
    STATIC_LIB := rsac_ffi.lib
    EXTRA_LDFLAGS := -lole32 -loleaut32 -lwinmm -lksuser -luuid
endif

# Rust target directory.
CARGO_TARGET := $(RSAC_ROOT)/target/release

# ── Targets ──────────────────────────────────────────────────────────────

.PHONY: all build rust-ffi go-build test clean lint fmt help

## all: Build the Rust staticlib and verify the Go package compiles.
all: build

## build: Build everything (Rust FFI + Go package).
build: rust-ffi go-build

## rust-ffi: Compile the Rust FFI crate as a static library.
rust-ffi:
	@echo "==> Building rsac-ffi static library..."
	cd $(FFI_DIR) && cargo build --release
	@mkdir -p $(LIB_DIR)
	@if [ -f "$(CARGO_TARGET)/$(STATIC_LIB)" ]; then \
		cp "$(CARGO_TARGET)/$(STATIC_LIB)" "$(LIB_DIR)/"; \
		echo "==> Copied $(STATIC_LIB) to $(LIB_DIR)/"; \
	else \
		echo "ERROR: $(CARGO_TARGET)/$(STATIC_LIB) not found."; \
		echo "       Make sure bindings/rsac-ffi/Cargo.toml has crate-type = [\"staticlib\"]"; \
		exit 1; \
	fi

## go-build: Compile the Go package (requires rust-ffi to have been built).
go-build:
	@echo "==> Building Go package..."
	CGO_ENABLED=1 \
	CGO_LDFLAGS="-L$(LIB_DIR) -lrsac_ffi $(EXTRA_LDFLAGS)" \
	go build -v ./...

## test: Run Go tests (requires rust-ffi to have been built).
test: rust-ffi
	@echo "==> Running Go tests..."
	CGO_ENABLED=1 \
	CGO_LDFLAGS="-L$(LIB_DIR) -lrsac_ffi $(EXTRA_LDFLAGS)" \
	go test -v -count=1 ./...

## test-pure: Run only pure Go tests (no CGo/FFI dependency).
test-pure:
	@echo "==> Running pure Go tests (no CGo)..."
	go test -v -count=1 -run 'TestErrorCode|TestError_|TestCaptureTarget|TestAudioBuffer|TestCaptureBuilder|TestDeviceKind|TestStream_Context|TestCallback|TestStreamResult|TestErrClosed' ./...

## lint: Run golangci-lint on the Go package.
lint:
	@echo "==> Running linter..."
	golangci-lint run ./...

## fmt: Format Go source files.
fmt:
	@echo "==> Formatting Go files..."
	gofmt -w .
	goimports -w .

## clean: Remove build artifacts.
clean:
	@echo "==> Cleaning..."
	rm -rf $(LIB_DIR)
	cd $(FFI_DIR) && cargo clean 2>/dev/null || true
	go clean -cache

## help: Show this help message.
help:
	@grep -E '^## ' $(MAKEFILE_LIST) | sed 's/## //' | column -t -s ':'
