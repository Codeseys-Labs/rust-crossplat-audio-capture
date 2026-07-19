# tauri-plugin-rsac

A thin [Tauri v2](https://v2.tauri.app) plugin exposing
[rsac](https://github.com/Codeseys-Labs/rust-crossplat-audio-capture)'s
audio-capture API to a webview, plus the Android MediaProjection consent flow.
Implements [ADR-0014](../../docs/designs/0014-tauri-integration-model.md).

> **Status:** ships **compile-proof**. Desktop builds + clippies in the
> workspace CI. The `android/` and `ios/` plugin directories are
> source-shipped; mobile **runtime** verification tracks rsac-e6d3 / rsac-97c8
> (unchanged from ADR-0012's honesty posture — the plugin does not claim a
> mobile runtime capability).

## When to use this (vs. a direct dependency)

Per ADR-0014, a Tauri **desktop** app is usually better off consuming rsac as a
**direct Rust dependency**: full API surface, zero IPC serialization of audio.
This plugin earns its keep in two situations:

1. **Mobile** — it is the sanctioned vehicle for shipping the Android/iOS native
   glue and the consent flow into a Tauri app.
2. **A reusable JS-facing API** — invoke/event commands governed by Tauri's
   permission system, shared across Tauri apps that prefer no Rust of their own.

The plugin is a **thin adapter**: consent flow + lifecycle commands +
subscription events. No capture policy or backend logic — that stays in rsac
(ADR-0012 ownership boundary). Desktop is a passthrough.

## Install

`src-tauri/Cargo.toml`:

```toml
[dependencies]
tauri-plugin-rsac = { path = "../../integrations/tauri-plugin-rsac" }
```

Register it in your Tauri builder:

```rust
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_rsac::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

Grant the plugin's default permission set in your capability file
(`src-tauri/capabilities/default.json`):

```json
{
  "permissions": ["rsac:default"]
}
```

The `rsac:default` set enables `list_targets`, `capabilities`,
`request_consent`, `start_capture`, `stop_capture`, and `subscribe_meta`. It
**deliberately excludes** `subscribe_raw` — see [Raw samples](#raw-samples-the-slow-path).

## Try it (JS)

```ts
import {
  requestConsent,
  listTargets,
  capabilities,
  startCapture,
  stopCapture,
  subscribeMeta,
  type ChunkMeta
} from 'tauri-plugin-rsac-api'

// 1. Check what the platform actually supports (honesty gate).
const caps = await capabilities()
if (!caps.supportsSystemCapture) {
  throw new Error(`system capture unsupported on ${caps.backendName}`)
}

// 2. Consent. Desktop: instant success. Android: MediaProjection dialog.
const consent = await requestConsent()
if (!consent.granted) throw new Error(consent.reason)

// 3. Pick a target and start.
const targets = await listTargets()
const { captureId } = await startCapture('system-default', {
  sampleRate: 48000,
  channels: 2
})

// 4. Subscribe to DERIVED meter events (raw samples never cross IPC here).
await subscribeMeta(captureId, (m: ChunkMeta) => {
  console.log(`peak ${m.peakDbfs.toFixed(1)} dBFS, rms ${m.rmsDbfs.toFixed(1)} dBFS`)
})

// 5. Later:
await stopCapture(captureId)
```

## Events — derived data by default

`subscribeMeta` streams `rsac://chunk-meta` events: per-chunk RMS/peak (linear
+ dBFS), per-channel meters, frame count, duration, and the negotiated format.
The meter primitives are computed by rsac core (alloc-free, NaN-safe); the
event assembly itself (per-channel vectors + the format string) allocates, but
runs on the plugin's consumer-side pump thread — never the OS audio callback.
**Raw samples never cross IPC on this path.** This mirrors the proven napi
`ChunkMeta` shape.

### Raw samples (the slow path)

`subscribeRaw` streams `rsac://chunk-raw` events carrying interleaved f32.
JSON-serializing interleaved f32 at 48 kHz through Tauri IPC is wasteful
(ADR-0014 §2); **prefer Rust-side consumption or `subscribeMeta`.** It is
present so a no-Rust JS app *can* access samples, at a documented cost.

Because of that cost, `subscribe_raw` is **not** in the default permission set.
A host must explicitly grant it:

```json
{
  "permissions": ["rsac:default", "rsac:allow-subscribe-raw"]
}
```

This enforces derived-data-by-default at the **permission layer**, not just by
convention.

## Android consent flow

On Android, `requestConsent()` drives the MediaProjection consent dialog via
the first-party `ai.codeseys.rsac.RsacProjection` (the `mobile/android/` AAR,
ADR-0012). The Kotlin plugin (`android/…/RsacTauriPlugin.kt`) is a **thin
forwarder** onto `RsacProjection.request` — it inherits the deferred
foreground-service-acquire ordering from rsac PR#64 and **must not** start the
`mediaProjection` FGS itself (doing so pre-consent throws `SecurityException`
on API 34+). On approval, the opaque projection token is threaded into
`AudioCaptureBuilder::with_android_projection` at `start_capture`; dropping the
capture (`stop_capture`) releases the token. The host stops the foreground
service (`RsacCaptureService.stop`) afterward.

## Platform support

| Target | Status |
|---|---|
| Desktop (Win/Linux/macOS) | ✅ builds + clippies in workspace CI; passthrough to rsac |
| Android | source-shipped, compile-proof; runtime tracks rsac-e6d3 |
| iOS | source-shipped stub, compile-proof; runtime tracks rsac-97c8 |

The `capabilities` command surfaces `PlatformCapabilities::query()` verbatim —
never trust a hardcoded feature list; ask the plugin.

## Example app

A full example Tauri app exercising consent → capture → meter on an Android
device/emulator is deferred (it needs device/emulator runtime, rsac-e6d3 /
rsac-97c8) and tracked as a follow-up seed. The **Try it** recipe above is the
hand-wiring reference in the meantime.
