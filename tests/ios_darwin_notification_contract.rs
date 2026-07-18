//! Contract guard (rsac-7e0a): the iOS ReplayKit Rust consumer
//! (`src/audio/ios/broadcast.rs`) must remain heartbeat-poll-only and never
//! grow a `CFNotificationCenterGetDarwinNotifyCenter` dependency without a
//! deliberate, documented decision. Runs on every host (no `target_os` gate)
//! since it's a static grep over the source text, not a runtime iOS check.

#[test]
fn broadcast_rs_has_no_darwin_notification_listener() {
    let src = include_str!("../src/audio/ios/broadcast.rs");
    for needle in [
        "CFNotificationCenter",
        "DarwinNotifyCenter",
        "CFNotificationName",
    ] {
        assert!(
            !src.contains(needle),
            "src/audio/ios/broadcast.rs now references `{needle}` — the Rust \
             consumer was heartbeat-poll-only by design (rsac-7e0a). If this \
             is a deliberate addition of a Darwin-notification listener, \
             update this test's assertion AND the contract docs in lockstep: \
             mobile/ios/Sources/RsacBroadcastKit/RingLayout.swift's banner, \
             SampleHandlerTemplate.swift's postDarwinNotification doc comment, \
             docs/MOBILE_BACKEND_DESIGN.md's \"Signaling\" bullet + risk table, \
             and docs/designs/0013-mobile-capturetarget-semantics.md's Decision \
             section (all touched by rsac-7e0a in the other direction)."
        );
    }
}
