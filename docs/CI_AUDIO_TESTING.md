# CI Audio Integration Testing

This document is the maintainer-facing reference for rsac's audio
integration tests — what they cover, what they cannot cover, and why.

The canonical loop retrospective is
[`docs/reviews/ci-audio-final-status-2026-04-25.md`](reviews/ci-audio-final-status-2026-04-25.md);
this document distills the durable facts from it.

## 1. Truth table (6 of 9 cells REAL)

Each cell is one tier (a step group) of the per-platform job in
[`.github/workflows/ci-audio-tests.yml`](../.github/workflows/ci-audio-tests.yml).

|  | **Linux** (PipeWire) | **Windows** (WASAPI) | **macOS** (CoreAudio) |
|---|---|---|---|
| **System capture** | REAL | REAL (VB-CABLE) | SKIP-EARLY (TCC) |
| **Device capture** | REAL | REAL (VB-CABLE) | ERRORS-GRACEFULLY (AUHAL) |
| **Process capture** | REAL | REAL (Process Loopback) | SKIP-EARLY (TCC) |

The two Process Tap gaps (System, Process) are platform-security
constraints: managed headless runners cannot grant the TCC service they
need. The Device gap is an environment hang that was observed on Blacksmith
macOS. Since `macos-audio` moved to GitHub-hosted `macos-15` with BlackHole
pinned as the default output and input, that cell is unverified; the first
run there decides whether it becomes REAL (7 of 9). All three macOS paths are
verified on real hardware by manual QA before each release (see
[`VISION.md`](../VISION.md) § "How We Verify the Vision").

### What "REAL" means

For the six REAL cells, every CI run:

1. Installs the platform's audio stack (PipeWire for Linux, VB-CABLE for
   Windows, BlackHole for the macOS device path, which is not yet REAL;
   see below).
2. Routes a 440 Hz test tone through the OS's default output.
3. Captures through the rsac public API.
4. Asserts the captured `AudioBuffer` has non-silent data (`max
   amplitude > 0.001`) and measurable RMS energy.

Reference evidence: Linux runs capture >40 K frames per test; Windows
reports ~137 buffers at RMS 0.570 from VB-CABLE loopback.

### What SKIP-EARLY / ERRORS-GRACEFULLY mean on macOS

- **SKIP-EARLY:** The test helpers call `require_system_capture!()` or
  `require_process_capture!()`, which gate on
  `RSAC_CI_MACOS_TCC_GRANTED=1`. Since managed runners never have this
  env var set, the test returns immediately with a diagnostic banner
  rather than blocking 10–18 minutes inside
  `AudioHardwareCreateProcessTap`.
