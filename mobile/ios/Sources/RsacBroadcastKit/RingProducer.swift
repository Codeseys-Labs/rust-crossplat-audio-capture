import Foundation
import CRsacRingAtomics

/// Producer side of the rsac cross-process broadcast ring (see
/// `RingLayout.swift` — THE canonical contract; the Rust consumer in
/// rsac-b3aa mirrors it exactly).
///
/// Lives inside the ReplayKit Broadcast Upload Extension. Creates (or
/// truncates) the ring file in the shared App Group container, memory-maps
/// it, publishes the header, and writes interleaved f32 frames with
/// **drop-not-block** semantics: if the consumer is slow and the ring fills,
/// whole buffers are dropped and `producerDropCount` is incremented — the
/// sample handler thread is never stalled (the extension budget and
/// ReplayKit's delivery cadence both forbid blocking).
///
/// Single-producer: exactly one `RsacRingProducer` may exist per ring file.
/// All `write`/`stampHeartbeat` calls must come from one thread at a time
/// (ReplayKit delivers sample buffers serially; the heartbeat timer in
/// `SampleHandlerTemplate` stamps via the atomic store, which is safe from
/// any thread).
public final class RsacRingProducer {

    /// Errors surfaced during ring creation.
    public enum ProducerError: Error, CustomStringConvertible {
        /// `containerURL(forSecurityApplicationGroupIdentifier:)` returned
        /// nil — the App Group entitlement is missing or the identifier is
        /// wrong (must be present in BOTH the host app and the extension).
        case appGroupUnavailable(String)
        /// open(2)/ftruncate(2) on the ring file failed (errno attached).
        case fileCreationFailed(String, errno: Int32)
        /// mmap(2) failed (errno attached).
        case mmapFailed(errno: Int32)
        /// Zero channels / zero capacity / zero sample rate.
        case invalidConfiguration(String)

        public var description: String {
            switch self {
            case let .appGroupUnavailable(group):
                return "App Group container unavailable for '\(group)' — check the "
                    + "com.apple.security.application-groups entitlement on the extension"
            case let .fileCreationFailed(path, errno):
                return "failed to create/size ring file at \(path) (errno \(errno))"
            case let .mmapFailed(errno):
                return "mmap of ring file failed (errno \(errno))"
            case let .invalidConfiguration(why):
                return "invalid ring configuration: \(why)"
            }
        }
    }

    /// Delivered sample rate (Hz), as published in the header.
    public let sampleRate: UInt32
    /// Interleaved channel count, as published in the header.
    public let channels: UInt32
    /// Ring capacity in frames, as published in the header.
    public let capacityFrames: UInt64

    private let base: UnsafeMutableRawPointer
    private let fileSize: Int
    private let fd: Int32
    /// Start of the interleaved f32 data region (base + dataOffset).
    private let data: UnsafeMutablePointer<Float32>

    // Atomic header fields (8-byte aligned by RingLayout construction).
    private let publishPtr: UnsafeMutablePointer<UInt64>       // offset 0
    private let writeCursorPtr: UnsafeMutablePointer<UInt64>   // offset 24
    private let readCursorPtr: UnsafeMutablePointer<UInt64>    // offset 32
    private let heartbeatPtr: UnsafeMutablePointer<UInt64>     // offset 40
    private let dropCountPtr: UnsafeMutablePointer<UInt64>     // offset 48

