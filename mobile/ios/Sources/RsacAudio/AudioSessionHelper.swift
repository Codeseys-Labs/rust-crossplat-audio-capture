import AVFAudio
import Foundation

/// `AVAudioSession` helpers for rsac's iOS **microphone path**.
///
/// rsac's Rust backend (`src/audio/ios/`, the AVAudioEngine mic slice)
/// deliberately does **NOT** touch the shared `AVAudioSession` — session
/// configuration is app-global policy that only the host application can
/// own. Before building a mic capture
/// (`CaptureTarget::Device(DeviceId("default"))`), the host app must:
///
/// 1. declare `NSMicrophoneUsageDescription` in its `Info.plist` (the app
///    crashes on first mic access without it),
/// 2. obtain the record permission (``requestRecordPermission()``), and
/// 3. configure + activate a record-capable session
///    (``configureForRecording(category:mode:options:)`` + ``activate()``).
///
/// These helpers wrap exactly that flow. They are conveniences over
/// `AVAudioSession.sharedInstance()` — apps with existing session management
/// can keep it and skip this type entirely.
public enum RsacAudioSession {

    /// Errors thrown by the helpers (beyond what AVAudioSession itself throws).
    public enum SessionError: Error, CustomStringConvertible {
        /// The requested category cannot record; rsac's mic capture would
        /// receive silence or fail to start.
        case categoryCannotRecord(AVAudioSession.Category)

        public var description: String {
            switch self {
            case let .categoryCannotRecord(cat):
                return "AVAudioSession category '\(cat.rawValue)' cannot record — "
                    + "use .record, .playAndRecord, or .multiRoute"
            }
        }
    }

    /// The shared session these helpers operate on.
    public static var session: AVAudioSession { .sharedInstance() }

    // ── Configuration ─────────────────────────────────────────────────────

    /// Configures the shared session with a record-capable category.
    ///
    /// - Parameters:
    ///   - category: must be one of `.record`, `.playAndRecord`, or
    ///     `.multiRoute`; anything else throws
    ///     ``SessionError/categoryCannotRecord(_:)`` instead of silently
    ///     producing a session rsac cannot capture from.
    ///   - mode: session mode; `.default` unless you know you need
    ///     `.measurement` (raw, unprocessed input) or `.voiceChat`.
    ///   - options: e.g. `[.allowBluetooth]` to admit BT headset input, or
    ///     `[.defaultToSpeaker]` (valid only with `.playAndRecord`). Defaults
    ///     to none — pick deliberately; options are app UX policy.
    ///
    /// Does NOT activate the session — call ``activate()`` when ready (order
    /// matters for route negotiation).
    public static func configureForRecording(
        category: AVAudioSession.Category = .playAndRecord,
        mode: AVAudioSession.Mode = .default,
        options: AVAudioSession.CategoryOptions = []
    ) throws {
        guard category == .record || category == .playAndRecord || category == .multiRoute
        else {
            throw SessionError.categoryCannotRecord(category)
        }
        try session.setCategory(category, mode: mode, options: options)
    }

    /// Activates the shared session. Call after
    /// ``configureForRecording(category:mode:options:)`` and before building
    /// the rsac capture — stream creation fails with an actionable error if
    /// no input route is active.
    public static func activate() throws {
        try session.setActive(true)
    }

    /// Deactivates the shared session (e.g. after stopping capture).
    ///
    /// - Parameter notifyOthersOnDeactivation: pass `true` (default) so
    ///   other apps' interrupted audio can resume.
    public static func deactivate(notifyOthersOnDeactivation: Bool = true) throws {
        try session.setActive(
            false,
            options: notifyOthersOnDeactivation ? [.notifyOthersOnDeactivation] : [])
    }

    // ── Record permission ─────────────────────────────────────────────────

    /// Whether the user has already granted the record permission.
    public static var hasRecordPermission: Bool {
        if #available(iOS 17.0, *) {
            // CI-VERIFY: AVAudioApplication.shared.recordPermission == .granted
            // (iOS 17 replacement API; enum case name spelled `granted`).
            return AVAudioApplication.shared.recordPermission == .granted
        } else {
            return session.recordPermission == .granted
        }
    }

    /// Requests the record permission (callback form). The FIRST request
    /// shows the system prompt (with the app's
    /// `NSMicrophoneUsageDescription` text); later calls report the stored
    /// decision immediately. The completion runs on an arbitrary queue —
    /// dispatch to the main queue yourself for UI work.
    public static func requestRecordPermission(
        _ completion: @escaping @Sendable (Bool) -> Void
    ) {
        if #available(iOS 17.0, *) {
            // CI-VERIFY: iOS 17 renamed the API to
            // AVAudioApplication.requestRecordPermission(completionHandler:) —
            // confirm the static-method spelling against the SDK.
            AVAudioApplication.requestRecordPermission(completionHandler: completion)
        } else {
            session.requestRecordPermission(completion)
        }
    }

    /// Requests the record permission (async form).
    public static func requestRecordPermission() async -> Bool {
        await withCheckedContinuation { continuation in
            requestRecordPermission { granted in
                continuation.resume(returning: granted)
            }
        }
    }
}
