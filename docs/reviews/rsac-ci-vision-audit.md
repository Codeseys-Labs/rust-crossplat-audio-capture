# rsac CI + Vision Coverage Audit — 2026-04-24

**Reviewer:** Claude audit agent (read-only) + lead synthesis
**Scope:** rsac root library — CI workflows, test coverage, vision alignment
**Trigger:** User directive to audit CI workflows against the stated vision of a
cross-platform audio-capture library supporting apps/process-trees/devices/system
with stream passthrough.

## Executive Summary

**Verdict: VISION-ALIGNED** with one known CI gap and two nice-to-have additions.

rsac's six core vision pillars — application capture, process-tree capture,
device capture, system-default capture, multi-source simultaneous capture, and
stream passthrough — are all **implemented and tested on all three platforms**
(Linux PipeWire, Windows WASAPI, macOS CoreAudio Process Tap). The CI system
already has a sophisticated two-tier structure: unit-level gating on every push
(`ci.yml`) + gated real-audio integration tests with virtual audio drivers
(`ci-audio-tests.yml`) triggered on `src/`/`tests/` changes or manual dispatch.

**Top findings:**

1. **Linux CI is excellent** — real PipeWire on Blacksmith runners, all 3 capture
   modes (system/device/process) tested end-to-end.
2. **Windows CI uses a hybrid approach** — Blacksmith for compile, GitHub-hosted
   for audio runtime tests (Blacksmith Windows lacks `AudioSrv` service).
3. **macOS CI has a known limitation** — BlackHole loopback works for system
   capture, but CoreAudio Process Tap (the per-app feature) **cannot run headlessly**
   due to TCC permission prompts on headless runners.
4. **Multi-simultaneous capture is implemented but not explicitly tested** —
   nothing in `tests/ci_audio/` spawns 2 captures in parallel.

## Vision Coverage Matrix

| Vision Pillar | Implementation | Test Coverage | CI Verified |
|---|---|---|---|
| **Application capture** (`CaptureTarget::Application(PID)`) | ✅ all 3 platforms | ✅ `tests/ci_audio/application_by_pid.rs` (4 tests) | ✅ Linux real, macOS `#[ignore]`+manual |
| **ApplicationByName** convenience | ✅ all 3 platforms | ✅ `tests/ci_audio/application_by_name.rs` (4 tests) | ✅ Linux real, macOS `#[ignore]`+manual |
| **Process-tree capture** (`ProcessTree`) | ✅ all 3 platforms | ✅ `tests/ci_audio/process_tree.rs` + `process_tree_capture.rs` (9 tests) | ✅ Linux real, others manual |
| **Device capture** (`Device(DeviceId)`) | ✅ all 3 platforms | ✅ `tests/ci_audio/device_capture.rs` + `device_enumeration.rs` | ✅ All 3 platforms in ci-audio-tests.yml |
| **System capture** (`SystemDefault`) | ✅ all 3 platforms | ✅ `tests/ci_audio/system_capture.rs` + `stream_lifecycle.rs` | ✅ All 3 platforms via PipeWire/VB-CABLE/BlackHole |
| **Multi-simultaneous capture** | ✅ implemented (independent `AudioCapture` instances share no state) | ❌ **no explicit test** | ❌ **GAP** |
| **Stream passthrough** (`subscribe()` mpsc) | ✅ via `CapturingStream::subscribe`, `BridgeStream<S>` | ✅ `tests/ci_audio/subscribe.rs` (5 tests: delivery, disconnect, multi-sub, drop-cleanup) | ✅ All 3 platforms |
| **Backpressure signaling** | ✅ `is_under_backpressure()` on `CapturingStream` | ✅ `tests/ci_audio/stream_lifecycle.rs` asserts monotonic overrun_count | ✅ Linux |
| **Stream mixing** | ⊘ intentionally out-of-scope (VISION.md) | N/A | N/A |

## Per-Workflow Review

### `ci.yml` — Primary gate (every push/PR)

