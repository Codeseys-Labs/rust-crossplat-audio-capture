# @rsac/audio

Node.js bindings for [rsac](https://github.com/baladita/rust-crossplat-audio-capture) — a Rust cross-platform audio capture library supporting per-application and system-wide loopback capture on Windows (WASAPI), macOS (CoreAudio Process Taps, 14.4+), and Linux (PipeWire).

Built with [napi-rs](https://napi.rs/).

## Install

```bash
bun add @rsac/audio
# or
npm install @rsac/audio
```

Prebuilt binaries are published for:

- `darwin-x64`, `darwin-arm64`
- `linux-x64-gnu`, `linux-arm64-gnu`
- `win32-x64-msvc`

## Usage

### Push-based streaming (recommended)

```ts
import { AudioCapture, CaptureTarget } from '@rsac/audio'

const capture = AudioCapture.create(
  CaptureTarget.systemDefault(),
  48000, // sampleRate
  2,     // channels
)

capture.onData((chunk) => {
  console.log(`Got ${chunk.numFrames} frames @ ${chunk.sampleRate} Hz`)
  // chunk.data is interleaved PCM samples as number[]
})

capture.start()

// later...
capture.stop()
```

### Capture a specific application

```ts
const capture = AudioCapture.create(
  CaptureTarget.applicationByName('Firefox'),
)
capture.onData((chunk) => { /* ... */ })
capture.start()
```

### Device enumeration

```ts
import { listDevices, getDefaultDevice, platformCapabilities } from '@rsac/audio'

const devices = await listDevices()
for (const d of devices) {
  console.log(`${d.name} (${d.id})${d.isDefault ? ' [default]' : ''}`)
}

const caps = platformCapabilities()
console.log(`Backend: ${caps.backendName}`)
```

### Capture targets

- `CaptureTarget.systemDefault()` — system default output (loopback)
- `CaptureTarget.device(id)` — a specific device by ID
- `CaptureTarget.application(sessionId)` — a specific application session
- `CaptureTarget.applicationByName(name)` — first app matching name
- `CaptureTarget.processTree(pid)` — a process and its children

Not every target is supported on every platform — check `platformCapabilities()` first.

## Platform notes

- **macOS**: Per-application capture requires macOS 14.4 or later (Process Tap API).
- **Linux**: Requires PipeWire 0.3.44+ and `libpipewire-0.3-dev` at runtime.
- **Windows**: Process Loopback is available on Windows 10 2004+.

## Building from source

```bash
cd bindings/rsac-napi
bun install
bun run build       # release build (runs `napi build --platform --release`)
bun run build:debug # debug build

# Or invoke the napi CLI directly without scripts:
bunx @napi-rs/cli build --platform --release
```

This produces a platform-specific `rsac-audio.<triple>.node` in the package directory.

`@napi-rs/cli` is a devDependency — no global install needed.

## License

MIT OR Apache-2.0
