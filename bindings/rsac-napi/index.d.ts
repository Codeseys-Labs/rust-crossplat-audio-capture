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
   *
   * **Not terminal-observable.** The non-blocking pull readers (`read`,
   * `readAsync`) route through rsac's `read_buffer`, which short-circuits to
   * a *recoverable* read error the moment the stream leaves the running
   * state — it never surfaces the terminal "stream ended" cause. So a
   * non-blocking pull consumer cannot distinguish a transient hiccup from a
   * real terminal end via the thrown error. The BLOCKING readers
   * (`readBlocking`, `readBlockingAsync`) ARE terminal-observable, as is the
   * push pump (`onData`), which reports *why* the stream ended via `onEnd`.
   * With the non-blocking pull API, treat `stop()`/`isRunning` as the
   * end-of-stream signal.
   *
   * This terminal-cause-invisibility is the *only* thing that makes a thrown
   * read error retryable: while the capture is running, a thrown error may be a
   * transient hiccup masking a terminal end, so retry it. It does **not** make
   * *every* thrown read error retryable — `read*()` also throws when the capture
   * is not running (before `start()` or after `stop()`), which is a normal usage
   * error, not a transient condition. Don't blind-retry: gate retries on
   * `isRunning` so a pre-start/stopped capture doesn't spin in a retry loop.
   */
  read(): AudioChunk | null;

  /**
   * Read a single audio chunk, blocking until data is available.
   * WARNING: This blocks the Node.js event loop. Use `readBlockingAsync()`
   * or `onData()` in production.
   *
   * **Terminal-observable**: once the stream has ended — after `stop()` or a
   * fatal backend error — this throws the stream's true terminal error
   * promptly (it routes through `read_chunk_blocking`), instead of
   * downgrading it to a recoverable "not running" error.
   */
  readBlocking(): AudioChunk;

  /**
   * Read a single audio chunk asynchronously (non-blocking, off main thread).
   * Returns `null` if no data is currently available.
   * Throws if the capture is not running.
   *
   * Like `read()`, this is **not terminal-observable** (it routes through
   * `read_buffer`). Use `onData` + `onEnd` to observe the terminal reason.
   */
  readAsync(): Promise<AudioChunk | null>;

  /**
   * Read a single audio chunk asynchronously, blocking the worker thread
   * until data is available. Does not block the Node.js event loop.
   *
   * **Terminal-observable**: like `readBlocking()`, once the stream has
   * ended this rejects with the stream's true terminal error promptly (it
   * routes through `read_chunk_blocking`).
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
   * Register a callback that fires exactly once when push-based delivery
   * (`onData`) ends, carrying *why* it ended.
   *
   * The data pump ends in one of two ways a plain `onData` consumer cannot
   * otherwise distinguish:
   *
   * - **Terminal (fatal):** the backend stream reached its terminal state
   *   (e.g. the capture ended). `error` is the formatted terminal error
   *   message (a non-null `string`).
   * - **Clean stop:** the pump was torn down by `stop()` / `offData()`.
   *   `error` is `null`.
   *
   * This is the Node parity for the Rust `subscribe_with_errors` and Go
   * `StreamWithErrors` APIs — an `onData` consumer registers `onEnd` to learn
   * the terminal reason instead of the end being silent. Optional and
   * independent of `onData`.
   *
   * The registration **persists across multiple `start()`/`stop()` sessions**,
   * exactly like `onData`: it fires once per session (when that session's data
   * pump ends) and stays armed for the next `start()`. It is cleared only by
   * `offEnd()`, or replaced by calling `onEnd()` again. A recoverable hiccup
   * never fires it — only the terminal end (fatal) or a clean stop does.
   *
   * @example
   * ```ts
   * capture.onData((chunk) => process(chunk));
   * capture.onEnd((err) => {
   *   if (err) console.error(`capture ended: ${err}`);
   *   else console.log('capture stopped cleanly');
   * });
   * capture.start();
   * capture.stop();   // fires onEnd(null)
   * capture.start();  // onEnd still armed — fires again at the next stop
   * ```
   */
  onEnd(callback: (error: string | null) => void): void;

  /**
   * Remove the registered data callback.
   * Stops the data pump thread if running. A callback registered via `onEnd`
   * is left registered for a later session; use `offEnd()` to clear it.
   */
  offData(): void;

  /**
   * Remove the registered terminal-observability callback (see `onEnd`).
   * This is the only way to clear an `onEnd` registration — the pump leaves it
   * armed across sessions. After this, the pump's end is no longer reported to
   * JS (a fatal terminal still emits a throttled stderr log; a clean stop is
   * silent). Does not stop the pump.
   */
  offEnd(): void;

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
   * A windowed snapshot of the stream's recent backpressure.
   *
   * Unlike the all-or-nothing `isUnderBackpressure` flag — which trips only on a
   * run of *consecutive* drops and resets on any successful push — `dropRate` is
   * computed over a recent window of push activity, so a sustained partial loss
   * (e.g. a steady 1-in-3 drop pattern) is visible. The legacy bool is carried
   * through unchanged.
   *
   * Reading this never allocates on or blocks the OS audio callback thread.
   * Before `start()` (or after `stop()`) the snapshot is the all-zero default
   * with `windowSecs === 0`, `dropRate === 0.0`, and
   * `isUnderBackpressure === false`.
   *
   * @example
   * ```ts
   * const r = capture.backpressureReport();
   * if (r.dropRate > 0.01) {
   *   console.warn(`losing ${(r.dropRate * 100).toFixed(1)}% of buffers ` +
   *                `(pushed=${r.pushed} dropped=${r.dropped})`);
   * }
   * ```
   */
  backpressureReport(): BackpressureReport;

  /**
   * The negotiated *delivery* format the backend actually produces, or `null`
   * before `start()` creates a stream. May differ from the requested settings
   * when the device forced a negotiation.
   */
  readonly format: AudioFormat | null;
}

