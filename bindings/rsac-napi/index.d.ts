// index.d.ts — TypeScript type definitions for @rsac/audio
//
// Production-ready Node.js bindings for rsac (Rust Cross-Platform Audio Capture).
// Streaming-first audio capture: callbacks, async reads, device enumeration.

/**
 * A chunk of captured audio data.
 *
 * Contains interleaved Float32 PCM samples along with format metadata.
 * This is the primary data unit flowing through the JS capture pipeline.
 */
export interface AudioChunk {
  /** Interleaved PCM audio samples as a native `Float32Array` (the captured
   * `f32` samples carried through directly — no `f32` -> `f64` widening). */
  data: Float32Array;
  /** Number of audio frames (samples per channel). */
  numFrames: number;
  /** Number of audio channels. */
  channels: number;
  /** Sample rate in Hz. */
  sampleRate: number;
  /** Total number of interleaved samples (numFrames * channels). */
  length: number;
  /** Duration of this chunk in seconds. */
  duration: number;
  /** RMS (root-mean-square) level across all samples/channels, linear
   * `0.0..=1.0`. Computed once by rsac core (alloc-free, NaN-safe); `0.0` for
   * silence or an empty chunk, never `NaN`. */
  rms: number;
  /** Peak (max absolute) level across all samples/channels, linear `0.0..=1.0`.
   * `0.0` for an empty chunk, never `NaN`. */
  peak: number;
  /** RMS level in dBFS (`20·log10(rms)`). `-Infinity` at silence; `0.0` dBFS is
   * full scale. */
  rmsDbfs: number;
  /** Peak level in dBFS (`20·log10(peak)`). `-Infinity` at silence; `0.0` dBFS
   * is full scale. */
  peakDbfs: number;
  /** Per-channel RMS levels in channel order, linear `0.0..=1.0`
   * (`channelRms[ch]` is the RMS of channel `ch`). Length equals `channels`;
   * empty when the chunk reports `0` channels. Computed once by rsac core. */
  channelRms: number[];
  /** Per-channel peak levels in channel order, linear `0.0..=1.0`
   * (`channelPeak[ch]` is the peak of channel `ch`). Length equals `channels`;
   * empty when the chunk reports `0` channels. Computed once by rsac core. */
  channelPeak: number[];
}

/**
 * Specifies what audio to capture. Use the static factory methods.
 *
 * @example
 * ```ts
 * CaptureTarget.systemDefault()
 * CaptureTarget.device("device-id-string")
 * CaptureTarget.applicationByName("Firefox")
 * CaptureTarget.processTree(12345)
 * ```
 */
export declare class CaptureTarget {
  /** Capture from the system default audio device. */
  static systemDefault(): CaptureTarget;
  /** Capture from a specific audio device by ID. */
  static device(deviceId: string): CaptureTarget;
  /** Capture audio from a specific application by session ID. */
  static application(appId: string): CaptureTarget;
  /** Capture audio from the first application matching the given name. */
  static applicationByName(name: string): CaptureTarget;
  /** Capture audio from a process and all its child processes. */
  static processTree(pid: number): CaptureTarget;
  /**
   * Parse a capture target from its canonical string form. The scheme prefix is
   * matched case-insensitively:
   *
   * - `"system"` / `"default"` — system default mix
   * - `"device:<id>"` — a specific device (the id may itself contain colons,
   *   e.g. `"device:hw:0,0"`)
   * - `"app:<id>"` — a specific application session
   * - `"name:<name>"` — the first application matching `<name>`
   * - `"tree:<pid>"` / `"pid:<pid>"` — a process and its children
   *
   * Throws an `Error` (code `ERR_RSAC_CONFIGURATION`) on an unknown scheme or a
   * non-numeric / out-of-range pid.
   *
   * @example
   * ```ts
   * CaptureTarget.parse("system");
   * CaptureTarget.parse("name:Firefox");
   * CaptureTarget.parse("pid:1234");
   * ```
   */
  static parse(spec: string): CaptureTarget;
  /** Returns a string description of this capture target. */
  describe(): string;
}

