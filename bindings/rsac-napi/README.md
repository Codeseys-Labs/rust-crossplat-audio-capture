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
  // chunk.data is interleaved PCM samples as a Float32Array
  // Level metering is precomputed by rsac core (alloc-free, NaN-safe):
  console.log(`rms=${chunk.rms.toFixed(4)} peak=${chunk.peak.toFixed(4)} ` +
              `(${chunk.rmsDbfs.toFixed(1)} dBFS / ${chunk.peakDbfs.toFixed(1)} dBFS)`)
})

capture.start()

// later...
capture.stop()
```

### Level metering

Each `AudioChunk` carries level meters computed once by rsac's core (no
re-iteration in JS, NaN-safe). Whole-buffer scalars: `rms`, `peak` (linear
`0.0..=1.0`) and their dBFS forms `rmsDbfs`, `peakDbfs` (`-Infinity` at silence,
`0.0` dBFS = full scale). Per-channel levels are the `channelRms` / `channelPeak`
arrays, indexed by channel (length equals `channels`):

```ts
capture.onData((chunk) => {
  console.log(`rms=${chunk.rms.toFixed(4)} peak=${chunk.peak.toFixed(4)}`)
  for (let ch = 0; ch < chunk.channels; ch++) {
    console.log(`ch${ch}: rms=${chunk.channelRms[ch]} peak=${chunk.channelPeak[ch]}`)
  }
})
```

### Stream stats & negotiated format

```ts
const s = capture.streamStats()
// counters are bigint (Rust u64) — they never lose precision on a long capture
console.log(`captured=${s.buffersCaptured} dropped=${s.buffersDropped} ` +
            `dropRatio=${s.droppedRatio.toFixed(4)} up=${s.uptimeSecs}s`)

const fmt = capture.format // AudioFormat | null (null before start())
if (fmt) console.log(`${fmt.channels}ch ${fmt.sampleRate}Hz ${fmt.sampleFormat}`)
```

`streamStats()` carries **lifetime** counters (cumulative since `start()`). For
the **windowed** drop-rate view — bounded to a recent window, so it surfaces a
sustained 1-in-N loss that the lifetime totals dilute — use
`backpressureReport()`:

```ts
const bp = capture.backpressureReport()
// pushed/dropped are bigint (Rust u64); windowSecs is the span in seconds
console.log(`windowSecs=${bp.windowSecs} pushed=${bp.pushed} dropped=${bp.dropped} ` +
            `dropRate=${bp.dropRate.toFixed(4)} underBackpressure=${bp.isUnderBackpressure}`)
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
- `CaptureTarget.parse(spec)` — parse any of the above from one string

`CaptureTarget.parse(spec)` accepts the canonical cross-binding grammar (scheme
matched case-insensitively) so you can take a capture target straight from a CLI
arg or config value:

```ts
CaptureTarget.parse('system')        // or 'default'
CaptureTarget.parse('device:hw:0,0') // device ids may contain colons
CaptureTarget.parse('app:session-id')
CaptureTarget.parse('name:Firefox')
CaptureTarget.parse('pid:1234')      // or 'tree:1234'
```

It throws an `Error` (`ERR_RSAC_CONFIGURATION`) on an unknown scheme or a
non-numeric pid — it never panics. The typed constructors above remain available.

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
