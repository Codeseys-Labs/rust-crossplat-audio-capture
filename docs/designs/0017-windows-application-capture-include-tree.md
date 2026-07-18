# ADR 0017 — Windows `Application(pid)` capture routes through process-loopback INCLUDE-tree

**Status:** Accepted
**Date:** 2026-07-18
**Scope:** `src/audio/windows/thread.rs` (`create_audio_client`,
`CaptureTarget::Application` / `ApplicationByName` arms), `src/core/config.rs`
(`CaptureTarget::Application` doc)
**Verdict:** On Windows (WASAPI), `CaptureTarget::Application(pid)` and
`ApplicationByName` are served by the **Process Loopback** API in
**INCLUDE-target-process-tree** mode, not the `IAudioSessionManager2` per-session
path. Because the OS process-loopback mode is binary (INCLUDE- vs
EXCLUDE-target-tree, with no "this PID only" mode), single-application capture is
expressed as INCLUDE-tree of the target PID — which for a leaf process is exactly
single-process capture, and for a process with audio-producing children captures
those children too (a documented per-platform divergence from Linux/macOS).

## 1. Context

`CaptureTarget::Application(pid)` on `windows-latest` CI delivered **only
silence** (max amplitude 0.000000 across the full 15 s window) on every recorded
run — `ci-audio-tests` run 28902137869 and all historical runs — while
`CaptureTarget::ProcessTree(pid)` of the **same** spawned player, on the same
endpoint in the same run, captured the 440 Hz tone at RMS ≈ 0.53. The Windows
process CI tier carried a step-level `RSAC_CI_AUDIO_DETERMINISTIC=0` opt-out to
keep the app-capture step from hard-failing on this.

Two competing hypotheses were on the table (seed `rsac-5b59`):

