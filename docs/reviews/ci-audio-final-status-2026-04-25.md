# CI Audio Integration — Final Status

**Date:** 2026-04-25 (revised after macOS verification)
**Scope:** Comprehensive platform-by-platform audit of rsac's CI audio integration
**Trigger:** User question "are we able to properly test application, device, system capture on all 3 platforms?"

## One-line answer (revised)

**6 of 9 capture-mode × platform cells are genuinely tested end-to-end on every CI run.** The 3 remaining gaps are all on macOS and all blocked by macOS-specific platform-security constraints (`kTCCServiceAudioCapture` for Process Tap / SystemDefault, plus AUHAL-needs-working-output for Device Capture on a headless VM). Correctly documented as manual-QA before release.

## Cell-by-cell truth table (revised)

| | **Linux** (PipeWire) | **macOS** (CoreAudio) | **Windows** (WASAPI) |
|---|---|---|---|
| **System Capture** | ✅ REAL | ⚠️ SKIP-EARLY (Process Tap gate) | ✅ REAL (PCM16+PlayLooping, VB-CABLE verified default) |
| **Device Capture** | ✅ REAL | ⚠️ ERRORS-GRACEFULLY (BlackHole-as-input hangs AUHAL on headless VM) | ✅ REAL (137 buffers, RMS 0.570) |
| **Process Capture** | ✅ REAL | ❌ SKIP-EARLY (TCC Audio Capture, industry-wide limit) | ✅ REAL (WASAPI Process Loopback) |

## Corrections landed this session

### 1. macOS System Capture DOES need TCC

The initial write-up (pre-revision) claimed "System Capture on macOS doesn't require TCC — only Process Tap does". **That was wrong.** Inspection of `src/audio/macos/tap.rs::new_system()` shows that `CaptureTarget::SystemDefault` calls `AudioHardwareCreateProcessTap` via `CATapDescription`'s `initStereoGlobalTapButExcludeProcesses:`. It's the SAME API that gates Process Tap, and it requires the SAME `kTCCServiceAudioCapture` grant. BlackHole as default output does NOT bypass this — taps are orthogonal to the default output route.

CI evidence (run 24920595687, commit 96499f2):
- `test_capture_format_correct` hit `gtimeout --preserve-status 360` at 359.1s with no `ok`/`FAILED` — `AudioHardwareCreateProcessTap` hung inside `.start()` waiting for TCC.
- Step "succeeded" only due to `--preserve-status` + `continue-on-error: true`. Silent failure.

**Fix:** New `require_system_capture!()` macro in `helpers.rs` applies the `macos_tcc_available()` gate for any test that uses `CaptureTarget::SystemDefault`. Same skip-early behavior as Process Tap tests.

### 2. macOS Device Capture hangs AUHAL on headless VM

`test_capture_from_selected_device` targeting BlackHole 2ch (the only enumerated device on blacksmith-6vcpu-macos-15) hangs inside `audio_unit.start()` for 11 minutes, eventually erroring with `OSStatus 2003332927` (`kAudio_UnimplementedError`). CoreAudio appears to wait for a functional output audio path that doesn't exist in headless Firecracker macOS VMs. The test code's graceful-skip handles the eventual error, but the hang wastes CI budget.

**Fix:** Added `gtimeout --preserve-status 120` at the step level in `macos-device` job. Test completes fast (error or skip) rather than blowing the full job timeout.

### 3. Windows: `System.Media.SoundPlayer` cannot play float32 WAVs

Already corrected in Wave F (commit 360025e). PCM16 sibling WAV + `PlayLooping` pattern is stable at 3/3 Windows tests passing.

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

### macOS (0/3 REAL end-to-end on managed CI)

**System Capture:** Blocked by `kTCCServiceAudioCapture` (via `AudioHardwareCreateProcessTap` inside `CATapDescription::initStereoGlobalTapButExcludeProcesses:`). Skip-early via `require_system_capture!()` gate.

**Device Capture:** BlackHole-as-input hangs AUHAL `.start()` on headless VM. Test handles the eventual error gracefully. Will produce real buffers on real hardware with a functional output device + BlackHole loopback.

**Process Capture:** Blocked by same `kTCCServiceAudioCapture` as System Capture. Skip-early via `require_app_capture!()` / `require_process_capture!()`.

All three macOS paths are verified via manual QA before each release on real hardware (developer laptops with TCC grants + audio routing).

### Windows (3/3 REAL)

