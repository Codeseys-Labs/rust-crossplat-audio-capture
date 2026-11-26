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

All five backends — Windows, Linux, macOS, Android and iOS — honor explicit `StreamConfig::buffer_size` as a requested ring-slot count, not a frame count; the macOS production path is fixed. The static `calculate_capacity` policy remains in use. This is the current implementation status: [ADR-0007](designs/0007-capacity-period-sizing.md) records the original decision, and its Windows-only statements describe that decision, not today's code. `calculate_capacity_for_period` remains unwired until each backend exposes and validates its effective negotiated callback period early enough to size the ring; preferred buffer sizes or maximum slice sizes alone do not establish that period. Do not add a selector whose period argument is always absent.

Keep `AudioBuffer::timestamp()` as stream position. Optional Windows device-clock metadata is later work, after the wasapi update and virtual-device validation; invalid timestamp metadata and unvalidated process-loopback positions must not be presented as valid. Do not change compose alignment on that basis.

### D5 — Desktop release and mobile freeze

Keep the existing default platform features and public capability types. Desktop is the release priority; Android and iOS remain experimental and parked, not removed. Maintain the sources and harnesses, preserve compile and packaging coverage, and record actual emulator/simulator and physical-device evidence separately.

CI is manually dispatched (owner decision: the full matrix is too expensive to run per push or PR). `ci.yml` scopes a branch dispatch by its diff against `master` and runs the mobile packaging legs only for mobile-relevant paths; the desktop virtual-audio matrix and the Android emulator and iOS simulator runtime workflows run only on dispatch, plus stable tags for `ci.yml` and `ci-audio-tests.yml`. The automatic exceptions are the PR changelog guard and the weekly non-Rust supply-chain scan. Hybrid runners use Blacksmith for normal compile lanes and GitHub-hosted Windows and macOS for audio. An actual Android emulator and iOS simulator run is required on the release candidate; skipped frame checks are not runtime proof. With no automatic runs, a regression surfaces only when someone dispatches CI: a PR without a dispatched run has no CI evidence.

## Deliberation record: the wave after PR #77 (2026-11-26)

PR #77 delivered D1 to D5 above. The open decisions that followed were settled by a second panel, not by asking the owner. Panelists (one memo each, no panelist saw another's): Opus, Sol, Sonnet, Kimi, GLM, DeepSeek and Gemini 4 Carbon. A GPT Terra agent tallied the memos; a Fable agent that was not a panelist challenged the tally against the memos and the source. A separate research pass produced a cited fact sheet for W3, and a four-model panel (Opus, Sol, Gemini 4 Carbon, Kimi) decided the WASAPI ownership model. Not used: `gpt6.1luna`, `v41flash1017` and `gemini4neon` (no registered agent under those names) and Haiku 5.5 (reachable, not needed).

### W1 — Merge PR #77 (7 of 7)

Squash-merge once the full audio matrix is green on the head, without waiting for real-device recovery evidence: the feature is default-off and the evidence work needs the merged code. Done: audio run 45798202053 passed on `566308a` (the head differed by `docs/RELEASE_PROCESS.md` only), PR #77 merged as `c6f32fa`, and PR #101 (seeds backlog to GitHub issues) was restacked onto `master` and merged as `7643d59`. A green audio run is not recovery evidence: that suite does not enable `hotplug-recovery`, and the macOS System and Process Tap tiers are TCC skips.

### W2 — Order of the next pull requests

