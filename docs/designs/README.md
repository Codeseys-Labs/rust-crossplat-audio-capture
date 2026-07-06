# Architecture Decision Records

Durable design decisions for rsac, in the repo's house ADR format
(`Status / Date / Scope / Verdict` header + context, drivers, options,
decision, consequences). ADRs are immutable once accepted — supersede, don't
edit. The **code is the source of truth**; when an ADR describes future work,
its Scope says so.

| # | Title | Status | Date |
|---|-------|--------|------|
| [ADR-0001](0001-rt-allocation-guarantee.md) | Real-time producer allocation guarantee | Accepted | 2026-05-29 |
| [ADR-0002](0002-callback-delivery.md) | `set_callback` delivery: wire it, don't remove it | Accepted | 2026-05-29 |
| [ADR-0003](0003-terminal-stream-error.md) | Distinguish terminal stream end from recoverable read errors | Accepted | 2026-05-29 |
| [ADR-0004](0004-device-change-notifications.md) | Device-change-notification delivery model (per-platform) | Accepted | 2026-05-30 |
| [ADR-0005](0005-device-watcher-raii-teardown.md) | `DeviceWatcher` RAII teardown / lifecycle contract | Accepted | 2026-05-30 |
| [ADR-0006](0006-bridge-zerocopy-samplering.md) | `bridge-zerocopy` `SampleRing`: an opt-in, default-off alternative data plane | Accepted | 2026-05-30 |
| [ADR-0007](0007-capacity-period-sizing.md) | Period-derived ring sizing and `buffer_size` semantics | Accepted | 2026-05-30 |
| [ADR-0008](0008-cache-padded-atomics.md) | Hand-rolled `CachePadded` for false-sharing mitigation in `BridgeShared` | Accepted | 2026-05-30 |
| [ADR-0009](0009-tracing-log-shim.md) | `tracing`/`log` dual-backend instrumentation shim with an RT-path prohibition | Accepted | 2026-05-30 |
| [ADR-0010](0010-producer-terminal-signal.md) | Producer-side terminal-signal contract | Accepted | 2026-05-30 |
| [ADR-0011](0011-compose-feature.md) | Multi-source channel composition in-crate behind an opt-in `compose` feature | Accepted | 2026-07-04 |
| [ADR-0012](0012-mobile-platform-strategy.md) | Batteries-included mobile platform strategy: backends in-crate, Kotlin AAR + Swift package owned by rsac | Accepted | 2026-07-04 |
| [ADR-0013](0013-mobile-capturetarget-semantics.md) | Mobile `CaptureTarget` semantics: strict Android mapping, ReplayKit-backed iOS `SystemDefault`, explicit consent token | Accepted | 2026-07-04 |
| [ADR-0014](0014-tauri-integration-model.md) | Tauri integration model: direct library dependency on desktop, `tauri-plugin-rsac` as the mobile vehicle | Proposed | 2026-07-04 |
| [ADR-0015](0015-macos-tcc-audiocapture-preflight.md) | macOS system-audio-capture permission preflight (private TCC SPI) | Accepted | 2026-07-06 |
| [ADR-0016](0016-macos-process-tap-silent-zeros-guard.md) | macOS Process-Tap silent-zeros diagnostic (denied-permission guard) | Accepted | 2026-07-06 |
| [abi3-decision](abi3-decision.md) | abi3 vs per-version Python wheels | Accepted | 2026-04-17 |
