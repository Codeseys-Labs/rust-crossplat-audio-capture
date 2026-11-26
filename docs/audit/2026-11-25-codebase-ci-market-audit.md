# Codebase, CI and runner-market audit (2026-11-25)

Point-in-time record of a full review of rsac at `eac7056` (`origin/master`
after v0.4.4 and PR #75), the tooling and CI changes it led to, and the
research behind them. The code and the workflow files are the source of
truth; where this record and they disagree, they win.

Evidence labels: **observed** means read in a file, an API response or a CI
log; **[INFERENCE]** means reasoned but not verified. Nothing in this pass
ran on audio hardware, and no cargo build or test was run for it.

Related records:
- [`reference/REFERENCE_ANALYSIS.md` § Reference Delta & Harvest (2026-11-25)](../../reference/REFERENCE_ANALYSIS.md#reference-delta--harvest-2026-11-25): submodule pins, upstream deltas, full harvest table.
- [`docs/CI_AUDIO_TESTING.md`](../CI_AUDIO_TESTING.md): audio E2E truth table, macOS TCC services, LABSN on macOS.

## 1. Owner decisions that frame this record

1. rsac is the capture engine for `audio-graph`, and is meant to be a
   cross-platform library for system, device, application and multi-source
   capture. Python, Go and JS/TS bindings stay; publishing to package
   registries is not planned yet.
2. Desktop first. Android and iOS research and physical-device testing come
   later (the owner has devices for both).
3. Capabilities beyond today's scope are to be researched and shipped as
   optional Cargo features. That design work (positioning, hotplug
   recovery, decomposition, ring sizing and device clocks, desktop
   sequencing) ran as a separate panel and is not recorded here.
4. Probe Blacksmith Windows once; if nothing changed, keep the hybrid
   runner setup and make the expensive CI manually triggerable.
5. No self-hosted macOS runner. Use what GitHub-hosted and Blacksmith
   macOS provide.

## 2. Codebase

### 2.1 In good shape

- Strict module DAG `core → bridge → audio → api → compose`, enforced by
  [`scripts/check-module-dag.sh`](../../scripts/check-module-dag.sh) with a
  CI self-test.
- Lock-free `rtrb` data ring plus a free-list return ring. Steady-state
  producer pushes do not allocate; [`tests/rt_alloc.rs`](../../tests/rt_alloc.rs)
  checks this per OS with a counting `#[global_allocator]` (ADR-0001).
- All five `CaptureTarget` modes on Windows (WASAPI, process loopback with
  INCLUDE-tree), Linux (PipeWire, native registry with `pw-dump` fallback)
  and macOS (CoreAudio Process Tap, `objc2`).
- Mobile backends and packaging exist (Android AAudio + playback capture
  and Kotlin AAR; iOS `AVAudioEngine` + ReplayKit ring and SwiftPM), with
  emulator/simulator evidence but no physical-device runs.
- Opt-in features `compose`, `async-stream`, `sink-wav`, `tracing`, `cli`,
  `macos-tcc-spi`. `bridge-zerocopy` was benchmarked and removed in v0.4.4
  (ADR-0006).
- Bindings: C FFI, Python (`cp39-abi3`), Node/TS (napi), Go; all four
  expose `Composition`. Tauri plugin under `integrations/`.

### 2.2 Size hotspots (observed, `wc -l` at `eac7056`)

| File | Lines | Note |
|---|---|---|
| `src/api.rs` | 5,365 | ~2,515 production, ~2,850 test: `mod tests` (2640–5365, 2,726 lines) plus `format_negotiation_tests` (1363–1486, 124 lines) |
| `src/audio/linux/thread.rs` | 4,151 | |
| `src/bridge/ring_buffer.rs` | 3,522 | |
| `src/audio/macos/coreaudio.rs` | 3,154 | |

None is broken. They are slow to review and to rebuild incrementally.

### 2.3 Defects and gaps found

| Finding | Evidence | Status |
|---|---|---|
| UTF-8 boundary panic: `.exe` stripping slices `base[cut..]` with `cut = len - 4`, which panics when `cut` falls inside a multi-byte character | `src/audio/linux/thread.rs:1131–1134` (observed; not reproduced) | open |
| Linux app matching accepts any `media.class` containing `"Stream"` and takes the first matching serial | `src/audio/linux/thread.rs` ~1753 | open |
| WASAPI packets flagged `SILENT` are copied as-is; only `data_discontinuity` is checked | `src/audio/windows/thread.rs` ~629 | open |
| `audio_client`, `capture_client` and `h_event` are still alive when `wasapi::deinitialize()` runs | `src/audio/windows/thread.rs` ~735–751 | open; UB in principle under wasapi `647bac6`'s contract [INFERENCE: other threads keep the MTA alive and mask it] |
| macOS ignores an explicit `buffer_size`: `calculate_capacity(None, 4)` | `src/audio/macos/coreaudio.rs:517`; Windows, Linux, Android and iOS pass `config.buffer_size` | open |
| `calculate_capacity_for_period` has no production caller | `grep` over `src/` finds only docs and tests in `src/bridge/ring_buffer.rs` | open (ADR-0007 direction) |
| macOS `supports_system_capture` is unconditional, but `SystemDefault` is a global Process Tap needing 14.4+ and System Audio Recording consent | `src/core/capabilities.rs` (`macos()`) | open |
| Device-clock timestamps not implemented (stream-position math only) | `rsac-ec25` | open |
| No registry release yet; publish workflows are disabled pending first publish | `release.yml`, `release-npm.yml`, `release-pypi.yml` | by decision |

Fix order and sources for the backend items are in the
[harvest table](../../reference/REFERENCE_ANALYSIS.md#harvest-ranking).

### 2.4 Documentation drift found

- `VISION.md` (revised 2026-05-30) omitted mobile and Tauri, still called
  `ci-audio-tests.yml` a 9-job matrix, and said Blacksmith Windows runners
  are preferred for audio.
- `AGENTS.md` §3 said mobile is "compile-checked only" above a table of
  emulator/simulator runs; §8 listed shipped work (`rsac-fba7` binding
  `compose`, Linux `ApplicationByName` tests) as remaining.
- `ci-audio-tests.yml` gap report said the Windows device/process tiers
  were not deterministic; all three Windows tiers already set
  `RSAC_CI_AUDIO_DETERMINISTIC=1`.
- `docs/designs/README.md` listed ADR-0014 as Proposed; the ADR says
  Accepted. Fixed.
- `docs/CI_AUDIO_TESTING.md` decoded `OSStatus 2003332927` as
  `kAudio_UnimplementedError`. It is `0x77686F3F` = `'who?'` =
  `kAudioHardwareUnknownPropertyError`. Fixed.
- `reference/REFERENCE_ANALYSIS.md` mapped macOS `SystemDefault` to plain
  AUHAL capture, gave 14.0 as the Process Tap floor, described the
  screencapturekit-rs v6-era API, and said OBS had no canonical upstream.
  Fixed.

### 2.5 MSRV 1.87 does not hold on Windows or macOS

- `Cargo.toml` declared `rust-version = "1.87"` at `eac7056`.
- `sysinfo = "0.38.4"` is a dependency on Windows and macOS only. Its
  manifest declares `rust-version = "1.88"` (observed in the registry
  copy). wasapi-rs's own `Cargo.toml` notes the same for its examples.
- The MSRV job ran on Linux only, where sysinfo is not compiled, so it
  could not catch this.
- Fix, landed in this pass: `Cargo.toml` declares `rust-version = "1.88"`,
  and the `ci.yml` `msrv` job checks 1.88 on `blacksmith-4vcpu-ubuntu-2404`
  and GitHub-hosted `windows-2025`. macOS is not in that matrix. The
  planned wasapi, rtrb and pipewire bumps (MSRV 1.85, 1.38, 1.80) do not
  change this.

## 3. Tooling

### 3.1 Blaze: not adopted

Two projects use the name.

- [`indexable-inc/blaze`](https://github.com/indexable-inc/blaze) is a
  daemon-based incremental Rust compiler (rust-analyzer analysis, Cranelift
  and LLVM tiers) aimed at millisecond edit-rebuild loops. Not a fit:
  - v2 has no Windows support; Windows is a primary rsac backend and the
    owner's workstation.
  - It ships its own pinned nightly toolchain, bypassing
    `rust-toolchain.toml` (1.95.0) and the MSRV.
  - It covers `build`/`check`/`run`/`test`/`bench` only, not `clippy`,
    `rustdoc`, `maturin`, napi, `cargo-ndk` or `cargo-semver-checks`.
  - It trades several GB of resident memory for warm rebuilds; ephemeral CI
    runners start cold every time. Its README asks users to keep `cargo`
    and CI as the final word.
  - At most an optional personal tool on macOS or Linux.
- [`blaze-monorepo.dev`](https://blaze-monorepo.dev/) is a polyglot task
  orchestrator, pre-alpha by its own site. mise tasks and the Cargo
  workspace already cover that.

Better levers for build time: the module splits in §2.2, Blacksmith's
co-located cache (already used), and `cargo-nextest` [INFERENCE: not
measured here].

### 3.2 mise and lefthook: already present, three gaps

Both were checked in before this pass (`rsac-7e19`). lefthook's hooks are
installed in the main repository's `.git/hooks`, which linked worktrees
share (`git rev-parse --git-common-dir`); an earlier note in this review
that hooks were not installed was wrong.

Gaps at `eac7056` and their resolution:

| Gap | Resolution in this pass |
|---|---|
| `lefthook.yml` ran bare `bash scripts/...`; on Windows with WSL that can resolve to `System32\bash.exe` (wrong OS, no `cargo`) | Every hook now runs `mise run <task>`; mise's `run_windows` routes through `scripts/run-bash.ps1` (Git bash by absolute path) |
| Runtime pins duplicated across `mise.toml` and five workflows, already drifted (Node 20 in `release-npm.yml` vs 22 elsewhere; Bun `1` vs `1.3.14`) | New `.tool-versions` (`bun 1.3.14`, `nodejs 22`, `golang 1.22`, `python 3.12`) read by mise and by the `setup-*` actions; `supply-chain.yml` keeps `go-version: stable` for govulncheck |
| Root `Makefile` duplicated mise tasks and pointed at nonexistent targets | Deleted; `mise run check:cross` replaces its one caller |

Trade-off: hooks now require mise on `PATH`, which the documented
onboarding (`mise install && mise run setup`) already provides.

### 3.3 betterleaks: adopted

- There was no secret scanning anywhere (hooks or CI), while the release
  surface spans crates.io, npm, PyPI, Maven/Android and Apple signing.
- Pinned to **v1.10.1** (latest stable, 2026-11-12). v2 is still a release
  candidate and changes the CLI: `--staged`, `--log-opts` and
  `--pre-commit` go away, `allowlist` is rejected, and `-v` changes from
  `--verbose` to `--validate`, which sends findings to provider APIs.
  Never pass `-v` or `--validation`.
- v1 does not auto-load `.betterleaks.toml`, so every call passes
  `--config`. On Windows Git the default git-config isolation fails, so the
  local mise tasks add `--trust-local-git-config`.
- Wiring: `mise.toml` `[tools] betterleaks = "1.10.1"`; lefthook
  `pre-commit` runs `mise run secrets:staged`; `ci.yml` job `secrets` runs
  independent of `changes`/`lint` via `jdx/mise-action` (SHA-pinned
  v5.3.0), scanning full history on every dispatched or tag run, with
  `--redact`.

## 4. CI

### 4.1 Inventory and cost (at `eac7056`)

- 12 workflows; `ci.yml` alone is 1,496 lines.
- The repository is public, so GitHub-hosted standard runners cost $0
  ([GitHub docs](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)).
  Only Blacksmith minutes cost money. Its pricing page lists $0.004/min
  Linux, $0.008/min Windows and $0.08/min macOS M4 at the base size
  ([pricing](https://www.blacksmith.sh/pricing), not re-verified); against
  the free tier, a 4-vCPU Windows minute counts 4× and a 6-vCPU macOS
  minute 20× a 2-vCPU Linux minute.
- Run 29728297288 (push to master, 26 jobs): 1,680 job-seconds, of which
  Linux 1,361 (`Binding Crates` alone 701), macOS 162, Windows 157.
  Blacksmith Windows jobs waited up to ~57 minutes to start in 3 of 4
  sampled runs (one of the three inferred from start times); that costs
  wall-clock, not money.
- macOS seconds per code PR, estimated from measured job times: about 446
  before (lint 21, unit 47, Go 30, `mobile-ios` 46, docs 18, iOS simulator
  134, macOS audio ~150) and about 116 after §4.2 (`mobile-ios`, the iOS
  simulator and macOS audio leave the desktop PR path). Roughly $0.59 →
  $0.15 at $0.08/min [INFERENCE: warm-cache timings; the macOS audio figure
  is assumed].
- `ci.yml` re-ran the full matrix on every label change because
  `pull_request.types` included `labeled`/`unlabeled`.

### 4.2 Changes landed in this pass

| Workflow | Change |
|---|---|
| `ci.yml` | `labeled`/`unlabeled` dropped; the label-driven `changelog-guard` job moved to its own `changelog-guard.yml` |
| All build/test workflows | Owner decision after the cost analysis above: **manual `workflow_dispatch` only** — no `pull_request` and no branch-push triggers. Automatic runs that remain: `changelog-guard.yml` on PRs, the weekly `supply-chain.yml` cron, and stable `v*.*.*` tags for `ci.yml` + `ci-audio-tests.yml`. `bench.yml` lost its weekly cron |
| `ci.yml` | `changes` classifier adds a `mobile` output and scopes a branch dispatch by its diff against `master`: docs-only → no matrix; `mobile-android`/`mobile-ios` only for mobile-relevant paths (always on tags, a default-branch dispatch, or `full=true`) |
| `ci.yml` | `coverage` runs only on tags or `full=true`; `secrets` (betterleaks) added; `msrv` checks 1.88 with `--locked` on Linux and Windows, plus the binding crates on Windows; `check-bindings` smokes one abi3 wheel on CPython 3.9 and 3.13 |
| `ci-audio-tests.yml` | Triggers: `workflow_dispatch` (input `platform`) and `v*.*.*` tags; no push or `pull_request`. `macos-audio` moved to GitHub-hosted `macos-15` with BlackHole pinned as default output and input |
| `ci-ios-sim.yml`, `ci-android-emu.yml` | Dispatch only |
| `ci-audio-macos-tcc.yml` | Weekly cron removed; dispatch only |
| `probe-blacksmith-windows-audio.yml` | One-shot probe: added, run once (2026-11-26), then removed. Result in §5.1 |

## 5. Can Blacksmith be the only runner? LABSN outside GitHub-hosted

### 5.1 Windows

- Blacksmith `blacksmith-4vcpu-windows-2025` is Windows Server 2025 build
  26100 with `InstallationType: Server Core` (observed in the 2026-04-13
  and 2026-07-17 probes, recorded in `AGENTS.md`). Server Core has no audio
  stack by SKU: no `AudioSrv` or `AudioEndpointBuilder`, no `AudioSes.dll`
  or `Audiodg.exe`, no `MMDevices\Audio` keys. Microsoft documents that
  Server Core cannot be converted to Desktop Experience in place
  ([Server Core](https://learn.microsoft.com/en-us/windows-server/administration/server-core/what-is-server-core)).
- What LABSN does on Windows: `devcon install` of the VB-CABLE driver,
  then `Restart-Service AudioEndpointBuilder, AudioSrv`. On Blacksmith the
  PnP node registers with `Status: Error` because no audio service exists.
  rsac's endpoint gate (`scripts/ci-windows-audio-default.ps1`) then needs
  `IMMDeviceEnumerator`, which needs `AudioSrv`.
- `Server-Media-Foundation` is listed as Available. It installs Media
  Foundation libraries, not the Core Audio service graph [INFERENCE], and
  may need a reboot a CI job cannot take.
- The one-shot `probe-blacksmith-windows-audio.yml` (removed after its run)
  installed it, started the services, ran LABSN and reported one of
  `AUDIO_STACK_USABLE`, `PARTIAL`, `RESTART_REQUIRED_UNTESTABLE` or
  `NO_CHANGE`. The verdict needs a running `AudioSrv`, a VB-CABLE PnP node
  with status OK and at least one playback endpoint, not just LABSN exiting
  0. Expected: `NO_CHANGE`.
- **Result (2026-11-26, run 45738685950): `AUDIO_STACK_USABLE`.** The
  expectation was wrong because its premise was stale. The image is now
  `InstallationType: Server` (Desktop Experience), build 26100.33296, and
  `AudioSrv` and `AudioEndpointBuilder` were already Running at baseline,
  before the feature install. The first three bullets above describe the image
  as probed in April and July, not today's. Blacksmith Windows can host an
  audio stack; whether rsac's WASAPI tiers pass there is untested.
- LABSN should work on any runner whose Windows image is Desktop
  Experience [INFERENCE: not run outside GitHub-hosted]. GitHub's
  `windows-2025` builds from `2025-Datacenter-g2`
  ([image template](https://github.com/actions/runner-images/blob/main/images/windows/templates/locals.windows.pkr.hcl));
  RunsOn builds `windows25-full-x64` from AWS's
  `Windows_Server-2025-English-Full-Base` AMI
  ([config](https://github.com/runs-on/runner-images-for-aws/blob/main/config.yml)).
  Depot and WarpBuild say they mirror the GitHub image [INFERENCE: not
  checked for the SKU].

### 5.2 macOS

LABSN's macOS script installs `background-music` and restarts
`coreaudiod`. It does not touch TCC, so it cannot unlock the Process Tap
tiers, and BlackHole already covers the Device tier. Details and the
GitHub-hosted TCC grants are in
[`CI_AUDIO_TESTING.md` § The TCC gate](../CI_AUDIO_TESTING.md#the-tcc-gate).

### 5.3 Verdict

Keep the hybrid: Blacksmith for Linux and for Windows/macOS compile and
unit work; GitHub-hosted `windows-latest` for WASAPI E2E and `macos-15`
for macOS E2E, both free on this repository. The probe did report
`AUDIO_STACK_USABLE` (§5.1), so Blacksmith Windows is now a possible home for
`windows-audio`. That is a cost and queue-time trade (4× billing, waits of up
to ~57 minutes), no longer a capability gap. Revisit if the repository goes
private or GitHub-hosted Windows audio regresses.

## 6. Runner vendors

Prices and capabilities as published by each vendor in Oct–Nov 2026 and
linked below; not re-verified in this pass.

| Provider | Windows with an audio stack | macOS | Price notes | Verdict |
|---|---|---|---|---|
| GitHub-hosted standard | Yes, proven (`windows-latest`, 3 of 3 WASAPI tiers deterministic) | `macos-15`; Microphone pre-granted to the agent, no Audio Capture | $0 on public repositories | Use for audio E2E |
| [Blacksmith](https://docs.blacksmith.sh/blacksmith-runners/overview) (current) | Yes as of 2026-11-26 (probe `AUDIO_STACK_USABLE`; rsac's suite not run there). Server Core in the April and July probes. Windows in public beta | M4; no Audio Capture; Device tier hung | 3,000 free 2-vCPU-equivalent min/month; macOS 20×, Windows 4× | Keep for Linux and compile/unit; possible Windows-audio fallback |
| BuildJet | — | — | Shut down 2026-03-31 | Out |
| [Ubicloud](https://www.ubicloud.com/docs/github-actions-integration/price-performance) | No Windows runners | No macOS runners | Cheap Linux | Out: cannot fix Windows or macOS |
| [Depot](https://depot.dev/docs/github-actions/runner-types) | Likely [INFERENCE] | No Audio Capture | $20/month base plus per-minute | Out: replaces a $0 job with a paid one |
| [WarpBuild](https://www.warpbuild.com/docs/ci/cloud-runners) | Likely [INFERENCE]; no WarpBuild cache on Windows | No Audio Capture | Windows 4 vCPU about $0.016/min | Out: same reason |
| [RunsOn](https://runs-on.com/docs/runners/platforms/windows/) | Yes, Desktop Experience AMI (observed in its image config) | No managed macOS | License about €300/year plus AWS costs and ops | Out unless the repo goes private and Windows minutes grow |
| [Cirrus Runners](https://cirrus-runners.app/) | No Windows | Tart VMs; no Audio Capture | $150/month flat per concurrent runner | Out |
| Self-hosted Mac (Tart, MacStadium/Orka) | — | Only route to an Audio Capture grant | Hardware or enterprise pricing | Out by decision 5; public-repo PR execution risk |

No vendor offers a managed macOS runner that grants
`kTCCServiceAudioCapture`.

## 7. Reference submodules

Six pins moved and four stayed; full table, deltas and the 16-item
harvest ranking are in
[`REFERENCE_ANALYSIS.md`](../../reference/REFERENCE_ANALYSIS.md#reference-delta--harvest-2026-11-25).

| Submodule | New pin |
|---|---|
| wasapi-rs | `13cbc80` v0.27.0 |
| rtrb | `5596032` 0.4.1 |
| pipewire-rs | `70b20f0` v0.10.1 |
| cpal | `44ba7ad` master |
| obs-pipewire-audio-capture | `cc5a661` 1.2.2 |
| screencapturekit-rs | `0d8e0f1` v12.0.0 |
| wiremix, AudioCap, audio-rec, camilladsp | unchanged |

The wasapi-rs, rtrb and pipewire-rs pins now lead `Cargo.toml` (0.23.0,
0.3.4, 0.9.2). The dependency bumps are separate changes, in this order:
rtrb 0.4.1 (no source edits expected), wasapi 0.27 (about ten call
sites), pipewire/libspa/libspa-sys 0.10.1 (six `iterate` calls).

Top five harvest items, all small and needing no bump:

1. Zero WASAPI packets flagged `SILENT` (wasapi `2f10077`).
2. Drop COM objects before `wasapi::deinitialize()` (wasapi `647bac6`).
3. PipeWire chunk offset handling (cpal `91d56f9`).
4. Stricter Linux app matching plus the UTF-8 fix (OBS, wiremix).
5. Honest macOS `supports_system_capture`.

## 8. Open items and what would change these conclusions

- The Blacksmith Windows probe ran once and reported `AUDIO_STACK_USABLE`
  (§5.1). Moving `windows-audio` to Blacksmith is possible but unproven: no
  rsac capture has run there.
- First `macos-audio` run on GitHub-hosted `macos-15` (2026-11-26, run
  45738663823): the Device tier delivered frames (94 buffers, 48,128 frames
  from BlackHole 2ch in 2.25 s), so AUHAL does not hang there and the truth
  table is 7 of 9. System and Process Tap tiers still skip on the Audio
  Capture TCC gate. rsac#26 is specific to the Blacksmith macOS image.
- Consider a macOS leg for the MSRV job.
- Land the three dependency bumps as separate commits; re-run
  `tests/rt_alloc.rs`, the bridge tests, the Windows and Linux audio tiers.
- Fix the §2.3 defects; re-check `docs/MOBILE_BACKEND_DESIGN.md`'s cpal
  AAudio citations and `docs/reviews/rsac-ux-enhancement-backlog.md`'s
  screencapturekit-rs and macOS 15 claims against the new pins.
- If the repository goes private, re-price: GitHub-hosted Windows and
  macOS minutes stop being free.
