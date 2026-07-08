# rsac — Cross-Platform Framework & Mobile Feasibility: Research, Documentation & Seed-Filing Plan

> **Scope guard:** This effort produces **research, documentation, ADRs, and seeds only**.
> No source code, no `Cargo.toml` changes, no CI changes, no binding changes. Implementation
> happens later via the seeds filed here (waves 2–5).

## Context (verified in-repo)

- rsac core is desktop-3 (WASAPI / PipeWire / CoreAudio). Any other target already compiles as an
  **honest stub**: `PlatformCapabilities::unsupported()` (src/core/capabilities.rs:248) +
  `AudioError::PlatformNotSupported` fallback arms (src/audio/mod.rs:83,124,239). The architecture
  is mobile-safe today; it just lacks mobile backends.
- Shipped bindings: C FFI (`bindings/rsac-ffi`, **54** extern fns — AGENTS.md still says 45),
  Python (PyO3 abi3), Node/Bun (napi-rs), Go (CGo). `docs/CROSS_LANGUAGE_BINDINGS.md` already rules:
  Electron main-process ✅ via napi; WASM ❌; Swift/Kotlin mobile = "research tier, needs new backends".
- `apps/audio-graph` (Tauri v2, submodule, not checked out here) consumes rsac as a **plain path
  dependency** in its Rust backend. No Tauri plugin structure exists anywhere in the org repos.
- Seeds convention: `.seeds/issues.jsonl`, ids `rsac-XXXX`, description ends with acceptance
  criteria + `(effort: … | verify: … | wave: N)` footer, plus labels. ADRs live in `docs/designs/`
  (0001–0011 taken; next free numbers are **0012, 0013, 0014**), MADR 3.0, ≥2 considered options.

## Decisions (resolved this session)

| # | Decision |
|---|---|
| D1 | **Full mobile push, batteries-included**: rsac owns the Android + iOS backends AND the Kotlin AAR + Swift package, so every framework (Tauri, Dioxus, Flutter, native) shares one core. |
| D2 | **CaptureTarget mobile mapping** — Android: `SystemDefault` = AudioPlaybackCapture all-UIDs; `Application`/`ApplicationByName` = `addMatchingUid` (package→UID); `ProcessTree` ≡ app via PID→UID (Android app processes share a UID — documented equivalence); `Device` = AAudio input (mic). iOS: `SystemDefault` = **ReplayKit Broadcast Upload Extension** path; `Device` = AVAudioEngine input (mic); `Application*`/`ProcessTree` = honestly unsupported (capabilities `false`, `PlatformNotSupported`). |
| D3 | This effort = research/documentation/planning/seed-filing **only**. |
| D4 | **Wave order**: W1 docs/ADRs/drift-fixes → W2 cheap framework verifications → W3 Android → W4 iOS → W5 tauri-plugin-rsac + audio-graph mobile adoption. |
| D5 | **Desk research now**; hands-on verification happens in W2 seeds. Every untested claim in the compatibility doc carries an explicit `expected, unverified` marker; W2 flips markers to `verified`. |
| D6 | **ReplayKit transport** = memory-mapped SPSC ring in the shared App Group container (extension = producer, host app = consumer; mirrors rtrb bridge semantics; Darwin notifications for control/liveness). |
| D7 | **Layout**: Rust backends in `src/audio/android/` + `src/audio/ios/` (module DAG unchanged); Kotlin/Swift glue in new top-level `mobile/android/` (Gradle → `rsac.aar`) + `mobile/ios/` (SwiftPM pkg incl. broadcast-extension template). `mobile/` added to the crates.io `exclude` list like `/apps`. |
| D8 | **Android consent flow**: Kotlin AAR helper runs the MediaProjection consent Intent → returns opaque token (JNI GlobalRef; opaque i64/pointer across FFI). Rust: `#[cfg(target_os="android")] AudioCaptureBuilder::with_android_projection(AndroidProjectionToken)`; `build()` preflight fails playback-capture targets without a token (ConfigurationError-class `AudioError`); mic/AAudio targets need no token. The AudioRecord read loop lives in Java (AudioPlaybackCapture has no NDK API) pushing into `BridgeProducer` via JNI. |
| D9 | Compatibility doc covers the six named frameworks in depth + a **brief long-tail section** (Rust-native GUIs egui/Iced/Slint, React Native, Capacitor, .NET MAUI, Qt/C++) — one honest paragraph each, **no seeds** for the long tail. |
| D10 | **Proposed (recorded, not committed)**: audio-graph keeps its direct rsac library dep on desktop (zero-IPC audio path); `tauri-plugin-rsac` is built as the mobile vehicle and reusable JS-facing API. Recorded as a *proposed* ADR. |

