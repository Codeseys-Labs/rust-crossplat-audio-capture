// Copyright 2026 Codeseys Labs
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// tauri-plugin-rsac iOS plugin — STUB (ADR-0014).
//
// iOS has no config-time MediaProjection-style consent dialog: rsac's iOS
// SystemDefault path is a ReplayKit Broadcast Upload Extension whose consent
// artifact is the App Group identifier (ADR-0013, rsac-b3aa), supplied on the
// Rust side via `AudioCaptureBuilder::with_ios_app_group`. There is no native
// dialog for this plugin to drive, so `requestConsent` resolves
// `{ granted: true }` (the App-Group artifact, not a runtime grant, is what
// iOS system capture needs) and the capture lifecycle runs entirely through
// the shared Rust `Sessions` path.
//
// This stub exists so the plugin's `register_ios_plugin(init_plugin_rsac)`
// binding resolves and the SwiftPM package compiles. On-device iOS capture is
// the runtime lane's problem (rsac-97c8), not this compile-proof lane's.

import Tauri
import UIKit
import WebKit

class RsacTauriPlugin: Plugin {
    /// Consent on iOS is the App-Group artifact supplied Rust-side, not a
    /// native dialog — resolve success so the JS API stays uniform. The Rust
    /// preflight surfaces `UserConsentRequired` honestly if the App Group is
    /// missing when a SystemDefault capture is built.
    @objc public func requestConsent(_ invoke: Invoke) throws {
        invoke.resolve(["granted": true])
    }
}

@_cdecl("init_plugin_rsac")
func initPlugin() -> Plugin {
    return RsacTauriPlugin()
}
