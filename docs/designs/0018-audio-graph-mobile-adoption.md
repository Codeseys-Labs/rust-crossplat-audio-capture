# ADR 0018 — audio-graph mobile adoption: defer, keep desktop-only, record re-evaluation triggers

**Status:** Proposed (awaiting owner ratification)
**Date:** 2026-07-20
**Scope:** `apps/audio-graph` (standalone checkout, gitignored per `/apps/` —
not a tracked submodule) integration posture; relationship to
[ADR-0014](0014-tauri-integration-model.md) (`tauri-plugin-rsac` accepted as
the Tauri mobile vehicle) and
[`docs/FRAMEWORK_COMPATIBILITY.md`](../FRAMEWORK_COMPATIBILITY.md) (Tauri
row). Does not change rsac's mobile backend scope (ADR-0012/0013) or the
plugin's shipped shape (ADR-0014) — this ADR answers only whether
audio-graph, the one flagship consumer, should target mobile now.
**Verdict (proposed):** **Option B — defer.** audio-graph stays desktop-only.
No adoption work is scheduled. The decision is re-opened only when one of the
concrete triggers in §5 fires, not on a calendar.

## 1. Context

`tauri-plugin-rsac` (`integrations/tauri-plugin-rsac`) now exists and ships
compile-proof per ADR-0014: a mobile consent flow (`request_consent` forwards
to the Kotlin `RsacTauriPlugin` → `RsacProjection.request`, threading the
`MediaProjection` token onto the capture builder — see
`integrations/tauri-plugin-rsac/src/mobile.rs`), session/lifecycle commands
(`start_capture`, `stop_capture`, `subscribe_meta`, `subscribe_raw`,
`list_targets`, `capabilities`), and Android + iOS source trees
(`integrations/tauri-plugin-rsac/{android,ios}`).