/**
 * The primary audio capture class for Node.js.
 *
 * Wraps rsac's AudioCaptureBuilder -> AudioCapture pipeline and exposes
 * streaming-first methods: `onData()` for push-based callbacks,
 * `read()` for sync pull, `readAsync()` for async pull, and
 * `start()`/`stop()` for lifecycle control.
 *
 * @example
 * ```ts
 * import { AudioCapture, CaptureTarget } from '@rsac/audio';
 *
 * // Create with specific target and settings
 * const capture = AudioCapture.create(
 *   CaptureTarget.systemDefault(),
 *   48000,  // sampleRate
 *   2,      // channels
 * );
 *
 * // Push-based streaming (most efficient)
 * capture.onData((chunk) => {
 *   console.log(`Got ${chunk.numFrames} frames at ${chunk.sampleRate}Hz`);
 * });
 *
 * capture.start();
 *
 * // ... later ...
 * capture.stop();
 * ```
 */
export declare class AudioCapture {
  /**
   * Create a new AudioCapture with system default target and default settings
   * (48kHz, stereo).
   */
  constructor();

  /**
   * Create a new AudioCapture with a specific target and optional settings.
   *
   * @param target - The capture target (from CaptureTarget static methods).
   * @param sampleRate - Desired sample rate in Hz (default: 48000).
   * @param channels - Desired number of channels (default: 2).
   * @param bufferSize - Desired buffer size in frames (optional).
   */
  static create(
    target: CaptureTarget,
    sampleRate?: number | null,
    channels?: number | null,
    bufferSize?: number | null,
  ): AudioCapture;

  /**
   * Start audio capture.
   *
   * If an `onData` callback is registered, a background thread is spawned
   * that reads audio chunks and pushes them to JavaScript.
   */
  start(): void;

  /**
   * Stop audio capture and release resources.
   *
   * Active data pump threads are terminated. The callback is preserved
   * and can be reused if a new capture session is started.
   */
  stop(): void;

  /** Whether the capture is currently running. */
  readonly isRunning: boolean;

  /**
   * Read a single audio chunk synchronously (non-blocking).
   * Returns `null` if no data is currently available.
   * Throws if the capture is not running.
   */
  read(): AudioChunk | null;

  /**
   * Read a single audio chunk, blocking until data is available.
   * WARNING: This blocks the Node.js event loop. Use `readBlockingAsync()`
   * or `onData()` in production.
   */
  readBlocking(): AudioChunk;

  /**
   * Read a single audio chunk asynchronously (non-blocking, off main thread).
   * Returns `null` if no data is currently available.
   * Throws if the capture is not running.
   */
  readAsync(): Promise<AudioChunk | null>;

  /**
   * Read a single audio chunk asynchronously, blocking the worker thread
   * until data is available. Does not block the Node.js event loop.
   */
  readBlockingAsync(): Promise<AudioChunk>;

  /**
   * Register a callback for push-based audio data delivery.
   *
   * The callback receives `AudioChunk` objects as audio is captured.
   * This is the most efficient way to consume audio data from Node.js.
   *
   * Only one callback can be active at a time. Calling `onData()` again
   * replaces the previous callback.
   */
  onData(callback: (chunk: AudioChunk) => void): void;

  /**
   * Remove the registered data callback.
   * Stops the data pump thread if running.
   */
  offData(): void;

  /**
   * Number of audio buffers dropped due to ring buffer overflow.
   * A non-zero value means the JavaScript consumer is not keeping up.
   */
  readonly overrunCount: number;

  /**
   * A point-in-time snapshot of the stream's diagnostic counters.
   *
   * Reading this never allocates on or blocks the OS audio callback thread.
   * Before `start()` (or after `stop()`) the snapshot is zeroed with
   * `isRunning === false` and `uptimeSecs === 0`.
   *
   * @example
   * ```ts
   * const s = capture.streamStats();
   * console.log(`pushed=${s.buffersPushed} dropped=${s.buffersDropped} ` +
   *             `ratio=${s.droppedRatio.toFixed(4)} up=${s.uptimeSecs}s`);
   * ```
   */
  streamStats(): StreamStats;