- **ERRORS-GRACEFULLY:** on `blacksmith-6vcpu-macos-15`, macOS Device
  capture targeting BlackHole hit `AUHAL.start()`, which hung ~11 minutes
  and then failed with `OSStatus 2003332927`. That value is `0x77686F3F`,
  the FourCC `'who?'`, which is `kAudioHardwareUnknownPropertyError` (the
  object does not know the requested property). Earlier versions of this
  document misread it as `kAudio_UnimplementedError` (`-4`). The cause is
  not proven. The two candidates are a missing functional default *output*
  path on the headless VM and a missing Microphone grant for the job's
  responsible process (see [The TCC gate](#the-tcc-gate)).
  `AudioOutputUnitStart` takes no timeout, so the Device tier runs under
  `gtimeout 120` and tolerates exit 124 only.

## 2. Test gate macros

All in [`tests/ci_audio/helpers.rs`](../tests/ci_audio/helpers.rs):

| Macro | Gates on | Used by |
|---|---|---|
| `require_audio!()` | `audio_infrastructure_available()` — env `RSAC_CI_AUDIO_AVAILABLE=1` or runtime probe (PipeWire socket, device enumeration). | Every integration test. |
| `require_system_capture!()` | `require_audio!()` + `macos_tcc_available()`. | `SystemDefault` targets — `system_capture.rs`. |
| `require_app_capture!()` | `require_audio!()` + TCC + `caps.supports_application_capture`. | Per-app tests — `app_capture.rs`, `application_by_name.rs`, `application_by_pid.rs`. |
| `require_process_capture!()` | `require_audio!()` + TCC + `caps.supports_process_tree_capture`. | Process-tree tests — `process_tree.rs`, `process_tree_capture.rs`. |
| `require_device_selection!()` | `require_audio!()` + `caps.supports_device_selection`. | Device-targeted tests — `device_capture.rs`, `device_enumeration.rs`. |

All five macros print a boxed "SKIPPING" banner and `return` — they do
not mark the test as failed, which matches CI's intent of "honest skip
when the environment lacks the prerequisites".

### The TCC gate

`macos_tcc_available()` returns `true` iff `target_os = "macos"` and
`RSAC_CI_MACOS_TCC_GRANTED=1` (non-macOS hosts always return `true`).

macOS gates rsac's capture paths on different TCC services. A grant for one
says nothing about the others:

| rsac path | API | TCC service |
|---|---|---|
| `SystemDefault`, `Application*`, `ProcessTree` | `AudioHardwareCreateProcessTap` (`CATapDescription`, including `initStereoGlobalTapButExcludeProcesses:`) | `kTCCServiceAudioCapture` (System Audio Recording) |
| `Device` | AUHAL input on a HAL device, BlackHole included | `kTCCServiceMicrophone` |
| none (ScreenCaptureKit, not used by rsac) | `SCStream` | `kTCCServiceScreenCapture` (Screen Recording) |

GitHub-hosted macOS images pre-seed TCC in
[`configure-tccdb-macos.sh`](https://github.com/actions/runner-images/blob/main/images/macos/scripts/build/configure-tccdb-macos.sh)
(read 2026-11-25):

- `kTCCServiceMicrophone`: `/opt/hca/hosted-compute-agent`,
  `/opt/hca/start_hca.sh`, `/usr/local/opt/runner/provisioner/provisioner`,
  `/usr/local/opt/runner/runprovisioner.sh`, and (user database)
  `com.apple.CoreSimulator.SimulatorTrampoline`. Not `/bin/bash`.
- `kTCCServiceScreenCapture`: `/bin/bash`, the agent and the provisioner.
- `kTCCServiceAudioCapture`: no rows.

TCC charges a request to the *responsible* process, which on a
GitHub-hosted runner is the agent. A `cargo test` binary started by a job
step may therefore pass the Microphone check on `macos-15` [INFERENCE, not
yet observed]. Blacksmith runs its own runner agent, which is consistent
with (but does not prove) the Device-tier hang there. On the same
Blacksmith runner class the iOS simulator leg did deliver BlackHole frames
(PR #67, run 29717781236); the simulator reaches the mic through
`SimulatorTrampoline`, not through the job's shell.

Nothing on a managed runner grants `kTCCServiceAudioCapture`. The system
TCC database is SIP-protected, so a job cannot insert the row, and the
consent prompt needs a person. Only a self-hosted macOS runner with a
one-time interactive grant
([`SELF_HOSTED_TCC_RUNNER.md`](SELF_HOSTED_TCC_RUNNER.md)) can lift this
gate.

### Why LABSN `background-music` does not help on macOS

The macOS half of `LABSN/sound-ci-helpers`
([`macos/setup_sound.sh`](https://github.com/LABSN/sound-ci-helpers/blob/main/macos/setup_sound.sh))
installs the `background-music` cask and restarts `coreaudiod`. Nothing
else. That adds one more HAL virtual device, the same plug-in mechanism as
BlackHole, and changes no TCC state:

- The Process Tap tiers still need `kTCCServiceAudioCapture`. No virtual
  device supplies it.
- The Device tier already has BlackHole. Reading Background Music's input
  is Microphone-gated the same way, and the device depends on its
  companion app (`BGMApp`), which BlackHole does not.
- LABSN's own CI only checks that an output device enumerates. It never
  starts a capture.

The workflow uses LABSN on Windows only, for VB-CABLE.

## 3. Test file layout

```
tests/ci_audio/
├── main.rs                    # harness entry; declares the other modules
├── helpers.rs                 # gate macros + WAV generator + player
│                              # spawner + verification helpers
├── system_capture.rs          # CaptureTarget::SystemDefault
├── device_capture.rs          # CaptureTarget::Device
├── device_enumeration.rs      # enumerator contract
├── app_capture.rs             # CaptureTarget::Application base
├── application_by_name.rs     # CaptureTarget::ApplicationByName
├── application_by_pid.rs      # CaptureTarget::Application(PID)
├── process_tree.rs            # CaptureTarget::ProcessTree
├── process_tree_capture.rs    # end-to-end tree capture
├── multi_source.rs            # two AudioCapture instances in one process
├── stream_lifecycle.rs        # Created → Running → Stopping → Stopped
├── lifecycle_terminal.rs      # request_stop() + terminal read (StreamEnded)
├── overrun.rs                 # G8 ring-buffer overflow (overrun_count)
├── subscribe.rs               # mpsc subscription contract
└── platform_caps.rs           # PlatformCapabilities::query sanity
```

Tests use only the public API (`rsac::*`); platform gating is
centralised in `helpers.rs` so the tests themselves stay portable.

## 4. Platform-specific setup the workflow performs

**Triggers.** The workflow runs on `workflow_dispatch` (input `platform`:
`all`, `linux`, `windows`, `macos` or `bindings`; default `all`) and on stable
`v*.*.*` tags. There is no `pull_request` or branch-push trigger; dispatch it
on a branch when a change needs real-audio evidence
(`gh workflow run ci-audio-tests.yml --ref <branch> -f platform=<p>`).

### Linux — `linux-audio` job (system / device / process tiers)

- Ubuntu 24.04 on `blacksmith-4vcpu-ubuntu-2404`.
- Installs PipeWire + WirePlumber + `pipewire-pulse` from
  `ppa:pipewire-debian/pipewire-upstream`.
- Launches `pipewire`, `wireplumber`, `pipewire-pulse` daemons manually
  because Blacksmith Firecracker microVMs lack a D-Bus user session
  (`systemctl --user` will not start them).
- `pactl load-module module-null-sink` creates a virtual sink.
- **Routing gate** (`scripts/ci-linux-audio-route.sh`, rsac-b106/rsac-6efb):
  pins `ci_test_sink` as the default sink (pactl → pw-metadata → wpctl
  ladder; single-sink fallback is accepted), then proves the route
  end-to-end at the OS level — `pw-play` a 440 Hz tone with default
  routing, `parecord` the sink monitor, assert RMS + rough frequency with
  sox. A broken route **fails the job at setup**; a proven route exports
  `RSAC_CI_AUDIO_DETERMINISTIC=1` and `PULSE_SINK=ci_test_sink`.
- `pw-play` (or `paplay`, pinned via `PULSE_SINK`) streams a 440 Hz float
  WAV tone during the tests.

### Windows — `windows-audio` job (system / device / process tiers)

- `windows-latest` runner.
- [`LABSN/sound-ci-helpers@v1`](https://github.com/LABSN/sound-ci-helpers)
  installs the VB-CABLE virtual-cable driver.
- **Endpoint gate** (`scripts/ci-windows-audio-default.ps1`, rsac-0f33,
  shared by all three tiers): sets VB-CABLE as the default playback
  endpoint via `AudioDeviceCmdlets` (bounded retries), **hard-verifies**
  the active default, then exports `RSAC_CI_AUDIO_DETERMINISTIC=1` — every
  tier's tone route flows through that verified endpoint (system loopback,
  `default_device()` capture, process loopback of the test player).
- Test tones use a **PCM16 sibling WAV** + `SoundPlayer.PlayLooping()`:
  `System.Media.SoundPlayer` silently drops `WAVE_FORMAT_IEEE_FLOAT`
  frames on the runner's default endpoint (rsac#24), so we convert to
  16-bit PCM before playback. The float WAV is still used by the
  capture-side assertions.

### macOS — `macos-audio` job (system / device / process tiers)

- GitHub-hosted `macos-15` runner, free on this public repository. It
  moved off `blacksmith-6vcpu-macos-15`, which bills at 20× a Linux minute
  and whose Device tier hung on every run.
- Installs `blackhole-2ch`, `switchaudio-osx` and `coreutils` (for
  `gtimeout`), restarts `coreaudiod` (`launchctl kickstart -kp`, falling
  back to `killall`), then pins BlackHole 2ch as the default output **and**
  input with `SwitchAudioSource`, mirroring the iOS simulator leg in
  [`ci-ios-sim.yml`](../.github/workflows/ci-ios-sim.yml). Each step warns
  and continues on failure.
- All Process Tap paths (System / Application / ProcessTree) skip early
  via `require_*_capture!()`, because `kTCCServiceAudioCapture` cannot be
  granted on a managed runner.
- The Device tier runs under `gtimeout 120` and tolerates exit 124 only
  (the known AUHAL hang); any other non-zero exit fails the tier.

## 5. Workflow knobs

- `RSAC_CI_AUDIO_AVAILABLE=1` — set by the workflow once the audio
  stack is up; makes `audio_infrastructure_available()` fast-path to
  `true`. Unset on non-audio jobs.
- `RSAC_CI_AUDIO_DETERMINISTIC=1` — flips the capture tests' soft
  non-silence warnings into **hard assertions**. Set in CI where audio
  routing is deterministic: all three Windows tiers (via the verified
  VB-CABLE endpoint gate, `scripts/ci-windows-audio-default.ps1` —
  rsac-0f33), and all three Linux tiers after the routing gate
  (`scripts/ci-linux-audio-route.sh`) proves the virtual-sink route
  end-to-end (seeds rsac-6efb / rsac-b106). On a local machine with
  real, working audio this is exactly what you want: export it to make
  a silent capture fail loudly instead of warn.
- `RSAC_CI_MACOS_TCC_GRANTED=1` — set only on self-hosted macOS
  runners where Audio Capture has been granted. Unset on Blacksmith
  and GH-hosted macOS.
- `RSAC_TEST_CAPTURE_TIMEOUT_SECS` — overrides the per-test
  capture-deadline (default 10 s).

## 6. Reading CI results

CI's step-level "success" is not enough. Always inspect the
`test result: ok. N passed; M failed; K ignored` line in the job log.
The previously-reported "8/9 REAL" figure turned out to be wrong
because `continue-on-error: true` + `gtimeout --preserve-status` masked
a silent 360-second hang inside macOS Process Tap. See the
[`ci-audio-final-status-2026-04-25`](reviews/ci-audio-final-status-2026-04-25.md)
review § "Key learnings" for the full post-mortem.

## 7. Known CI open items

| Issue | Status | Notes |
|---|---|---|
| **rsac#26** — macOS device-path hang | not reproduced on GitHub-hosted | `macos-audio` runs on GitHub-hosted `macos-15` with BlackHole as default I/O. First run (45738663823, 2026-11-26): the Device tier captured 94 buffers / 48,128 frames, no AUHAL hang. The hang was only ever seen on the Blacksmith macOS image. |
| **rsac#21** — failing-test signalling | requires opt-in PR | User-driven. |
| **rsac#19** — Alpine musl + PipeWire runtime | blocked on infra session | 4-6h Alpine Docker + audio. |
| **rsac#16** — docs.rs post-publish spot-check | gated on first `cargo publish` | `scripts/verify-docs-rs.sh` is ready. |
| Blacksmith Windows audio stack | probed 2026-11-26: `AUDIO_STACK_USABLE` | Run 45738685950: the image is now Desktop Experience with `AudioSrv` running and a VB-CABLE playback endpoint (details in AGENTS.md §6). The hybrid stays because GitHub-hosted `windows-latest` is free and proven; the `ci_audio` windows tier has never run on Blacksmith. |

## 8. Related docs

- [`docs/reviews/ci-audio-final-status-2026-04-25.md`](reviews/ci-audio-final-status-2026-04-25.md) — full retrospective (source of truth).
- [`docs/CONTRIBUTING.md`](CONTRIBUTING.md) — how to run integration tests locally.
- [`docs/LOCAL_TESTING_GUIDE.md`](LOCAL_TESTING_GUIDE.md) — manual QA on real hardware (Windows / macOS / Linux).
- [`docs/MACOS_VERSION_COMPATIBILITY.md`](MACOS_VERSION_COMPATIBILITY.md) — macOS API / version matrix, including the TCC landscape.
- [`docs/SELF_HOSTED_TCC_RUNNER.md`](SELF_HOSTED_TCC_RUNNER.md) — the one route to a `kTCCServiceAudioCapture` grant in CI.
- [`docs/audit/2026-11-25-codebase-ci-market-audit.md`](audit/2026-11-25-codebase-ci-market-audit.md) — runner-vendor comparison, LABSN analysis and the 2026-11-25 CI changes.
- Skill: `headless-ci-audio-hangs` v1.1 — diagnostic runbook.