- **G1 — per-PID audio-session resolution.** The backend resolves the target's
  WASAPI session by PID via `IAudioSessionManager2` /
  `IAudioSessionEnumerator` and something about that lookup fails on the runner
  (session created after enumeration; the `PlaySound`/`SoundPlayer` session
  attributed to a different PID; the session manager only enumerating the default
  render endpoint's sessions).
- **G2 — process-loopback mode.** `Application(pid)` and `ProcessTree(pid)` both
  go through the *same* Process Loopback activation
  (`ActivateAudioInterfaceAsync` with
  `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`) and differ only in the
  `include_tree` boolean; the boolean was wrong for the single-app case.

### Code-reading finding

`enumerate_application_audio_sessions()` in `wasapi.rs` (the
`IAudioSessionManager2` → `GetSessionEnumerator` → per-session `GetProcessId`
path) is **discovery-only**. Nothing on the capture path calls it — the capture
thread's `create_audio_client()` never touches session enumeration. So **G1 was a
red herring**: there is no per-PID session-attribution step to fail.

The actual capture path for all three per-app/tree targets is
`wasapi::AudioClient::new_application_loopback_client(pid, include_tree)`. In
wasapi 0.23.0 (`api.rs::new_application_loopback_client`) the boolean maps
directly to `AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS::ProcessLoopbackMode`:

```rust
ProcessLoopbackMode: if include_tree {
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
} else {
    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
}
```

The `Application` and `ApplicationByName` arms passed `include_tree = false` →
**EXCLUDE**-target-tree = "capture everything **except** this PID's tree". When
the spawned player is the only audio source on the runner, its complement is
**silence** — the exact CI symptom. `ProcessTree` passed `true` → INCLUDE, so it
got the tone. (Note the wasapi crate's own doc comment describes `false` as
"only audio from the target process is captured", which is **wrong** vs. the
code above; the code — verified against the 0.23.0 source — is authoritative.)

`AUDIOCLIENT_PROCESS_LOOPBACK_MODE`
([Microsoft docs](https://learn.microsoft.com/en-us/windows/win32/api/audioclientactivationparams/ne-audioclientactivationparams-process_loopback_mode))
has exactly **two** values — INCLUDE- and EXCLUDE-target-process-tree. There is
**no** "single PID, excluding descendants" mode. So there is no OS primitive that
expresses "this process only" for a process that has children.

## 2. Decision drivers

- **Correctness over silence.** A single-app capture that returns the complement
  of the app (silence) is the worst kind of wrong: it looks alive and carries no
  signal, with no error.
- **Only expressible mapping.** With a binary OS mode, INCLUDE-tree is the *only*
  value that ever captures the target's own audio. EXCLUDE can never.
- **Leaf == single process.** INCLUDE-tree of a process with no audio-producing
  descendants is bit-for-bit single-process capture. The CI player
  (`System.Media.SoundPlayer.PlayLooping()` runs *in-process* in the captured
  `powershell.exe`) is exactly such a leaf, so the fix is semantically exact for
  the deterministic CI assertion.
- **Honest capabilities** (`AGENTS.md`, `src/core/capabilities.rs`). Where the
  platform cannot match the strict single-process semantics Linux/macOS give, we
  document the divergence rather than silently pretend parity.

## 3. Decision

In `create_audio_client()`, the `CaptureTarget::Application(pid)` and
`CaptureTarget::ApplicationByName(name)` arms pass **`include_tree = true`** to
`new_application_loopback_client`, identical to `ProcessTree(pid)`. All three
per-app/tree Windows targets therefore use
`PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE`.

The `IAudioSessionManager2` enumeration path stays as it is —
**discovery-only** (`enumerate_application_audio_sessions`, used to list active
sessions for a UI/picker), never wired into capture.

`CaptureTarget::Application`'s doc in `core/config.rs` records the resulting
per-platform semantics explicitly:

- **Linux (PipeWire) / macOS (CoreAudio):** captures **only** the one process
  (no descendants).
- **Windows (WASAPI):** INCLUDE-tree of the PID — single-process for a leaf; if
  the target has audio-producing children, their audio is included too.

## 4. Consequences

- `Application(pid)` / `ApplicationByName` now capture the target's audio on
  Windows instead of its complement. The CI `RSAC_CI_AUDIO_DETERMINISTIC=0`
  opt-out on the Windows process tier's app-capture step is removed; the step
  runs hard-gated like the others.
- **Semantic divergence, accepted and documented:** on Windows, capturing an app
  that spawns audio-producing child processes will include the children. There is
  no OS mechanism to avoid this short of the session-based path (which cannot
  target a live capture stream by PID the way process-loopback does). Callers who
  need strict single-process semantics with a known child-spawning target on
  Windows have no OS primitive for it today; this is called out on the variant.
- The fix is verified structurally (wasapi 0.23.0 source confirms the mode
  mapping; the CI player is an in-process leaf). The **authoritative proof is the
  `windows-latest` CI run** — see acceptance criteria in `rsac-5b59` and §5.

## 5. Alternatives considered

- **Fix a per-PID session-resolution retry loop (G1).** Rejected: there is no
  session-resolution step on the capture path to fix — `create_audio_client`
  never enumerates sessions. Building one (resolve PID → session → some
  session-scoped capture) would be a new, more fragile path than process
  loopback, which already keys directly off the PID and is what `ProcessTree`
  proves works on the same runner.
- **Pass a single-PID-only loopback mode.** Impossible: no such value exists in
  `AUDIOCLIENT_PROCESS_LOOPBACK_MODE`.
- **Post-filter EXCLUDE-tree output.** Nonsensical: EXCLUDE captures everything
  *but* the target, so it contains no target audio to recover.

## 6. References

- Seed `rsac-5b59`.
- wasapi 0.23.0 `api.rs::new_application_loopback_client` (the `include_tree`
  → `PROCESS_LOOPBACK_MODE_*` mapping, verified against crate source).
- [`AUDIOCLIENT_PROCESS_LOOPBACK_MODE`](https://learn.microsoft.com/en-us/windows/win32/api/audioclientactivationparams/ne-audioclientactivationparams-process_loopback_mode)
  — the binary INCLUDE/EXCLUDE enumeration.
- [ADR-0016](0016-macos-process-tap-silent-zeros-guard.md) — the sibling
  "capture looks alive but streams silence" trap on macOS (same honest-failure
  principle).
- [ADR-0013](0013-mobile-capturetarget-semantics.md) — precedent for documenting
  per-platform `CaptureTarget` semantic divergence.
