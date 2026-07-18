// swift-tools-version:5.3
// tauri-plugin-rsac iOS package (ADR-0014). Source-shipped, compile-proof.
//
// This is a STUB: the iOS runtime path (ReplayKit broadcast consumer,
// App-Group system capture) rides rsac's iOS backend (mobile/ios, ADR-0013)
// and is verified by the runtime lane (rsac-97c8), not here. The plugin
// commands resolve `unsupported` on iOS until that lands.

import PackageDescription

let package = Package(
    name: "tauri-plugin-rsac",
    platforms: [
        .macOS(.v10_13),
        .iOS(.v13),
    ],
    products: [
        .library(
            name: "tauri-plugin-rsac",
            type: .static,
            targets: ["tauri-plugin-rsac"])
    ],
    dependencies: [
        // Injected by the Tauri CLI at build time.
        .package(name: "Tauri", path: "../.tauri/tauri-api")
    ],
    targets: [
        .target(
            name: "tauri-plugin-rsac",
            dependencies: [
                .byName(name: "Tauri")
            ],
            path: "Sources")
    ]
)
