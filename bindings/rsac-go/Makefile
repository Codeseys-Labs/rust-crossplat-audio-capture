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
    # macOS needs frameworks for CoreAudio, Security, etc. IOKit + AppKit are
    # pulled in transitively by `sysinfo` (process-tree discovery) + objc2-app-kit
    # once feat_macos actually compiles the CoreAudio backend (rsac-b624) — a
    # staticlib consumer must link every system framework explicitly; cargo's
    # automatic framework linking only applies to a binary/cdylib built directly
    # by cargo, not to a downstream cgo link against the .a.
    EXTRA_LDFLAGS := -framework CoreAudio -framework AudioToolbox \
                     -framework CoreFoundation -framework Security \
                     -framework SystemConfiguration -framework IOKit \
                     -framework AppKit
    # rsac-b624: rsac-ffi's `default = []` means a bare `cargo build` compiles
    # NO backend, so PlatformCapabilities() always returned the honest-empty
    # "unsupported" stub regardless of host OS. Select the matching platform
    # feature (mirrors bindings/rsac-napi + bindings/rsac-python's per-target
    # `features = ["feat_<os>"]` convention) so the real CoreAudio backend
    # actually compiles in.
    #
    # `compose` is appended so librsac_ffi.a exports the rsac_composition_* /
    # rsac_group_* symbols the Go compose wrapper (compose.go) links against
    # (rsac-fba7). Without it the staticlib has no compose symbols and cgo's
    # `#define RSAC_FEATURE_COMPOSE` would reference undefined externals at link.
    FFI_FEATURES := feat_macos,compose
else ifeq ($(UNAME_S),Linux)
    STATIC_LIB := librsac_ffi.a
    # NOTE: no `-lspa-0.2` — SPA (libspa-0.2) is a header/plugin API with NO
    # shared object to link against (`pkg-config --libs libspa-0.2` is empty),
    # so `-lspa-0.2` makes ld fail with "cannot find -lspa-0.2". PipeWire itself
    # is the only system .so the staticlib needs at link time.
    EXTRA_LDFLAGS := -lpipewire-0.3 -lpthread -ldl -lm
    # `compose` appended for the rsac_composition_*/rsac_group_* symbols (rsac-fba7).
    FFI_FEATURES := feat_linux,compose
else
    # Windows (MSYS2/MinGW). Build rsac-ffi for the *-pc-windows-gnu target so
    # cargo emits librsac_ffi.a (GNU archive) that cgo's gcc can link.
    STATIC_LIB := librsac_ffi.a
    # WASAPI/COM libs plus the Win32 system libs the Rust std runtime pulls in.
    EXTRA_LDFLAGS := -lole32 -loleaut32 -lwinmm -lksuser -luuid \
                     -lbcrypt -lntdll -luserenv -lws2_32 -ladvapi32 -lkernel32
    # cgo links through MinGW gcc/ld (GNU ABI). The DEFAULT Windows Rust target
    # is MSVC (x86_64-pc-windows-msvc), whose staticlib references MSVC-only
    # symbols (__chkstk, __CxxFrameHandler3, ??_7type_info@@6B@) that MinGW ld
    # cannot resolve. So we MUST build the GNU target explicitly.
    # `rustup target add x86_64-pc-windows-gnu` is a prerequisite.
    RUST_TARGET := x86_64-pc-windows-gnu
    # `compose` appended for the rsac_composition_*/rsac_group_* symbols (rsac-fba7).
    FFI_FEATURES := feat_windows,compose
endif

# When RUST_TARGET is set (Windows), build + read from target/<triple>/release;
# otherwise use the default host target/release dir.
ifeq ($(RUST_TARGET),)
    CARGO_TARGET_FLAG :=
    CARGO_TARGET := $(RSAC_ROOT)/target/release
else
    CARGO_TARGET_FLAG := --target $(RUST_TARGET)
    CARGO_TARGET := $(RSAC_ROOT)/target/$(RUST_TARGET)/release
endif

# ── Targets ──────────────────────────────────────────────────────────────

.PHONY: all build rust-ffi go-build test clean lint fmt help

## all: Build the Rust staticlib and verify the Go package compiles.
all: build

## build: Build everything (Rust FFI + Go package).
build: rust-ffi go-build

## rust-ffi: Compile the Rust FFI crate as a static library.
rust-ffi:
	@echo "==> Building rsac-ffi static library..."
	cd $(FFI_DIR) && cargo build --release $(CARGO_TARGET_FLAG) --no-default-features --features $(FFI_FEATURES)
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
	go test -v -count=1 -run 'TestErrorCode|TestError_|TestErr|TestCaptureTarget|TestAudioBuffer|TestCaptureBuilder|TestDeviceKind|TestStream_Context|TestCallbackHandle|TestStreamResult|TestCapabilities|TestComposition|TestGroup|TestCompositionBuilder' ./...

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
