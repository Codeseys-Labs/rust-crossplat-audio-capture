# Delegation Backlog Seeds - 2026-07-02

Purpose: hand off remaining platform/reference/CI work to later subagents without changing implementation files. Each prompt is ready to send to a depth-2 subagent.

Global delegation constraints for every prompt:

```text
Current depth: 2. Maximum delegation depth: 5. Further nesting must not exceed depth 5. Do not commit. Avoid touching unrelated files. Prefer small, reviewable changes or file precise follow-up seeds if implementation is blocked.
```

## 1. Reference And Submodule Follow-Ups

Scope: refresh/verify reference learnings from upstream projects and ensure rsac has actionable deltas only where they affect shipped code or tests.

Items:

- `cpal` timing: inspect current upstream timing/callback-period handling and compare against rsac's period-aware capacity, timestamp, and backend callback assumptions.
- `screencapturekit` async lifecycle: inspect current upstream start/stop/drop patterns and map any lifecycle lessons to macOS Process Tap/CoreAudio teardown tests or docs.
- `rtrb` abandoned-side semantics: confirm current behavior for producer/consumer drop, abandoned rings, and push/pop error modes; verify rsac tests cover shutdown/abandonment paths.

Ready prompt:

```text
Current depth: 2. Maximum delegation depth: 5. Further nesting must not exceed depth 5. Do not commit. Avoid touching unrelated files.

Audit reference/submodule follow-ups for rsac: current cpal timing/callback-period behavior, screencapturekit async lifecycle start/stop/drop patterns, and rtrb abandoned-side semantics. Compare findings to rsac's bridge/audio/macOS code and tests. Make only minimal doc/seed/test changes if clearly warranted; otherwise update this seed with precise follow-up tasks, file paths, risks, and verification commands. Do not modify implementation code unless the gap is small and directly proven.
```

## 2. CI Coverage Matrix Gaps

Scope: make the test matrix explicit and identify missing coverage by platform, feature, and runner capability.

Items:

- Compare `.github/workflows/*`, `docs/CI_AUDIO_TESTING.md`, `docs/PLATFORM_TESTING.md`, and current feature flags.
- Identify gaps for application-by-name, `subscribe()`, `overrun_count()`/stats, deterministic non-silence assertions, bindings builds, and platform-specific audio integration tiers.
- Separate compile-only coverage from real-audio coverage and from self-hosted/manual verification.

Ready prompt:

```text
Current depth: 2. Maximum delegation depth: 5. Further nesting must not exceed depth 5. Do not commit. Avoid touching unrelated files.

Audit rsac's CI coverage matrix. Read workflows and testing docs, then produce a concise gap list by platform, feature flag, runner type, and test tier. Fix stale docs or add focused seed entries if needed. Do not broaden CI jobs unless the change is minimal and low-risk. Include exact verification commands and note where real audio hardware or self-hosted runners are required.
```

## 3. Linux Deterministic PipeWire Routing

Scope: make Linux audio integration tests deterministic instead of relying on ambient default routing.

Items:

- Route generated test audio into a known PipeWire/Pulse null sink/source pair.
- Ensure app/process capture tests target the deterministic node rather than default desktop state.
- Harden Linux non-silence assertions where the null-sink setup is deterministic.
- Document local and CI setup commands, including `XDG_RUNTIME_DIR`, `pipewire`, `wireplumber`/session manager, and `pactl` requirements.

Ready prompt:

```text
Current depth: 2. Maximum delegation depth: 5. Further nesting must not exceed depth 5. Do not commit. Avoid touching unrelated files.

Implement or seed Linux deterministic PipeWire routing for rsac audio integration tests. Target a known null sink/source path, avoid ambient default-device assumptions, and harden non-silence assertions only when the route is deterministic. Update tests/docs/scripts minimally. Verify with the narrowest Linux cargo test command available; if host PipeWire is unavailable, document the exact unverified commands and file a follow-up seed.
```

## 4. Windows Runner Boundary And Audio-Capable Options