    /// Creates the ring file in the App Group container, maps it, and
    /// publishes the v1 header. Any pre-existing file with the same name is
    /// re-initialized from scratch (a new broadcast supersedes a dead one).
    ///
    /// - Parameters:
    ///   - appGroupIdentifier: e.g. `"group.com.example.myapp.rsac"`. Must be
    ///     an App Group both the host app and the extension are entitled to.
    ///   - sampleRate: delivered rate in Hz (from the first CMSampleBuffer).
    ///   - channels: interleaved channel count.
    ///   - capacityFrames: ring depth in frames; default ≈ 2 s
    ///     (`RsacRingLayout.defaultCapacityFrames`). Keep single-digit MB —
    ///     the extension is capped at ~50 MB total.
    ///   - fileName: defaults to the contract name `RsacRingLayout.ringFileName`.
    public init(
        appGroupIdentifier: String,
        sampleRate: UInt32,
        channels: UInt32,
        capacityFrames: UInt32? = nil,
        fileName: String = RsacRingLayout.ringFileName
    ) throws {
        guard sampleRate > 0, channels > 0 else {
            throw ProducerError.invalidConfiguration(
                "sampleRate=\(sampleRate) channels=\(channels)")
        }
        let capacity = capacityFrames ?? RsacRingLayout.defaultCapacityFrames(sampleRate: sampleRate)
        guard capacity > 0 else {
            throw ProducerError.invalidConfiguration("capacityFrames == 0")
        }

        guard let container = FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: appGroupIdentifier)
        else {
            throw ProducerError.appGroupUnavailable(appGroupIdentifier)
        }
        let url = container.appendingPathComponent(fileName, isDirectory: false)
        let path = url.path

        let size = RsacRingLayout.totalFileSize(capacityFrames: capacity, channels: channels)

        // open + size the backing file. O_TRUNC deliberately NOT used: we
        // zero the header through the mapping instead, so a live consumer
        // mapping the old generation never sees the file shrink under it.
        let fd = open(path, O_CREAT | O_RDWR, 0o644)
        guard fd >= 0 else {
            throw ProducerError.fileCreationFailed(path, errno: errno)
        }
        guard ftruncate(fd, off_t(size)) == 0 else {
            let e = errno
            Darwin.close(fd) // bare close() resolves to RingProducer.close()
            throw ProducerError.fileCreationFailed(path, errno: e)
        }

        guard let mapped = mmap(nil, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0),
              mapped != MAP_FAILED
        else {
            let e = errno
            Darwin.close(fd) // bare close() resolves to RingProducer.close()
            throw ProducerError.mmapFailed(errno: e)
        }

        self.fd = fd
        self.fileSize = size
        self.base = mapped
        self.sampleRate = sampleRate
        self.channels = channels
        self.capacityFrames = UInt64(capacity)
        self.data = mapped
            .advanced(by: RsacRingLayout.dataOffset)
            .assumingMemoryBound(to: Float32.self)
        self.publishPtr = mapped.assumingMemoryBound(to: UInt64.self)
        self.writeCursorPtr = mapped
            .advanced(by: RsacRingLayout.offsetWriteCursor)
            .assumingMemoryBound(to: UInt64.self)
        self.readCursorPtr = mapped
            .advanced(by: RsacRingLayout.offsetReadCursor)
            .assumingMemoryBound(to: UInt64.self)
        self.heartbeatPtr = mapped
            .advanced(by: RsacRingLayout.offsetHeartbeatMillis)
            .assumingMemoryBound(to: UInt64.self)
        self.dropCountPtr = mapped
            .advanced(by: RsacRingLayout.offsetProducerDropCount)
            .assumingMemoryBound(to: UInt64.self)