/**
 * Options accepted by {@link CompositionBuilder.create}.
 */
export interface CompositionBuilderOptions {
  /** Session sample rate in Hz (default 48000). Sources at a different rate are resampled. */
  sampleRate?: number | null;
  /** Saturating clamp of the summed output to [-1.0, 1.0] (default false). */
  clampOutput?: boolean | null;
  /** Composed tick (output buffer) duration in ms (default 10). */
  quantumMs?: number | null;
  /** How long to wait for the master source before a wall-clock fallback tick, in ms (default 250). */
  stallTimeoutMs?: number | null;
  /** Per-source buffering bound in ms (default 1000). */
  maxBufferMs?: number | null;
}

/**
 * A composition group: a named set of capture sources sharing a mixdown layout
 * (multi-source channel composition, ADR-0011).
 *
 * Build it up with `source`/`sourceWithGain` and a layout
 * (`mixdownMono`/`mixdownStereo`/`keepChannels`), then hand it to
 * {@link CompositionBuilder.addGroup}.
 *
 * @example
 * ```ts
 * const voice = new Group('voice');
 * voice.mixdownMono();
 * voice.source('name:Discord');
 * voice.sourceWithGain('name:Zoom', 0.8);
 * ```
 */
export declare class Group {
  /**
   * Create a group with the given name and the default stereo layout. The name
   * must be non-empty and unique within a composition (both enforced at
   * `CompositionBuilder.build()`).
   */
  constructor(name: string);
  /**
   * Add a capture source with unit gain (1.0). `spec` uses the canonical target
   * grammar (`"system"`, `"device:<id>"`, `"app:<id>"`, `"name:<n>"`,
   * `"tree:<pid>"`). Throws (`ERR_RSAC_CONFIGURATION`) on an invalid spec.
   */
  source(spec: string): void;
  /**
   * Add a capture source with an explicit linear mixdown gain (1.0 = unity).
   * The gain must be finite and >= 0; an invalid gain throws eagerly.
   */
  sourceWithGain(spec: string, gain: number): void;
  /** Fold every source in the group to mono and sum into one output channel. */
  mixdownMono(): void;
  /** Fold every source to stereo and sum into two output channels (the default). */
  mixdownStereo(): void;
  /**
   * Pass the group's single source through with its native channel count. A
   * keep-channels group must contain exactly one source (enforced at `build()`).
   */
  keepChannels(): void;
}

/**
 * Builder for a multi-source {@link Composition} (ADR-0011). Configure the
 * session knobs via {@link CompositionBuilder.create}, add groups, optionally
 * `preflight()`, then `build()`.
 */
