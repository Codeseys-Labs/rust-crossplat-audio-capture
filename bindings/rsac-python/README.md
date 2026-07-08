# rsac — Python bindings

Streaming-first cross-platform audio capture. Capture system audio,
per-application audio, or process-tree audio on Windows (WASAPI),
Linux (PipeWire), and macOS (CoreAudio Process Tap) from Python.

Built with [PyO3](https://pyo3.rs) + [maturin](https://www.maturin.rs) on top
of the Rust `rsac` crate in this repository.

## Install

The PyPI package is not published yet. Build from source for now:

```bash
cd bindings/rsac-python
pip install maturin
maturin develop --release
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

Or parse the canonical string grammar (case-insensitive scheme) with a single
entry point — handy for CLI args / config files:

```python
rsac.CaptureTarget.parse("system")
rsac.CaptureTarget.parse("device:<id>")
rsac.CaptureTarget.parse("app:<id>")
rsac.CaptureTarget.parse("name:Firefox")
rsac.CaptureTarget.parse("tree:12345")        # process tree by PID
# Invalid strings raise rsac.ConfigurationError:
rsac.CaptureTarget.parse("not-a-target")
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
buf.rms()             # float — RMS across all channels
buf.peak()            # float — peak magnitude across all channels
buf.rms_dbfs()        # float — RMS in dBFS (-inf at silence)
buf.peak_dbfs()       # float — peak in dBFS (-inf at silence)
buf.channel_rms(0)    # float | None — per-channel RMS (None if out of range)
buf.channel_peak(0)   # float | None — per-channel peak (None if out of range)
```

Metering delegates to rsac's core (NaN-safe, zero-alloc), so the values match
every other rsac binding.

## Stream stats and format

A running capture exposes live counters and its negotiated delivery format:

```python
with rsac.AudioCapture() as cap:
    cap.read()
    stats = cap.stream_stats()
    print(stats.overruns, stats.buffers_captured, stats.buffers_dropped)
    print(stats.buffers_pushed, stats.uptime_secs, stats.is_running)
    print(stats.dropped_ratio())          # 0.0..=1.0

    fmt = cap.format                       # None before start / after close
    if fmt is not None:
        print(fmt.sample_rate, fmt.channels, fmt.sample_format)  # e.g. 48000 2 'f32'
```

`stream_stats()` reports **lifetime** counters (cumulative since the stream
opened). For the **windowed** drop-rate view — bounded to a recent window, so it
surfaces a sustained 1-in-N loss that the lifetime totals dilute — use
`backpressure_report()`:

```python
with rsac.AudioCapture() as cap:
    cap.read()
    bp = cap.backpressure_report()
    print(bp.window_secs, bp.pushed, bp.dropped)   # window_secs: float seconds
    print(bp.drop_rate)                             # 0.0..=1.0 over the window
    if bp.is_under_backpressure:
        print("sustained drops right now")
```

## Async usage

`AudioCapture` is also an async context manager (`async with`). `__aexit__`
closes the stream best-effort and never masks an exception raised in the body:

```python
async def capture():
    async with rsac.AudioCapture() as cap:
        buf = await_loop_safe_read(cap)   # your async-aware read wrapper
```

If a capture is garbage-collected without an explicit `close()`, `with`, or
`async with`, a `__del__` finalizer stops the underlying OS stream so it never
leaks. Calling `close()` more than once is safe (idempotent).

## Error handling

Most runtime exceptions inherit from `rsac.RsacError` (which itself is an
`OSError`). The one exception is `rsac.ConfigurationError` — raised for invalid
configuration and bad target strings (e.g. `CaptureTarget.parse()`) — which
extends `ValueError`, not `RsacError`, so catch it separately (or catch
`Exception`) when validating input:

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

# ConfigurationError is a ValueError, not an RsacError:
try:
    target = rsac.CaptureTarget.parse("not-a-valid-target")
except rsac.ConfigurationError:  # also catchable as ValueError
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
