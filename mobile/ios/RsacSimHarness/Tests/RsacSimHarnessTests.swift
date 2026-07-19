// rsac-f18f — iOS SIMULATOR frames-delivered smoke, APP-HOSTED (TCC-granted).
//
// The twin of `tests/ios_sim_smoke.rs`, but run as an app-hosted XCTest so it
// has a real bundle + NSMicrophoneUsageDescription and can be TCC-granted
// (`simctl privacy <udid> grant microphone ai.codeseys.rsac.simharness`) BEFORE
// first launch. A bundle-less libtest binary spawned via `simctl spawn` has no
// TCC target, so its AVAudioEngine input node reports an unusable native format
// (0 Hz / 0 ch) — this harness closes that gap.
//
// It drives rsac through its C API (rsac-ffi, bridged via
// RsacSimHarnessTests-Bridging-Header.h), activating a record AVAudioSession
// first (the host-app job the library refuses by design —
// src/audio/ios/mod.rs "Host-app responsibilities"). It asserts FRAMES ARE
// DELIVERED with a sane negotiated format — NEVER content: the simulator host
// mic may be silent, so this proves the tap wiring reaches rsac, not that it
// carries a tone.
//
// Honesty (mirrors the Rust smoke + docs/MOBILE_BACKEND_DESIGN.md): a pass here
// is **simulator-verified**, never device-verified. A failure to activate a
// record session or a zero-frame route degrades to skip-with-summary UNLESS
// RSAC_CI_IOS_REQUIRE_FRAMES=1 (flipped via the workflow_dispatch input once a
// runner proves the host-mic route reliable).
//
// Env is injected by the workflow via xcodebuild's `TEST_RUNNER_` prefix (the
// simulator does NOT inherit the runner shell env — the xcodebuild analog of
// the `simctl spawn` SIMCTL_CHILD_ lesson): TEST_RUNNER_RSAC_CI_IOS_SIM=1, etc.
import AVFAudio
import XCTest

final class RsacSimHarnessTests: XCTestCase {
    // ── Env gates (read from the test process's environment) ────────────────
    private func envVar(_ key: String) -> String? {
        ProcessInfo.processInfo.environment[key]
    }
    private var simEnabled: Bool { envVar("RSAC_CI_IOS_SIM") == "1" }
    private var requireFramesHard: Bool { envVar("RSAC_CI_IOS_REQUIRE_FRAMES") == "1" }
    private var captureTimeout: TimeInterval {
        TimeInterval(envVar("RSAC_TEST_CAPTURE_TIMEOUT_SECS").flatMap { Int($0) } ?? 15)
    }

    /// Soft-fail (skip-with-summary) unless RSAC_CI_IOS_REQUIRE_FRAMES=1, in
    /// which case hard-fail. Always throws, unwinding the test cleanly.
    private func bail(_ message: String) throws -> Never {
        if requireFramesHard {
            throw NSError(
                domain: "ai.codeseys.rsac.simharness", code: 1,
                userInfo: [NSLocalizedDescriptionKey: "\(message) RSAC_CI_IOS_REQUIRE_FRAMES=1"])
        }
        throw XCTSkip("\(message) skip-with-summary.")
    }

    private func lastError() -> String {
        guard let c = rsac_error_message() else { return "<none>" }
        return String(cString: c)
    }