export declare class CompositionBuilder {
  /** Create a composition builder with optional session settings. */
  static create(opts?: CompositionBuilderOptions | undefined | null): CompositionBuilder;
  /** Append a group (cloned into the builder). Groups contribute output channels in the order added. */
  addGroup(group: Group): void;
  /**
   * Run every device-independent validation `build()` performs, without
   * building. Throws (`ERR_RSAC_CONFIGURATION` / …) on an invalid configuration.
   */
  preflight(): void;
  /**
   * Validate and build a (not-yet-started) {@link Composition}. No devices are
   * touched here — the inner captures are created and started by
   * `Composition.start()`. Throws on an invalid configuration.
   */
  build(): Composition;
}

/**
 * A multi-source composed capture session (ADR-0011). Created via
 * {@link CompositionBuilder.build}; inert until `start()`.
 *
 * Mixes each group down according to its layout and appends the groups'
 * channels into one interleaved stream. The blocking readers
 * (`readBlocking`/`readBlockingAsync`) and the `onData` pump are
 * terminal-observable (they end on the fatal terminal, reported via `onEnd`).
 *
 * An explicit `stop()` discards any buffered composed tail; read until the
 * terminal error before stopping to capture everything (the composition's
 * natural end — all sources ended — drains the tail first).
 *
 * @example
 * ```ts
 * const g = new Group('main');
 * g.source('system');
 * const comp = CompositionBuilder.create({ sampleRate: 48000 });
 * comp.addGroup(g);
 * const c = comp.build();
 * c.onData((chunk) => console.log(chunk.channels));
 * c.start();
 * // ... later ...
 * c.stop();
 * ```
 */
export declare class Composition {
  /**
   * Start the composition (build + start one capture per source, resolve the
   * composed layout, spawn the compositor thread). If an `onData` callback is
   * registered, a background pump thread is spawned. Starting twice is a no-op.
   */
  start(): void;
  /**
   * Stop the composition: signal the ring + engine (waking any parked reader)
   * and join the compositor thread. Discards the buffered composed tail.
   */
  stop(): void;
  /** Whether the composed stream is currently running. */
  readonly isRunning: boolean;
  /**
   * Read the next composed buffer (non-blocking). Returns `null` if no data is
   * available yet. **Terminal-observable**: throws the fatal terminal error
   * once the composition ends and drains.
   */
  read(): AudioChunk | null;
  /**
   * Read the next composed buffer, blocking until data is available.
   * **Terminal-observable**. WARNING: blocks the Node.js event loop — prefer
   * `readBlockingAsync()` or `onData()`.
   */
  readBlocking(): AudioChunk;
  /**
   * Read the next composed buffer asynchronously (non-blocking, off the main
   * thread). Returns `null` if no data is available yet.
   */
  readAsync(): Promise<AudioChunk | null>;
  /**
   * Read the next composed buffer asynchronously, blocking the worker thread
   * until data is available (does not block the event loop).
   * **Terminal-observable**.
   */
  readBlockingAsync(): Promise<AudioChunk>;
  /**
   * Register a callback for push-based composed-audio delivery. Only one
   * callback is active at a time; calling again replaces it. If the composition
   * is running the pump starts immediately, otherwise on `start()`.
   */
  onData(callback: (chunk: AudioChunk) => void): void;
  /**
   * Register a callback that fires exactly once when push-based delivery ends,
   * carrying *why* it ended: the formatted terminal error message (a non-null
   * `string`) on a fatal terminal, or `null` on a clean stop. Parity with the
   * `AudioCapture` `onEnd`; persists across sessions and is cleared only by
   * `offEnd()`.
   */
  onEnd(callback: (error: string | null) => void): void;
  /**
   * Remove the registered data callback and stop the pump. An `onEnd` callback
   * is left registered for a later session; use `offEnd()` to clear it.
   */
  offData(): void;
  /** Remove the registered terminal-observability callback (see `onEnd`). */
  offEnd(): void;
  /**
   * Number of composed-ring overruns (composed buffers dropped because the
   * consumer read slower than the compositor produced). `0` before start. This
   * counts loss at the composed ring only; per-source upstream loss is in
   * `sourceStats()`.
   */
  readonly overrunCount: number;
  /** Number of composed output channels (`0` before a successful start). */
  readonly channelCount: number;
  /**
   * Name of the group producing composed output channel `channel` (0-based), or
   * `null` if the composition is not started or `channel` is out of bounds.
   */
  channelGroup(channel: number): string | null;
  /**
   * Index of composed output channel `channel` within its group (0-based; e.g.
   * 0 = L, 1 = R for a stereo group), or `null` if not started or out of bounds.
   */
  channelInGroup(channel: number): number | null;
  /** Point-in-time composition counters, or `null` if not started. */
  stats(): CompositionStats | null;
  /**
   * Per-source counters for the source at `index` (flat declaration order), or
   * `null` if not started or `index` is out of bounds.
   */
  sourceStats(index: number): SourceStats | null;
  /**
   * Total composed buffers dropped by this composition's subscribe pumps
   * because a subscriber's bounded channel was full. `0` before start.
   */
  subscriberDroppedCount(): bigint;
}