  /**
   * The negotiated *delivery* format the backend actually produces, or `null`
   * before `start()` creates a stream. May differ from the requested settings
   * when the device forced a negotiation.
   */
  readonly format: AudioFormat | null;
}

/**
 * A point-in-time snapshot of an {@link AudioCapture}'s diagnostic counters.
 *
 * Cumulative counters are `bigint` (Rust `u64`) so they do not silently lose
 * precision past `Number.MAX_SAFE_INTEGER` on a long-running capture.
 */
export interface StreamStats {
  /** Buffers dropped due to ring-buffer overflow (alias of `buffersDropped`,
   * kept for parity with `overrunCount`). */
  overruns: bigint;
  /** Cumulative buffers delivered to the consumer (popped off the ring). */
  buffersCaptured: bigint;
  /** Cumulative buffers dropped due to ring-buffer overflow. */
  buffersDropped: bigint;
  /** Cumulative buffers enqueued by the producer (the OS audio callback). */
  buffersPushed: bigint;
  /** How long the stream has been running, in seconds. `0` when not started. */
  uptimeSecs: number;
  /** Fraction of accounted-for buffers lost to overflow, in `0.0..=1.0`
   * (`buffersDropped / (buffersCaptured + buffersDropped)`; `0.0` when none). */
  droppedRatio: number;
  /** Whether the stream is currently capturing. */
  isRunning: boolean;
  /** Compact human-readable format description (e.g. `"2ch 48000Hz F32"`);
   * empty before the stream starts. */
  formatDescription: string;
}

/**
 * The negotiated audio delivery format.
 */
export interface AudioFormat {
  /** Samples per second (e.g. 48000). */
  sampleRate: number;
  /** Number of interleaved channels (e.g. 2 for stereo). */
  channels: number;
  /** Sample format name: one of `"I16"`, `"I24"`, `"I32"`, `"F32"`. */
  sampleFormat: string;
}

/**
 * Information about an audio device.
 */
export interface AudioDevice {
  /** Unique platform-specific device identifier. */
  id: string;
  /** Human-readable device name. */
  name: string;
  /** Whether this is the system default device. */
  isDefault: boolean;
}

/**
 * List all available audio devices on the system.
 *
 * Returns an array of device objects with id, name, and isDefault fields.
 * Performs device enumeration on a worker thread.
 *
 * @example
 * ```ts
 * const devices = await listDevices();
 * for (const dev of devices) {
 *   console.log(`${dev.name} (${dev.id}) ${dev.isDefault ? '[default]' : ''}`);
 * }
 * ```
 */
export declare function listDevices(): Promise<AudioDevice[]>;

/**
 * Get the default audio output device.
 */
export declare function getDefaultDevice(): Promise<AudioDevice>;

/**
 * Platform capability information.
 */
export interface PlatformCapabilities {
  /** Whether system-wide audio capture is supported. */
  supportsSystemCapture: boolean;
  /** Whether per-application audio capture is supported. */
  supportsApplicationCapture: boolean;
  /** Whether process-tree audio capture is supported. */
  supportsProcessTreeCapture: boolean;
  /** Whether device selection is supported. */
  supportsDeviceSelection: boolean;
  /** Maximum number of channels supported. */
  maxChannels: number;
  /** Minimum supported sample rate in Hz. */
  minSampleRate: number;
  /** Maximum supported sample rate in Hz. */
  maxSampleRate: number;
  /** Name of the audio backend (e.g., "WASAPI", "CoreAudio", "PipeWire"). */
  backendName: string;
}

/**
 * Query the audio capabilities of the current platform.
 *
 * Returns information about what capture modes, sample rates, and
 * channel configurations are supported.
 *
 * @example
 * ```ts
 * const caps = platformCapabilities();
 * console.log(`Backend: ${caps.backendName}`);
 * if (caps.supportsApplicationCapture) {
 *   // Safe to use CaptureTarget.application() / applicationByName()
 * }
 * ```
 */
export declare function platformCapabilities(): PlatformCapabilities;
