import ReplayKit
import CoreMedia
import AudioToolbox
import Foundation

/// Open template for the consumer app's Broadcast Upload Extension.
///
/// Subclass this in the extension target, override ``appGroupIdentifier``,
/// and set the subclass as the extension's principal
/// `RPBroadcastSampleHandler` (`NSExtensionPrincipalClass`). Everything else
/// — ring creation, f32 conversion, drop-not-block writes, Darwin
/// notifications, heartbeat — is handled here.
///
/// ```swift
/// // In the extension target:
/// class SampleHandler: RsacBroadcastSampleHandler {
///     override var appGroupIdentifier: String { "group.com.example.myapp.rsac" }
/// }
/// ```
///
/// Data path: `processSampleBuffer(.audioApp)` → `CMSampleBuffer` →
/// `AudioBufferList` → interleaved f32 → ``RsacRingProducer/write``. The ring
/// contract is `RingLayout.swift` (canonical; mirrored by the Rust consumer,
/// rsac-b3aa). `.audioMic` and `.video` buffers are ignored: the mic path is
/// the host app's `AVAudioEngine` slice (`src/audio/ios/`), and rsac never
/// touches video.
open class RsacBroadcastSampleHandler: RPBroadcastSampleHandler {

    // ── Subclass surface ──────────────────────────────────────────────────

    /// REQUIRED override: the App Group shared by the host app and this
    /// extension (both targets need the `com.apple.security.application-groups`
    /// entitlement listing it).
    open var appGroupIdentifier: String {
        // Overriding is mandatory; the base class cannot know your group.
        // Returning "" fails fast in broadcastStarted with a user-facing error.
        ""
    }

    /// Ring depth in seconds (default 2.0 — 768 KiB at 48 kHz stereo, far
    /// below the ~50 MB extension memory cap). Override to tune.
    open var ringSeconds: Double { 2.0 }

    // ── State (extension-side only) ───────────────────────────────────────

    private let lock = NSLock()
    private var producer: RsacRingProducer?
    private var heartbeatTimer: DispatchSourceTimer?
    /// Grow-only scratch for format conversion (reused across buffers; the
    /// sample-handler path allocates only when a larger buffer arrives).
    private var scratch: [Float32] = []
    /// Reusable AudioBufferList storage, sized for up to 8 planar channel
    /// buffers — allocated once, freed on deinit.
    private static let ablMaxBuffers = 8
    private static let ablBytes = MemoryLayout<AudioBufferList>.size
        + (ablMaxBuffers - 1) * MemoryLayout<AudioBuffer>.size
    private lazy var ablRaw = UnsafeMutableRawPointer.allocate(
        byteCount: Self.ablBytes,
        alignment: MemoryLayout<AudioBufferList>.alignment)

    deinit {
        ablRaw.deallocate()
    }
    /// One-shot warning latch for unsupported sample formats.
    private var warnedUnsupportedFormat = false

    // ── RPBroadcastSampleHandler lifecycle ────────────────────────────────

    override open func broadcastStarted(withSetupInfo setupInfo: [String: NSObject]?) {
        guard !appGroupIdentifier.isEmpty else {
            finishBroadcastWithError(NSError(
                domain: "ai.codeseys.rsac",
                code: 1,
                userInfo: [NSLocalizedDescriptionKey:
                    "RsacBroadcastSampleHandler subclass must override appGroupIdentifier"]))
            return
        }
        // The ring is created lazily on the FIRST .audioApp buffer — only
        // then are the delivered sample rate and channel count known (from
        // the CMSampleBuffer's ASBD). Until then only the heartbeat concept
        // exists; the `started` notification tells the host to begin
        // watching for the ring header publish.
        postDarwinNotification(RsacDarwinNotification.started)
    }

    override open func broadcastPaused() {
        // Heartbeat continues while paused (the extension is alive; the host
        // must not declare it dead) — see RingLayout heartbeat contract.
        postDarwinNotification(RsacDarwinNotification.paused)
    }

    override open func broadcastResumed() {
        postDarwinNotification(RsacDarwinNotification.resumed)
    }

    override open func broadcastFinished() {
        lock.lock()
        let timer = heartbeatTimer
        heartbeatTimer = nil
        // `producer` is created iff the heartbeat timer is (see
        // ensureProducer). The producer's final heartbeat + unmap runs in the
        // timer's cancel handler (startHeartbeatTimer), NOT inline here:
        // DispatchSource runs the cancel handler after any in-flight event
        // handler completes and guarantees no further event handler fires, so
        // close()'s munmap can never race a heartbeat tick that would touch
        // the ring after the mapping is gone (finding 9). Dropping the
        // property reference is safe — the cancel handler retains the
        // producer until it runs.
        producer = nil
        lock.unlock()

        if let timer = timer {
            timer.cancel()
        }
        // The file stays mapped-and-drainable until the cancel handler's
        // close(); the consumer's terminal signal is heartbeat staleness, so
        // this advisory notification's ordering relative to close() is
        // immaterial (see RingProducer.close()).
        postDarwinNotification(RsacDarwinNotification.finished)
    }