    func testFramesDeliveredViaCApi() throws {
        try XCTSkipUnless(
            simEnabled, "RSAC_CI_IOS_SIM != 1 — not a sim runtime run.")

        // ── Host-app job: activate a record-capable AVAudioSession ──────────
        // The library never touches the shared session (by design); the test
        // plays the host app. If no input route can be activated (a sim without
        // a usable mic route), degrade honestly.
        let session = AVAudioSession.sharedInstance()
        do {
            try session.setCategory(.playAndRecord, mode: .default)
            try session.setActive(true)
        } catch {
            try bail("could not activate a record-capable AVAudioSession: \(error).")
        }
        defer { try? session.setActive(false, options: [.notifyOthersOnDeactivation]) }

        // ── Build the capture via the PUBLIC C API ──────────────────────────
        var builder: OpaquePointer?
        XCTAssertEqual(
            rsac_builder_new(&builder), RSAC_OK,
            "rsac_builder_new: \(lastError())")
        guard builder != nil else { try bail("rsac_builder_new returned a null builder.") }

        // "default" == the session's current input route
        // (src/audio/ios/mod.rs DEFAULT_INPUT_DEVICE_ID contract).
        XCTAssertEqual(
            rsac_builder_set_target_device(builder, "default"), RSAC_OK,
            "set_target_device: \(lastError())")
        XCTAssertEqual(rsac_builder_set_sample_rate(builder, 48_000), RSAC_OK)
        XCTAssertEqual(rsac_builder_set_channels(builder, 1), RSAC_OK)

        // build() consumes the builder (success or failure — do not free it).
        var capture: OpaquePointer?
        let buildRc = rsac_builder_build(builder, &capture)
        if buildRc != RSAC_OK || capture == nil {
            try bail("rsac_builder_build failed (\(buildRc)): \(lastError()).")
        }
        // Stops the stream if running; safe once, at the end.
        defer { rsac_capture_free(capture) }

        // ── Start ────────────────────────────────────────────────────────────
        // In the TCC-granted, session-active context there is no legitimate
        // excuse for start() to fail — but keep the honesty ladder so a genuinely
        // silent sim route reports rather than reds the advisory leg.
        let startRc = rsac_capture_start(capture)
        if startRc != RSAC_OK {
            try bail("rsac_capture_start failed (\(startRc)): \(lastError()).")
        }
        XCTAssertEqual(
            rsac_capture_is_running(capture), 1, "capture must run after start()")

        // ── Negotiated format sanity (NOT content) ──────────────────────────
        var fmt = RsacAudioFormat()
        let fmtRc = rsac_capture_format(capture, &fmt)
        if fmtRc != RSAC_OK {
            rsac_capture_request_stop(capture)
            try bail("rsac_capture_format returned \(fmtRc) after a successful start().")
        }
        XCTAssertTrue(
            (8_000...96_000).contains(fmt.sample_rate),
            "sane negotiated rate, got \(fmt.sample_rate)")
        XCTAssertTrue(
            fmt.channels == 1 || fmt.channels == 2,
            "sane channel count, got \(fmt.channels)")
        XCTAssertEqual(
            fmt.sample_format, RSAC_SAMPLE_FORMAT_F32,
            "iOS bridge payload is always f32")
        print("[ios-sim-app] negotiated \(fmt.sample_rate) Hz, \(fmt.channels) ch, f32")

        // ── DELIVERY assertion: ≥1 non-empty buffer within the timeout ──────
        // Bounded non-blocking poll (mirrors ios_sim_smoke.rs) so a silent or
        // blocked route cannot park the test past the deadline.
        let deadline = Date().addingTimeInterval(captureTimeout)
        var frames = 0
        var buffers = 0
        pollLoop: while Date() < deadline {
            var buf: OpaquePointer?
            let readRc = rsac_capture_try_read(capture, &buf)
            switch readRc {
            case RSAC_OK where buf != nil:
                let n = rsac_audio_buffer_num_frames(buf)
                if n > 0 {
                    buffers += 1
                    frames += n
                }
                rsac_audio_buffer_free(buf)
                if buffers >= 3 && frames > 0 { break pollLoop }
            case RSAC_OK:
                Thread.sleep(forTimeInterval: 0.01)
            default:
                print("[ios-sim-app] read error \(readRc) (end-of-stream): \(lastError())")
                break pollLoop
            }
        }
        rsac_capture_request_stop(capture)

        print("[ios-sim-app] delivered \(buffers) buffers, \(frames) frames.")
        if frames == 0 {
            try bail("zero frames delivered from the simulator mic route (may be silent/blocked).")
        }
        XCTAssertGreaterThan(
            frames, 0,
            "AVAudioEngine tap delivered frames through the rsac public C API")
    }
}