Per `AGENTS.md`'s honesty-contract mobile matrix, the underlying rsac mobile
backends are **emulator/simulator-verified, not real-device-verified**:
Android mic capture and Android `SystemDefault` playback capture are
emulator-verified (PRs #66/#67/#68); iOS mic is simulator-verified (PR #67);
UID-filtered Android playback tiers beyond self-capture, iOS ReplayKit
playback (`SystemDefault`), and every real-**device** cell remain open
(rsac-e6d3, rsac-97c8, rsac-b3aa). `tauri-plugin-rsac` itself has **zero
runtime verification** of its own — no consent-dialog-to-capture-to-meter
round trip has been driven through the plugin on a device or emulator; only
compile/clippy across the mobile triples is proven today.

rsac-0ac9 asks: now that the plugin exists, should audio-graph (the project's
one shipping Tauri consumer, verified on desktop) adopt it and target
Android/iOS? ADR-0014 already fixed the desktop answer (direct dependency,
unchanged); this ADR is scoped narrowly to the mobile half of that question,
which ADR-0014 explicitly deferred ("audio-graph adopts the plugin only
if/when it targets mobile").

The project memory also carries a **provider-architecture goal**: every
pipeline stage (ASR, diarization, extraction, chat) should have a
local + cloud alternative. That goal is orthogonal to platform targeting —
it constrains audio-graph's *pipeline* design, not whether it ships on
mobile — but it does add mobile-adoption cost, addressed in §3.

## 2. Options considered

### Option A — Adopt now: audio-graph targets Android/iOS via the plugin

audio-graph would add `tauri-plugin-rsac` as a dependency, wire the JS guest
API (`requestConsent`, `startCapture`/`stopCapture`, `subscribeMeta`) into its
React frontend, stand up `android/` + `ios/` Tauri mobile scaffolding
(`tauri android init` / `tauri ios init`), and ship store-distributable
builds.

### Option B — Defer: audio-graph stays desktop-only; record re-evaluation triggers (recommended)

No code changes to audio-graph. This ADR's job is solely to make the
"not now" decision durable, legible, and revisit-able — closing rsac-0ac9
without silently letting the question re-litigate itself every wave.

### Option C — Adopt Android-only first (partial adoption)

Target Android via the plugin (mic + consent-gated `SystemDefault` playback)
while leaving iOS for later, on the theory that Android's MediaProjection
consent flow is further along (emulator-verified end-to-end for
mic + `SystemDefault`) than iOS's ReplayKit path (`SystemDefault` is
compiled-unverified, rsac-b3aa).

## 3. Analysis

### 3.1 Does audio-graph's UI even suit mobile?

Read `apps/audio-graph/src` directly (not inferred). The layout is a
**three-pane desktop workspace**: a left source-picker column
(`AudioSourceSelector.tsx` — four grouped categories: System / Devices /
Applications / Running Processes, each row togglable, a search filter across
all groups), a center knowledge-graph viewer (`KnowledgeGraphViewer.tsx`) and
live transcript pane (`LiveTranscript.tsx`), and a right `ChatSidebar.tsx`.
The Tauri window is fixed at `1400×900` with `resizable: true` but no
mobile/compact breakpoint (`src-tauri/tauri.conf.json`'s only `app.windows`
entry). A repo-wide grep for `@media`, `useMediaQuery`, or an `isMobile` flag
across `src/**/*.{tsx,css}` returns **zero matches** — there is no responsive
layout today, at any breakpoint. `AudioSourceSelector`'s "Applications" and
"Running Processes" groups are meaningful largely on desktop, where
processes and per-app audio streams are user-legible concepts; on a phone,
"which of these 40 running processes is making sound" is not a UI a user
would want. This is a real, verified cost, not a hypothetical one: shipping
on mobile is not "add two build targets," it is a second UI information
architecture for the source picker plus a responsive rework of a
three-pane desktop layout that has never been built for a small screen.

### 3.2 What would the Android consent flow look like through the plugin?

Concretely, via `integrations/tauri-plugin-rsac/src/mobile.rs`: the frontend
calls the guest-JS `requestConsent()` (`guest-js/index.ts:89`), which invokes
`plugin:rsac|request_consent`; on Android this forwards through
`PluginHandle::run_mobile_plugin_async` to the Kotlin `RsacTauriPlugin`,
which is a thin forwarder onto `RsacProjection.request` — the same
`MediaProjection` system dialog every Android app doing screen/audio capture
shows (a modal "Start recording or casting?" overlay owned by the OS, not by
Tauri or rsac). A granted response carries a `jlong` `MediaProjection` token
that the plugin wraps once (`AndroidProjectionToken::from_raw`) and threads
onto `start_capture`'s builder. This is a normal, well-understood Android
UX pattern — the cost here is not novel UI design, it is that **nobody has
driven this flow end-to-end on a device or emulator through the plugin**
(the plugin's own runtime gap, distinct from and stacked on top of the
mobile backend's device-verification gap). iOS has **no consent dialog at
all** in the current implementation — `request_consent` on iOS returns
`granted: false` unconditionally with an explanatory `reason`
(`mobile.rs:90-102`, "consent is not applicable on iOS ... system capture
uses the broadcast extension path"), because the real iOS mechanism is a
ReplayKit Broadcast Upload Extension the user starts from Control Center,
not an in-app dialog — and that extension-embedding step is itself
unshipped for audio-graph (§3.3).

### 3.3 What does Tauri v2 mobile actually require?

Cited from general Tauri v2 knowledge, **flagged uncertain** where this ADR
does not have primary-source confirmation in-repo (Context7 lookup was
attempted and returned a quota error, so these are not doc-verified in this
pass — the owner or a follow-up seed should confirm before relying on any
specific version number):

- **Android:** `tauri android init` scaffolds a Gradle project
  (`gen/android/`) requiring an Android SDK + NDK install, a configured
  `compileSdk`/`targetSdk` (rsac's own `mobile/android/build.gradle.kts` uses
  `compileSdk = 35`, `minSdk = 29` — 🟡 *uncertain whether audio-graph's
  generated Tauri Android project would need to match these exactly or can
  diverge; the plugin's own AAR and the app's generated shell are separate
  Gradle modules*), a signing keystore for release builds, and either
  `cargo mobile` tooling or Android Studio for builds. `MediaProjection`
  additionally requires a targeted foreground-service type
  (`mediaProjection`) declared in the manifest — already handled by rsac's
  `mobile/android/src/main/AndroidManifest.xml` per the plugin's inherited
  FGS ordering, but audio-graph's *own* app manifest (generated by
  `tauri android init`) would need the corresponding permissions merged in.
- **iOS:** `tauri ios init` generates an Xcode project (`gen/apple/`)
  requiring a paid Apple Developer account for device deployment and App
  Store distribution (a free account can build to a simulator only — 🟡
  *uncertain exact current Apple policy, verify before committing to a
  release timeline*), a provisioning profile, and — specific to rsac's iOS
  system-audio path — an **embedded Broadcast Upload Extension target**
  (ADR-0013) plus an **App Group** shared container
  (`with_ios_app_group`) wired between the host app and the extension. This
  is not generated by `tauri ios init`; it is manual Xcode target
  configuration that audio-graph does not have today, and that
  `tauri-plugin-rsac`'s current iOS half does not automate (§3.2's "iOS stub"
  note in `mobile.rs`: no native plugin is registered on iOS because the
  broadcast path needs no dialog, but the extension embedding is still the
  consuming app's job).
- **Both platforms:** app store review (Google Play consent/policy review
  for `MediaProjection` "screen recording" classification; App Store review
  for a broadcast-extension audio-capture app) is a recurring release-gate
  cost with no desktop analogue, plus ongoing OS-version compatibility
  maintenance (Android SDK bumps, iOS SDK bumps) that today's desktop-3
  CI matrix does not carry.

### 3.4 What audio-graph would gain

- A mobile build target for a project whose current identity (Whisper/
  llama.cpp/Sherpa local models, AWS/Groq/Deepgram/Gemini cloud providers,
  a knowledge-graph viewer) is desktop-workstation-shaped — heavy local
  model inference (`whisper-rs`, `llama-cpp-2`) is a poor fit for phone
  thermal/battery/storage budgets without a substantial rework of the
  provider-architecture's local tier for mobile.
- Zero-IPC desktop path is unaffected either way — ADR-0014 §4.3 already
  guarantees the plugin's desktop passthrough doesn't regress the direct
  dependency, so "adopt now" carries no desktop downside beyond the added
  dependency surface.

### 3.5 What audio-graph would cost

- A second UI (§3.1): responsive layout, a mobile-appropriate source picker,
  touch-first interaction for start/stop and the knowledge-graph viewer.
- A second consent/runtime-verification lift stacked on top of rsac's own
  open device-verification gaps (rsac-e6d3, rsac-97c8, rsac-b3aa) — audio-graph
  would be the first consumer to exercise `tauri-plugin-rsac`'s mobile commands
  at all, on backends that are themselves not device-verified.
- Release/store maintenance overhead (§3.3) with no current user asking for
  it — no GitHub issue, no support request, no roadmap commitment in
  audio-graph's own README mentions mobile.
- Maintenance surface for the provider architecture's local-model tier under
  mobile constraints (§3.4) — a cost the project's own goals (memory:
  "every pipeline stage should have local + cloud alternatives") already
  point at as unresolved for mobile, independent of rsac.

### 3.6 Why not Option C (Android-only)?

Android-only adoption is a smaller slice of Option A's cost (§3.3's iOS
extension-embedding tax and app-store-review tax disappear), and Android's
consent path is further along in verification than iOS's. But it does not
remove the two largest costs: audio-graph's UI is not mobile-shaped on
*either* platform (§3.1), and no user has asked for mobile audio-graph on
*either* platform. Option C would spend real engineering effort (Gradle
scaffolding, manifest merge, a first mobile-shaped source-picker UI, driving
the plugin's consent flow live for the first time ever) against a backend
that itself has open device-verification gaps, for a platform nobody has
requested. It only makes sense as a *first step toward* Option A, and the
analysis in §3.1/3.4/3.5 that argues against Option A applies just as much
to doing half of it first. Rejected for the same reasons as Option A, one
platform at a time.

## 4. Recommendation

**Option B — defer.** audio-graph remains desktop-only. `tauri-plugin-rsac`
continues to exist and ships (ADR-0014, unchanged) as the sanctioned vehicle
for *whichever* Tauri app targets mobile first — that consumer does not have
to be audio-graph, and building audio-graph's mobile support now would be
speculative engineering against a UI that was never designed for it, a
plugin that has never been runtime-exercised, and mobile backends that are
not device-verified.

This does **not** amend or supersede ADR-0014's decision (plugin exists,
desktop direct-dependency unchanged, plugin is the mobile vehicle
*if/when* audio-graph or another consumer targets mobile) — it answers the
question ADR-0014 explicitly left open by recording "not now, and here is
what would change that."

