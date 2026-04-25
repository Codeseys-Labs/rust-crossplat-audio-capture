# CI Audio Integration Testing

This document is the maintainer-facing reference for rsac's audio
integration tests — what they cover, what they cannot cover, and why.

The canonical loop retrospective is
[`docs/reviews/ci-audio-final-status-2026-04-25.md`](reviews/ci-audio-final-status-2026-04-25.md);
this document distills the durable facts from it.

## 1. Truth table (6 of 9 cells REAL)

Each cell is a separate GitHub Actions job in
[`.github/workflows/ci-audio-tests.yml`](../.github/workflows/ci-audio-tests.yml).

|  | **Linux** (PipeWire) | **Windows** (WASAPI) | **macOS** (CoreAudio) |
|---|---|---|---|
| **System capture** | REAL | REAL (VB-CABLE) | SKIP-EARLY (TCC) |
| **Device capture** | REAL | REAL (VB-CABLE) | ERRORS-GRACEFULLY (AUHAL) |
| **Process capture** | REAL | REAL (Process Loopback) | SKIP-EARLY (TCC) |

The three macOS gaps are not bugs in rsac — they are platform-security
constraints that cannot be worked around on managed headless runners.
All three macOS paths are verified on real hardware by manual QA before
each release (see [`VISION.md`](../VISION.md) § "How We Verify the
Vision").

### What "REAL" means

For the six REAL cells, every CI run:

1. Installs the platform's audio stack (PipeWire for Linux, VB-CABLE for
   Windows, BlackHole for macOS device-path — currently failing, see
   below).
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
- **ERRORS-GRACEFULLY:** macOS Device Capture targeting BlackHole on a
  Firecracker headless VM hits `AUHAL.start()`, which hangs ~11 minutes
  then errors `OSStatus 2003332927` (`kAudio_UnimplementedError`).
  CoreAudio appears to expect a functional *output* audio path even when
  capturing an input-capable virtual device, and that path does not
  exist in the managed VM class. A step-level `gtimeout
  --preserve-status 120` caps the wasted budget.

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

Why: `CATapDescription::initStereoGlobalTapButExcludeProcesses:`
(and every other `AudioHardwareCreateProcessTap` call) is gated by
`kTCCServiceAudioCapture`. This is *not* the same TCC service as
`kTCCServiceScreenCapture` — GitHub Actions' runner-images pre-grant
Screen Capture to `/bin/bash` but not Audio Capture. Only a self-hosted
macOS runner with a one-time interactive TCC grant can lift this gate.

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
├── subscribe.rs               # mpsc subscription contract
└── platform_caps.rs           # PlatformCapabilities::query sanity
```

Tests use only the public API (`rsac::*`); platform gating is
centralised in `helpers.rs` so the tests themselves stay portable.

## 4. Platform-specific setup the workflow performs

### Linux — `linux-system` / `linux-device` / `linux-process`

- Ubuntu 24.04 on `blacksmith-4vcpu-ubuntu-2404`.
- Installs PipeWire + WirePlumber + `pipewire-pulse` from
  `ppa:pipewire-debian/pipewire-upstream`.
- Launches `pipewire`, `wireplumber`, `pipewire-pulse` daemons manually
  because Blacksmith Firecracker microVMs lack a D-Bus user session
  (`systemctl --user` will not start them).
- `pactl load-module module-null-sink` creates a virtual sink.
- `pw-play` (or `paplay`) streams a 440 Hz float WAV tone.

### Windows — `windows-system` / `windows-device` / `windows-process`

- `windows-latest` runner.
- [`LABSN/sound-ci-helpers@v1`](https://github.com/LABSN/sound-ci-helpers)
  installs the VB-CABLE virtual-cable driver.
- `AudioDeviceCmdlets` + `Set-AudioDevice -DefaultOnly` sets VB-CABLE
  as the default playback endpoint; a gating step verifies this and
  exits 1 otherwise.
- Test tones use a **PCM16 sibling WAV** + `SoundPlayer.PlayLooping()`:
  `System.Media.SoundPlayer` silently drops `WAVE_FORMAT_IEEE_FLOAT`
  frames on the runner's default endpoint (rsac#24), so we convert to
  16-bit PCM before playback. The float WAV is still used by the
  capture-side assertions.

### macOS — `macos-system` / `macos-device` / `macos-process`

- `blacksmith-6vcpu-macos-15` runner.
- BlackHole 2ch is installed via Homebrew for the device-path tests.
- `SwitchAudioSource` is installed to set BlackHole as default
  output for a few test paths (not currently sufficient — see the
  AUHAL hang above).
- All Process Tap paths skip early via `require_*_capture!()`.
- Step-level `gtimeout --preserve-status 120` bounds the
  device-path wasted budget.

## 5. Workflow knobs

- `RSAC_CI_AUDIO_AVAILABLE=1` — set by the workflow once the audio
  stack is up; makes `audio_infrastructure_available()` fast-path to
  `true`. Unset on non-audio jobs.
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
| **rsac#26** — macOS device-path hang | fix-in-progress | Needs 1-2 green runs to validate. |
| **rsac#21** — failing-test signalling | requires opt-in PR | User-driven. |
| **rsac#19** — Alpine musl + PipeWire runtime | blocked on infra session | 4-6h Alpine Docker + audio. |
| **rsac#16** — docs.rs post-publish spot-check | gated on first `cargo publish` | `scripts/verify-docs-rs.sh` is ready. |
| macOS BlackHole-Device path on GH-hosted | future work | Different runner class may not have the AUHAL hang. |

## 8. Related docs

- [`docs/reviews/ci-audio-final-status-2026-04-25.md`](reviews/ci-audio-final-status-2026-04-25.md) — full retrospective (source of truth).
- [`docs/CONTRIBUTING.md`](CONTRIBUTING.md) — how to run integration tests locally.
- [`docs/LOCAL_TESTING_GUIDE.md`](LOCAL_TESTING_GUIDE.md) — manual QA on real hardware (Windows / macOS / Linux).
- [`docs/MACOS_VERSION_COMPATIBILITY.md`](MACOS_VERSION_COMPATIBILITY.md) — macOS API / version matrix, including the TCC landscape.
- Skill: `headless-ci-audio-hangs` v1.1 — diagnostic runbook.
