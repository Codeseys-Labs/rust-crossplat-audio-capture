# rsac — Python bindings

Streaming-first cross-platform audio capture. Capture system audio,
per-application audio, or process-tree audio on Windows (WASAPI),
Linux (PipeWire), and macOS (CoreAudio Process Tap) from Python.

Built with [PyO3](https://pyo3.rs) + [maturin](https://www.maturin.rs) on top
of the Rust [`rsac`](https://crates.io/crates/rsac) crate.

## Install

```bash
pip install rsac
```

## Quick start

```python
import rsac

# Query what the current platform supports
caps = rsac.platform_capabilities()
print(f"Backend: {caps.backend_name}")
print(f"App capture: {caps.supports_application_capture}")

# List audio devices
for dev in rsac.list_devices():
    print(f"  {dev.name} (default={dev.is_default})")

# Stream audio as a context manager + iterator
with rsac.AudioCapture(target=rsac.CaptureTarget.system_default()) as cap:
    for buffer in cap:
        print(f"frames={buffer.num_frames} rms={buffer.rms():.4f}")
```

## Capture targets

```python
rsac.CaptureTarget.system_default()
rsac.CaptureTarget.device("device-id-string")
rsac.CaptureTarget.application("app-session-id")
rsac.CaptureTarget.application_by_name("Firefox")
rsac.CaptureTarget.process_tree(12345)        # PID
```

Not every target is supported on every platform — check
`rsac.platform_capabilities()` first.

## AudioBuffer

Each iteration yields an `AudioBuffer`:

```python
buf.num_frames        # int
buf.channels          # int
buf.sample_rate       # int (Hz)
buf.duration_secs     # float
buf.to_list()         # list[float] — interleaved f32
buf.to_bytes()        # bytes — little-endian f32
buf.channel_data(0)   # list[float] for one channel
buf.rms()             # float
buf.peak()            # float
```

## Error handling

All exceptions inherit from `rsac.RsacError` (which itself is an `OSError`):

```python
try:
    with rsac.AudioCapture() as cap:
        buf = cap.read()
except rsac.PermissionDeniedError:
    ...
except rsac.DeviceNotFoundError:
    ...
except rsac.StreamError:
    ...
```

## Build from source

```bash
pip install maturin
cd bindings/rsac-python
maturin develop --release
```

## License

MIT OR Apache-2.0
