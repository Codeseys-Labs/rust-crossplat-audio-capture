# Framework Compatibility

> **Status legend (honesty contract):**
> ✅ **verified** — proven by a shipped consumer or an executed test.
> 🟢 **expected, unverified** — API-level desk research says it works; no hands-on
> verification has been run yet. A wave-2 seed exists to flip the marker.
> 🟡 **blocked** — the integration path is understood, but it depends on the
> Android/iOS backends that do not exist yet (see
> [`MOBILE_BACKEND_DESIGN.md`](MOBILE_BACKEND_DESIGN.md)).
> ❌ **not viable** — a structural platform restriction, not an rsac gap.
>
> Every claim in this document carries one of these markers. Do not soften a 🟡
> or ❌ — honest capability reporting is a core project rule (AGENTS.md §7).

This document answers: *"Can I use rsac from framework X, on platform Y, and
how?"* It covers UI/app frameworks and JS runtimes. For language bindings
(Python, Node, Go, C) see
[`CROSS_LANGUAGE_BINDINGS.md`](CROSS_LANGUAGE_BINDINGS.md). For the mobile
backend design (Android/iOS) see
[`MOBILE_BACKEND_DESIGN.md`](MOBILE_BACKEND_DESIGN.md).

## Summary matrix

| Framework / runtime | Desktop (Win/Linux/macOS) | Android | iOS | Integration path |
|---|---|---|---|---|
| **Tauri v2** | ✅ verified (audio-graph) | 🟡 blocked on backend | 🟡 blocked on backend | Direct Rust dependency; mobile via `tauri-plugin-rsac` (planned, [ADR-0014](designs/0014-tauri-integration-model.md)) |
| **Dioxus** | 🟢 expected | 🟡 blocked on backend | 🟡 blocked on backend | Direct Rust dependency (rsac is UI-framework-agnostic) |
| **Electron** | ✅ documented (napi, main process) | n/a | n/a | `@rsac/audio` (napi-rs) |
| **Deno 2** | 🟢 expected | n/a | n/a | `npm:@rsac/audio` (Node-API compat) or `Deno.dlopen` over rsac-ffi |
| **Bun / `bun --compile`** | 🟢 expected (napi CI is Bun-first); compile has a packaging caveat | n/a | n/a | `@rsac/audio` or `bun:ffi` over rsac-ffi |
| **Flutter** | 🟢 expected via `dart:ffi` + ffigen | 🟡 blocked on backend | 🟡 blocked on backend | New Dart package over `bindings/rsac-ffi/include/rsac.h` |
| Rust-native GUIs (egui/Iced/Slint/GPUI) | 🟢 expected (trivially) | 🟡 | 🟡 | Direct Rust dependency |
| React Native | 🟢 expected on paper (JSI/Nitro or uniffi over rsac-ffi) | 🟡 | 🟡 | Long tail — no commitment |
| Capacitor / Ionic | — | 🟡 | 🟡 | Long tail — no commitment |
| .NET MAUI | 🟢 expected (P/Invoke over rsac-ffi) | 🟡 | 🟡 | Long tail — no commitment |
| Qt / C++ | 🟢 expected (direct C FFI) | 🟡 | 🟡 | Long tail — no commitment |
| Browser / WASM | ❌ not viable | ❌ | ❌ | Ruled out in [`CROSS_LANGUAGE_BINDINGS.md`](CROSS_LANGUAGE_BINDINGS.md#5-wasm--not-viable-for-capture) |

**The single most important row-shape:** every 🟡 in the Android/iOS columns is
the *same* blocker — rsac has no mobile backends yet. The framework integrations
themselves are not the hard part; the OS capture backends are. Once
`src/audio/android/` and `src/audio/ios/` exist (waves 3–4 of the mobile plan),
every 🟡 above resolves through its listed integration path. Conversely, the ❌
cells are Apple/Google/browser policy, and **no framework choice changes them**.

## Tauri v2

**Desktop: ✅ verified.** [AudioGraph](https://github.com/Codeseys-Labs/audio-graph)
(the `apps/audio-graph` submodule) is a shipping Tauri v2 app that consumes rsac
as a plain path dependency in its Rust backend — `rsac::list_audio_sources()`,
the builder API, and capture streaming all work with zero Tauri-specific glue.
Audio data never crosses the webview IPC boundary; the Rust side consumes
buffers and sends only *derived* results (transcripts, meters) to the frontend.

**Recipe (desktop):** add `rsac` to `src-tauri/Cargo.toml`, capture in a
`#[tauri::command]`-spawned thread or async task, emit derived events to the
webview. Do **not** ship raw `f32` buffers through `emit()` — serialize cost at
audio rates is significant; process in Rust and emit summaries.

**Library vs plugin — which is the better implementation?** For a single
desktop app, the **direct dependency is better**: full Rust API surface, no IPC
serialization of audio, no plugin permission plumbing. A `tauri-plugin-rsac`
earns its keep in exactly two situations: (a) **mobile** — Tauri v2 plugins are
the sanctioned vehicle for shipping the Kotlin/Swift glue and consent flows a
mobile backend requires; (b) a reusable **JS-facing API** (invoke/events with
Tauri's permission system) shared across multiple Tauri apps. This is recorded
as *proposed* in [ADR-0014](designs/0014-tauri-integration-model.md): audio-graph
keeps its direct dependency on desktop; the plugin is planned as the mobile
vehicle (wave 5).

**Mobile: 🟡 blocked on backends.** Tauri v2 builds for Android/iOS today, and
rsac already *compiles* for those targets as an honest stub
(`PlatformCapabilities::unsupported()`, `AudioError::PlatformNotSupported` —
src/audio/mod.rs, src/core/capabilities.rs). Real capture requires the backends
in [`MOBILE_BACKEND_DESIGN.md`](MOBILE_BACKEND_DESIGN.md); the consent flow
(Android MediaProjection) ships in the planned plugin.

## Dioxus

**Desktop: 🟢 expected, unverified.** Dioxus desktop apps are plain Rust
processes (wry webview, like Tauri) — rsac needs nothing framework-specific.
Spawn capture on a background thread/task, feed UI state via Dioxus signals.
A wave-2 seed adds a worked example to flip this to ✅.

**Mobile: 🟡 blocked on backends.** Dioxus has no plugin system like Tauri's, so
on Android the host app must obtain the MediaProjection consent token itself
(via its own Kotlin activity glue) and hand it to
`AudioCaptureBuilder::with_android_projection(...)` — the documented recipe in
[`MOBILE_BACKEND_DESIGN.md`](MOBILE_BACKEND_DESIGN.md#android-consent-token-flow).
The batteries-included `mobile/android/` AAR ([ADR-0012](designs/0012-mobile-platform-strategy.md))
exists precisely so Dioxus/Flutter/native hosts don't re-implement that glue.

## Electron

**✅ documented.** `@rsac/audio` (napi-rs) works in Electron's **main process**;
the push path (`onData`) maps onto a ThreadsafeFunction and integrates with the
event loop — see
[`CROSS_LANGUAGE_BINDINGS.md` §3](CROSS_LANGUAGE_BINDINGS.md#3-nodejsbun-napi-rs).
Constraints: keep capture in the main process (or a utility process), not the
renderer; ship the prebuilt `.node` for each `platform-arch` you target (the
napi release pipeline already builds darwin-x64/arm64, linux-x64/arm64-gnu/musl,
win32-x64). Electron's Node ABI is covered by Node-API — no per-Electron-version
rebuilds.

## Deno 2

**🟢 expected, unverified.** Two viable paths, neither requiring new rsac code:

1. **Node-API compat:** Deno 2 loads Node-API native addons via `npm:`
   specifiers — `import { AudioCapture } from "npm:@rsac/audio"` with
   `--allow-ffi`. This is the preferred path (typed, maintained surface).
2. **`Deno.dlopen`:** load the `rsac_ffi` cdylib directly against the curated
   `rsac.h` symbol set (56 functions). More work (hand-written FFI signatures),
   full control, no npm dependency.

Open verification questions for the wave-2 seed: ThreadsafeFunction callback
delivery under Deno's event loop (the `onData` push path), and BigInt counter
round-trip. Until run, this row stays 🟢.

## Bun and `bun --compile`

**🟢 expected.** Bun runs Node-API addons, and rsac's own npm release pipeline
is already Bun-first (`bun install`, `bunx @napi-rs/cli` — see
`.github/workflows/release-npm.yml`), so plain `bun add @rsac/audio` is expected
to work. `bun:ffi` (`dlopen`) over rsac-ffi is the alternative path.

**`bun --compile` caveat:** single-file executables must carry the native
module. Two recipes to verify in wave 2: (a) embed the `.node` as an asset and
load it from the extraction dir at runtime; (b) ship the `.node`/cdylib next to
the compiled binary and `dlopen` by relative path. Bun's native-addon embedding
support is still maturing — the doc marker stays 🟢 until the recipe is proven
per-Bun-version.

## Flutter

**Desktop: 🟢 expected, unverified.** `dart:ffi` + `package:ffigen` generated
against the curated `bindings/rsac-ffi/include/rsac.h` gives Dart the full
56-function surface. The capture read loop runs on a Dart isolate calling the
blocking read, or uses the callback path via `NativeCallable.listener`. A
wave-2 spike produces the Dart package skeleton and a desktop capture smoke
test; publishing a `rsac_dart`/`rsac_flutter` package is a later decision.

**Mobile: 🟡 blocked on backends.** Once the backends exist, Flutter consumes
them through the same C FFI (rsac-ffi cross-compiled for
`aarch64-linux-android` / `aarch64-apple-ios`), plus a thin Flutter plugin for
the Android consent flow (backed by the `mobile/android/` AAR) and the iOS
broadcast-extension template from `mobile/ios/`.

## Android and iOS (all frameworks)

Full design: [`MOBILE_BACKEND_DESIGN.md`](MOBILE_BACKEND_DESIGN.md). The short
honest version:

- **Android — genuinely feasible, near-full tier coverage.**
  `AudioPlaybackCapture` (API 29+) supports system capture and per-app capture
  (`addMatchingUid`). Hard constraints: it is a **Java-only API** (no NDK
  equivalent — the capture loop must live in Kotlin/Java and push into rsac via
  JNI), it requires a **MediaProjection user-consent dialog** plus a foreground
  service of type `mediaProjection`, and apps targeting pre-Android-10 or
  setting `allowAudioPlaybackCapture=false` are **uncapturable**. Mic capture is
  pure-NDK (AAudio). CaptureTarget mapping is fixed in
  [ADR-0013](designs/0013-mobile-capturetarget-semantics.md).
- **iOS — structurally restricted by Apple.** Mic and own-app audio: fine
  (AVAudioEngine). **Capturing another app's audio is impossible for
  third-party apps — permanently, regardless of framework.** System-wide audio
  is reachable only via a ReplayKit **Broadcast Upload Extension**
  (user-initiated, separate extension process, ~50 MB memory cap, host app must
  embed an extension target). rsac wires that path as iOS `SystemDefault`
  (ADR-0013) and reports everything else honestly unsupported.

## Long tail (no commitments)

Brief honest paragraphs; **no seeds are filed for these** — they ride on the
same rsac-ffi / direct-dep paths as the frameworks above.

- **Rust-native GUIs (egui, Iced, Slint, GPUI):** trivially supported on
  desktop — rsac is a plain Rust crate with no UI assumptions; use the builder
  API directly and feed your render loop from `subscribe()`. Mobile follows the
  same 🟡 backend story as everything else.
- **React Native:** viable on paper via a TurboModule/JSI or
  [Nitro](https://nitro.margelo.com/)-style module over rsac-ffi, or UniFFI
  Swift/Kotlin bindings (still research-tier). Blocked on mobile backends like
  the rest; desktop RN (Windows/macOS) could reuse the napi path. Unverified,
  uncommitted.
- **Capacitor / Ionic:** a Capacitor plugin wrapping the same
  `mobile/android/` AAR + `mobile/ios/` SwiftPM glue would work once backends
  exist. Web builds get nothing (WASM ❌). Uncommitted.
- **.NET MAUI:** P/Invoke over the rsac-ffi cdylib is straightforward on
  desktop (the C surface was designed for exactly this class of consumer);
  mobile is the usual 🟡. Uncommitted.
- **Qt / C++ (and anything with C interop):** link `rsac_ffi`
  (cdylib/staticlib) against `rsac.h` — this is the C FFI's primary design
  target. Desktop 🟢; mobile 🟡. Uncommitted.

## Verification backlog

The 🟢→✅ flips and the mobile work are tracked as seeds in
`.seeds/issues.jsonl` (labels `xplat`, waves 2–5): Deno smoke test, Bun-compile
packaging recipe, Flutter desktop ffigen spike, Dioxus desktop example,
rsac-napi per-target dependency migration (prerequisite for mobile triples),
then the Android (wave 3) and iOS (wave 4) backends and `tauri-plugin-rsac`
(wave 5).