/**
 * A point-in-time snapshot of a running {@link Composition}'s counters.
 *
 * Counters are `bigint` (Rust `u64`) so they do not silently lose precision past
 * `Number.MAX_SAFE_INTEGER` on a long-running session.
 */
export interface CompositionStats {
  /** Composed buffers (ticks) emitted so far. */
  ticks: bigint;
  /** Ticks emitted by the wall-clock stall fallback (the master had no data). */
  fallbackTicks: bigint;
  /** Number of composed sources, in flat declaration order. */
  numSources: bigint;
}

/**
 * A point-in-time snapshot of one composed source's counters. Exposes the full
 * Rust `SourceStats` set — including `gapPaddedFrames` / `innerDropped`, which
 * the C FFI struct omits. u64 counters are `bigint`.
 */
export interface SourceStats {
  /** Name of the group the source belongs to. */
  group: string;
  /** The source's capture target in canonical grammar (e.g. `"system"`). */
  target: string;
  /** Buffers received from the inner capture so far. */
  buffersReceived: bigint;
  /** Frames of silence inserted because the source was behind at tick time. */
  paddedFrames: bigint;
  /** Frames trimmed because the source drifted past the buffering bound. */
  trimmedFrames: bigint;
  /** Frames of silence inserted to compensate intra-source timestamp gaps. */
  gapPaddedFrames: bigint;
  /** Ring-overflow drops inside the source's own capture (loss upstream of the compositor). */
  innerDropped: bigint;
  /** Whether this source is being resampled to the session rate. */
  resampling: boolean;
  /** Whether the source's stream has ended. */
  ended: boolean;
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
 * A windowed snapshot of an {@link AudioCapture}'s recent backpressure.
 *
 * Unlike the all-or-nothing {@link isUnderBackpressure} flag — which trips only
 * on a run of *consecutive* drops and resets on any successful push —
 * {@link dropRate} is computed over a recent window of push activity, so a
 * sustained partial loss (e.g. a steady 1-in-3 drop pattern) is visible.
 *
 * The `pushed`/`dropped` tallies are `bigint` (Rust `u64`) so they do not
 * silently lose precision past `Number.MAX_SAFE_INTEGER` on a long-running
 * capture.
 */
export interface BackpressureReport {
  /** The wall-clock span the `pushed`/`dropped` tallies cover, in seconds. `0`
   * when the span cannot be attributed (no stream / not yet negotiated). */
  windowSecs: number;
  /** Buffers successfully pushed by the producer within the window. */
  pushed: bigint;
  /** Buffers dropped due to ring-buffer overflow within the window. */
  dropped: bigint;
  /** Fraction of buffers lost within the window, in `0.0..=1.0`
   * (`dropped / (pushed + dropped)`; `0.0` when nothing has been pushed or
   * dropped). Surfaces sustained partial loss the legacy bool misses. */
  dropRate: number;
  /** The legacy consecutive-drop backpressure flag, carried unchanged: trips
   * only on a run of consecutive drops and resets on any successful push. */
  isUnderBackpressure: boolean;
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
  /**
   * Whether the backend delivers device hot-plug / default-change
   * notifications.
   */
  supportsDeviceChangeNotifications: boolean;
  /**
   * True when starting a capture requires a config-time user-consent
   * artifact (mobile platforms; see docs/MOBILE_BACKEND_DESIGN.md);
   * false on all desktop backends.
   */
  requiresUserConsent: boolean;
  /** Maximum number of channels supported. */
  maxChannels: number;
  /** Minimum supported sample rate in Hz. */
  minSampleRate: number;
  /** Maximum supported sample rate in Hz. */
  maxSampleRate: number;
  /** Supported sample formats (short names, e.g. "I16", "F32"). */
  supportedSampleFormats: Array<string>;
  /**
   * The config-time sample-rate whitelist the capture constructor accepts —
   * identical on every platform and intentionally narrower than the
   * device-negotiable min/max sample-rate range.
   */
  supportedSampleRates: Array<number>;
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
