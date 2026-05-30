# rsac Cross-Language Bindings

> **Status (0.2.0 line):** C FFI, Python, Node.js, and Go bindings are
> **shipped and at feature parity** for the core capture surface. Swift/Kotlin
> (UniFFI) and WASM remain research-only — see [§ Not yet implemented](#not-yet-implemented).
> rsac is, as far as we know, the first Rust audio-capture crate with
> cross-language bindings.

## Summary

| Language | Status | Tooling | All Capture Tiers? |
|---|---|---|---|
| **C FFI** | **Shipped** (`bindings/rsac-ffi`) | cbindgen + curated `rsac.h` | Yes (all 5) |
| **Python** | **Shipped** (`bindings/rsac-python`) | PyO3 + maturin, abi3-py39 | Yes (all 5) |
| **Node.js/Bun** | **Shipped** (`bindings/rsac-napi`) | napi-rs | Yes (all 5) |
| **Go** | **Shipped** (`bindings/rsac-go`) | CGo over the C FFI | Yes (all 5) |
| **Swift/Kotlin** | Research only | UniFFI | Partial (needs mobile backends) |
| **WASM** | Not viable | wasm-bindgen | No (0/5, sandbox) |

The five capture tiers are the `CaptureTarget` variants: system default, device,
application (by PID), application-by-name, and process tree.

## Shipped binding parity (0.2.0)

The three high-level bindings (Python, Node, Go) — all sitting on the same
`rsac` core (Go via the C FFI) — expose the same capture surface. The matrix
below is the contract every binding meets; per-language spellings differ but the
semantics are identical.

| Capability | Rust core | Python (`rsac`) | Node (`@rsac/audio`) | Go (`rsac`) |
|---|---|---|---|---|
| Diagnostic counters snapshot | `AudioCapture::stream_stats()` | `capture.stream_stats()` → `StreamStats` | `capture.streamStats()` | `capture.StreamStats()` |
| Negotiated delivery format | `AudioCapture::format()` | `capture.format()` → `AudioFormat \| None` | `capture.format` (getter) → `AudioFormat \| null` | `capture.Format()` |
| Buffer RMS / peak (linear) | `AudioBuffer::rms()` / `peak()` | `buffer.rms()` / `buffer.peak()` | fields `chunk.rms` / `chunk.peak` | `buf.RMS()` / `buf.Peak()` |
| Buffer RMS / peak (dBFS) | `rms_dbfs()` / `peak_dbfs()` | `buffer.rms_dbfs()` / `peak_dbfs()` | `chunk.rmsDbfs` / `peakDbfs` | `buf.RMSDbfs()` / `PeakDbfs()` |
| Per-channel meters | `channel_rms(ch)` / `channel_peak(ch)` | (via core) | `chunk.channelRms[]` / `channelPeak[]` | (via core) |
| Target-string parse | `CaptureTarget::from_str` | `CaptureTarget.parse(spec)` | `CaptureTarget.parse(spec)` | `builder.WithTargetString` / `SetTargetString` |
| Context manager / RAII cleanup | `RunningCapture` (Drop) | `with` (sync) + `async with` | (explicit `start`/`stop`) | `defer Close()` + finalizer |

Notes per row:

- **`stream_stats` / `format`.** Both are cheap, non-locking, non-allocating
  consumer-side reads of the bridge's diagnostic counters and the
  backend-published delivery format. `format()` returns `None`/`null` before
  `start()` has created a stream. In Node it is exposed as a property getter
  (`capture.format`); Python and Go use a method. (Caveat: the delivery format
  reflects what the backend negotiated where the backend records it — see the
  `set_negotiated_format` note in [`PERFORMANCE.md`](PERFORMANCE.md) for the
  current limitation that backends fall back to the requested format.)
- **Metering.** The meters wrap the core's alloc-free `rms`/`peak`/dBFS/
  per-channel reductions (see [`PERFORMANCE.md`](PERFORMANCE.md)). napi widens
  the per-channel meters into `Float32Array`-style number arrays; Go re-derives
  dBFS in Go from the linear values via a shared `linToDbfs` helper.
- **Target strings.** All bindings round-trip the same canonical grammar
  parsed by `CaptureTarget::from_str`: `system`, `device:<id>`, `app:<pid>`,
  `name:<n>`, `tree:<pid>` (case-insensitive scheme; the colon split preserves
  device ids like `hw:0,0`). An invalid spec surfaces as the binding's error
  type, never a panic.
- **Lifetime management.** Python provides both a synchronous context manager
  (`__enter__`/`__exit__`) and an async one (`__aenter__`/`__aexit__`, whose
  bodies are non-blocking and return an immediately-resolved awaitable), plus
  GIL release (`py.allow_threads`) around blocking reads. Go registers a
  `runtime.SetFinalizer` safety net but documents explicit `Close()` (idempotent)
  as preferred; the finalizer is cleared on explicit close.

### Known binding limitations (tracked)

These are real, current behaviours — documented here rather than papered over.
They are tracked code fixes, not doc fixes.

- **Python iterator raises on natural end-of-capture (tracked, critique
  BFFI-03).** `PyAudioCapture.__next__` maps only `AudioError::StreamReadError`
  to `StopIteration`. The bridge's *terminal* signal is `StreamEnded` (Fatal per
  [ADR-0003](designs/0003-terminal-stream-error.md)), which falls through to the
  general error mapping and is raised as a `StreamError` exception. So iterating
  `for buf in capture:` to the natural end of a capture currently raises instead
  of stopping cleanly. The fix is to end iteration on any terminal stream signal
  (`StreamEnded`, or branch on `is_fatal()`); until then, callers should catch
  the terminal error. Sync/async context managers and `read_buffer` itself are
  unaffected.
