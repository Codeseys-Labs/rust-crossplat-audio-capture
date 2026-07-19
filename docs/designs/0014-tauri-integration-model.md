# ADR 0014 — Tauri integration model: direct library dependency on desktop, `tauri-plugin-rsac` as the mobile vehicle

**Status:** Accepted
**Date:** 2026-07-04 (accepted 2026-07-18)
**Scope:** the `tauri-plugin-rsac` crate at **`integrations/tauri-plugin-rsac`**
(a new `integrations/` top-level dir, excluded from the crates.io package,
joined to the version-lockstep set), `apps/audio-graph` integration guidance,
[`docs/FRAMEWORK_COMPATIBILITY.md`](../FRAMEWORK_COMPATIBILITY.md) (Tauri
section).
**Verdict (accepted):** Tauri v2 apps on **desktop** should consume rsac as a
**direct Rust dependency** — audio-graph's current integration is the
recommended shape and does not change. A **`tauri-plugin-rsac`** is built in
wave 5 as (a) the sanctioned vehicle for the mobile consent flow (wrapping the
first-party `mobile/android/` AAR and `mobile/ios/` SwiftPM glue from
ADR-0012 into Tauri's plugin packaging), and (b) an optional JS guest API
(invoke/events: start/stop/subscribe, meter events) governed by Tauri's
permission system. audio-graph adopts the plugin **only if/when it targets
mobile**.

> **Accepted at wave-5 implementation (rsac-f21c).** The plugin ships
> compile-proof: desktop builds + clippies in the workspace CI; `android/` +
> `ios/` are source-shipped, bridging the ADR-0012 AAR / SwiftPM glue; mobile
> **runtime** verification remains blocked on rsac-e6d3 / rsac-97c8 (unchanged
> from ADR-0012's honesty posture). audio-graph's desktop integration is
> unchanged.

## 1. Context

The `apps/audio-graph` Tauri v2 app consumes rsac as a plain path dependency
in its Rust backend and processes audio entirely in Rust, emitting only
derived results to the webview. The project owner asked which integration is
"better": library or plugin. Meanwhile the mobile push (ADR-0012/0013)
introduces Kotlin/Swift glue and a consent flow that Tauri apps need — and
Tauri v2 plugins are the framework's sanctioned way to ship
Android/iOS-native code and permissions into an app.

## 2. Decision drivers

- **Audio data must not cross IPC.** Raw f32 at 48 kHz through Tauri's
  JSON-serialized `emit()` is wasteful; the desktop-optimal path is Rust-side
  consumption — which the direct dependency already gives.
- **Mobile needs a vehicle.** Tauri mobile apps get platform-native code via
  plugins; hand-wiring the AAR/SwiftPM per app is the alternative and it
  duplicates consent plumbing per consumer.
- **Reusability.** A JS-facing capture API with Tauri permissions benefits
  future Tauri consumers beyond audio-graph.
- **Don't build ahead of need.** A desktop-only plugin today would wrap an
  API its one consumer (audio-graph) already uses more directly.

## 3. Considered options

### Option A — Direct dep on desktop; plugin as mobile vehicle + optional JS API (accepted)

- ✅ Desktop keeps the zero-IPC, full-API integration that already ships.
- ✅ Plugin work lands exactly when it has value (mobile glue exists to wrap).
- ✅ JS API + permissions give lighter Tauri apps a no-Rust option, with the
  documented caveat that heavy audio processing belongs on the Rust side.
- ➖ Two documented integration modes (dep vs plugin) — the compatibility doc
  must say clearly when to use which.

### Option B — Plugin-only: migrate audio-graph onto `tauri-plugin-rsac` everywhere

- ✅ One uniform integration story.
- ❌ Desktop regression: audio buffers cross the plugin's event boundary or
  need a side-channel; audio-graph loses direct access to the full Rust API
  (compose, sinks, introspection) for no functional gain.
- ❌ Makes the plugin a hard dependency of the flagship consumer before the
  plugin has proven its API.

### Option C — No plugin: documented manual recipe per app

- ✅ Nothing new to maintain.
- ❌ Every Tauri mobile consumer re-wires the AAR/SwiftPM glue, consent
  Intent, and foreground service by hand — precisely the duplication
  ADR-0012's batteries-included stance exists to prevent.
- ❌ No JS-facing API for non-Rust Tauri teams.

## 4. Decision (accepted)

**Option A.** Sub-points for the wave-5 implementer:

1. The plugin is a **thin adapter**: consent flow + lifecycle commands +
   subscription events. No capture policy, no backend logic — those stay in
   rsac (ADR-0012 ownership boundary).
2. Event payloads carry **derived data by default** (meters, format, stats);
   raw-buffer delivery to JS is opt-in and documented as the slow path.
3. Desktop support in the plugin is a passthrough so a single app codebase
   can use the plugin API uniformly if it chooses — but the compatibility doc
   keeps recommending the direct dependency for Rust-heavy desktop apps.
4. audio-graph: no change now; adopt the plugin only when it targets mobile.

## 5. Consequences

- audio-graph's shipped integration is validated as the recommended desktop
  shape — no migration churn.
- **Negative:** until wave 5, Tauri mobile has no story beyond "wait" — the
  compatibility doc must keep its 🟡 markers honest.
- **Negative:** once built, the plugin is a second public API surface to
  version, document, and keep in lockstep with rsac releases (it joins the
  version-lockstep set).
- Neutral: Dioxus/Flutter consumers are unaffected — they consume the AAR /
  SwiftPM glue directly per ADR-0012.

## 6. Implementation notes (accepted)

Deltas discovered during the wave-5 implementation (rsac-f21c):

1. **Location confirmed `integrations/`** (was a candidate at proposal time):
   the crate lives at `integrations/tauri-plugin-rsac`, is added to
   `[workspace].members`, and is excluded from the crates.io `rsac` package via
   `[package].exclude += "/integrations"`. Workspace membership is orthogonal
   to packaging (bindings already do this).
2. **Derived-data-by-default is enforced at the *permission layer*, not just by
   convention.** `subscribe_raw` is excluded from the default permission set, so
   raw interleaved-f32 samples require an explicit `allow-subscribe-raw` grant
   in the host's capability file. The default `rsac://chunk-meta` path carries
   only derived meters/format (computed Rust-side, alloc-free, before the buffer
   is dropped — the napi `ChunkMeta` precedent).
3. **Plugin identifier is `rsac`** → the invoke namespace is `plugin:rsac|*` and
   the event channels use the `rsac://…` scheme (`rsac://chunk-meta`,
   `rsac://chunk-raw`, and the reserved-not-yet-emitted `rsac://stats` — its
   `StreamStatsInfo` wire shape is defined but no code path emits it yet).
4. **Desktop `request_consent` is a no-op success** (desktop
   `requires_user_consent == false`, `src/core/capabilities.rs`), keeping the JS
   API uniform across platforms (§4.3 passthrough). iOS likewise resolves
   success — its consent artifact is the App Group id supplied Rust-side
   (ADR-0013), not a native dialog.
5. **The Android consent bridge is a thin forwarder onto `RsacProjection`** — it
   inherits PR#64's deferred-FGS-acquire ordering rather than re-implementing
   it. The Kotlin `RsacTauriPlugin` never starts the `mediaProjection` FGS
   itself (pre-consent typed-FGS start → `SecurityException` on API 34+).
6. **Compile-proof shipping.** Desktop builds + clippies in the workspace CI;
   `android/` (Gradle) + `ios/` (SwiftPM) are source-shipped and the
   `#[cfg(mobile)]` Rust is cross-checked for both mobile triples. Mobile
   *runtime* verification (consent → capture → meter on device) is deferred to
   rsac-e6d3 / rsac-97c8 and filed as a follow-up example-app seed.
