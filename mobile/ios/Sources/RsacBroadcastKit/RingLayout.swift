import Foundation

// ═══════════════════════════════════════════════════════════════════════════
// RingLayout.swift — THE CANONICAL cross-process mmap SPSC ring contract.
//
//   ┌─────────────────────────────────────────────────────────────────────┐
//   │  ⚠️  THIS FILE IS THE CONTRACT.                                      │
//   │                                                                     │
//   │  The Rust consumer in src/audio/ios/broadcast.rs (seed rsac-b3aa)   │
//   │  MUST mirror this layout EXACTLY — every offset, every ordering,    │
//   │  every constant. Any change here is a breaking cross-process ABI    │
//   │  change: bump `layoutVersion`, and make BOTH sides reject a         │
//   │  mismatched version. Never change the layout without a version      │
//   │  bump.                                                              │
//   └─────────────────────────────────────────────────────────────────────┘
//
// One producer (the ReplayKit Broadcast Upload Extension), one consumer (the
// host app's rsac Rust backend). The ring is a fixed-size file in the shared
// App Group container, memory-mapped by both processes.
//
// ── File layout (all multi-byte fields LITTLE-ENDIAN) ─────────────────────
//
//   offset  size  field               type          notes
//   ──────  ────  ─────────────────   ───────────   ─────────────────────────
//        0     4  magic               u32           ASCII "RSAC" (bytes
//                                                   52 53 41 43); read as a
//                                                   little-endian u32 this is
//                                                   0x4341_5352
//        4     4  layoutVersion       u32           = 1
//        8     4  sampleRate          u32           frames per second (Hz)
//       12     4  channels            u32           interleaved channel count
//       16     4  capacityFrames      u32           ring capacity in FRAMES
//       20     4  reserved            u32           = 0 in v1; consumer must
//                                                   ignore
//       24     8  writeCursor         atomic u64    monotonic frame count,
//                                                   producer-owned
//       32     8  readCursor          atomic u64    monotonic frame count,
//                                                   consumer-owned
//       40     8  heartbeatMillis     atomic u64    producer liveness stamp
//       48     8  producerDropCount   atomic u64    buffers dropped ring-full
//       56     8  (padding)           —             = 0; aligns data to 64
//       64     …  data                f32[]         interleaved little-endian
//                                                   IEEE-754 f32 frames;
//                                                   region length =
//                                                   capacityFrames * channels
//                                                   * 4 bytes
//
// Header size is 56 bytes; the data region starts at byte 64 so it is
// 64-byte (cache-line) aligned. Total file size =
// 64 + capacityFrames * channels * 4.
//
// ── Cursor semantics ───────────────────────────────────────────────────────
//
// Cursors are MONOTONIC frame counts, never wrapped: the slot of cursor `c`
// is `c % capacityFrames`. Fill level = writeCursor − readCursor (always
// ≤ capacityFrames). u64 at 48 kHz overflows after ~12 million years — wrap
// is a non-concern.
//
//   producer:  free = capacityFrames − (writeCursor − readCursor)
//              if frames > free → drop the WHOLE buffer, producerDropCount+1
//              (drop-not-block: the sample handler must never stall)
//              else: copy frames (two segments on wrap), then
//              RELEASE-store writeCursor += frames
//   consumer:  ACQUIRE-load writeCursor; frames in [readCursor, writeCursor)
//              are safe to read; after copying out, RELEASE-store
//              readCursor += consumed
//
// ── Torn-read defense ──────────────────────────────────────────────────────
//
// The release-store of writeCursor happens-after the frame bytes are written;
// the consumer's acquire-load of writeCursor therefore observes fully-written
// frames — never torn f32 data. Symmetrically, the producer acquire-loads
// readCursor before reusing slots. Non-atomic header fields (sampleRate,
// channels, capacityFrames) are written once by the producer BEFORE the
// publish point and are immutable afterwards. The publish point: the producer
// zero-fills the file, writes all non-atomic fields and initial atomic values
// while magic is still 0, then commits magic+layoutVersion as a SINGLE
// release-store of the u64 at offset 0 (little-endian
// (layoutVersion << 32) | magic). The consumer acquire-loads offset 0 and
// only trusts the header once it equals that committed value.
//
// ── Heartbeat ──────────────────────────────────────────────────────────────
//
// heartbeatMillis is milliseconds derived from CLOCK_MONOTONIC
// (clock_gettime_nsec_np(CLOCK_MONOTONIC) / 1_000_000). CLOCK_MONOTONIC is
// shared by all processes on the device within one boot, so the Rust
// consumer compares against its own CLOCK_MONOTONIC reading. The producer
// stamps at least every `heartbeatIntervalMillis`; a stamp older than
// `heartbeatTimeoutMillis` means the extension was killed or the broadcast
// ended without a Darwin notification ⇒ producer terminal signal (ADR-0010)
// ⇒ the host stream ends with the fatal terminal (ADR-0003).
//
// ── Sizing guidance ────────────────────────────────────────────────────────
//
// The broadcast extension is hard-capped at ~50 MB of memory. The ring must
// stay in the low single-digit MB: 2 seconds of 48 kHz stereo f32 is
// 48_000 * 2 * 2 * 4 = 768 KiB — the default. Do not exceed a few seconds;
// ring-full is handled by drop + producerDropCount, never by growing.
// ═══════════════════════════════════════════════════════════════════════════

