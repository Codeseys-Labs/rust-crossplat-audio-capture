# RsacSimHarness — iOS SIMULATOR frames-delivered TCC harness (rsac-f18f)

An app-hosted XCTest bundle that proves rsac's AVAudioEngine mic tap delivers
frames **through the rsac C API** on a TCC-granted iOS simulator. It is the
"wrapped app" follow-up to the bare-binary smoke in `tests/ios_sim_smoke.rs`.

## Why an Xcode project (not a SwiftPM `testTarget`)

A libtest binary spawned via `xcrun simctl spawn` has **no bundle → no TCC
target**. Its `AVAudioEngine` input node then reports an unusable native format
(`0 Hz / 0 ch`), so the bare-binary frames smoke soft-fails. TCC on the
simulator keys on a **bundle id**: `simctl privacy <udid> grant microphone
<bundle-id>` only works against a real installed `.app`.

So this harness ships:

- **`RsacSimHarnessApp`** — a near-empty host app whose `App/Info.plist` carries
  `NSMicrophoneUsageDescription` and the fixed bundle id
  `ai.codeseys.rsac.simharness`. It exists only to own a TCC identity.
- **`RsacSimHarnessTests`** — an **app-hosted** unit-test bundle (`TEST_HOST` +
  `BUNDLE_LOADER` set to the app) that runs *inside* the app process, so it
  inherits the app's TCC grant and mic-usage string. It drives rsac through the
  C API (`rsac-ffi`, via the bridging header) and asserts frames are delivered
  with a sane negotiated format — **never content**.

## The project is generated AND committed

`RsacSimHarness.xcodeproj` is generated from `project.yml` with
[XcodeGen](https://github.com/yonaskolb/XcodeGen) and **committed**. CI consumes
the committed `.xcodeproj` directly with `xcodebuild`, so **the runner needs no
XcodeGen**. After editing `project.yml`, regenerate and recommit:

```sh
cd mobile/ios/RsacSimHarness
xcodegen generate --spec project.yml
```

## How CI drives it (`.github/workflows/ci-ios-sim.yml`)

1. `cargo build -p rsac-ffi --target aarch64-apple-ios-sim --release
   --no-default-features --features feat_ios` → `librsac_ffi.a` under
   `target/aarch64-apple-ios-sim/release/` (where `LIBRARY_SEARCH_PATHS` points).
2. `xcodebuild build-for-testing -scheme RsacSimHarness -destination
   "platform=iOS Simulator,id=$UDID"` → builds the app + test bundle.
3. `xcrun simctl install $UDID <app>` then `xcrun simctl privacy $UDID grant
   microphone ai.codeseys.rsac.simharness` — **grant BEFORE first launch**.
4. `xcodebuild test-without-building -scheme RsacSimHarness -destination ...`
   with `TEST_RUNNER_RSAC_CI_IOS_SIM=1` (the simulator does not inherit the
   runner shell env — the `xcodebuild` analog of the `SIMCTL_CHILD_` lesson).

## Honesty

A pass is **simulator-verified**, never device-verified. A silent/blocked
simulator input route degrades to skip-with-summary unless
`RSAC_CI_IOS_REQUIRE_FRAMES=1` (flipped once a runner proves the host-mic route
reliable). Physical-device capture, interleaved delivery, and start-failure
rollback remain a runbook (`needs-real-device`).
