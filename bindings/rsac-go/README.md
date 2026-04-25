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

## Build

The `Makefile` handles both the Rust staticlib build and the cgo link.

```bash
cd bindings/rsac-go
make build       # builds rsac-ffi + Go package
make test        # runs all Go tests (requires audio infrastructure)
make test-pure   # Go-only tests that don't need real audio
make clean
```

Behind the scenes, `make rust-ffi` runs `cargo build --release -p
rsac-ffi` at the repo root and copies `librsac_ffi.a` (macOS/Linux) or
`rsac_ffi.lib` (Windows) into `bindings/rsac-go/lib/`. `make go-build`
then sets `CGO_LDFLAGS` to include the platform-specific system
libraries:

- macOS: `-framework CoreAudio -framework AudioToolbox
  -framework CoreFoundation -framework Security -framework
  SystemConfiguration`
- Linux: `-lpipewire-0.3 -lspa-0.2 -lpthread -ldl -lm`
- Windows (MinGW): `-lole32 -loleaut32 -lwinmm -lksuser -luuid`

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