Scope: clarify what Windows CI can and cannot cover, especially the Blacksmith versus LABSN boundary.

Items:

- Blacksmith Windows Server images are compile/unit-test only because the audio subsystem is absent.
- LABSN/GitHub-hosted Windows can provide virtual audio for integration tests but should remain scoped to audio-capable jobs.
- Evaluate self-hosted Windows options for full WASAPI and application/process capture coverage.
- Keep compile-only and audio-capable workflows distinct to avoid false failures.

Ready prompt:

```text
Current depth: 2. Maximum delegation depth: 5. Further nesting must not exceed depth 5. Do not commit. Avoid touching unrelated files.

Audit and update rsac's Windows CI/audio runner plan. Preserve the boundary that Blacksmith Windows is compile/unit-test only due to missing audio subsystem, LABSN/GitHub-hosted Windows is for virtual-audio integration tests, and self-hosted/audio-capable Windows is needed for full WASAPI application/process coverage. Make minimal docs/workflow label changes only if stale. Include a recommended matrix and verification commands.
```

## 5. macOS TCC And Self-Hosted Runner Plan

Scope: make macOS audio capture verification reliable across TCC permission requirements and hosted/self-hosted runner differences.

Items:

- Document which tests need Screen & System Audio Recording or related TCC permissions for Process Tap.
- Define a self-hosted macOS runner setup path with BlackHole, permission preflight, and clear failure diagnostics.
- Preserve hosted macOS compile/unit coverage while routing permission-dependent capture tests to explicitly prepared machines.
- Add or seed preflight checks that fail with actionable `AudioError::PermissionDenied`/user-facing guidance instead of opaque backend errors.

Ready prompt:

```text
Current depth: 2. Maximum delegation depth: 5. Further nesting must not exceed depth 5. Do not commit. Avoid touching unrelated files.

Create a macOS TCC/self-hosted runner plan for rsac. Identify which Process Tap/CoreAudio tests require system audio capture permissions, how to provision BlackHole and permissions on self-hosted macOS, and how hosted macOS CI should be limited. Make minimal docs/test-preflight changes if clearly safe; otherwise update seeds with exact tasks. Include diagnostics, expected failure modes, and verification commands.
```

## 6. Focused Reference Pin Triage - 2026-07-02

Scope: follow-up from the submodule/reference update pass for `reference/cpal`, `reference/screencapturekit-rs`, `reference/rtrb`, and the configured-but-missing `apps/audio-graph` submodule.

Observed state:

- `reference/cpal`: pinned at `5418f0b` (`asio-sys-v0.2.6-143-g5418f0b`); upstream `origin/master` is `e22fb7e` (`v0.18.1-10-ge22fb7e`). Ahead range includes the `v0.18.0`/`v0.18.1` releases plus post-release fixes, with breaking API/MSRV changes and timing/reroute behavior changes. Do not bump as a blind reference pin; audit cpal timing lessons first.
- `reference/screencapturekit-rs`: pinned at `185e39c` (`v6.1.0-16-g185e39c`); upstream `origin/main` is `744cf43` (`v8.0.0-1-g744cf43`). Ahead range crosses `v7.0.0`, `v7.0.1`, and `v8.0.0`, including async lifecycle breaking changes and multi-output async stream work. Do not bump as a blind reference pin; extract lifecycle/FFI-safety lessons first.
- `reference/rtrb`: pinned exactly at release tag `0.3.4` (`f5d14ec`); upstream `origin/main` is `9dfc2b8` (`0.3.4-2-g9dfc2b8`). Ahead range is one CI-only Dependabot commit plus `Replace Arc with ArcRingBuffer (#176)`. Because rsac's shipped dependency is the released `rtrb 0.3.4`, keep the reference pin at the release tag until a new rtrb release exists or rsac intentionally tracks unreleased main.
- `apps/audio-graph`: `.gitmodules` and local `.git/config` still configure `apps/audio-graph`, but `git ls-tree HEAD apps/audio-graph` has no gitlink and the path is absent. `git submodule status apps/audio-graph` fails with `pathspec ... did not match any file(s) known to git`. Remote `Codeseys-Labs/audio-graph` currently has `master` at `583260e`. This needs a policy decision: restore the gitlink, or remove the stale `.gitmodules` entry and update docs that still say AudioGraph is included as a submodule.

