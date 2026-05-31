# rsac-go — Go bindings

Go bindings for [rsac](../../), a cross-platform Rust audio capture
library. Built on top of [`rsac-ffi`](../rsac-ffi/) (the C FFI layer) via
cgo and linked as a static library.

> **Status:** consumer-ready for in-tree integration. Not yet published
> as a tagged Go module — depend on it by path, not by version.

## What you get

- A builder API that mirrors the Rust one:
  `NewCaptureBuilder().WithSystemDefault().SampleRate(48000).Channels(2).Build()`.
- Two consumption models:
  - `capture.Stream(ctx)` — returns a `<-chan AudioBuffer`, respects
    `context.Context` cancellation. Preferred for idiomatic Go.
  - `capture.ReadBuffer()` / `capture.TryReadBuffer()` — blocking /
    non-blocking reads for lower-level control.
- A callback API for push-style consumers.
- A typed `ErrorCode` enum mapped from `rsac_error_t`.

## Prerequisites

- Go 1.22+
- Rust toolchain (`cargo`, `rustc`). The pinned channel lives in
  [`rust-toolchain.toml`](../../rust-toolchain.toml) at the repo root.
- `CGO_ENABLED=1` (the default on every supported host).
- Platform build dependencies for the active rsac backend. See
  [`docs/features.md`](../../docs/features.md).

### Per-platform prerequisites

The cgo link pulls in the active backend's system libraries, so each host
needs that backend's dev packages installed before `make build`:

| Platform | Toolchain / target | System dependencies |
|----------|--------------------|---------------------|
| **Linux** (x86_64, aarch64) | host `cc` + `clang`/`libclang` (bindgen) | `libpipewire-0.3-dev`, `libspa-0.2-dev`, `pkg-config` |
| **macOS** (arm64, x86_64) | Xcode command-line tools | CoreAudio / AudioToolbox / CoreFoundation / Security / SystemConfiguration frameworks (ship with macOS — no install) |
| **Windows** (x86_64) | MinGW gcc **and** the `x86_64-pc-windows-gnu` Rust target (`rustup target add x86_64-pc-windows-gnu`) | WASAPI/COM system libs (ship with Windows) |

> **Windows / cgo note:** cgo links through MinGW `gcc`, which consumes the
> GNU-archive `librsac_ffi.a`. An MSVC-built `rsac_ffi.lib` will **not** link,
> so the FFI crate must be built for the `*-pc-windows-gnu` target (the Makefile
> copies whichever archive cargo emits, but only the GNU `.a` links under cgo).

These are exercised in CI: the `Go Bindings (<os>)` job builds the staticlib and
runs `make test-pure` natively on Linux x86_64, Windows, and macOS arm64, and
`Go Bindings ARM64 Staticlib Check` compile-checks the aarch64 staticlib. The
per-platform `librsac_ffi.a` is **never committed** — it is built in CI (and by
`make` locally) from `bindings/rsac-ffi`.

## Build

The `Makefile` handles both the Rust staticlib build and the cgo link.

```bash
cd bindings/rsac-go
make build       # builds rsac-ffi + Go package
make test        # runs all Go tests (requires audio infrastructure)
make test-pure   # Go-only tests that don't need real audio
make clean
```

Behind the scenes, `make rust-ffi` runs `cargo build --release` in
`bindings/rsac-ffi/` and copies the GNU static archive `librsac_ffi.a`
into `bindings/rsac-go/lib/` (cgo links through the GNU toolchain on
every platform — on Windows build the `*-pc-windows-gnu` target so cargo
emits `librsac_ffi.a` rather than an MSVC `rsac_ffi.lib`). `make
go-build` then sets `CGO_LDFLAGS` to include the platform-specific
system libraries:

- macOS: `-framework CoreAudio -framework AudioToolbox
  -framework CoreFoundation -framework Security -framework
  SystemConfiguration`
- Linux: `-lpipewire-0.3 -lspa-0.2 -lpthread -ldl -lm`
- Windows (MinGW): `-lole32 -loleaut32 -lwinmm -lksuser -luuid`
  plus the Win32 libs the Rust std runtime needs
  (`-lbcrypt -lntdll -luserenv -lws2_32 -ladvapi32 -lkernel32`)

## Quick start

```go
package main

import (
    "context"
    "fmt"
    "log"
    "time"

    rsac "github.com/Codeseys-Labs/rsac-go"
)

func main() {
    capture, err := rsac.NewCaptureBuilder().
        WithSystemDefault().
        SampleRate(48000).
        Channels(2).
        Build()
    if err != nil {
        log.Fatal(err)
    }
    defer capture.Close()

    if err := capture.Start(); err != nil {
        log.Fatal(err)
    }

    ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
    defer cancel()

    for buf := range capture.Stream(ctx) {
        fmt.Printf("%d frames, %d channels @ %d Hz\n",
            buf.NumFrames(), buf.Channels(), buf.SampleRate())
    }
}
```

## Capture targets

Matches the Rust `CaptureTarget` enum:

- `WithSystemDefault()` — the system default output (loopback).
- `WithDevice(id)` — a specific device by ID (use the device
  enumerator to find one).
- `WithApplicationByName(name)` — first app whose name
  substring-matches (case-insensitive).
- `WithApplication(id)` — a specific application session by ID.
- `WithProcessTree(pid)` — a parent process plus all its descendants.

Not every target is supported on every platform. Query capabilities
first with `rsac.PlatformCapabilities()`.

## Smoke test

The `example_test.go` file contains runnable examples that also serve
as smoke tests:

```bash
cd bindings/rsac-go
make test-pure    # verifies the binding compiles and Go-only logic works
make test         # full end-to-end test, needs real audio infrastructure
```

For macOS the test harness respects the same TCC gate as the Rust
integration tests — set `RSAC_CI_MACOS_TCC_GRANTED=1` if Audio Capture
has been granted on the host, or leave it unset to skip Process Tap
paths cleanly. See
[`docs/CI_AUDIO_TESTING.md`](../../docs/CI_AUDIO_TESTING.md) for the
background.

## License

MIT OR Apache-2.0 — matches the parent crate.