## Framework feasibility matrix (content basis for the doc)

| Framework | Desktop | Mobile | Integration path |
|---|---|---|---|
| Tauri v2 | ✅ verified (audio-graph) | 🟡 blocked on backends | direct Rust dep; mobile via `tauri-plugin-rsac` (W5) |
| Dioxus | 🟢 expected (plain Rust) | 🟡 blocked on backends | direct Rust dep; mobile host provides projection token per D8 recipe |
| Electron | ✅ documented (napi, main process) | n/a | `@rsac/audio` |
| Deno 2 | 🟢 expected, unverified | n/a | `npm:@rsac/audio` (Node-API compat) or `Deno.dlopen` over rsac-ffi cdylib |
| Bun / `bun --compile` | 🟢 expected (napi pipeline is Bun-first); compile = packaging caveat (`.node` must ship/embed) | n/a | `@rsac/audio` or `bun:ffi` |
| Flutter | 🟢 expected via `dart:ffi` + ffigen over `include/rsac.h` | 🟡 blocked on backends | new Dart package (W2 spike is desktop-only) |
| Long tail (D9) | brief honest paragraphs | — | no commitments |

**Mobile honesty anchors** (must appear verbatim-equivalent in the docs):
- Android AudioPlaybackCapture: API 29+, MediaProjection consent dialog + foreground service
  (`mediaProjection` type) required; apps targeting pre-Q or setting
  `allowAudioPlaybackCapture=false` are **uncapturable**; the capture loop is Java-only.