/// Canonical constants of the rsac cross-process broadcast ring (layout v1).
///
/// Mirrored byte-for-byte by the Rust consumer (rsac-b3aa). Version-gate any
/// change: bump ``layoutVersion`` and update both sides in lockstep.
public enum RsacRingLayout {
    // ── Identity ──────────────────────────────────────────────────────────

    /// ASCII "RSAC" read as a little-endian u32 (bytes `52 53 41 43`).
    public static let magic: UInt32 = 0x4341_5352

    /// Layout version this file describes. Bump on ANY layout change.
    public static let layoutVersion: UInt32 = 1

    /// The u64 committed at offset 0 as the header publish point:
    /// little-endian `(layoutVersion << 32) | magic`.
    public static let publishedMagicVersion: UInt64 =
        (UInt64(layoutVersion) << 32) | UInt64(magic)

    /// Default file name of the ring inside the App Group container. Part of
    /// the contract: the Rust consumer opens the same name.
    public static let ringFileName = "rsac_broadcast_ring_v1"

    // ── Header field offsets (bytes) ──────────────────────────────────────

    /// Offset of `magic` (u32).
    public static let offsetMagic = 0
    /// Offset of `layoutVersion` (u32).
    public static let offsetLayoutVersion = 4
    /// Offset of `sampleRate` (u32).
    public static let offsetSampleRate = 8
    /// Offset of `channels` (u32).
    public static let offsetChannels = 12
    /// Offset of `capacityFrames` (u32).
    public static let offsetCapacityFrames = 16
    /// Offset of `reserved` (u32, = 0 in v1).
    public static let offsetReserved = 20
    /// Offset of `writeCursor` (atomic u64, producer-owned, monotonic frames).
    public static let offsetWriteCursor = 24
    /// Offset of `readCursor` (atomic u64, consumer-owned, monotonic frames).
    public static let offsetReadCursor = 32
    /// Offset of `heartbeatMillis` (atomic u64, CLOCK_MONOTONIC ms).
    public static let offsetHeartbeatMillis = 40
    /// Offset of `producerDropCount` (atomic u64).
    public static let offsetProducerDropCount = 48

    /// Header size in bytes (fields only, excluding pad-to-data).
    public static let headerSize = 56

    /// Byte offset where interleaved f32 frame data begins (64-byte aligned).
    public static let dataOffset = 64

    // ── Heartbeat contract ────────────────────────────────────────────────

    /// The producer stamps `heartbeatMillis` at least this often.
    public static let heartbeatIntervalMillis: UInt64 = 500

    /// A stamp older than this ⇒ the consumer treats the producer as dead
    /// (terminal, ADR-0010/ADR-0003).
    public static let heartbeatTimeoutMillis: UInt64 = 2_000

    // ── Sizing helpers ────────────────────────────────────────────────────

    /// Total ring file size in bytes for a given geometry.
    public static func totalFileSize(capacityFrames: UInt32, channels: UInt32) -> Int {
        dataOffset + Int(capacityFrames) * Int(channels) * MemoryLayout<Float32>.size
    }

    /// Default capacity: ~2 seconds of audio at the given rate (768 KiB at
    /// 48 kHz stereo — far below the ~50 MB extension cap).
    public static func defaultCapacityFrames(sampleRate: UInt32) -> UInt32 {
        sampleRate * 2
    }

    /// Current CLOCK_MONOTONIC time in milliseconds — the clock domain of
    /// `heartbeatMillis`. The Rust consumer uses
    /// `libc::clock_gettime(CLOCK_MONOTONIC)` for the same domain.
    public static func monotonicNowMillis() -> UInt64 {
        clock_gettime_nsec_np(CLOCK_MONOTONIC) / 1_000_000
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Darwin notification names — the signaling half of the contract.
//
// Posted on CFNotificationCenterGetDarwinNotifyCenter() by the extension;
// observed by the host app / Rust consumer (rsac-b3aa listens for the same
// strings). Darwin notifications carry NO payload — state lives in the ring
// header. Defined here, in one place, so the producer template and any
// future Swift consumer share them; the Rust side mirrors the literals.
// ═══════════════════════════════════════════════════════════════════════════

/// Darwin notification names posted by the broadcast extension.
public enum RsacDarwinNotification {
    /// Broadcast started; the ring file exists and its header is published.
    public static let started = "ai.codeseys.rsac.broadcast.started"
    /// Broadcast paused by the user (heartbeat continues while paused).
    public static let paused = "ai.codeseys.rsac.broadcast.paused"
    /// Broadcast resumed.
    public static let resumed = "ai.codeseys.rsac.broadcast.resumed"
    /// Broadcast finished — producer terminal signal (ADR-0010): the host
    /// stream must end with the fatal terminal (ADR-0003).
    public static let finished = "ai.codeseys.rsac.broadcast.finished"
}
