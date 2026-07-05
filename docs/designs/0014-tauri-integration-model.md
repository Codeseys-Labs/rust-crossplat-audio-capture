# ADR 0014 — Tauri integration model: direct library dependency on desktop, `tauri-plugin-rsac` as the mobile vehicle

**Status:** Proposed
**Date:** 2026-07-04
**Scope:** a future `tauri-plugin-rsac` crate (location TBD at implementation
— candidate: a new `integrations/` top-level dir, excluded from the crates.io
package), `apps/audio-graph` integration guidance,
[`docs/FRAMEWORK_COMPATIBILITY.md`](../FRAMEWORK_COMPATIBILITY.md) (Tauri
section).
**Verdict (proposed):** Tauri v2 apps on **desktop** should consume rsac as a
**direct Rust dependency** — audio-graph's current integration is the
recommended shape and does not change. A **`tauri-plugin-rsac`** is built in
wave 5 as (a) the sanctioned vehicle for the mobile consent flow (wrapping the
first-party `mobile/android/` AAR and `mobile/ios/` SwiftPM glue from
ADR-0012 into Tauri's plugin packaging), and (b) an optional JS guest API
(invoke/events: start/stop/subscribe, meter events) governed by Tauri's
permission system. audio-graph adopts the plugin **only if/when it targets
mobile**.

> Proposed, not accepted: the plugin does not exist yet, and the final call on
> its API surface and audio-graph's adoption belongs to the wave-5
> implementation once the mobile backends (waves 3–4) are real. Flip to
> Accepted (or supersede) at that point.

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

### Option A — Direct dep on desktop; plugin as mobile vehicle + optional JS API (proposed)

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

## 4. Decision (proposed)

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