- iOS: capturing *other apps* is **impossible** for third-party apps. `SystemDefault` works only via
  the Broadcast Upload Extension (user-initiated, separate process, ~50 MB memory cap, consumer app
  must embed an extension target built from rsac's template). Mic + own-app audio are fine.

## W1 tasks (this effort's actual work — docs/ADRs/seeds only)

1. **`docs/FRAMEWORK_COMPATIBILITY.md`** (new): matrix above; per-framework integration recipe;
   library-vs-plugin guidance (D10); verified/expected/blocked status markers (D5); long-tail
   section (D9). Cross-link from README + CROSS_LANGUAGE_BINDINGS.md.
2. **`docs/MOBILE_BACKEND_DESIGN.md`** (new): Android + iOS backend architecture —
   JNI→`BridgeProducer` data path; D8 token flow + preflight; AAudio mic path (pure NDK;
   `reference/cpal` is a working example); PID→UID equivalence note; iOS AVAudioEngine mic path;
   ReplayKit extension + D6 mmap-ring transport design (layout, framing, liveness); new
   `PlatformCapabilities` fields (at minimum `requires_user_consent: bool`); module layout per D7;
   mobile CI strategy (cargo-ndk `aarch64-linux-android` + `aarch64-apple-ios` check builds first,
   emulator/device testing later); explicit non-goals (iOS per-app capture).
3. **ADR-0012 — Mobile platform strategy & packaging** (accepted): batteries-included D1/D7 vs
   plugin-owned glue vs separate repos.
4. **ADR-0013 — Mobile CaptureTarget semantics** (accepted): D2 mapping incl. ReplayKit-as-
   SystemDefault and D6 transport; options: honest-limited iOS vs ReplayKit-wired vs mic-fallback
   (rejected as dishonest).
5. **ADR-0014 — Tauri integration model** (proposed): D10; options: direct dep only vs plugin for
   mobile+JS API vs migrate audio-graph fully onto plugin.
6. **Doc-drift fixes** found during research: AGENTS.md "45 extern C functions" → 54;
   CROSS_LANGUAGE_BINDINGS.md stale claim that rsac-ffi uses per-target dep blocks (it uses a single
   `default-features = false` dep) and stale cbindgen double-prefix warning (fixed by rsac-1413);
   update CROSS_LANGUAGE_BINDINGS.md mobile section from "research tier" to link the new design doc.
7. **File the seeds below** into `.seeds/issues.jsonl` (repo seeds tooling; conform to the existing
   schema: acceptance criteria + `(effort | verify | wave)` footer + labels).

## Seeds to file (implementation backlog, worked later)

| Wave | Seed title | Acceptance sketch | Labels |
|---|---|---|---|
| 2 | Deno 2 smoke test for @rsac/audio | `deno run` with `npm:@rsac/audio` + `Deno.dlopen` fallback path exercised on one desktop OS; matrix marker flipped to verified; findings appended to FRAMEWORK_COMPATIBILITY.md | bindings, napi, docs |
| 2 | bun --compile packaging test | single-file executable embedding/co-shipping the `.node`; document the working recipe; marker flipped | bindings, napi, docs |
| 2 | Flutter desktop ffigen spike | Dart package skeleton generated from `include/rsac.h`; capture smoke test on one desktop OS; publish decision deferred | bindings, ffi, docs |
| 2 | Dioxus desktop example | example app using rsac directly (desktop); recipe added to compatibility doc | docs, examples |
| 2 | rsac-napi per-target dep migration | migrate to `[target.'cfg(...)']` rsac blocks like rsac-python (prereq for mobile triples) | bindings, napi, tech-debt |
| 3 | Android AAudio mic backend | `src/audio/android/` + `PlatformStream` impl via BridgeStream; `Device` target only; cfg-gated; capabilities honest | android, backend |
| 3 | Android playback-capture backend + JNI ingest | Java AudioRecord loop in AAR pushes via JNI into `BridgeProducer` (`push_samples_or_drop`); D2 target mapping; D8 token preflight | android, backend, jni |
| 3 | mobile/android Gradle AAR | consent-Intent helper returning opaque token; foreground service (mediaProjection type); AAR build in CI | android, packaging |
| 3 | Android cross-compile CI job | cargo-ndk check build for `aarch64-linux-android` (+ AAR assemble); no emulator yet | android, ci |
| 4 | iOS AVAudioEngine mic backend | `src/audio/ios/`; `Device` target; capabilities honest (`supports_application_capture:false`) | ios, backend |
| 4 | mobile/ios SwiftPM package + broadcast-extension template | AVAudioSession helpers; extension template writing D6 mmap SPSC ring in App Group container | ios, packaging |
| 4 | iOS ReplayKit SystemDefault wiring | host-side ring consumer bridged into BridgeStream; Darwin-notification liveness; documented UX constraints | ios, backend |
| 4 | iOS cross-compile CI job | `aarch64-apple-ios` check build on macOS runner | ios, ci |
| 5 | tauri-plugin-rsac | thin plugin: mobile consent flow via AAR/SwiftPM, JS guest API (start/stop/subscribe events) with Tauri permissions; desktop passthrough | tauri, integrations |
| 5 | audio-graph mobile adoption decision | revisit ADR-0014 with plugin in hand; audio-graph stays direct-dep on desktop | tauri, downstream |

## Risks / constraints to record in the docs

- AudioPlaybackCapture is Java-only → the Android RT path crosses JNI; RT-safety guarantee (ADR-0001)
  must be re-stated for the JNI ingest boundary (Java thread is not an OS RT audio thread).
- ReplayKit: user can stop the broadcast anytime → stream must terminate with the ADR-0003 terminal
  error semantics; 50 MB extension memory cap bounds the ring size.
- App Store / Play Store review implications (foreground service types, broadcast extensions) —
  document, don't solve.
- `bun --compile` native-addon embedding is still maturing; recipe may need per-Bun-version notes.
- iOS per-app capture is a permanent "no" — never soften this in docs.

## Validation (for the implementing agent)

- Docs: every claim carries verified/expected/blocked status; mobile claims cross-checked against
  current Apple/Google docs (AudioPlaybackCaptureConfiguration, RPBroadcastSampleHandler) at
  writing time; no capability overstated (§7 honesty rule).
- ADRs: MADR 3.0, ≥2 options, index updated, ADR-0014 marked *proposed*.
- Seeds: parse as valid JSONL, schema-conformant (footer + labels), wave numbers per D4.
- `cargo check` untouched — no source files modified besides docs (drift fixes in §W1.6 are
  markdown-only).

## Out of scope (explicit)

- Any implementation code, CI changes, binding code, or `Cargo.toml` edits (beyond none at all).
- WASM (already ruled out), React Native / MAUI seeds (long-tail doc coverage only, D9).
- UniFFI Swift/Kotlin *bindings* (distinct from backend glue) — stays research-tier; revisit after W4.
- crates.io publish, emulator/device farm CI, live per-source gain (tracked elsewhere).
