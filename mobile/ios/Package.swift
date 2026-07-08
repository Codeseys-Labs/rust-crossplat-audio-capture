// swift-tools-version: 5.9
// rsac mobile/ios — SwiftPM glue for the iOS backend (ADR-0012, ADR-0013).
//
// Two library products:
//   RsacAudio        — AVAudioSession helpers for the mic path (host app).
//   RsacBroadcastKit — ReplayKit Broadcast Upload Extension glue: the
//                      canonical App-Group mmap SPSC ring (RingLayout.swift),
//                      the producer, and an open RPBroadcastSampleHandler
//                      template.
//
// CRsacRingAtomics is an internal C11 stdatomic shim target (NOT a product):
// Swift < iOS 18 has no standard-library atomics usable on raw mmap'd memory
// shared across processes, so the acquire/release u64 cursor operations are
// implemented in C. It is a dependency of RsacBroadcastKit only.
//
// Status: source-complete; not yet built in CI (rsac-48e7 adds the
// `xcodebuild`/`swift build` CI job). See README.md.

import PackageDescription

let package = Package(
    name: "RsacMobile",
    platforms: [
        .iOS(.v14)
    ],
    products: [
        .library(name: "RsacAudio", targets: ["RsacAudio"]),
        .library(name: "RsacBroadcastKit", targets: ["RsacBroadcastKit"]),
    ],
    targets: [
        .target(
            name: "RsacAudio",
            path: "Sources/RsacAudio"
        ),
        .target(
            name: "CRsacRingAtomics",
            path: "Sources/CRsacRingAtomics"
        ),
        .target(
            name: "RsacBroadcastKit",
            dependencies: ["CRsacRingAtomics"],
            path: "Sources/RsacBroadcastKit"
        ),
    ]
)