Ready prompt:

```text
Current depth: 2. Maximum delegation depth: 5. Further nesting must not exceed depth 5. Do not commit. Avoid touching unrelated files.

Resolve rsac's focused reference/submodule triage. Inspect the pinned states: cpal `5418f0b` vs upstream `e22fb7e`, screencapturekit-rs `185e39c` vs upstream `744cf43`, rtrb `f5d14ec`/tag `0.3.4` vs upstream `9dfc2b8`, and the configured-but-missing `apps/audio-graph` submodule. Do not blindly bump cpal or screencapturekit-rs because the ahead ranges cross breaking releases; extract specific timing/lifecycle lessons into rsac issues/docs/tests instead. Keep rtrb pinned to released `0.3.4` unless a newer release exists or rsac intentionally tracks unreleased main. For audio-graph, make one coherent low-risk choice: either restore the gitlink to a vetted audio-graph commit, or remove the stale .gitmodules entry and update README/AGENTS/docs references that claim it is included as a submodule. Verify with `git status --short --branch`, `git submodule status --recursive`, and `git ls-tree HEAD apps/audio-graph reference/cpal reference/screencapturekit-rs reference/rtrb`.
```

## 7. Deferred Dirty Worktree Imports - 2026-07-02

Scope: candidate Agent Manager worktrees from base `eb5d723` whose dirty diffs were not safe to squash-patch into `work/critique-ci-audio-integration` during the integration pass because they overlap existing dirty integration files or newly imported core API contract files. Re-run each in a depth-2 subagent and either rebase/adapt onto the integration branch or split into smaller non-overlapping imports. Do not touch `.gitmodules` or submodule gitlinks unless the task explicitly changes submodules.

Observed during import triage:

- `critique-linux-pipewire-native-resolution`: coherent Linux native-first PipeWire resolution work, but patch dry-run failed against the integration branch after core import with dirty-overlap errors in `src/core/config.rs` and `tests/ci_audio/app_capture.rs`. Needs manual reconciliation with the imported CaptureTarget/ApplicationId docs and the current app-capture CI changes.
- `critique-windows-wasapi-hardening`: coherent WASAPI packet/format hardening plus a new Windows ApplicationByName integration module, but patch dry-run failed on the already-dirty `.github/workflows/ci-audio-tests.yml`. Needs manual workflow merge and review of the untracked `tests/ci_audio/application_by_name_windows.rs` before import.
- `critique-macos-coreaudio-tap-safety`: large macOS Process Tap safety diff; patch dry-run failed on already-dirty `tests/ci_audio/application_by_name.rs`. Needs manual reconciliation with current macOS ApplicationByName integration changes.
- `critique-tests-ci-coverage`: broad docs/workflow/test coverage diff with untracked `tests/ci_audio/lifecycle_terminal.rs`; patch dry-run failed on already-dirty `.github/workflows/ci-audio-tests.yml`. Needs splitting so docs-only changes, workflow changes, and real-backend lifecycle tests are reviewed independently.

Ready prompt:

```text
Current depth: 2. Maximum delegation depth: 5. Further nesting must not exceed depth 5. Do not commit. Avoid touching unrelated files. Preserve dirty integration branch changes and do not touch `bindings/rsac-ffi/include/rsac_generated.h` unless a diff intentionally requires it.

Reconcile one deferred dirty worktree import into `work/critique-ci-audio-integration` using a squash-patch approach. Start from the named worktree, inspect `git status --short`, `git diff --check`, tracked diff, and untracked files. Adapt only the coherent, low-risk subset onto the integration branch; if overlap is nontrivial, leave implementation untouched and update this seed with exact conflicts and next steps. Validate with `git diff --check` plus the narrowest relevant cargo check/test command. Do not stage, commit, push, or modify submodule gitlinks.
```