- **C FFI `rsac_version()` is hardcoded (tracked, critique BFFI-04).**
  `rsac_version()` in `bindings/rsac-ffi/src/lib.rs` returns a static
  `"0.1.0"`, while the workspace crate and the Python/Node bindings are `0.2.0`
  (`rsac-python` reports `env!("CARGO_PKG_VERSION")`). Go's C ABI therefore
  reports a wrong version string. The fix is to return `env!("CARGO_PKG_VERSION")`
  and bump `rsac-ffi`'s package version to align with the workspace. Do **not**
  rely on `rsac_version()` for version gating until this lands.
- **Go `ReadBuffer`/`TryReadBuffer` concurrent-`Close()` UAF (tracked, critique
  BFFI-02).** These methods snapshot the handle, drop the mutex, then call the C
  read; a concurrent `Close()` can free the handle mid-read. This is a
  memory-safety bug fixed separately — the in-source comment claiming it is
  "safe to use without the lock" is misleading. Until fixed, do not call
  `Close()` concurrently with an in-flight `ReadBuffer`.
- **Python `CaptureTarget.__str__` is not the canonical target string.** It
  returns the constructor-style repr (`CaptureTarget.device(...)`), not the
  round-trippable `device:<id>` grammar accepted by `CaptureTarget.parse`. Use
  the grammar literals directly when you need a parseable string.

### Python packaging: single abi3 wheel per platform

`rsac-python` builds a **single `cp39-abi3` wheel per platform** (Python 3.9+),
not one wheel per minor version, via PyO3's `abi3-py39` feature. One macOS/
Windows/Linux wheel covers every CPython ≥ 3.9 on that platform. The decision
and its trade-offs are recorded in
[`docs/designs/abi3-decision.md`](designs/abi3-decision.md).

### Backend feature selection

