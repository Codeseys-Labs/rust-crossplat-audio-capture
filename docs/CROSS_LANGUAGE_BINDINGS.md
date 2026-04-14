# rsac Cross-Language Bindings Research

> **Status:** Research complete, implementation not started  
> **Note:** No existing Rust audio crate has cross-language bindings. rsac would be the first.

## Summary

| Language | Feasibility | Tooling | Effort | All Capture Tiers? |
|---|---|---|---|---|
| **C FFI** | High | cbindgen | 8-12 hours | Yes (all 5) |
| **Python** | High | PyO3 + maturin | 3-5 days | Yes (all 5) |
| **Node.js/Bun** | High | napi-rs | 5-7 days | Yes (all 5) |
| **Go** | Medium-High | CGo over C FFI | 15-20 hours | Yes (all 5) |
| **Swift/Kotlin** | Medium | UniFFI | 6-10 weeks | Partial (needs mobile backends) |
| **WASM** | Low | wasm-bindgen | 2-3 weeks | No (0/5, sandbox) |

**Recommended order:** C FFI → Python → Node.js → Go → Swift/Kotlin → WASM

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

```python
import rsac
import numpy as np

with rsac.AudioCapture(
    target=rsac.CaptureTarget.system_default(),
    sample_rate=48000, channels=2,
) as capture:
    for buffer in capture:  # __iter__ with GIL release
        audio = buffer.to_numpy()  # zero-copy via rust-numpy
        rms = np.sqrt(np.mean(audio ** 2))

# Application capture
target = rsac.CaptureTarget.application_by_name("Firefox")
with rsac.AudioCapture(target=target) as cap:
    buffer = cap.read()

# Device enumeration
devices = rsac.list_devices()
caps = rsac.platform_capabilities()
```

**Key:** GIL release during blocking reads via `py.allow_threads()`. NumPy integration via `rust-numpy`.

## 3. Node.js/Bun (napi-rs)

```typescript
import { AudioCapture, CaptureTarget } from '@rsac/audio';

const capture = new AudioCapture({ sampleRate: 48000, channels: 2 });
capture.onData((chunk) => {
  const data: Float32Array = chunk.data;
  console.log(`${chunk.numFrames} frames`);
});
capture.start();

// Async iterator
for await (const chunk of capture.stream()) {
  await process(chunk.data.buffer);
}

// Node.js Readable stream
const stream = capture.createReadableStream();
stream.pipe(analyzer);
```

**Streaming:** rsac's `subscribe()` → `mpsc::Receiver` maps to napi-rs `ThreadsafeFunction` for event loop integration. Works in Electron (main process) and Bun.

**For Tauri apps:** Use rsac as a direct Rust dependency instead — no napi-rs needed.

## 4. Go (CGo over C FFI)

```go
capture, _ := rsac.NewCaptureBuilder().
    WithApplicationByName("Firefox").
    SampleRate(48000).Build()
defer capture.Close()
capture.Start()

for buf := range capture.Stream(ctx) {
    fmt.Printf("%d frames\n", buf.NumFrames)
}
```

**Requires C FFI (Phase 1) first.** Buffer transfer is always copy (~1μs for 10ms chunk). CGo overhead (~100ns/call) is negligible at audio rates.

## 5. WASM — Not Viable for Capture

WASM runs in a browser sandbox with zero OS audio API access. All 5 `CaptureTarget` variants are impossible. Only `AudioBuffer` processing utilities could work in WASM (RMS, peak, mixing, etc.).

## 6. Swift/Kotlin (Mobile)

**UniFFI** generates both Swift and Kotlin from a single Rust source. The binding generation is straightforward, but **new audio backends are needed**:

| Platform | Current Backend | Mobile Reality |
|---|---|---|
| iOS | macOS CoreAudio | Needs `AVAudioEngine` backend |
| Android | None | Needs `AAudio` + `MediaProjection` |

Estimated 6-10 weeks including new backends.

## Crate Structure

```
rsac/                    # workspace root
├── rsac/                # core library (existing)
├── rsac-ffi/            # C FFI (cbindgen)
├── rsac-python/         # PyO3 bindings
├── rsac-napi/           # napi-rs bindings
├── rsac-uniffi/         # UniFFI (Swift + Kotlin)
└── rsac-go/             # Go wrapper (separate repo)
```
