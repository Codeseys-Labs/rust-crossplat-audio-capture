# Desktop refinement toward v0.5

This is the decision record and release bar for the desktop-first refinement after v0.4.4. It distinguishes decisions and required evidence from work already shipped. See [VISION](../VISION.md), the [feature matrix](features.md), and [ADR-0019](designs/0019-hotplug-auto-recovery.md).

## Deliberation record

The multi-model deliberation ran Fable, Kimi, GLM and DeepSeek as panelists; Sol tallied their proposals; Opus challenged the tally. Gemini 4 Carbon and Sonnet researched CI and tooling; Opus and Sol mined reference submodules; Gemini 4 Neon orchestrated. The host harness did not provide the user-named aliases `gpt6.1luna`, `haiku5.5`, and `v41flash1017`; these were not participants under those aliases.

### D1 — Scope and integration

Position rsac as an application-aware desktop capture and session layer. Keep `compose` opt-in; its live per-source gain/mute and group gain already exist, including bindings. Add only `hotplug-recovery`, default off, for recovery; no separate surround, timestamp, codec, DSP, or general-resampling feature. Keep `DeviceWatcher` ungated.

Compose requests two channels per source on desktop; `KeepChannels` retains the negotiated width, not necessarily a device's native width. The OS may downmix to that requested width. A channel count alone does not specify speaker positions; defer explicit surround layouts and do not introduce a speculative 5.1/7.1 rejection. The even/odd fold for wider buffers is not the normal desktop source path.

### D2 — Hotplug recovery

Adopt [ADR-0019](designs/0019-hotplug-auto-recovery.md): a stable facade, fresh bridges, bounded event delivery, generation markers and desktop `SystemDefault` only. Default-device switches must not consume the failure budget merely for occurring. Require a bounded Windows initialization handshake and prevent Linux targeted capture from silently falling back to the default. Linux native default-follow behavior is an inference until source-identifying routing tests pass.

### D3 — Decomposition

Split the API facade first, then the Linux thread module, preserving public paths and behavior. Move the ring-buffer split after recovery and make it optional; defer the public macOS CoreAudio split until macOS semver coverage exists. Update the module-DAG guard for the new API root and fail if that root is missing. Compare test inventories, preserve existing cases, and run targeted platform, rustdoc and semver checks for each cut.

### D4 — Ring sizing and clocks

All five backends honor explicit `StreamConfig::buffer_size` as a ring-slot count; the macOS production path is fixed. `calculate_capacity_for_period` remains unwired until effective negotiated periods are available before allocation and validated per backend ([ADR-0007 status](designs/0007-capacity-period-sizing.md#status-addendum--2026-11-25)). Do not add a selector whose period argument is always absent.

Keep `AudioBuffer::timestamp()` as stream position. Optional Windows device-clock metadata is later work, after the wasapi update and virtual-device validation; invalid timestamp metadata and unvalidated process-loopback positions must not be presented as valid. Do not change compose alignment on that basis.

### D5 — Desktop release and mobile freeze

Keep the existing default platform features and public capability types. Desktop is the release priority; Android and iOS remain experimental and parked, not removed. Maintain the sources and harnesses, preserve compile and packaging coverage, and record actual emulator/simulator and physical-device evidence separately.

The intended cost-controlled CI split keeps cheap cross-target checks in the main CI lane, mobile packaging on manual/scheduled/mobile-path and release lanes, Android emulator runtime manual-only, and iOS simulator runtime manual plus weekly. The expensive desktop virtual-audio matrix is not an every-PR requirement; consult the workflow files for the active triggers. Hybrid runners use Blacksmith for normal compile lanes and GitHub-hosted Windows for audio. An actual Android emulator and iOS simulator run is required on the release candidate; skipped frame checks are not runtime proof. Less frequent runtime jobs can delay discovery of mobile regressions.

## v0.5 desktop bar

- Honest Rust 1.88 MSRV, including Windows coverage for platform-only dependencies; semver checks on the public platform-specific APIs, including macOS.
- Desktop terminal/read lifecycle and real-time allocation gates; targeted feature combinations, bindings, downstream `audio-graph`, docs and rustdoc links.
- The macOS ring-slot fix and Linux target-routing fix; deterministic recovery races and desktop virtual-device tests as specified in ADR-0019. Source identity must be checked, not merely the receipt of an event.
- Address WASAPI silent packets and bound initialization; validate dependency upgrades separately, with RT-allocation coverage. Do not equate a dependency version bump with a demonstrated backend fix.
- Run the full virtual-audio matrix before release, and record actual mobile emulator/simulator release-candidate results. Period-derived ring sizing, unvalidated hardware timestamps, surround support, and mobile feature parity are not v0.5 release requirements.

Sequence CI and metadata, API split, Linux split, backend fixes and individually reviewed dependency upgrades, recovery, optional validated Windows device timestamps, then release evidence. A backend fix or prerequisite can land earlier when it reduces revalidation or closes a live defect.

## Mobile freeze and unfreeze

During the freeze, accept maintenance needed for shared-layer correctness, security, build/packaging health and regression coverage; do not add mobile feature scope or claim desktop parity. Preserve existing defaults, target gates, bindings and test harnesses. See the per-mode evidence in [AGENTS.md §3](../AGENTS.md#3-current-state).

Before reopening mobile feature work, record:

- [ ] Physical Android and iOS device models, OS versions, builds, and reproducible capture results for the affected modes; emulator/simulator results separately.
- [ ] Consent and permission flows, including denial, cancellation and re-consent; MediaProjection foreground-service ordering or ReplayKit broadcast lifecycle as applicable.
- [ ] Route changes, disconnect/reconnect, interruption, backgrounding and teardown, including clean resource release.
- [ ] First-party AAR/JNI or SwiftPM/FFI packaging and binding tests on the affected targets, plus relevant cross-target compile checks.
- [ ] An owner, test plan, unresolved platform limitations and explicit decision to unfreeze the affected scope; do not infer physical-device proof from a green compile or skipped smoke.

## Companion-crate seams

- **Sinks and encoders:** implement `AudioSink` downstream and use `drain_to`; the bundled `NullSink`, `ChannelSink` and opt-in WAV adapter are examples. FLAC/Opus encoding belongs in downstream or companion crates, not the capture core.
- **Mic plus loopback:** compose a default microphone `Device` and `SystemDefault` into separate groups for a downstream voice pipeline. Capture/compose does not implement echo cancellation or claim hardware-clock alignment; VAD, AEC and far-end reference policy remain downstream.
- **One-source rate alignment:** enable `compose`, create a `CompositionBuilder` at the desired session rate with one `Group::new("source").source(target).keep_channels()`, then build and start. The compositor resamples when the negotiated rate differs; it keeps negotiated channels and adds a non-RT compositor thread and quantum buffering. This is not a general-purpose resampler API.