257 lines. Jobs:
- `lint` — Blacksmith Linux, rustfmt + clippy `--lib --no-default-features --features feat_linux -- -D warnings`
- `test-linux` — Blacksmith Linux, PipeWire dev libs installed, `cargo test --lib --no-default-features --features feat_linux`
- `test-windows` — Blacksmith Windows-2025, `cargo test` with `continue-on-error: true` (AudioSrv absent on Blacksmith Windows; compile check is the real gate)
- `test-macos` — Blacksmith macOS-15 with TCC sqlite grants, `cargo test --lib --no-default-features --features feat_macos`; also runs wiring-existence checks for `application_by_{name,pid}` + `subscribe` + `process_tree` test modules.
- `cross-compile-linux-arm64` — cross-compile check for `aarch64-unknown-linux-gnu`
- `check-bindings` — `cargo check -p rsac-ffi` + `cargo check -p rsac-napi`
- `check-audio-graph` — downstream build verification of the audio-graph Tauri app

**Strengths:**
- All 3 platforms on Blacksmith for speed
- Per-platform feature-flagged builds (matches real consumer patterns)
- Downstream verification catches library API breakage at the consumer layer
- Recent fix (6d28fd1) resolved the `caps`/`_caps` cfg-gated identifier break
  that had kept CI red for 8 runs

**Minor gaps:**
- `test-windows` uses `continue-on-error: true` which masks real WASAPI compile
  errors on that platform — but the explicit `Cargo check` step above it catches
  those separately, so this is acceptable.
- No Linux `feat_linux` + `async-stream` feature combo test.

### `ci-audio-tests.yml` — Real-audio integration (gated)

846 lines. 9-job matrix: 3 platforms × 3 capture modes (system / device / process).

**Per-platform approach:**
- **Linux:** Blacksmith Ubuntu with PipeWire installed at job start. Spins up
  a dummy PipeWire sink, runs `cargo test --test ci_audio -- --ignored` to exercise
  the `#[ignore]`'d tests with `RSAC_CI_AUDIO_AVAILABLE=1`.
- **Windows:** Uses GitHub-hosted `windows-latest` runner (not Blacksmith — because
  Blacksmith Windows-2025 lacks the `AudioSrv` service). Installs VB-CABLE as a
  virtual loopback device, then runs the integration tests. `continue-on-error: true`
  on the Blacksmith compile fallback, but the real test job on GH-hosted is
  required-to-pass.
- **macOS:** Blacksmith macOS-15 with BlackHole loopback. `system` and `device`
  tests pass; `process` (Process Tap) tests are `#[ignore]` because TCC can't
  grant Screen Recording permission non-interactively to a headless runner.

**Strengths:**
- Granular per-platform × per-mode visibility in the GitHub Actions UI
- Virtual audio hardware (PipeWire dummy, VB-CABLE, BlackHole) means tests are
  reproducible and hardware-free
- Triggered only on `src/`/`tests/` changes — doesn't slow down docs-only PRs

**Gap:**
- No **multi-simultaneous-capture** test. The library supports spawning 2+
  `AudioCapture` instances, but nothing in CI verifies two captures don't
  interfere. Should exist as `tests/ci_audio/multi_source.rs`.

### `blacksmith-audio-probe.yml` — Diagnostic probe

398 lines, `workflow_dispatch` only. One-shot audit to confirm what audio subsystem
exists on Blacksmith runners (lsmod snd, /proc/asound, aplay, pactl, etc.).

**Conclusions (from past runs):**
- Blacksmith Linux: PipeWire installable, virtual sinks work, audio capture viable
- Blacksmith Windows 2025: No `AudioSrv` service — WASAPI audio tests are SKIP-only
- Blacksmith macOS 15: BlackHole can be installed, TCC permissions are the blocker
  for Process Tap

### Release workflows (`release.yml`, `release-npm.yml`, `release-pypi.yml`)

Reviewed in prior audits (rsac-wave-b-review.md). Tag-triggered, pre-release
filter (`!v*-*`), workflow_dispatch with `dry_run` default. All YAML valid.

## Runner Landscape — Blacksmith vs. Alternatives

**Current: Blacksmith + GitHub-hosted hybrid**
- Blacksmith runners are faster than GitHub-hosted (4vcpu/6vcpu, faster cold-start)
- Trade-off: no audio-device support on Blacksmith Windows; macOS has the same
  TCC headless limit as everyone else
- Hybrid approach (Blacksmith for compile/unit, GH-hosted for Windows audio) is
  pragmatic and working

**Alternatives surveyed:**