    override open func processSampleBuffer(
        _ sampleBuffer: CMSampleBuffer,
        with sampleBufferType: RPSampleBufferType
    ) {
        switch sampleBufferType {
        case .audioApp:
            handleAppAudio(sampleBuffer)
        case .audioMic, .video:
            break // mic = host-app AVAudioEngine slice; video = out of scope
        @unknown default:
            break
        }
    }

    // ── App-audio path ────────────────────────────────────────────────────

    private func handleAppAudio(_ sampleBuffer: CMSampleBuffer) {
        guard CMSampleBufferDataIsReady(sampleBuffer),
              let formatDesc = CMSampleBufferGetFormatDescription(sampleBuffer),
              let asbdPtr = CMAudioFormatDescriptionGetStreamBasicDescription(formatDesc)
        else { return }
        let asbd = asbdPtr.pointee
        let frameCount = CMSampleBufferGetNumSamples(sampleBuffer)
        guard frameCount > 0, asbd.mChannelsPerFrame > 0 else { return }

        let p: RsacRingProducer
        do {
            p = try ensureProducer(asbd: asbd)
        } catch {
            finishBroadcastWithError(NSError(
                domain: "ai.codeseys.rsac",
                code: 2,
                userInfo: [NSLocalizedDescriptionKey:
                    "rsac broadcast ring setup failed: \(error)"]))
            return
        }

        // Extract the AudioBufferList backed by a retained block buffer,
        // into the reusable instance storage (no per-buffer allocation).
        let ablPtr = ablRaw.assumingMemoryBound(to: AudioBufferList.self)

        var blockBuffer: CMBlockBuffer?
        // CI-VERIFY: exact Swift signature/argument labels of
        // CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer on the iOS 14 SDK.
        let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            sampleBuffer,
            bufferListSizeNeededOut: nil,
            bufferListOut: ablPtr,
            bufferListSize: Self.ablBytes,
            blockBufferAllocator: kCFAllocatorDefault,
            blockBufferMemoryAllocator: kCFAllocatorDefault,
            flags: kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment,
            blockBufferOut: &blockBuffer)
        guard status == noErr, blockBuffer != nil else { return }

