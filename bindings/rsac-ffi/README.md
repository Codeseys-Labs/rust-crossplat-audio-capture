# rsac-ffi — C FFI bindings

`rsac-ffi` exposes [rsac](../../) — a cross-platform Rust audio capture
library — through a C-compatible foreign function interface. It is the
substrate that the Go binding ([`rsac-go`](../rsac-go/)) builds against
and can also be linked directly from C, C++, or any language with a C
FFI story.

This crate carries full crates.io package metadata and is publishable, but
ships with `publish = false` set in `Cargo.toml` so routine workspace builds
can never trigger an accidental `cargo publish`. For local use, build it
against a checkout of the rsac repository. To release it to crates.io, see
[Publishing to crates.io](#publishing-to-cratesio) below.

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

Feature flags: `feat_windows`, `feat_linux`, `feat_macos`, `sink-wav`,
`compose` (multi-source channel composition — see
[Composing multiple sources](#composing-multiple-sources-compose-feature)).

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

To run cbindgen manually, target **`rsac_generated.h`** — never the curated
`rsac.h`, which is hand-maintained and linked against by `rsac-go`:

```bash
cbindgen --config bindings/rsac-ffi/cbindgen.toml \
         --crate rsac-ffi \
         --output bindings/rsac-ffi/include/rsac_generated.h
```

> **Do not** point `--output` at `include/rsac.h`. Overwriting the curated
> header with the generated twin would clobber its hand-written documentation
> and ABI grouping. The two are kept honest by CI: the `check-bindings` job
> builds `rsac-ffi` (which runs cbindgen via `build.rs`) and diffs the
> generated header's **symbol set** — every `typedef`/`struct`/`enum` name plus
> every exported function name — against the curated `rsac.h`, tolerating only
> the tagged-vs-anonymous typedef style difference. Any genuine drift (a new,
> renamed, or removed symbol) fails CI, so the curated header can never silently
> fall behind the generated one. The generated header carries the same C names
> as the curated header because the Rust types are already named in their final
> C-ABI form (no cbindgen prefix is applied — see `cbindgen.toml`).

## Stream statistics and negotiated format

Two out-parameter accessors expose diagnostics without any heap allocation —
the caller passes a stack value and the library fills it in. Neither result
needs freeing.

```c
RsacStreamStats stats;
if (rsac_capture_stream_stats(capture, &stats) == RSAC_OK) {
    printf("pushed=%llu captured=%llu dropped=%llu (%.1f%% lost), up %.1fs, running=%d\n",
           (unsigned long long)stats.buffers_pushed,
           (unsigned long long)stats.buffers_captured,
           (unsigned long long)stats.buffers_dropped,
           stats.dropped_ratio * 100.0,
           stats.uptime_secs,
           stats.is_running);
}

RsacAudioFormat fmt;
if (rsac_capture_format(capture, &fmt) == RSAC_OK) {
    printf("%u ch @ %u Hz, %u-bit\n",
           fmt.channels, fmt.sample_rate, fmt.bits_per_sample);
}
```

`rsac_capture_stream_stats()` works at any time: before the stream starts (or
after it stops) it reports an all-zero snapshot with `is_running == 0`.
`rsac_capture_format()` returns `RSAC_ERROR_STREAM_FAILED` until a stream has
been created (the negotiated format is only known once the backend opens it),
so call it after `rsac_capture_start()`.

`rsac_capture_stream_stats()` exposes **lifetime** counters (cumulative since
the stream opened). A third out-parameter accessor,
`rsac_capture_backpressure_report()`, fills a stack `RsacBackpressureReport` with
the **windowed** drop-rate view — `pushed`/`dropped`/`drop_rate` measured over a
bounded recent window, so it surfaces a sustained 1-in-N loss that the lifetime
totals dilute. Like the others it allocates nothing and the result needs no
freeing.

```c
RsacBackpressureReport bp;
if (rsac_capture_backpressure_report(capture, &bp) == RSAC_OK) {
    printf("window %.1fs: pushed=%llu dropped=%llu (%.1f%% over window), backpressure=%d\n",
           bp.window_secs,
           (unsigned long long)bp.pushed,
           (unsigned long long)bp.dropped,
           bp.drop_rate * 100.0,
           bp.is_under_backpressure);
}
```

## Composing multiple sources (compose feature)

Build with the `compose` feature to expose rsac's multi-source channel
composition (ADR-0011) through C:

```bash
cargo build --release -p rsac-ffi --features compose
```

The compose declarations in `rsac.h` are guarded by the
`RSAC_FEATURE_COMPOSE` preprocessor define — add `-DRSAC_FEATURE_COMPOSE` to
your compiler flags when linking a compose-enabled `librsac_ffi` (without the
define the section preprocesses away, matching a library that does not export
those symbols).

Sources are declared in named groups; each group contributes output channels
according to its layout (mono mixdown → 1, stereo mixdown → 2, keep-channels →
the single source's native width). Groups append in declaration order into one
interleaved-f32 stream at the session rate:

```c
/* Build: discord+zoom mixed to 1 mono channel, system audio as stereo. */
RsacGroup *voice = NULL, *sys = NULL;
rsac_group_new("voice", &voice);
rsac_group_add_source(voice, "name:discord");
rsac_group_add_source_with_gain(voice, "name:zoom", 0.8f);
rsac_group_set_layout(voice, RSAC_GROUP_LAYOUT_MONO);

rsac_group_new("system", &sys);
rsac_group_add_source(sys, "system");
/* default layout is RSAC_GROUP_LAYOUT_STEREO */

RsacCompositionBuilder *builder = NULL;
rsac_composition_builder_new(&builder);
rsac_composition_builder_set_sample_rate(builder, 48000);
rsac_composition_builder_add_group(builder, voice); /* consumes voice */
rsac_composition_builder_add_group(builder, sys);   /* consumes sys   */

RsacComposition *comp = NULL;
if (rsac_composition_builder_build(builder, &comp) != RSAC_OK) { /* builder consumed */
    fprintf(stderr, "build failed: %s\n", rsac_error_message());
    return 1;
}

if (rsac_composition_start(comp) != RSAC_OK) {
    fprintf(stderr, "start failed: %s\n", rsac_error_message());
    rsac_composition_free(comp);
    return 1;
}

/* Channel map: which output channel belongs to which group. */
uint16_t n = rsac_composition_channel_count(comp); /* 3 here: voice + L + R */
for (uint16_t ch = 0; ch < n; ch++) {
    printf("channel %u <- group '%s' (channel %d in group)\n",
           ch,
           rsac_composition_channel_group(comp, ch),
           rsac_composition_channel_in_group(comp, ch));
}

/* Read composed interleaved f32 exactly like a single capture. */
RsacAudioBuffer *buf = NULL;
rsac_error_t rc = rsac_composition_try_read(comp, &buf);
if (rc == RSAC_OK && buf != NULL) {
    /* rsac_audio_buffer_data / _num_frames / _channels as usual */
    rsac_audio_buffer_free(buf);
} else if (rc == RSAC_ERROR_STREAM_FAILED) {
    /* terminal: composition ended and drained — do not retry */
}

/* Runtime counters, all-zero before start; no freeing needed. */
RsacCompositionStats stats;
if (rsac_composition_stats(comp, &stats) == RSAC_OK) {
    for (size_t i = 0; i < stats.num_sources; i++) {
        RsacSourceStats s;
        if (rsac_composition_source_stats(comp, i, &s) == RSAC_OK) {
            printf("%s (%s): recv=%llu padded=%llu trimmed=%llu%s%s\n",
                   rsac_composition_source_group(comp, i),
                   rsac_composition_source_target(comp, i),
                   (unsigned long long)s.buffers_received,
                   (unsigned long long)s.padded_frames,
                   (unsigned long long)s.trimmed_frames,
                   s.resampling ? " [resampling]" : "",
                   s.ended ? " [ended]" : "");
        }
    }
}

rsac_composition_stop(comp);
rsac_composition_free(comp); /* joins the compositor; stops inner captures */
```

Ownership rules (also in the header): a group is **consumed** by a successful
`rsac_composition_builder_add_group()`; the builder is **always consumed** by
`rsac_composition_builder_build()`; the composition **owns its inner
captures** — `rsac_composition_free()` stops the engine (joining its thread)
and releases them. Buffers returned by reads own their samples and stay valid
after the composition is freed. `rsac_composition_stop()` may run concurrently
with a parked `rsac_composition_read()` to unblock it, but never concurrently
with `rsac_composition_free()`.

## Publishing to crates.io

This crate is publish-ready: it declares `description`, `license`,
`repository`, `readme`, `keywords`, `categories`, and `rust-version`, and its
`rsac` dependency carries both a `path` (for workspace/local dev) and a
`version` requirement. crates.io ignores `path` and resolves the published
`rsac` by its `version`, so the path entry does **not** block publishing.

To cut a release:

1. Ensure the matching `rsac` version is already published to crates.io (the
   `version = "x.y.z"` in `[dependencies].rsac` must resolve there).
2. In `Cargo.toml`, set `publish = true` (or delete the `publish = false`
   line). It is intentionally `false` in-tree to prevent an accidental
   `cargo publish` during normal development.
3. `cargo publish -p rsac-ffi --dry-run` to verify, then publish for real.

The `path` entry can stay in place for the published manifest; if you prefer a
pure-registry dependency, drop `path` and keep only
`rsac = { version = "x.y.z", default-features = false }`.

## License

MIT OR Apache-2.0 — matches the parent crate.
