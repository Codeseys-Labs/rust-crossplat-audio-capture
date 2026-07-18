# Framework Compatibility

> **Status legend (honesty contract):**
> ✅ **verified** — proven by a shipped consumer or an executed test.
> 🟢 **expected, unverified** — API-level desk research says it works; no hands-on
> verification has been run yet. Markers flip to ✅ when a real downstream
> integration verifies them (see [Verification policy](#verification-policy)).
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
| **Tauri v2** | ✅ verified (audio-graph) | 🟡 blocked on runtime proof | 🟡 blocked on runtime proof | Direct Rust dependency; mobile via `tauri-plugin-rsac` (ships compile-proof, [ADR-0014](designs/0014-tauri-integration-model.md) Accepted) |
| **Dioxus** | 🟢 expected | 🟡 blocked on runtime proof | 🟡 blocked on runtime proof | Direct Rust dependency (rsac is UI-framework-agnostic) |
| **Electron** | ✅ documented (napi, main process) | n/a | n/a | `@rsac/audio` (napi-rs) |
| **Deno 2** | 🟢 expected | n/a | n/a | `npm:@rsac/audio` (Node-API compat) or `Deno.dlopen` over rsac-ffi |
| **Bun / `bun --compile`** | 🟢 expected (napi CI is Bun-first); compile has a packaging caveat | n/a | n/a | `@rsac/audio` or `bun:ffi` over rsac-ffi |
| **Flutter** | 🟢 expected via `dart:ffi` + ffigen | 🟡 blocked on runtime proof | 🟡 blocked on runtime proof | New Dart package over `bindings/rsac-ffi/include/rsac.h` |
| Rust-native GUIs (egui/Iced/Slint/GPUI) | 🟢 expected (trivially) | 🟡 | 🟡 | Direct Rust dependency |
| React Native | 🟢 expected on paper (JSI/Nitro or uniffi over rsac-ffi) | 🟡 | 🟡 | Long tail — no commitment |
| Capacitor / Ionic | — | 🟡 | 🟡 | Long tail — no commitment |
| .NET MAUI | 🟢 expected (P/Invoke over rsac-ffi) | 🟡 | 🟡 | Long tail — no commitment |
| Qt / C++ | 🟢 expected (direct C FFI) | 🟡 | 🟡 | Long tail — no commitment |
| Browser / WASM | ❌ not viable | ❌ | ❌ | Ruled out in [`CROSS_LANGUAGE_BINDINGS.md`](CROSS_LANGUAGE_BINDINGS.md#5-wasm--not-viable-for-capture) |

**The single most important row-shape:** every 🟡 in the Android/iOS columns is
the *same* blocker — rsac's mobile backends are not runtime-verified yet.
Status: the **mobile backends are code-complete and compile-checked** — the
microphone slices (`feat_android` AAudio / `feat_ios` AVAudioEngine), the iOS
`SystemDefault` broadcast consumer (ReplayKit ring), and Android playback
capture — what frameworks want for "system audio" on Android — all four
`AudioPlaybackCapture` tiers via the AAR Kotlin loop + JNI ingest (rsac-77f1)
— with **zero runtime verification on any device** (seeds
rsac-e6d3/rsac-97c8). Once runtime verification lands, every 🟡 above
resolves through its listed integration path. Conversely, the ❌ cells are
Apple/Google/browser policy, and **no framework choice changes them**.

## Tauri v2

**Desktop: ✅ verified.** [AudioGraph](https://github.com/Codeseys-Labs/audio-graph)
(the `apps/audio-graph` standalone checkout, not a tracked submodule) is a shipping Tauri v2 app that consumes rsac
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
as **Accepted** in [ADR-0014](designs/0014-tauri-integration-model.md):
audio-graph keeps its direct dependency on desktop; the plugin ships as the
mobile vehicle (wave 5). Derived-data-by-default is enforced via the plugin's
permission set — raw-sample delivery (`subscribe_raw`) is opt-in, requiring an
explicit `allow-subscribe-raw` grant.

**Mobile: 🟡 blocked on runtime proofs.** Tauri v2 builds for Android/iOS today, and
rsac already *compiles* for those targets. The consent flow (Android
MediaProjection) ships in `tauri-plugin-rsac`
(`integrations/`, [ADR-0014](designs/0014-tauri-integration-model.md) Accepted)
— **compile-shipping today; mobile runtime still blocked on rsac-e6d3 /
rsac-97c8**. The plugin wraps `mobile/android/`'s `RsacProjection` consent flow
and `mobile/ios/` glue behind Tauri commands/permissions (a thin forwarder — it
inherits PR#64's deferred foreground-service-acquire ordering, adding no capture
policy of its own).

## Dioxus

**Desktop: 🟢 expected, unverified.** Dioxus desktop apps are plain Rust
processes (wry webview, like Tauri) — rsac needs nothing framework-specific.
Spawn capture on a background thread/task, feed UI state via Dioxus signals.
The marker flips to ✅ when a downstream Dioxus integration verifies it (see
[Verification policy](#verification-policy)).

**Mobile: 🟡 blocked on runtime proofs.** Dioxus has no plugin system like Tauri's, so
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

Open questions for whoever verifies first: ThreadsafeFunction callback
delivery under Deno's event loop (the `onData` push path), and BigInt counter
round-trip. Until a downstream runs it, this row stays 🟢.

## Bun and `bun --compile`

**🟢 expected.** Bun runs Node-API addons, and rsac's own npm release pipeline
is already Bun-first (`bun install`, `bunx @napi-rs/cli` — see
`.github/workflows/release-npm.yml`), so plain `bun add @rsac/audio` is expected
to work. `bun:ffi` (`dlopen`) over rsac-ffi is the alternative path.

**`bun --compile` caveat:** single-file executables must carry the native
module. Two recipes for whoever verifies first: (a) embed the `.node` as an
asset and load it from the extraction dir at runtime; (b) ship the
`.node`/cdylib next to the compiled binary and `dlopen` by relative path.
Bun's native-addon embedding support is still maturing — the doc marker stays
🟢 until the recipe is proven per-Bun-version.

## Flutter

**Desktop: 🟢 expected, unverified.** `dart:ffi` + `package:ffigen` generated
against the curated `bindings/rsac-ffi/include/rsac.h` gives Dart the full
56-function surface. The capture read loop runs on a Dart isolate calling the
blocking read, or uses the callback path via `NativeCallable.listener`. We
provide guidance when a downstream Flutter project integrates; publishing a
`rsac_dart`/`rsac_flutter` package is a later decision.

**Mobile: 🟡 compiled, not runtime-verified.** The consumption path exists
end-to-end at the compile level (rsac-7a18): rsac-ffi carries
`feat_android`/`feat_ios` passthrough features and **cross-checks green for
both mobile triples in CI**; the C header is target-independent. Recipe:
**Android** — cdylib in the app's `jniLibs` (coordinate with the
`mobile/android-native` shim that already produces `librsac.so` for the AAR,
rsac-0aa9); **iOS** — build rsac-ffi as a staticlib for `aarch64-apple-ios`
and bind via `dart:ffi`, plus a thin Flutter plugin for the Android consent
flow (backed by the `mobile/android/` AAR) and the iOS broadcast-extension
template from `mobile/ios/`. Runtime verification tracks
rsac-e6d3/rsac-97c8 — until those are green, treat mobile Flutter capture as
unproven.

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

## Verification policy

**Runtime/framework verification is guidance-on-demand, not committed rsac
work** (owner decision, 2026-07-05): when a downstream project integrates via
Deno, Bun, Flutter, Dioxus, or a long-tail framework, we support them with the
recipes in this document and flip the 🟢 markers to ✅ from their verified
results — we don't run speculative verification ourselves. (The original
wave-2 verification seeds were closed as not-planned with this rationale.)

The committed backlog is the **mobile push**, tracked as `sd` epics:
umbrella `rsac-0991` → Android backend `rsac-5823` (wave 3), iOS backend
`rsac-57cb` (wave 4), framework delivery / `tauri-plugin-rsac` `rsac-71d2`
(wave 5). The core consent surface (rsac-82d4) and the rsac-napi per-target
dependency migration (rsac-e8a3) are already landed. The wave-5
`tauri-plugin-rsac` **skeleton landed compile-proof** (rsac-f21c,
`integrations/`) — desktop builds/clippies in CI, `android/`+`ios/` are
source-shipped, and derived-data-by-default is enforced via the permission set
(raw opt-in); framework **runtime** verification remains guidance-on-demand and
mobile runtime stays blocked on rsac-e6d3 / rsac-97c8.