## 5. Re-evaluation triggers

Re-open this decision (new ADR or an amendment to this one — do not just
start coding) when **any** of the following becomes true:

1. **A concrete mobile user request** — an audio-graph issue, a support
   request, or a project-owner directive asking for Android/iOS capture,
   as opposed to this ADR's own speculative "should we."
2. **The plugin gets an external consumer** — a Tauri app other than
   audio-graph adopts `tauri-plugin-rsac` for mobile and drives its consent
   flow live (device or emulator), which would retire the "plugin has zero
   runtime verification" cost noted in §3.5 for whoever adopts next,
   including audio-graph.
3. **rsac's own mobile device-verification gaps close** — rsac-e6d3 and
   rsac-97c8 (real-device mic/playback capture) and rsac-b3aa (iOS ReplayKit
   `SystemDefault`) land, removing the "adopting on top of an unverified
   backend" risk layer.
4. **audio-graph's provider architecture gets a mobile-viable local tier** —
   if the local-model stage (Whisper/llama.cpp/Sherpa) is re-scoped or
   swapped for something phone-appropriate (e.g., cloud-only mode as the
   mobile default), the §3.4/§3.5 "heavy local inference on a phone" cost
   drops out and the calculus changes.

If two or more of these fire together, that is a strong signal to revisit
even absent an explicit user request.