The **rsac-ffi** and **rsac-python** bindings depend on `rsac` with
`default-features = false` and select exactly the one backend matching the build
target via `[target.'cfg(...)'.dependencies.rsac]` blocks, so a Linux build never
compiles the Windows/CoreAudio backends. This is the canonical convention
documented in
[`features.md`](features.md#binding-feature-resolution-convention-canonical).
**rsac-napi** has not yet migrated — it currently depends on
`rsac = { path = "../.." }` (all default backends); `features.md` tracks it as
still migrating.

## 1. C FFI (Foundation)

The C API is the foundation for Go, C#, Ruby, Lua, Dart, Java, and any language with C interop.

**Tooling:** `cbindgen` auto-generates `rsac.h` from `#[no_mangle] pub extern "C" fn` declarations.

**Design principles:**
- Opaque handles (`rsac_builder_t*`, `rsac_capture_t*`, `rsac_audio_buffer_t*`)
- Error codes returned from every function + thread-local `rsac_error_message()`
- `std::panic::catch_unwind()` on every FFI boundary
- Rust allocates, Rust frees (`rsac_audio_buffer_free()`)
- Behind `c-api` Cargo feature

**Key API surface:**
```c
rsac_builder_t *rsac_builder_new(void);
rsac_error_t rsac_builder_set_target_system(rsac_builder_t *b);
rsac_error_t rsac_builder_set_target_app_by_name(rsac_builder_t *b, const char *name);
rsac_error_t rsac_builder_set_target_process_tree(rsac_builder_t *b, uint32_t pid);
rsac_error_t rsac_builder_build(rsac_builder_t *b, rsac_capture_t **out);
rsac_error_t rsac_capture_start(rsac_capture_t *c);
rsac_error_t rsac_capture_read(rsac_capture_t *c, rsac_audio_buffer_t **out);
const float *rsac_audio_buffer_data(const rsac_audio_buffer_t *b);
void rsac_audio_buffer_free(rsac_audio_buffer_t *b);
```

## 2. Python (PyO3 + maturin)

Shipped surface (`bindings/rsac-python`). The constructor is
`AudioCapture(target=None, sample_rate=48000, channels=2, buffer_size=None)`.

```python
import rsac
import numpy as np

with rsac.AudioCapture(sample_rate=48000, channels=2) as capture:
    for buffer in capture:          # __iter__/__next__ with GIL release
        # to_bytes() returns little-endian IEEE-754 f32, numpy-frombuffer-ready
        audio = np.frombuffer(buffer.to_bytes(), dtype="<f4")
        print(buffer.rms_dbfs(), buffer.peak_dbfs())   # alloc-free meters

# Target by canonical string (round-trips CaptureTarget::from_str)
target = rsac.CaptureTarget.parse("name:Firefox")
with rsac.AudioCapture(target=target) as cap:
    buf = cap.read_buffer_blocking()

# Async lifecycle
async with rsac.AudioCapture() as cap:
    cap.start()
    stats = cap.stream_stats()
    fmt = cap.format()              # None until start() opens a stream

# Device / capability introspection
devices = rsac.list_devices()
caps = rsac.platform_capabilities()
```

**Key:** GIL release during blocking reads via `py.allow_threads()`. Sample data
crosses as `to_bytes()` (numpy `frombuffer(..., dtype="<f4")`-compatible LE f32),
using the provably-sound `f32::to_le_bytes` path — there is no zero-copy
`rust-numpy` `to_numpy()` method in the shipped binding. See the
[Python iterator end-of-stream caveat](#known-binding-limitations-tracked).

## 3. Node.js/Bun (napi-rs)

Shipped surface (`bindings/rsac-napi`). Push delivery is via `onData()`; pull
delivery is `read()` (non-blocking) / `readBlocking()`. (There is no
`capture.stream()` async iterator or `createReadableStream()` in the shipped
binding — those were research-stage ideas.)

```typescript
import { AudioCapture, CaptureTarget } from '@rsac/audio';

const capture = new AudioCapture({ sampleRate: 48000, channels: 2 });

// Push-based delivery via a ThreadsafeFunction-backed callback
capture.onData((chunk) => {
  const data: Float32Array = chunk.data;
  console.log(`${chunk.numFrames} frames, rmsDbfs=${chunk.rmsDbfs}`);
});
capture.start();

// Or pull in a loop
const chunk = capture.readBlocking();

// Target by canonical string
const target = CaptureTarget.parse('name:Firefox');

// Observability: counters carried as BigInt to avoid >2^53 precision loss;
// format is a getter, null until start() opens a stream.
const stats = capture.streamStats();
const fmt = capture.format;
```

**Numeric typing:** `u64` counters cross as `BigInt`; `f32` sample data crosses
as `Float32Array`, both to avoid IEEE-754 double precision loss. The push path
maps rsac's read loop onto a napi-rs `ThreadsafeFunction`, so it integrates with
the Node/Bun event loop and works in Electron's main process.

> **Caveat (tracked, critique BFFI-07):** the `onData` background pump currently
> breaks on *any* read error, including recoverable transient ones, rather than
> branching on `is_fatal()`/recoverability — a transient hiccup can silently
> stop push delivery. Code fix tracked separately.

**For Tauri apps:** use rsac as a direct Rust dependency instead — no napi-rs needed.

## 4. Go (CGo over C FFI)

Shipped surface (`bindings/rsac-go`). Built over the C FFI; the cgo layer copies
the borrowed C buffer into Go memory before handing it to user code.

```go
capture, _ := rsac.NewCaptureBuilder().
    WithApplicationByName("Firefox").  // or WithTargetString("name:Firefox")
    SampleRate(48000).
    Build()
defer capture.Close()                  // idempotent; a finalizer is a safety net
capture.Start()

for buf := range capture.Stream(ctx) {
    fmt.Printf("%d frames, rmsDbfs=%.1f\n", buf.NumFrames, buf.RMSDbfs())
}

// Or surface terminating errors explicitly:
for res := range capture.StreamWithErrors(ctx) {
    if res.Err != nil { /* handle */ }
}

stats, _ := capture.StreamStats()      // diagnostic counters snapshot
fmt, _ := capture.Format()             // negotiated delivery format
```

**Target strings:** `WithTargetString(spec)` is the fluent (deferred-error)
setter; `SetTargetString(spec)` validates immediately and returns an error.
**Metering:** `RMS()`/`Peak()` come from core; `RMSDbfs()`/`PeakDbfs()` are
derived in Go via a shared `linToDbfs` helper. Buffer transfer is always a copy
(~1μs for a 10 ms chunk); cgo per-call overhead (~100 ns) is negligible at audio
rates.

> **Caveat (tracked, critique BFFI-02):** see the
> [concurrent-`Close()` use-after-free note](#known-binding-limitations-tracked)
> — do not call `Close()` concurrently with an in-flight `ReadBuffer`/
> `TryReadBuffer` until the code fix lands.

## Not yet implemented

The tiers below remain research-only — there is no `bindings/rsac-uniffi` or
WASM target in the workspace today.

### 5. WASM — Not Viable for Capture

WASM runs in a browser sandbox with zero OS audio API access. All 5
`CaptureTarget` variants are impossible. Only `AudioBuffer` observation utilities
(RMS, peak, dBFS) could conceivably run in WASM; capture itself cannot.

### 6. Swift/Kotlin (Mobile) — research only

**UniFFI** could generate both Swift and Kotlin from a single Rust source. The
binding generation is straightforward, but **new audio backends are needed** and
none exist yet:

| Platform | Current Backend | Mobile Reality |
|---|---|---|
| iOS | macOS CoreAudio (desktop) | Needs `AVAudioEngine` backend |
| Android | None | Needs `AAudio` + `MediaProjection` |

## Repository layout (as shipped)

The Rust crates are Cargo workspace members (see `Cargo.toml` `[workspace]`); the
Go module sits beside them but is not a Cargo member (it builds over the C FFI).

```
rsac/                        # workspace root (the `rsac` core crate)
└── bindings/
    ├── rsac-ffi/            # C FFI — workspace member; curated rsac.h
    ├── rsac-python/         # PyO3 bindings — workspace member (abi3-py39)
    ├── rsac-napi/           # napi-rs bindings — workspace member
    └── rsac-go/             # Go wrapper over the C FFI (cgo; not a Cargo member)
```

There is no `rsac-uniffi` crate yet — Swift/Kotlin remain research-only (see
[Not yet implemented](#not-yet-implemented)).

> **C-header note (tracked, critique BFFI-01):** the curated
> `bindings/rsac-ffi/include/rsac.h` is the source of truth the Go layer links
> against. The cbindgen-generated `rsac_generated.h` currently emits
> double-prefixed names (e.g. `RsacRsacBuilder`); do not regenerate over
> `rsac.h` until the cbindgen prefix config is fixed, or you will break the C/Go
> ABI.