1. Release-gate tooling: `scripts/tag-release.sh`, `scripts/check-release-published.sh`, a `Release Readiness` job, and Windows and macOS semver legs (#106, the CI half of #97). It goes first so the semver baseline against v0.4.4 is measured on all three OSes before API-affecting work lands.
2. #103: no logging under the supervisor lock; deterministic recovery tests.
3. Core surface: variant-level `#[non_exhaustive]` on the recovery variants that carry fields, the macOS `supports_system_capture` gate at 14.4 (part of #98), and a lockstep guard between `PlatformCapabilities` and every binding surface.
4. The recovery test harness and the Windows default-switch tests (part of #94).
5. #105: WASAPI control-plane handles hold no COM pointers.
6. The Linux lane: #102 and the Linux default-switch tests. Verifiable by dispatch only.
7. #104 and the activity-probe recipe, sharing one Linux audio dispatch with 6.
8. Release evidence enforcement: the tag script also requires the audio and mobile-runtime runs on the exact commit.
9. macOS attended evidence and the ADR-0019 addendum (closes #94), or the recorded waiver.
10. The 0.5.0 cutover (#108).

Rulings: #102 and #104 are separate pull requests. #104 keeps `start()` non-waiting: `format()` reports the requested format until the first delivered buffer, and sinks are built from that buffer (a bounded wait cannot cover a stream that is never linked). The pipewire 0.10.1 bump (#93) starts only after the Linux lane is green and does not gate 0.5.0, against a 5 of 7 vote to include it: it rewrites the function #102 changes, has no defect behind it, and can only be compiled on paid runners. The napi-rs v3 migration (#92), the module splits (#95), the rest of #98 and #99, and everything mobile or publishing-related are not in this wave.

### W3 — Proving `hotplug-recovery` on real stacks (#94)

Evidence from Windows and Linux gates v0.5 and the removal of the experimental caveat. macOS evidence gates the caveat, and v0.5 unless a waiver is recorded in an ADR-0019 addendum. Every test holds one session open across the switch, plays a different tone on each endpoint, and counts a skip or zero executed tests as a failure; the negative control is the same switch with recovery off.

- Windows: set and read back the `eConsole` render endpoint by id. The existing default-device script sets `eMultimedia`, which is not the role rsac opens and watches. The switch test needs two render endpoints, so it runs on a workstation; endpoint loss can run on a hosted runner.
- Linux: two null sinks and `wpctl set-default` in the audio workflow. The same capture node and generation are expected to follow the new sink; an explicit `Device` target whose node disappears must end with an error, never with the other sink's audio.
- macOS: an attended Mac with the TCC grant. Identity is the reopened aggregate's main sub-device UID: tones cannot prove endpoint identity under a global tap. The aggregate is built from the system-output selector while the watcher follows the default-output selector (#107).

The numeric thresholds (20 dB dominance over consecutive 200 ms windows) are proposals until the harness self-test confirms them.

### W4 — `#[non_exhaustive]`

`PlatformCapabilities` and `ApplicationEnumeration` become `#[non_exhaustive]` only in the 0.5.0 cutover, as one reviewed batch of breaking changes (7 of 7 for the attribute; the timing is the challenger's amendment, argued as keeping `master` releasable as a 0.4.x patch). The recovery types already carry the type-level attribute; their field-carrying variants get the variant-level attribute now, which breaks nothing because they are absent from v0.4.4. The same rule moves the removal of the deprecated `WindowsApplicationCapture` out of #105 and into the cutover.

Measured afterwards: the first run of the Windows semver leg (CI 45810052216) showed `master` is already not releasable as a 0.4.x patch. PR #77 turned the public unit struct `ComInitializer` into a struct with a field, which the Linux-only gate could not see. The Linux and macOS legs are clean. The batch rule stands, and the next release is 0.5.0.

### W5 — The activity probe (#76)

Deferred past v0.5 and kept open (6 of 7 for deferring; the challenger recommended declining and closing, which the orchestrator did not adopt because the request is the owner's own). A documented recipe ships with pull request 7. Revisit with a downstream prototype that proves bounded, single-session behaviour on the three desktop backends.

### W6 — WASAPI ownership model (#105)

Stateless handles (3 of 4; one panelist preferred a dedicated owner thread): the enumerator and device handles store plain data and an MTA usage pin, every COM object is created, used and released inside one call on the calling thread, and the device watcher's COM objects live on its one helper thread. Measured on the workstation: `MMDeviceEnumerator` is registered `ThreadingModel=both`, so STA callers get the object in their own apartment without a proxy. STA callers therefore work, where today they are refused.

## Deliberation record: panel 2, the recovery and release-gate decisions (2026-11-26)

Six decisions were open after pull requests #109 to #112. Panelists (one memo each, none saw another's): Opus, Sol, Gemini 4 Carbon, Kimi, GLM and DeepSeek. A Sonnet agent tallied five memos (the Gemini memo finished late); a Fable agent that was not a panelist folded in the sixth, checked the tally against the memos and the source, and proposed amended resolutions. The orchestrator made the final calls below. Three of them overrule the panel's majority, each because of a fact a minority measured in the code. Not used: `gpt6.1luna`, `v41flash1017` and `gemini4neon` (no registered agent under those names) and Haiku 5.5.

### P1 — A failed device-watcher registration (#114)

Votes: keep running, retry and expose a degraded state 4; fail `start()` 1 (Gemini); warn and continue 1 (Sol). **Decided: `start()` fails where the watcher is the only trigger for a default switch (Windows; macOS when enabled). Linux warns once and continues; a backend with no notifications continues as before.** The majority option needed a late registration to repair the unwatched interval, and the supervisor cannot do that honestly: the OS replays no missed switch, and the only available repair (forcing a default-change trigger) closes and reopens a healthy stream, emits a false `DefaultChanged` and counts a recovery. Failing the start adds no public surface and can be relaxed later without a break; new snapshot fields and events could never be removed. The error is transient: retry `start()`, or build without auto-recovery.

### P2 — Which macOS selector `SystemDefault` follows (#107)

Votes: the ordinary default-output selector for both the watcher and the aggregate, 6 of 6; change now 4, wait for an attended run 2 (Opus, Sol). **Decided: the default-output selector, scoped to the system tap (the application and process-tree taps share the helper and keep the system-output selector), and not merged until an attended run exists.** The change alters plain `SystemDefault` capture for every macOS user, hosted CI sees only permission skips, and under P6 the mismatch cannot be reached in 0.5.0. The attended run also records whether audio simply continues across a switch with recovery off: the tap is global, so macOS may need no reopen at all.

### P3 — Gating the workflow tag path (#119)

Votes: remove tagging on merge and keep one script path 4; a dispatched workflow that runs the script 1 (Sol); verify the pull request head's tree 1 (Gemini). A pull-request ruleset on `master`: 6 of 6, split 3 to 3 on a bypass. **Decided: `release-tag.yml` becomes dispatch-only and runs `scripts/tag-release.sh`, which is the only code that creates tags; it pushes with the workflow token.** A tag pushed by a person starts the full CI matrix, the audio matrix and the three publish workflows on the tag: paid runs that add nothing once the evidence is on the commit, and red publish runs while publishing is off. A tag pushed with the workflow token starts none. The workflow checks out the requested commit instead of requiring it to be the tip of `master` (the orchestrator's amendment: a later merge must not strand a release candidate). The local script remains for emergencies.

The ruleset: pull requests required on `master`, no approvals, squash only, no force-push or deletion, no required checks (CI never starts by itself), no bypass. It is applied only after the release procedure no longer pushes the bump commit directly. **Applied 2026-11-26 as ruleset `28200733`, after #126 merged**; `AGENTS.md` §6 says how to inspect or remove it.

### P4 — Evidence the tag gate requires

Votes: runs on the exact commit 5, on a tree-equal commit 1; mobile runs required 4, advisory or required once 2; a scripted waiver 3, none or ADR-only 3. **Decided: four dispatched runs on the exact commit, each with a terminal evidence job that fails on any skip** (`ci.yml` `Release Readiness`; `ci-audio-tests.yml` `Audio Release Evidence`; `ci-android-emu.yml` `Android Release Evidence`; `ci-ios-sim.yml` `iOS Release Evidence`). A green run alone proves little: today a runner without KVM, a missing simulator runtime or a skipped audio tier all stay green. D5 already requires the mobile runs, so they stay required; the two mobile requirements can be waived by an open `release-waiver` issue named on the command line and written into the tag message and the Release notes. A waiver committed to the repository would change the commit and repeat every paid run. The macOS System and Process Tap tiers count as "the matrix ran", never as capture or recovery evidence.

### P5 — Windows evidence for `hotplug-recovery` (#94)

Votes: the workstation switch plus hosted endpoint loss is enough, and no second virtual endpoint on the hosted runner, 6 of 6. **Decided as proposed**: one session across A→B→A between two virtual endpoints with endpoint-pinned 440 Hz and 997 Hz tones, the same switch with recovery off as the control, and endpoint loss on the hosted runner. The claim is scoped to virtual endpoints. The result is a note under `docs/evidence/` with the tested commit, and the tag gate fails if the recovery or WASAPI sources changed since that commit. A second hosted endpoint is deferred past 0.5.

### P6 — `hotplug-recovery` on macOS in 0.5.0

Votes: ship with a recorded waiver and a caveat 4; refuse the opt-in on macOS until evidence exists 2 (Opus, Sol). **Decided: refuse.** In 0.5.0 a macOS build that sets `with_auto_recovery` fails `preflight()` and `build()` with `PlatformNotSupported`. Shipping with a caveat is coherent only with changing the selector now (P2), and with the mismatch a switch can report `Recovered` on an aggregate still clocked by the alerts device, which fails W3's own identity test. The feature has never been released, so refusing breaks nothing and enabling it later is additive. ADR-0021 records this, P1 and the attended runbook. Plain capture and the device watcher on macOS are unchanged.

### What the reviews of this wave changed

- W2 said `format()` "reports the requested format until the first delivered buffer". The cross-family review showed the code promises less: the value changes when the backend publishes the negotiated format, which is not tied to the first delivery, and off Linux `build()` may already have replaced the request. The rule for callers is unchanged (build format-dependent consumers from the first delivered buffer); the documentation now says only what is true.
- The first version of the Windows evidence harness passed on the workstation, and a review then showed nine ways it could have passed without proving its claim (an event from before the switch, windows collected before the read-back, an outage that was never verified, a restore that was reported but skipped). The harness was hardened before any result was recorded as evidence, and a deliberate break of the product (no reopen on a default change) makes the switch test fail.
- The Linux registry change could not be compiled on the development host. A reviewer from another model family traced it against the pinned `pipewire` sources as the compile check, and the Linux evidence tests were type-checked on Windows by widening their `cfg` gate in a scratch copy.
- The release gate first ran the copy of `scripts/tag-release.sh` that was in the commit being tagged. The bot review of #126 showed what that means: dispatching an older ancestor of `master` would run that commit's older, weaker gate with a write token. The workflow now checks out its own revision for the gate's code and treats the requested commit as data (`RELEASE_GATE_DIR`, `check-version-lockstep.sh --root`). A second reviewer then found that a publish run on the Go module tag satisfied the "release tag" check, and that `release.yml` replaced the evidence lines in an existing GitHub Release.
- Stricter test assertions paid for themselves twice in that review: comparing whole lines showed that a dry run printed the tag message with a log prefix, and a shell error in the Linux log showed two cases that passed for the wrong reason (their setup never created the file under test).
- W4's removal of `WindowsApplicationCapture` took away one capability with no replacement: `new(pid, false)` captured everything except a process tree. The first migration note pointed those users at a target that captures the opposite audio; the cross-family review caught it, the changelog says so, and #127 tracks a public target for the excluding mode.

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
