// tauri-plugin-rsac — JS/TS guest API (ADR-0014).
//
// Thin invoke/event wrappers over the `plugin:rsac|*` command namespace. The
// default event path carries DERIVED data (meters/format), computed Rust-side;
// raw interleaved samples are opt-in via `subscribeRaw` and require the host to
// grant the `allow-subscribe-raw` permission (ADR-0014 §4.2).

import { invoke, Channel } from '@tauri-apps/api/core'

// ── Types (mirror src/models.rs on the wire, camelCase) ───────────────────

export interface CaptureConfig {
  sampleRate?: number
  channels?: number
  /** One of "i16" | "i24" | "i32" | "f32" (case-insensitive). */
  sampleFormat?: string
  /** Ring-buffer depth in slots (honored on Windows today). */
  bufferSize?: number
}

export interface FormatInfo {
  sampleRate: number
  channels: number
  sampleFormat: string
}

export interface ConsentResult {
  granted: boolean
  reason?: string
}

export interface StartCaptureResult {
  captureId: string
  format: FormatInfo
}

export interface TargetInfo {
  id: string
  name: string
  /** "systemDefault" | "device" | "application" | "unknown". */
  kind: string
}

export interface Capabilities {
  supportsSystemCapture: boolean
  supportsApplicationCapture: boolean
  supportsProcessTreeCapture: boolean
  supportsDeviceSelection: boolean
  supportsDeviceChangeNotifications: boolean
  requiresUserConsent: boolean
  supportedSampleFormats: string[]
  sampleRateRange: [number, number]
  maxChannels: number
  backendName: string
}

/** Derived per-chunk meter event — the DEFAULT event (rsac://chunk-meta). */
export interface ChunkMeta {
  sampleRate: number
  channels: number
  frames: number
  durationSecs: number
  rms: number
  peak: number
  rmsDbfs: number
  peakDbfs: number
  channelRms: number[]
  channelPeak: number[]
  format: FormatInfo
}

/** Raw interleaved-f32 chunk event — the OPT-IN slow path (rsac://chunk-raw). */
export interface ChunkRaw {
  sampleRate: number
  channels: number
  frames: number
  /** Interleaved f32 PCM samples (frames * channels values). */
  samples: number[]
}

// ── Commands ───────────────────────────────────────────────────────────────

/**
 * Requests capture consent. On desktop this resolves `{ granted: true }`
 * immediately (no consent artifact required). On Android it drives the
 * MediaProjection dialog and, on approval, threads the projection token into
 * subsequent captures.
 */
export async function requestConsent(): Promise<ConsentResult> {
  return await invoke('plugin:rsac|request_consent')
}

/**
 * Builds and starts a capture. `target` uses rsac's canonical target grammar
 * (e.g. "system-default", "device:hw:0,0", "app:1234", "name:VLC", "tree:42").
 */
export async function startCapture(
  target: string,
  config: CaptureConfig = {}
): Promise<StartCaptureResult> {
  return await invoke('plugin:rsac|start_capture', { target, config })
}

/** Stops (and releases) a capture. Idempotent. */
export async function stopCapture(captureId: string): Promise<void> {
  await invoke('plugin:rsac|stop_capture', { captureId })
}

/** Lists capturable audio sources. */
export async function listTargets(): Promise<TargetInfo[]> {
  return await invoke('plugin:rsac|list_targets')
}

/**
 * Returns the platform capabilities verbatim from PlatformCapabilities::query().
 * Consult this before offering system/app capture — the plugin never claims a
 * feature this reports false.
 */
export async function capabilities(): Promise<Capabilities> {
  return await invoke('plugin:rsac|capabilities')
}

/**
 * Subscribes to derived per-chunk meter events (rsac://chunk-meta). Raw samples
 * never cross IPC on this path — prefer it for level meters / visualizers.
 */
export async function subscribeMeta(
  captureId: string,
  onChunk: (meta: ChunkMeta) => void
): Promise<void> {
  const channel = new Channel<ChunkMeta>()
  channel.onmessage = onChunk
  await invoke('plugin:rsac|subscribe_meta', { captureId, channel })
}

/**
 * Subscribes to raw interleaved-f32 chunk events (rsac://chunk-raw) — the
 * documented SLOW PATH. JSON-serializing f32 at 48 kHz through Tauri IPC is
 * wasteful (ADR-0014 §2); prefer Rust-side consumption or `subscribeMeta`.
 * Requires the host to grant the `allow-subscribe-raw` permission.
 */
export async function subscribeRaw(
  captureId: string,
  onChunk: (raw: ChunkRaw) => void
): Promise<void> {
  const channel = new Channel<ChunkRaw>()
  channel.onmessage = onChunk
  await invoke('plugin:rsac|subscribe_raw', { captureId, channel })
}
