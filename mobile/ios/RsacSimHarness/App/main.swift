// rsac-f18f — near-empty host app for the iOS SIMULATOR TCC harness.
//
// This app does nothing on its own; it exists only to provide a real bundle +
// Info.plist (with NSMicrophoneUsageDescription and the fixed bundle id
// ai.codeseys.rsac.simharness) so that the app-HOSTED XCTest bundle inherits a
// TCC identity. `simctl privacy <udid> grant microphone ai.codeseys.rsac.simharness`
// then pre-authorizes the microphone BEFORE the test process first activates a
// record AVAudioSession — the whole reason a bundle-less spawned libtest binary
// cannot get a usable input route.
//
// A plain UIApplicationMain with no scene/window is enough: the hosted test
// runs inside this process; the app never needs to render anything.
import UIKit

UIApplicationMain(
    CommandLine.argc,
    CommandLine.unsafeArgv,
    nil,
    NSStringFromClass(UIResponder.self)
)
