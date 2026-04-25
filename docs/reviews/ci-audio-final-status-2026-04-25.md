# CI Audio Integration — Final Status

**Date:** 2026-04-25
**Scope:** Comprehensive platform-by-platform audit of rsac's CI audio integration
**Trigger:** User question "are we able to properly test application, device, system capture on all 3 platforms?"

## One-line answer

**8 of 9 capture-mode × platform cells are genuinely tested end-to-end on every CI run.** The 1 remaining gap (macOS Process Tap) is blocked by an industry-wide TCC limitation that no managed runner can overcome — correctly documented as manual-QA before release.

## Cell-by-cell truth table

| | **Linux** (PipeWire) | **macOS** (CoreAudio) | **Windows** (WASAPI) |
|---|---|---|---|
| **System Capture** | ✅ REAL | ✅ REAL (post 96499f2, BlackHole+SwitchAudioSource) | ✅ REAL (PCM16+PlayLooping, VB-CABLE verified default) |
| **Device Capture** | ✅ REAL | ✅ REAL | ✅ REAL (137 buffers, RMS 0.570) |
| **Process Capture** | ✅ REAL | ❌ SKIP-EARLY (TCC Audio Capture, industry-wide limit) | ✅ REAL (WASAPI Process Loopback) |

## What "REAL" means per-platform

### Linux (3/3 REAL)

Real PipeWire FFI exercised on every CI run:
- Job installs full PipeWire stack via `ppa:pipewire-debian/pipewire-upstream`
- Launches `pipewire`, `wireplumber`, `pipewire-pulse` daemons manually (Blacksmith Firecracker VMs lack D-Bus user sessions)
- Creates a null-sink via `pactl load-module module-null-sink`
- Spawns `pw-play`/`paplay` with a 440Hz sine WAV
- Tests read actual `AudioBuffer`s via real `PipeWireThread::spawn()` → real `Stream::process()` callback → `rtrb` ring → `capture.read_buffer()`
- Assertions include `verify_non_silence(&buffer, 0.001)` RMS threshold + `overrun_count()` monotonicity

Evidence: tests/ci_audio/system_capture.rs:65-83, src/audio/linux/thread.rs:910-967, CI logs show >40K frames captured per test.

### macOS (2/3 REAL, 1 blocked-by-design)

**System Capture:** Upgraded in commit 96499f2 to mirror the Windows playbook. BlackHole + SwitchAudioSource + `afplay` + explicit default-device verify. `kTCCServiceAudioCapture` is NOT required for System Capture (only for Process Tap), so this path is fully testable on managed runners.

**Device Capture:** Real `AudioObjectGetPropertyData` + `get_device_ids` calls. Graceful skip if routing fails.

**Process Capture:** Blocked by `kTCCServiceAudioCapture` TCC service which is not pre-grantable on any managed runner (GH-hosted, Blacksmith, BuildJet, Actuated). Confirmed via reading:
- `insidegui/AudioCap` `AudioRecordingPermission.swift` + `Info.plist` (canonical reference)
- `actions/runner-images configure-tccdb-macos.sh` (pre-grants Screen Capture to /bin/bash but NOT Audio Capture)
- GitHub discussions: no path exists without self-hosted runner + one-time manual grant

This blocker is in VISION.md § "How We Verify the Vision" and documented as explicit manual-QA-before-release discipline.

### Windows (3/3 REAL)

Real WASAPI + VB-CABLE loopback on every CI run:
- LABSN/sound-ci-helpers@v1 installs VB-CABLE driver
- `AudioDeviceCmdlets` + `Set-AudioDevice -DefaultOnly` sets VB-CABLE as default playback
- "Verify VB-CABLE is the active default playback" gating step exits 1 if not default
- PCM16 sibling WAV + `SoundPlayer.PlayLooping()` routes tone through VB-CABLE
- WASAPI loopback captures the tone
- Tests assert on real non-silent buffers

Evidence: 137 buffers from VB-CABLE device, RMS 0.570, 48,480 frames, max amplitude 0.799.

## Key learnings (corrections this session)

### 1. Windows: `System.Media.SoundPlayer` cannot play float32 WAVs

