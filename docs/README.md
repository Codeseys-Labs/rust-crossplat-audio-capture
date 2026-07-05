# rsac Documentation Index

Every file directly under `docs/`, with its status. **The code is the source
of truth** (AGENTS.md §2) — docs marked *historical* are kept for context,
not guidance. Entry points (⭐) are the places to start.

## Using rsac

| Doc | What it covers |
|---|---|
| ⭐ [`CONSUMING_RSAC.md`](CONSUMING_RSAC.md) | Every public surface (Rust, C FFI, Python, Node/Bun, Go, CLI) + install/link recipes |
| ⭐ [`API.md`](API.md) | Task-oriented tour of the public Rust API |
| [`FRAMEWORK_COMPATIBILITY.md`](FRAMEWORK_COMPATIBILITY.md) | Tauri / Dioxus / Electron / Deno / Bun / Flutter status + recipes |
| [`CROSS_LANGUAGE_BINDINGS.md`](CROSS_LANGUAGE_BINDINGS.md) | Binding design, parity matrix, per-binding semantics |
| [`INTEROP.md`](INTEROP.md) | Bridging buffers to dasp / cpal / rodio / hound / symphonia |
| [`features.md`](features.md) | Cargo feature matrix + per-feature host dependencies |
| [`macos_application_capture.md`](macos_application_capture.md) | Per-app / process-tree capture how-to on macOS |
| [`system_requirements.md`](system_requirements.md) | OS/build prerequisites (aging; `features.md` supersedes on overlap) |
| ⭐ [`troubleshooting.md`](troubleshooting.md) | Build/runtime fixes by symptom |

## Understanding rsac

| Doc | What it covers |
|---|---|
| ⭐ [`ARCHITECTURE.md`](ARCHITECTURE.md) | Accurate 3-layer architecture overview (start here) |
| [`PERFORMANCE.md`](PERFORMANCE.md) | RT-safety story, ring sizing, backpressure diagnostics |
| [`MOBILE_BACKEND_DESIGN.md`](MOBILE_BACKEND_DESIGN.md) | Planned Android/iOS backends (design; nothing implemented) |
| [`MACOS_VERSION_COMPATIBILITY.md`](MACOS_VERSION_COMPATIBILITY.md) | macOS 14.4–26 API compatibility matrix |
| [`MACOS26_PROCESS_TAP_FIX.md`](MACOS26_PROCESS_TAP_FIX.md) | Dated fix record: the 3-path Process Tap fallback |
| [`OBJC2_MIGRATION_PLAN.md`](OBJC2_MIGRATION_PLAN.md) | Completed cocoa/objc → objc2 migration record |
| [`designs/`](designs/README.md) | **ADRs 0001–0014** — durable decisions (indexed) |
| [`architecture/`](architecture/) | Original design docs — **historical**, divergence-banner'd; code wins |
| [`reviews/`](reviews/) | Review-loop retrospectives (point-in-time snapshots) |

## Contributing & operating

| Doc | What it covers |
|---|---|
| ⭐ [`CONTRIBUTING.md`](CONTRIBUTING.md) | Onboarding (mise + lefthook), the local gate, test suites, PR checklist |
| [`CI_AUDIO_TESTING.md`](CI_AUDIO_TESTING.md) | The 9-cell audio-test truth table, gate macros, workflow knobs |
| [`LOCAL_TESTING_GUIDE.md`](LOCAL_TESTING_GUIDE.md) | Manual verification on physical Windows/macOS/Linux machines |
| [`PLATFORM_TESTING.md`](PLATFORM_TESTING.md) | Map of the verification layers |
| [`DOCKER_TESTING.md`](DOCKER_TESTING.md) | Docker cross-compile/test harness (aging; parts unmaintained) |
| [`RELEASE_PROCESS.md`](RELEASE_PROCESS.md) | End-to-end release procedure |
| [`STACKED_PRS.md`](STACKED_PRS.md) | gh-native stacked-PR playbook |
| [`CONTRIBUTING.md` §6](CONTRIBUTING.md#6-commit-style) / [`AGENTS.md` §6](../AGENTS.md) | Commit + review-disposition rules |
| [`audit/docs-queue.md`](audit/docs-queue.md) | Documentation audit queue |
| [`history/`](history/README.md) | **Archived snapshots** — frozen, do not follow |

Not listed here = it was moved to [`history/`](history/README.md) in a rot
cleanup; see that README for what each archived doc was.