Real WASAPI + VB-CABLE loopback on every CI run:
- LABSN/sound-ci-helpers@v1 installs VB-CABLE driver
- `AudioDeviceCmdlets` + `Set-AudioDevice -DefaultOnly` sets VB-CABLE as default playback
- "Verify VB-CABLE is the active default playback" gating step exits 1 if not default
- PCM16 sibling WAV + `SoundPlayer.PlayLooping()` routes tone through VB-CABLE
- WASAPI loopback captures the tone
- Tests assert on real non-silent buffers

Evidence: 137 buffers from VB-CABLE device, RMS 0.570, 48,480 frames, max amplitude 0.799.

## macOS CI: paths not taken and why

### ❌ "Pre-grant TCC Audio Capture on runner"
- `tccutil` only REVOKES, not GRANTS
- Editing `TCC.db` requires SIP disabled — impossible on managed runners
- GitHub runner-images pre-grants `kTCCServiceScreenCapture` to `/bin/bash` via `configure-tccdb-macos.sh`, but NOT `kTCCServiceAudioCapture`
- No path exists without self-hosted runner + one-time manual Audio Capture grant

### ❌ "Use BlackHole as default output, capture via SystemDefault"
- `SystemDefault` code path internally calls `AudioHardwareCreateProcessTap` — same TCC gate regardless of default output device
- Tested empirically in run 24920595687: hung for 6 min inside `.start()`

### ⏳ "Use BlackHole as default output, capture via Device(BlackHole_ID)"
- Theoretically valid (AUHAL input path doesn't need TCC Audio Capture)
- Empirically fails on headless Blacksmith VM: AUHAL `.start()` hangs 11 min then errors `OSStatus 2003332927` — CoreAudio waits for functional output audio path that doesn't exist in Firecracker macOS VMs
- May work on GH-hosted macos-14 (different runner class with more complete audio stack) — untested, future work

### ✅ "Skip-early on TCC gate + manual QA on real hardware before release"
- Current approach. Documented in VISION.md § "How We Verify the Vision".

## Key learnings

### 1. macOS has NO non-TCC path for system-wide capture (Apple intentional)
Since macOS 14.4, Apple deprecated all non-TCC system-audio-capture APIs. Process Tap + `CATapDescription` is the only supported path. Pre-14.4 alternatives (kext-style virtual drivers like SoundFlower, Rogue Amoeba ACE) all required SIP modifications or Reduced Security mode. There is no "just plug in BlackHole" shortcut for `SystemDefault`.

### 2. CoreAudio AUHAL needs a working output path even for input-only capture
Even when targeting a virtual input-capable device (BlackHole), AUHAL's `.start()` hangs on a headless runner with no output hardware. This is not TCC-related — it's a deeper CoreAudio expectation about the audio subsystem being "complete". May be fixable by installing a 2nd virtual device (e.g., "Null Output") to satisfy the output-side requirement; untested.

### 3. `continue-on-error: true` + `gtimeout --preserve-status` can mask silent failures
Our initial claim "8/9 cells REAL" was wrong because the macOS-system job appeared green but had actually timed out silently. Fix: always check the `test result: ok. N passed` line in job logs, not just the step's overall `success` status. Skill `headless-ci-audio-hangs` v1.1 updated to call this out.

## Open backlog after this loop

| # | Status | Reason |
|---|---|---|
| **rsac#26** | In progress | Fix for hang identified + landing; 1-2 green runs needed to validate |
| **rsac#21** | Requires deliberately-failing test PR | User opt-in |
| **rsac#19** | Alpine musl + PipeWire runtime validation | Needs 4-6h dedicated Alpine Docker + audio infra session |
| **rsac#16** | docs.rs post-publish spot-check | Gated on user running first `cargo publish` |
| (new) **macOS-BlackHole-Device-path** | Future enhancement | Try GH-hosted macos-14 runner, or install a virtual output device alongside BlackHole |

## Commits this session

- **6d28fd1**: fixed 8-run CI-red from `cargo clippy --fix` renaming cfg-gated `caps` → `_caps`
- **17fd984**: VISION.md canonical doc
- **f4e6a2a**, **a6c7772**, **ea73798**, **efbe0e4**: multi_source tests + fmt hygiene
- **360025e**: Wave F — rsac#22/#23/#24(partial)/#25 closure with 4 parallel agents
- **6ba694f**: test_capture_format_correct tone-player fix (closed rsac#24)
- **96499f2**: macOS System Capture BlackHole+SwitchAudioSource wiring (superseded by this loop's fix)
- **749cf7a**: initial final-status doc (superseded by this revision)
- (this loop): `require_system_capture!()` macro + gtimeout on macos-device + corrected truth table
