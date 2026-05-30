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

# cgo always links through the GNU toolchain (gcc/MinGW on Windows), which
# consumes GNU-archive static libs — librsac_ffi.a — so that is the preferred
# artifact on every platform. The Windows path also accepts rsac_ffi.lib as a
# fallback for an MSVC-built staticlib, but cgo can only link the GNU .a.
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
    # Windows (MSYS2/MinGW). Build rsac-ffi for the *-pc-windows-gnu target so
    # cargo emits librsac_ffi.a (GNU archive) that cgo's gcc can link.
    STATIC_LIB := librsac_ffi.a
    # WASAPI/COM libs plus the Win32 system libs the Rust std runtime pulls in.
    EXTRA_LDFLAGS := -lole32 -loleaut32 -lwinmm -lksuser -luuid \
                     -lbcrypt -lntdll -luserenv -lws2_32 -ladvapi32 -lkernel32
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
	@if [ -f "$(CARGO_TARGET)/librsac_ffi.a" ]; then \
		cp "$(CARGO_TARGET)/librsac_ffi.a" "$(LIB_DIR)/librsac_ffi.a"; \
		echo "==> Copied librsac_ffi.a to $(LIB_DIR)/"; \
	elif [ -f "$(CARGO_TARGET)/rsac_ffi.lib" ]; then \
		cp "$(CARGO_TARGET)/rsac_ffi.lib" "$(LIB_DIR)/librsac_ffi.a"; \
		echo "==> Copied rsac_ffi.lib to $(LIB_DIR)/librsac_ffi.a (note: cgo links via the GNU toolchain; an MSVC .lib may not link — prefer the *-pc-windows-gnu target)"; \
	else \
		echo "ERROR: neither librsac_ffi.a nor rsac_ffi.lib found in $(CARGO_TARGET)."; \
		echo "       bindings/rsac-ffi/Cargo.toml must set crate-type to include \"staticlib\","; \
		echo "       and on Windows you must build the *-pc-windows-gnu target for cgo."; \
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

## test-pure: Run the Go-only logic tests (no real audio infrastructure).
##
## NOTE: this package imports "C", so the test binary still links librsac_ffi
## even when only pure-Go logic tests are selected — hence the dependency on
## rust-ffi and the CGO_LDFLAGS. The -run filter merely restricts which tests
## execute (skipping the ones that need a live audio device).
test-pure: rust-ffi
	@echo "==> Running Go-only logic tests..."
	CGO_ENABLED=1 \
	CGO_LDFLAGS="-L$(LIB_DIR) -lrsac_ffi $(EXTRA_LDFLAGS)" \
	go test -v -count=1 -run 'TestErrorCode|TestError_|TestErr|TestCaptureTarget|TestAudioBuffer|TestCaptureBuilder|TestDeviceKind|TestStream_Context|TestCallbackHandle|TestStreamResult' ./...

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