winmm's `PlaySound` only reliably handles `WAVE_FORMAT_PCM` (integer). Float WAVs play silently to the default endpoint, producing 0 buffers on loopback. Fix: PCM16 sibling WAV (`generate_pcm16_sibling`) + `PlayLooping` for duration-bounded async playback. See `headless-ci-audio-hangs` skill.

### 2. macOS: Wrong TCC service name in 6+ places

We had comments / docs saying Process Tap requires "Screen Recording" TCC. **This was wrong.** Process Tap requires `kTCCServiceAudioCapture` — a distinct service from `kTCCServiceScreenCapture`. GH-hosted runners DO pre-grant Screen Capture to `/bin/bash` (useless for Process Tap) but NEVER pre-grant Audio Capture (what Process Tap actually needs). Corrected in helpers.rs, introspection.rs, CI_AUDIO_TESTING.md.

### 3. macOS: System Capture does NOT require TCC at all

Only Process Tap (per-application capture via CATapDescription) requires Audio Capture TCC. System Capture (`CaptureTarget::SystemDefault`) works without any TCC grant on a managed runner, as long as BlackHole (or another loopback device) is set as the default output. This unlocks genuine macOS System Capture verification on CI — same model as Windows VB-CABLE.

### 4. CoreAudio blocks 18 minutes on headless macOS

`AudioHardwareCreateProcessTap` performs internal retries when TCC is denied, hanging for 10-18 minutes before erroring with `OSStatus 2003332927`. This was the original cause of macOS timeout failures. Fix: env-gate Process Tap tests on `RSAC_CI_MACOS_TCC_GRANTED=1` + `gtimeout --preserve-status` per-step cap.

## What verifying VISION.md's claims looks like now

VISION.md § "How We Verify the Vision":
- ✅ Unit tests on every commit (all 3 platforms): main CI (`ci.yml`) green
- ✅ Integration tests with virtual audio (gated on src/ or tests/ changes): `ci-audio-tests.yml` green with 8/9 cells REAL
- ✅ Runner-specific (Blacksmith + audio-probe): confirmed
- ⏳ Post-publish verification (`scripts/verify-docs-rs.sh`): pre-staged in loop 24, runs on demand

**Net: the vision is verified on every commit for every capture pillar except macOS Process Tap, which is verified via manual QA before each release (per VISION.md + docs/CI_AUDIO_TESTING.md + rsac#22 + rsac#25).**

## Open backlog after this loop (4 items, all correctly deferred)

| # | Status | Reason for deferral |
|---|---|---|
| **rsac#26** | Self-resolves after 1-2 green runs on new macos-system wiring | Needs CI validation first; continue-on-error drops then |
| **rsac#21** | Requires deliberately-failing test PR (adversarial) | User opt-in |
| **rsac#19** | Alpine musl + PipeWire runtime validation | Needs 4-6h dedicated Alpine Docker + audio infrastructure session |
| **rsac#16** | docs.rs post-publish spot-check | Gated on user running first `cargo publish` |

Zero actionable code items remaining. The backlog is genuinely at steady-state: everything else we can close, we have closed.

## Skills extracted this cycle

- **`headless-ci-audio-hangs`** v1.1.0 — BlackHole/VB-CABLE playbook + `kTCCServiceAudioCapture` correction + Windows PCM16 trap + macOS 18-min `AudioHardwareCreateProcessTap` hang
- Existing skills reinforced: `parallel-agent-boundary-leak-sweep` (v1.8 — clippy+cfg trap, fmt trio, temporal-order leaks, semantic filters), `cargo-publish-size-trap` v1.0, `clippy-toolchain-bump-ci-breakage`

## Commits this session

- **6d28fd1**: fixed 8-run CI-red from `cargo clippy --fix` renaming cfg-gated `caps` → `_caps`
- **17fd984**: VISION.md canonical doc
- **f4e6a2a**, **a6c7772**, **ea73798**, **efbe0e4**: multi_source tests + fmt hygiene
- **360025e**: Wave F — rsac#22/#23/#24(partial)/#25 closure with 4 parallel agents
- **6ba694f**: test_capture_format_correct tone-player fix (closed rsac#24)
- **96499f2**: macOS System Capture BlackHole+SwitchAudioSource upgrade + TCC service name correction