        publishHeader(capacity: capacity)
    }

    /// Header publish protocol (see RingLayout "Torn-read defense"):
    /// 1. zero offset 0 (un-publishes any previous generation),
    /// 2. write all non-atomic fields + initial atomic values,
    /// 3. RELEASE-store (layoutVersion << 32) | magic at offset 0.
    private func publishHeader(capacity: UInt32) {
        rsac_atomic_store_u64_relaxed(publishPtr, 0)

        // Non-atomic u32 fields, written little-endian. iOS targets are
        // little-endian (arm64), so a plain store IS the little-endian
        // representation; `littleEndian` makes the intent explicit and keeps
        // this correct even on a hypothetical BE host.
        func storeU32(_ value: UInt32, at offset: Int) {
            base.advanced(by: offset)
                .assumingMemoryBound(to: UInt32.self)
                .pointee = value.littleEndian
        }
        storeU32(sampleRate, at: RsacRingLayout.offsetSampleRate)
        storeU32(channels, at: RsacRingLayout.offsetChannels)
        storeU32(capacity, at: RsacRingLayout.offsetCapacityFrames)
        storeU32(0, at: RsacRingLayout.offsetReserved)
        // Pad bytes 56..64 (zeroed).
        base.advanced(by: RsacRingLayout.headerSize)
            .assumingMemoryBound(to: UInt64.self)
            .pointee = 0

        rsac_atomic_store_u64_relaxed(writeCursorPtr, 0)
        rsac_atomic_store_u64_relaxed(readCursorPtr, 0)
        rsac_atomic_store_u64_relaxed(dropCountPtr, 0)
        rsac_atomic_store_u64_relaxed(heartbeatPtr, RsacRingLayout.monotonicNowMillis())

        // Publish point.
        rsac_atomic_store_u64_release(publishPtr, RsacRingLayout.publishedMagicVersion)
    }

    // ── Write path (drop-not-block) ───────────────────────────────────────

    /// Writes `frameCount` interleaved f32 frames (`frameCount * channels`
    /// samples) into the ring.
    ///
    /// All-or-nothing: if the ring lacks space for the WHOLE buffer, nothing
    /// is written, `producerDropCount` is incremented by 1 (one drop = one
    /// buffer, matching the desktop bridge's overrun accounting), and `false`
    /// is returned. Never blocks.
    @discardableResult
    public func write(interleavedSamples samples: UnsafePointer<Float32>, frameCount: Int) -> Bool {
        guard frameCount > 0 else { return true }

        // Producer owns writeCursor (relaxed); readCursor needs acquire so
        // the consumer's release-store happens-before we reuse its slots.
        let write = rsac_atomic_load_u64_relaxed(writeCursorPtr)
        let read = rsac_atomic_load_u64_acquire(readCursorPtr)
        let free = capacityFrames - (write - read)
        guard UInt64(frameCount) <= free else {
            _ = rsac_atomic_fetch_add_u64_relaxed(dropCountPtr, 1)
            return false
        }

        let ch = Int(channels)
        let startSlot = Int(write % capacityFrames)
        let firstFrames = min(frameCount, Int(capacityFrames) - startSlot)
        let firstSamples = firstFrames * ch

        // Segment 1: up to the physical end of the ring.
        memcpy(data.advanced(by: startSlot * ch), samples, firstSamples * 4)
        // Segment 2 (wraparound): remainder at the physical start.
        if firstFrames < frameCount {
            let restFrames = frameCount - firstFrames
            memcpy(data, samples.advanced(by: firstSamples), restFrames * ch * 4)
        }

        // Release: frame bytes above happen-before the cursor becomes
        // visible to the consumer's acquire-load (torn-read defense).
        rsac_atomic_store_u64_release(writeCursorPtr, write + UInt64(frameCount))
        return true
    }

    /// Buffers dropped so far because the ring was full.
    public var dropCount: UInt64 {
        rsac_atomic_load_u64_relaxed(dropCountPtr)
    }

    // ── Heartbeat ─────────────────────────────────────────────────────────

    /// Stamps `heartbeatMillis` with the current CLOCK_MONOTONIC time.
    /// Atomic; safe to call from the heartbeat timer's queue while the
    /// sample-handler thread is writing.
    public func stampHeartbeat() {
        rsac_atomic_store_u64_relaxed(heartbeatPtr, RsacRingLayout.monotonicNowMillis())
    }

    // ── Teardown ──────────────────────────────────────────────────────────

    /// Stamps a final heartbeat and unmaps/closes the ring. The FILE is
    /// deliberately left in place so the consumer can drain any remaining
    /// frames after teardown. Per the reconciled liveness contract (rsac-7e0a),
    /// the *absence of heartbeats* (staleness past `heartbeatTimeoutMillis`) is
    /// the sole terminal signal the Rust consumer observes — it never watches
    /// the `finished` Darwin notification (that stays a Swift-side advisory).
    /// Neither file deletion nor a notification is part of the terminal signal.
    public func close() {
        stampHeartbeat()
        munmap(base, fileSize)
        Darwin.close(fd)
    }

    deinit {
        // Idempotence: munmap/close on already-closed resources is avoided by
        // convention — SampleHandlerTemplate calls close() exactly once, in
        // broadcastFinished. If the extension is killed hard, the OS reclaims
        // the mapping and fd anyway.
    }
}
