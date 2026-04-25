# rsac-ffi — C FFI bindings

`rsac-ffi` exposes [rsac](../../) — a cross-platform Rust audio capture
library — through a C-compatible foreign function interface. It is the
substrate that the Go binding ([`rsac-go`](../rsac-go/)) builds against
and can also be linked directly from C, C++, or any language with a C
FFI story.

This crate is `publish = false` — it is not shipped to crates.io. Use it
by building it locally against a checkout of the rsac repository.

## What you get

- A single generated C header:
  [`include/rsac.h`](include/rsac.h) / the cbindgen-generated twin
  [`include/rsac_generated.h`](include/rsac_generated.h).
- Two output artifacts (configured via `crate-type = ["cdylib",
  "staticlib"]`):
  - `librsac_ffi.{so,dylib,dll}` — dynamic library.
  - `librsac_ffi.a` / `rsac_ffi.lib` — static library used by the Go binding.
- Opaque handle types for the builder (`RsacBuilder`), capture session
  (`RsacCapture`), audio buffer (`RsacAudioBuffer`), device enumerator
  (`RsacDeviceEnumerator`), and device list (`RsacDeviceList`).
- An `rsac_error_t` enum covering null-pointer, invalid-parameter,
  platform-not-supported, permission-denied, timeout, backend, and
  panic-at-FFI-boundary codes.
- Panic safety: every exported function wraps its body in
  `panic::catch_unwind` and returns `RSAC_ERROR_PANIC` rather than
  unwinding across the FFI boundary.

## Build

From the repository root:

```bash
cargo build --release -p rsac-ffi
```

Output lives under `target/release/`. The `build.rs` in this crate runs
cbindgen to regenerate `include/rsac_generated.h` on every build, but
the curated `include/rsac.h` header is what consumers should include.

To build with only one platform backend (mirrors rsac's own feature flags):

```bash
cargo build --release -p rsac-ffi \
  --no-default-features --features feat_linux
```

Feature flags: `feat_windows`, `feat_linux`, `feat_macos`, `sink-wav`.

## Linking

### macOS

```
-lrsac_ffi \
  -framework CoreAudio -framework AudioToolbox \
  -framework CoreFoundation -framework Security -framework SystemConfiguration
```

### Linux

```
-lrsac_ffi -lpipewire-0.3 -lspa-0.2 -lpthread -ldl -lm
```

### Windows (MSVC)

```
rsac_ffi.lib ole32.lib oleaut32.lib winmm.lib ksuser.lib uuid.lib
```

## Smoke test — minimal C capture

Save as `smoke.c`:

```c
#include <stdio.h>
#include <unistd.h>
#include "rsac.h"

int main(void) {
    RsacBuilder *builder = NULL;
    if (rsac_builder_new(&builder) != RSAC_OK) {
        fprintf(stderr, "builder_new failed: %s\n", rsac_error_message());
        return 1;
    }

    if (rsac_builder_set_target_system(builder) != RSAC_OK) {
        rsac_builder_free(builder);
        fprintf(stderr, "set_target_system failed: %s\n", rsac_error_message());
        return 1;
    }

    RsacCapture *capture = NULL;
    if (rsac_builder_build(builder, &capture) != RSAC_OK) {
        fprintf(stderr, "build failed: %s\n", rsac_error_message());
        return 1;
    }

    if (rsac_capture_start(capture) != RSAC_OK) {
        fprintf(stderr, "start failed: %s\n", rsac_error_message());
        rsac_capture_free(capture);
        return 1;
    }

    sleep(2);

    RsacAudioBuffer *buf = NULL;
    if (rsac_capture_try_read(capture, &buf) == RSAC_OK && buf != NULL) {
        printf("Got %zu frames, %u channels, %u Hz\n",
               rsac_audio_buffer_num_frames(buf),
               rsac_audio_buffer_channels(buf),
               rsac_audio_buffer_sample_rate(buf));
        rsac_audio_buffer_free(buf);
    }

    rsac_capture_stop(capture);
    rsac_capture_free(capture);
    return 0;
}
```

Build (Linux):

```bash
cargo build --release -p rsac-ffi
cc smoke.c \
  -I bindings/rsac-ffi/include \
  -L target/release -lrsac_ffi \
  -lpipewire-0.3 -lspa-0.2 -lpthread -ldl -lm \
  -o smoke
LD_LIBRARY_PATH=$PWD/target/release ./smoke
```

## Memory ownership

The rules are spelled out in the crate-level Rust doc. Summary:

- Functions returning `*mut T` transfer ownership — caller must call
  the matching `rsac_*_free()` exactly once.
- Functions taking `*const T` or `*mut T` borrow; the caller retains
  ownership.
- The string returned by `rsac_error_message()` is thread-local and
  valid until the next rsac-ffi call on the same thread.

## Regenerating the header

`build.rs` regenerates `include/rsac_generated.h` on every build. The
curated `include/rsac.h` is hand-maintained and should track the
generated file; use it as the consumer-facing header.

To run cbindgen manually:

```bash
cbindgen --config bindings/rsac-ffi/cbindgen.toml \
         --crate rsac-ffi \
         --output bindings/rsac-ffi/include/rsac.h
```

## License

MIT OR Apache-2.0 — matches the parent crate.