        let abl = UnsafeMutableAudioBufferListPointer(ablPtr)
        convertAndWrite(abl: abl, asbd: asbd, frameCount: frameCount, producer: p)
    }

    /// Converts whatever LPCM layout ReplayKit delivered into interleaved
    /// f32 and writes it to the ring. Handles the common cases: f32/i16/i32,
    /// interleaved and non-interleaved. Anything else is dropped (counted
    /// once in the log, never crashes the broadcast).
    ///
    /// CI-VERIFY: on-device, ReplayKit .audioApp is typically 44.1 kHz stereo
    /// signed 16-bit interleaved LPCM — confirm and, if it is the ONLY format
    /// ever delivered, the f32/i32 branches are just safety margin.
    private func convertAndWrite(
        abl: UnsafeMutableAudioBufferListPointer,
        asbd: AudioStreamBasicDescription,
        frameCount: Int,
        producer p: RsacRingProducer
    ) {
        let channels = Int(asbd.mChannelsPerFrame)
        let isFloat = asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0
        let isSignedInt = asbd.mFormatFlags & kAudioFormatFlagIsSignedInteger != 0
        let isNonInterleaved = asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0
        let bits = Int(asbd.mBitsPerChannel)

        // Fast path: interleaved f32 — write straight from the block buffer.
        if isFloat, bits == 32, !isNonInterleaved, let buf = abl.first,
           let raw = buf.mData {
            let samples = raw.assumingMemoryBound(to: Float32.self)
            p.write(interleavedSamples: samples, frameCount: frameCount)
            p.stampHeartbeat()
            return
        }

        // Everything else converts into the grow-only scratch buffer.
        let needed = frameCount * channels
        if scratch.count < needed {
            scratch = [Float32](repeating: 0, count: needed)
        }

        let ok: Bool = scratch.withUnsafeMutableBufferPointer { out -> Bool in
            guard let outBase = out.baseAddress else { return false }
            switch (isFloat, isSignedInt, bits, isNonInterleaved) {
            case (true, _, 32, true): // f32 planar → interleave
                for (ch, buf) in abl.enumerated() where ch < channels {
                    guard let raw = buf.mData else { return false }
                    let src = raw.assumingMemoryBound(to: Float32.self)
                    for f in 0..<frameCount { outBase[f * channels + ch] = src[f] }
                }
                return true
            case (false, true, 16, false): // i16 interleaved
                guard let raw = abl.first?.mData else { return false }
                let src = raw.assumingMemoryBound(to: Int16.self)
                for i in 0..<needed { outBase[i] = Float32(src[i]) / 32768.0 }
                return true
            case (false, true, 16, true): // i16 planar
                for (ch, buf) in abl.enumerated() where ch < channels {
                    guard let raw = buf.mData else { return false }
                    let src = raw.assumingMemoryBound(to: Int16.self)
                    for f in 0..<frameCount {
                        outBase[f * channels + ch] = Float32(src[f]) / 32768.0
                    }
                }
                return true
            case (false, true, 32, false): // i32 interleaved
                guard let raw = abl.first?.mData else { return false }
                let src = raw.assumingMemoryBound(to: Int32.self)
                for i in 0..<needed { outBase[i] = Float32(src[i]) / 2147483648.0 }
                return true
            case (false, true, 32, true): // i32 planar
                for (ch, buf) in abl.enumerated() where ch < channels {
                    guard let raw = buf.mData else { return false }
                    let src = raw.assumingMemoryBound(to: Int32.self)
                    for f in 0..<frameCount {
                        outBase[f * channels + ch] = Float32(src[f]) / 2147483648.0
                    }
                }
                return true
            default:
                return false
            }
        }

        guard ok else {
            if !warnedUnsupportedFormat {
                warnedUnsupportedFormat = true
                NSLog("[rsac] unsupported .audioApp LPCM layout: flags=0x%x bits=%d — dropping audio",
                      asbd.mFormatFlags, bits)
            }
            return
        }

        scratch.withUnsafeBufferPointer { buf in
            if let base = buf.baseAddress {
                p.write(interleavedSamples: base, frameCount: frameCount)
            }
        }
        p.stampHeartbeat()
    }

    /// Creates the ring producer on first use, geometry taken from the
    /// delivered ASBD, and starts the heartbeat timer.
    private func ensureProducer(asbd: AudioStreamBasicDescription) throws -> RsacRingProducer {
        lock.lock()
        defer { lock.unlock() }
        if let existing = producer { return existing }

        let sampleRate = UInt32(asbd.mSampleRate.rounded())
        let channels = asbd.mChannelsPerFrame
        let capacity = UInt32((Double(sampleRate) * ringSeconds).rounded())
        let created = try RsacRingProducer(
            appGroupIdentifier: appGroupIdentifier,
            sampleRate: sampleRate,
            channels: channels,
            capacityFrames: capacity)
        producer = created
        startHeartbeatTimer(for: created)
        return created
    }

    /// Periodic heartbeat, independent of audio delivery, so the host can
    /// distinguish "paused / silent app audio" from "extension killed".
    /// Interval is the contract value `RsacRingLayout.heartbeatIntervalMillis`.
    private func startHeartbeatTimer(for p: RsacRingProducer) {
        let timer = DispatchSource.makeTimerSource(
            queue: DispatchQueue(label: "ai.codeseys.rsac.heartbeat", qos: .utility))
        timer.schedule(
            deadline: .now(),
            repeating: .milliseconds(Int(RsacRingLayout.heartbeatIntervalMillis)))
        // Captures the producer instance directly — no shared mutable state;
        // stampHeartbeat is a single atomic store, safe from this queue.
        timer.setEventHandler { p.stampHeartbeat() }
        // close() (final heartbeat + munmap) runs HERE, in the cancel handler,
        // so it is ordered strictly after any in-flight event handler and no
        // tick can touch the ring after munmap (finding 9). The handler
        // retains `p` until it runs, so broadcastFinished may drop the
        // `producer` property reference immediately. DispatchSource runs a
        // cancel handler at most once.
        timer.setCancelHandler { p.close() }
        timer.resume()
        heartbeatTimer = timer
    }

    // ── Darwin notifications (optional Swift-side signal) ───────────────────

    /// Posts on the Darwin notify center (names: `RsacDarwinNotification`,
    /// defined once in RingLayout.swift). Darwin notifications cross the
    /// extension/app process boundary and carry no payload; all state lives
    /// in the ring header. NOT consumed by the Rust host backend — the Rust
    /// consumer's start/stop/liveness contract is heartbeat-poll-only (see
    /// RingLayout.swift's "Heartbeat" section). A host app MAY observe these
    /// for its own UI (e.g. an immediate "broadcast started" toast instead of
    /// waiting for the first polled ring header), but rsac's Rust backend
    /// never registers a Darwin notification observer.
    private func postDarwinNotification(_ name: String) {
        CFNotificationCenterPostNotification(
            CFNotificationCenterGetDarwinNotifyCenter(),
            CFNotificationName(name as CFString),
            nil,
            nil,
            true) // deliverImmediately
    }
}