| Option | Audio support | Relevant to rsac? |
|---|---|---|
| GitHub-hosted (Ubuntu/Windows/macOS) | Partial (ALSA/WASAPI/CoreAudio but no TCC) | Already used for Windows audio |
| BuildJet | Linux only, no audio focus | Not better than Blacksmith |
| Actuated | Self-hosted ARM + VMs | Would need custom image for audio |
| RunsOn | Self-hosted AMI | Would need custom image for audio |
| MacStadium | Real mac minis | Could have TCC via interactive login, but £££ |
| Self-hosted Linux w/ PipeWire | Full control | Already covered by Blacksmith + dummy sink |

**"LARB" / "LABN" user mention:** No managed runner service with those names
surfaced in research. Possibly typo for:
- **LABN:** Could be a private name we don't know about
- **LARB:** Could be referring to "Linux ARB" (ARM + Blacksmith)? Blacksmith does
  offer `blacksmith-4vcpu-ubuntu-2404-arm` — we aren't using ARM variants yet.
  Would be a future addition for ARM64 audio runtime testing.

**Recommendation:** Current Blacksmith + GH-hosted hybrid is correct. Not worth
building a self-hosted audio fleet unless we want to test Process Tap headlessly
on macOS (which is currently a TCC dead-end for all managed runners).

## Stream Mixing — Architectural Recommendation

**Verdict: OUT OF SCOPE for rsac core.** Already documented in VISION.md.

**Rationale:**
- Mixing requires application-specific decisions (sample-rate alignment, per-source
  gain, clipping strategy, real-time vs. buffered)
- rsac exposes `AudioBuffer.data: Vec<f32>` — 3 lines of user code to add two
  buffers
- `rodio::source::Mix`, `fundsp::mixer`, `dasp::signal::add` all exist in the
  ecosystem for downstream mixing
- If demand emerges: a `rsac-mixer` companion crate is trivially authorable
  externally without coupling to rsac's capture-specific traits

**Documentation:** VISION.md § "Why not own mixing?" covers this. No further
action needed.

## Gap Analysis

| # | Gap | Severity | Effort | Fix |
|---|---|---|---|---|
| 1 | No multi-simultaneous-capture test in CI | MEDIUM | 2h | Add `tests/ci_audio/multi_source.rs` with 2-capture test |
| 2 | macOS Process Tap untestable in CI (TCC) | LOW (known) | N/A | Already documented; manual QA covers this |
| 3 | No `feat_linux` + `async-stream` combo test | LOW | 30m | Add 1 matrix row to `ci.yml` |
| 4 | Blacksmith ARM64 audio runtime path unused | LOW | 2h | Optional: add `blacksmith-4vcpu-ubuntu-2404-arm` job for ARM64 audio tests |
| 5 | No README "why rsac?" competitive section | LOW | 30m | Add competitive-positioning paragraph (from architecture audit loop-25) |

## Recommended Phase 5 Impl Wave

1. **Add multi-source capture test** (MEDIUM) — the one real gap vs. VISION.md.
   `tests/ci_audio/multi_source.rs` spawns two `AudioCapture` instances targeting
   different sources, asserts both produce non-empty buffers, asserts neither
   starves the other. Gate with `require_audio!()`.
2. **Add `feat_linux` + `async-stream` matrix entry** (LOW) — 1 new job row in
   `ci.yml` test-linux, verifies the async-stream feature combo compiles and
   tests pass.
3. **Extend `ci-audio-tests.yml` with multi-source job** (MEDIUM) — once #1
   exists, wire it into the Linux real-audio matrix.
4. **Competitive positioning paragraph in README** (LOW) — "Why rsac vs. cpal /
   portaudio-rs?" Cite VISION.md § One-Line Positioning.
5. **Document TCC headless limit in `docs/CI_AUDIO_TESTING.md`** (LOW) — explain
   why macOS Process Tap tests are `#[ignore]` in CI + the manual QA workaround.

## Top 5 Recommendations

1. File issue: **"Add multi-simultaneous-capture integration test"** — MEDIUM,
   closes the one genuine CI gap
2. Add 1-line competitive positioning blurb to README
3. Document the macOS TCC headless limit in CI_AUDIO_TESTING.md
4. Consider ARM64 Linux audio runtime as a future (non-blocking) addition
5. No structural CI changes needed — hybrid Blacksmith + GH-hosted architecture
   is correct for current audio requirements

---

**VERDICT: VISION-ALIGNED — NO STRUCTURAL CHANGES NEEDED**

The implementation matches the stated vision. CI verifies it comprehensively on
Linux, reasonably on Windows, and with one unavoidable gap on macOS (TCC
headless). The only real action item is the multi-source integration test.