## 6. Consequences

- rsac-0ac9 closes with a durable, legible answer instead of resurfacing
  untracked each wave (its stated acceptance criterion).
- ADR-0014 is unamended; its "audio-graph adopts the plugin only if/when it
  targets mobile" clause is now backed by an explicit rationale and trigger
  list rather than an open question.
- `docs/FRAMEWORK_COMPATIBILITY.md`'s Tauri row needs no change — it already
  states the plugin ships compile-proof and desktop is unaffected; this ADR
  adds no new capability claim.
- **Negative:** the mobile-matrix 🟡 markers for Tauri Android/iOS in
  `FRAMEWORK_COMPATIBILITY.md` stay 🟡 with no consumer actively pushing them
  toward ✅ — rsac's own mobile runtime verification (rsac-e6d3/rsac-97c8/
  rsac-b3aa) remains the only path to closing that gap until a trigger in §5
  fires.
- Neutral: no code, CI, or release-process changes result from this ADR by
  itself.

## 7. Acceptance / follow-up (rsac-0ac9)

**What the owner needs to do:** ratify Option B as written (accept this ADR
as-is), or amend it — e.g. if the owner has non-public information about an
upcoming mobile user story that changes §5's trigger set, or wants Option C
scoped as deliberate exploratory spend rather than rejected. This ADR does
not require owner sign-off on any code change — only on the *decision*, since
Option A/C were not implemented.

**Follow-up seeds per option** (file only the ones matching the ratified
option):

- **If B is ratified (as recommended):** no new implementation seed. Close
  rsac-0ac9 referencing this ADR. Optionally file a low-priority tracking
  seed for "re-evaluate audio-graph mobile adoption" with the §5 triggers
  copied into its body, `blockedBy` the seeds named in trigger 3
  (rsac-e6d3, rsac-97c8, rsac-b3aa) so `sd ready` doesn't surface it until at
  least one closes.
- **If the owner instead ratifies A:** file seeds for (a) audio-graph mobile
  UI rework (source-picker redesign + responsive layout), (b)
  `tauri android init` / `tauri ios init` scaffolding in the audio-graph
  repo, (c) first live runtime exercise of `tauri-plugin-rsac`'s consent
  flow (this would also retroactively satisfy trigger 2 for future
  consumers), (d) iOS Broadcast Upload Extension embedding + App Group
  wiring for audio-graph specifically. Each is a separate seed since they're
  independently schedulable and owned by different skill sets (frontend vs.
  Tauri/Gradle vs. Xcode).
- **If the owner instead ratifies C (Android-only):** file (a) and (b) above
  scoped to Android only, plus (c); defer iOS-specific seeds until a later
  re-evaluation.

This ADR itself is the "rationale recorded" deliverable rsac-0ac9's
acceptance criteria call for when audio-graph stays desktop-only; no separate
audio-graph-repo issue is filed unless the owner ratifies A or C.
