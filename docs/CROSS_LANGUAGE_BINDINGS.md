# rsac Cross-Language Bindings

> **Status (shipped since the 0.2.0 line; current release 0.4.0):** C FFI,
> Python, Node.js, and Go bindings are **shipped and at feature parity** for the
> core capture surface — including the windowed `backpressure_report()` added in
> 0.4.0. WASM is not viable; Swift/Kotlin (UniFFI) bindings remain
> research-only, though the Android/iOS *backends* they'd need are now designed
> ([`MOBILE_BACKEND_DESIGN.md`](MOBILE_BACKEND_DESIGN.md)) — see
> [§ Not yet implemented](#not-yet-implemented). rsac is, as far as we know, the
> first Rust audio-capture crate with cross-language bindings.

## Summary

| Language | Status | Tooling | All Capture Tiers? |
|---|---|---|---|
| **C FFI** | **Shipped** (`bindings/rsac-ffi`) | cbindgen + curated `rsac.h` | Yes (all 5) |
| **Python** | **Shipped** (`bindings/rsac-python`) | PyO3 + maturin, abi3-py39 | Yes (all 5) |
| **Node.js/Bun** | **Shipped** (`bindings/rsac-napi`) | napi-rs | Yes (all 5) |
| **Go** | **Shipped** (`bindings/rsac-go`) | CGo over the C FFI | Yes (all 5) |
| **Swift/Kotlin** | Backends designed ([design](MOBILE_BACKEND_DESIGN.md)); UniFFI bindings research-only | UniFFI (later); first-party AAR/SwiftPM glue (planned) | Blocked on mobile backends (seeded, waves 3–4) |
| **WASM** | Not viable | wasm-bindgen | No (0/5, sandbox) |

The five capture tiers are the `CaptureTarget` variants: system default, device,
application (by PID), application-by-name, and process tree.

> Framework-level questions (Tauri, Dioxus, Electron, Deno, Bun, Flutter, …)
> are covered separately in
> [`FRAMEWORK_COMPATIBILITY.md`](FRAMEWORK_COMPATIBILITY.md).

## Shipped binding parity (0.2.0)

The three high-level bindings (Python, Node, Go) — all sitting on the same
`rsac` core (Go via the C FFI) — expose the same capture surface. The matrix
below is the contract every binding meets; per-language spellings differ but the
semantics are identical.

| Capability | Rust core | Python (`rsac`) | Node (`@rsac/audio`) | Go (`rsac`) |
|---|---|---|---|---|
| Diagnostic counters snapshot | `AudioCapture::stream_stats()` | `capture.stream_stats()` → `StreamStats` | `capture.streamStats()` | `capture.StreamStats()` |
| Windowed backpressure report | `AudioCapture::backpressure_report()` | `capture.backpressure_report()` → `BackpressureReport` | `capture.backpressureReport()` | `capture.BackpressureReport()` |
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
- **`backpressure_report`.** A second cheap, non-locking consumer-side read
  that returns a `BackpressureReport { window: Duration, pushed, dropped,
  drop_rate, is_under_backpressure }`. Where `stream_stats` exposes the bridge's
  **lifetime** counters (cumulative since the stream opened), this is the
  **windowed** view: `pushed`/`dropped`/`drop_rate` are measured over a bounded
  recent window, so it surfaces *sustained* loss — a steady 1-in-N drop rate —
  that lifetime totals dilute and that the consecutive-drop `is_under_backpressure`
  bool on the lifetime stats can miss (rsac-cfe4). Use `stream_stats` for "how
  much have we dropped overall?" and `backpressure_report` for "are we dropping
  *right now*?". Spellings: Rust `backpressure_report()`, Python
  `capture.backpressure_report()`, Node `capture.backpressureReport()`, Go
  `capture.BackpressureReport()`, C FFI `rsac_capture_backpressure_report()`
  filling an `RsacBackpressureReport`.
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

### Terminal-error & recoverable-error delivery

rsac distinguishes a **recoverable** read error (a transient hiccup — a momentary
over-/under-run, timeout, or backend blip) from a **fatal/terminal** one (the
stream is over: `AudioError::StreamEnded` and the other Fatal variants). The
classification lives in the core and is recorded in
[ADR-0003](designs/0003-terminal-stream-error.md); every binding projects it
through `is_fatal()`/`is_recoverable()` (Go reads it off the FFI error code via
`ErrorCode.IsRecoverable`). All error-aware streaming surfaces honour **two
invariants**:

1. **A recoverable error never ends the stream.** The consumption loop yields and
   retries; a transient hiccup is not a reason to stop delivering audio.
2. **The stream ends cleanly on, and only on, the fatal terminal.** On the
   **error-carrying** surfaces (`subscribe_with_errors()`, Go `StreamWithErrors()`,
   the Node `onEnd`/`onError` callback, Python's `StopIteration`) the terminal
   cause is delivered, so a consumer learns *why* the stream ended rather than
   racing a bare close. The **value-only** surfaces (`subscribe()`, Go `Stream()`,
   the `AsyncAudioStream`/`buffers_iter()` items) simply finish (channel close /
   `None`) and do **not** carry the terminal `AudioError` — read it off an
   error-carrying surface if the reason matters. (Producer-side, the backend
   signals this terminal once the capture loop exits — see
   [ADR-0010](designs/0010-producer-terminal-signal.md).)

Within those invariants the bindings **deliberately diverge on whether a
recoverable error is *delivered* to the consumer or *swallowed*** before the
retry. This is an intentional, documented difference — not a bug — and it only
affects whether a consumer *observes* transient hiccups; it never affects when a
stream ends:

| Surface | Recoverable error | Fatal/terminal error |
|---|---|---|
| Rust `AudioCapture::subscribe_with_errors()` (`mpsc::Receiver<AudioResult<…>>`) | **Forwarded** as a non-terminal `Err` item, then continues | Forwarded as the FINAL `Err` item, then the `Sender` drops (channel disconnects) |
| Rust `AudioCapture::subscribe()` (`mpsc::Receiver<AudioBuffer>`) | Swallowed (logged + retried) | Channel closes (the bare disconnect *is* the terminal signal; no error carried) |
| Rust async (`AsyncAudioStream`) / `buffers_iter()` (`AudioResult<AudioBuffer>` items) | Forwarded as an `Err` item, then continues | **Stream-end only** — `poll_next`/`next` return `None` (the iterator/stream simply finishes); the terminal `AudioError` is **not** carried. Read it off `subscribe_with_errors()` if you need the reason. |
| Go `StreamWithErrors()` (`<-chan StreamResult`) | **Swallowed** (`runtime.Gosched()` + retry; nothing sent) | Delivered as the FINAL `StreamResult{Err}`, then the channel is closed |
| Go `Stream()` (`<-chan AudioBuffer`) | Swallowed (yield + retry) | Channel closes (no error carried) |
| Node `onData()` pump | Swallowed *(intended)* — the pump branches on `is_fatal()` (rsac-napi `src/lib.rs`) | Pump stops and fires `onEnd`/`onError` with the terminal cause |
| Python `for buf in capture:` iterator | Swallowed (retried) | Raises `StopIteration` on the fatal terminal (ships today — rsac-python `src/lib.rs`) |

The two **error-carrying** surfaces — Rust `subscribe_with_errors()` and Go
`StreamWithErrors()` — agree on both invariants but differ on the recoverable
case: Rust **forwards** each recoverable `Err` (so an `mpsc` consumer can meter
the hiccups), whereas Go **swallows** it (so a `for r := range ch` consumer is
never tempted to `break` on a transient error it can do nothing about). Go's
swallow matches its own value-only `Stream()` loop, the napi `onData` pump, and
the blueprint's "logs + sleeps + continues" note; Rust's forwarding is the more
informative outlier. A Go consumer that needs to *see* transient hiccups should
poll [`AudioCapture.StreamStats`](#shipped-binding-parity-020) (`Overruns` /
`BuffersDropped`) rather than expecting them on the channel.

> **Note (Go `ErrClosed`):** `ErrClosed` carries a recoverable code
> (`ErrStreamRead`) but always stops the Go loops — it means the capture was
> closed underneath the consumer, which is terminal for that consumer. The loops
> gate it with `errors.Is(err, ErrClosed)` so it bypasses the recoverable-retry
> path.

### Binding behaviours (current)

The following items were tracked binding limitations in earlier 0.2.x drafts and
are now **resolved** in 0.3.0; recorded here so the history is clear:

- **Python iterator ends cleanly on terminal (was BFFI-03 — RESOLVED 0.3.0).**
  `PyAudioCapture.__next__` reads via the terminal-observable path and raises
  `StopIteration` on the fatal terminal (`StreamEnded`/`is_fatal()`), retrying
  only genuine recoverable hiccups — so `for buf in capture:` stops cleanly at
  end-of-capture. (rsac-python `src/lib.rs`.)
- **C FFI `rsac_version()` reports the real version (was BFFI-04 — RESOLVED
  0.3.0).** It now returns `concat!(env!("CARGO_PKG_VERSION"), "\0")` and
  `rsac-ffi` is in version lockstep with the workspace (enforced by the
  `version-lockstep` CI gate across all six manifests).
- **Go `ReadBuffer`/`TryReadBuffer` concurrent-`Close()` is safe (was BFFI-02 /
  #28 — RESOLVED 0.3.0).** `Close()` sets a `closing` flag, calls
  `rsac_capture_request_stop` to unblock a parked read, and drains a
  `sync.WaitGroup` of in-flight reads **before** freeing the handle. The
  misleading "safe without the lock" comment was removed.

Genuine current behaviours (by design, not bugs):

- **Python `CaptureTarget.__str__` is the repr, not the canonical string.** It
  returns the constructor-style repr (`CaptureTarget.device(...)`), not the
  round-trippable `device:<id>` grammar accepted by `CaptureTarget.parse`. Use
  the grammar literals directly when you need a parseable string.
- **Go `ErrorCode.IsRecoverable()` is a lossy projection of the FFI codes.** The
  C ABI collapses the core's recoverable `BackendError` and the fatal
  `BackendNotAvailable`/`BackendInitializationFailed` onto one
  `RSAC_ERROR_BACKEND` code, which `IsRecoverable()` reports as recoverable. This
  is only reachable at `Build()`/`Start()` (never the streaming read loop), so it
  does not affect stream termination; documented in `rsac.go`.

### Python packaging: single abi3 wheel per platform

`rsac-python` builds a **single `cp39-abi3` wheel per platform** (Python 3.9+),
not one wheel per minor version, via PyO3's `abi3-py39` feature. One macOS/
Windows/Linux wheel covers every CPython ≥ 3.9 on that platform. The decision
and its trade-offs are recorded in
[`docs/designs/abi3-decision.md`](designs/abi3-decision.md).

### Backend feature selection

The **rsac-python** and **rsac-napi** bindings depend on `rsac` with
`default-features = false` and select exactly the one backend matching the
build target via `[target.'cfg(...)'.dependencies.rsac]` blocks, so a Linux
build never compiles the Windows/CoreAudio backends. This is the canonical
convention documented in
[`features.md`](features.md#binding-feature-resolution-convention-canonical)
(rsac-napi migrated in rsac-e8a3 — it is also a prerequisite for building
mobile triples). **rsac-ffi** takes a different (also correct) shape: a single
`default-features = false` dependency plus passthrough features
(`feat_windows`/`feat_linux`/`feat_macos`, `default = []`), so the consumer —
the Makefile, CI, or rsac-go — picks the backend at build time.

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
`rust-numpy` `to_numpy()` method in the shipped binding. The iterator ends
cleanly on the fatal terminal (see [Binding behaviours
(current)](#binding-behaviours-current), BFFI-03 — RESOLVED 0.3.0).

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

> **Resolved (was BFFI-07, 0.3.0):** the `onData` background pump now branches on
> `is_fatal()`/recoverability — it retries recoverable transient errors and stops
> only on the fatal terminal, then fires `onEnd`/`onError` with the terminal
> cause (rsac-napi `src/lib.rs`). A transient hiccup no longer silently stops
> push delivery.

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

> **Concurrent `Close()` is safe (was BFFI-02 / #28 — RESOLVED 0.3.0):**
> `Close()` may be called concurrently with an in-flight `ReadBuffer`/
> `TryReadBuffer` — see [Binding behaviours
> (current)](#binding-behaviours-current).

## Not yet implemented

The tiers below remain research-only — there is no `bindings/rsac-uniffi` or
WASM target in the workspace today.

### 5. WASM — Not Viable for Capture

WASM runs in a browser sandbox with zero OS audio API access. All 5
`CaptureTarget` variants are impossible. Only `AudioBuffer` observation utilities
(RMS, peak, dBFS) could conceivably run in WASM; capture itself cannot.

### 6. Swift/Kotlin (Mobile) — backends designed, bindings still research

**The blocker was never binding generation — it was backends, and those are
now designed.** [`MOBILE_BACKEND_DESIGN.md`](MOBILE_BACKEND_DESIGN.md) +
[ADR-0012](designs/0012-mobile-platform-strategy.md)/[ADR-0013](designs/0013-mobile-capturetarget-semantics.md)
specify the Android (`AudioPlaybackCapture` + AAudio, JNI ingest, first-party
Kotlin AAR) and iOS (AVAudioEngine + ReplayKit broadcast extension, first-party
Swift package) backends; implementation is seeded (label `xplat`, waves 3–4).

| Platform | Current Backend | Designed Backend |
|---|---|---|
| iOS | None (honest stub) | `AVAudioEngine` mic + ReplayKit `SystemDefault` (per-app capture: impossible, permanent) |
| Android | None (honest stub) | `AudioPlaybackCapture` (system/app/tree, consent-gated) + `AAudio` mic |

**UniFFI**-generated Swift/Kotlin *bindings* remain research-only: the
first mobile consumers (Tauri via `tauri-plugin-rsac`, Dioxus, Flutter over
the C FFI) don't need them — the batteries-included AAR/SwiftPM glue plus the
Rust/C surfaces cover them. Revisit UniFFI after the backends ship (wave 4+).

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

> **C-header note (was BFFI-01 — RESOLVED, rsac-1413):** the curated
> `bindings/rsac-ffi/include/rsac.h` remains the source of truth the Go layer
> links against. The old cbindgen double-prefix problem (`RsacRsacBuilder`) was
> fixed by removing the export prefix in `bindings/rsac-ffi/cbindgen.toml`;
> `build.rs` regenerates `include/rsac_generated.h` on every build and the CI
> `check-bindings` job diffs the two headers' symbol sets to catch drift.
