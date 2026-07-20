# Self-hosted macOS TCC-granted audio runner — runbook

This is the operator runbook for the one CI leg managed runners cannot cover:
**real macOS Process-Tap capture** (`CaptureTarget::SystemDefault` /
`Application` / `ProcessTree`), which is gated by the
`kTCCServiceAudioCapture` TCC service. That grant requires a **one-time
interactive "Allow"** and therefore cannot be obtained on Blacksmith or
GitHub-hosted runners — so those legs skip the Process-Tap paths via the
`require_*!()` gates (see [`CI_AUDIO_TESTING.md`](CI_AUDIO_TESTING.md) §2). This
runbook is how the owner stands up a self-hosted Mac that runs those paths for
real.

The kit is three files plus this doc:

| File | Role |
|---|---|
| [`.github/workflows/ci-audio-macos-tcc.yml`](../.github/workflows/ci-audio-macos-tcc.yml) | The on-demand / weekly CI leg. Guard-probes the grant, then runs the full `ci_audio` suite with TCC unlocked. |
| [`scripts/setup-tcc-runner.sh`](../scripts/setup-tcc-runner.sh) | One-shot attended onboarding (run ONCE). |
| [`scripts/start-tcc-runner.sh`](../scripts/start-tcc-runner.sh) | Post-reboot relaunch (no token needed). |

Proven evidence (owner's M4, macOS 26, VS Code terminal, 2026-07-07): the full
`ci_audio` suite is **42/42 GREEN** with `RSAC_CI_AUDIO_AVAILABLE=1
RSAC_CI_AUDIO_DETERMINISTIC=1 RSAC_CI_MACOS_TCC_GRANTED=1`, in ~23 s.

> **Status: kit ready, runner pending owner setup.** Nothing below has run on a
> live self-hosted runner yet — the responsible-bundle inheritance (see §3) is an
> assumption the *first attended run* proves. Do not read this doc as "the tier is
> green."

---

## 1. Security posture — READ BEFORE ENABLING

**A self-hosted runner on a PUBLIC repo that executes PR code is remote code
execution on your physical Mac.** Any forker could open a PR whose `build.rs` or
test code runs as your user. The kit defends against this in two layers, and you
must set up both:

1. **Triggers (in the workflow, already done).**
   `ci-audio-macos-tcc.yml` fires on **`workflow_dispatch` + a weekly cron
   ONLY**. It has **no `pull_request` / `pull_request_target` trigger**, by
   design — never add one. dispatch + cron only ever run code already merged to
   a trusted branch. A `github.repository == …` job guard also stops the cron
   from firing in fork contexts (defense-in-depth).

2. **Runner restrictions (you must set these in the GitHub UI).**
   - **Settings → Actions → Runners →** *(this runner)* → restrict it to **this
     repository only** (not org-wide / not all-repos).
   - **Settings → Actions → General → Fork pull request workflows** → require
     approval for **all outside collaborators** (the strictest setting).
   - Consider a dedicated low-privilege macOS user account for the runner.

If you cannot satisfy both layers, do not bring the runner online.

---

## 2. Grant mechanics (why the probe waits, why captures are ≥10 s)

macOS `kTCCServiceAudioCapture` denial is **silently deceptive** — there is no
error code. The three states, from
[`ADR-0016`](designs/0016-macos-process-tap-silent-zeros-guard.md) and the
`macos-tcc-grant-latency-vs-silence-watchdog` skill:

| Grant state | Symptom |
|---|---|
| **Fresh grant, still propagating (~6.7 s)** | tap "starts" (`noErr`), delivers **all-zero buffers** until it lands. A capture shorter than ~7 s sees only silence and looks denied. |
| **Never granted / denied** | all-zero buffers **forever**, no error. |
| **Stale (granted for an OLD binary hash after a rebuild)** | **indefinite HANG** inside `AudioHardwareCreateProcessTap` — no error, nothing starts. |

Consequences baked into the kit:

- **First capture after a grant must run ≥10 s.** ADR-0016's silence-warn window
  is **10 s** (clears the ~6.7 s propagation with margin,
  `RSAC_SILENCE_GRACE_SECS` overridable). The setup probe and the workflow guard
  both capture **12–14 s** and the setup waits **8 s** between probe #1 (fires
  the prompt) and probe #2 (verifies the grant took).
- **The guard step wraps capture in `gtimeout`** so the stale-grant *hang* mode
  surfaces as a timeout kill (exit 124) rather than a wedged job, with a re-grant
  message pointing back here.
- **A lost grant is a FAILURE, not a skip.** A TCC-gated tier that can no longer
  capture has nothing left to test — the guard fails the job loudly.

---

## 3. The responsible-bundle trap (the crux)

**TCC attaches an Audio-Capture grant to a process's RESPONSIBLE BUNDLE, not to
the process itself.** A shell in VS Code's integrated terminal has **VS Code** as
its responsible bundle, and VS Code ships `NSAudioCaptureUsageDescription` in its
`Info.plist`. On the owner's machine (evidence 2026-07-07) **VS Code's terminal
was the ONLY proven grant path** — `Terminal.app`, Ghostty, and cmux lack the key
and were **categorically refused** (no prompt ever appears; capture is silent).

Child processes inherit their parent's TCC responsibility. That drives the whole
launch design:

- **Default = terminal-child.** `setup-tcc-runner.sh` launches the runner with
  `nohup ./run.sh` **from your VS Code terminal**, so the runner (and every
  `cargo test` it spawns) inherits VS Code's responsibility and thus the grant
  you approved. This is the default **because** the VS Code terminal is the only
  proven grant path on this machine.
- **Why NOT launchd.** A runner installed as a launchd LaunchAgent
  (`svc.sh install`) gets **its own** responsible process (`launchd`), *not* VS
  Code. The grant you approved in the terminal would **not** apply, and the first
  CI capture would try to prompt headlessly — which on a service means a hang or
  a silent deny. See the honest alternative in §7.
- **The tradeoff.** A terminal-child dies on **logout/reboot**. That is the
  accepted cost of the only proven grant path; recover with
  `start-tcc-runner.sh` (§5).

> **What only an attended run can prove:** that a `nohup` terminal-child *truly*
> inherits VS Code's TCC responsibility (rather than re-parenting to `launchd`/
> `init` and losing it), and that the grant survives across the runner's
> job-spawned `cargo` children. **Probe #2 in setup is that proof.** If probe #2
> is silent, the inheritance assumption failed on your machine — capture the
> evidence and treat the launchd path in §7 as the fallback (re-validating the
> responsible process attended).

---

## 4. First-time setup (attended, ONCE)

Do this **in a VS Code integrated terminal** (`TERM_PROGRAM=vscode`).

```bash
# 1. Mint a runner registration token (expires ~1h; consumed by config.sh,
#    never written to disk by the script).
TOKEN=$(gh api -X POST \
  repos/Codeseys-Labs/rust-crossplat-audio-capture/actions/runners/registration-token \
  --jq .token)

# 2. Run the one-shot onboarding.
bash scripts/setup-tcc-runner.sh "$TOKEN"
```

What it does, in order: preflights the terminal's responsible bundle for
`NSAudioCaptureUsageDescription` (aborts with VS Code guidance if missing) →
downloads + configures the runner into `~/actions-runner-rsac` with labels
`self-hosted,macos,tcc-audio`, name `$(hostname)-tcc-audio` → launches it as a
terminal-child → runs probe #1 (**this raises the "Allow" prompt — click Allow**)
→ waits 8 s → runs probe #2 and confirms a non-silent 440 Hz capture.

Then trigger the leg:

```bash
gh workflow run ci-audio-macos-tcc.yml   # or wait for the Monday 07:00 UTC cron
```

Also complete the **runner restrictions in §1** in the GitHub UI now.

---

## 5. Recovery after reboot / logout

The terminal-child runner is gone after a reboot. From a **VS Code terminal**:

```bash
bash scripts/start-tcc-runner.sh
```

No token needed — it only relaunches the already-configured runner (re-running
the same responsible-bundle preflight so you can't accidentally relaunch from the
wrong terminal).

---

## 6. Recovery after a macOS update / grant rot

macOS point updates and security changes can reset or invalidate TCC records; a
rebuilt ad-hoc-signed binary re-keys its grant to the new code hash. Symptoms:
the **guard step fails** (silent probe → "lost grant"; exit 124 → "stale grant
hang").

Remediation ladder (from the `macos-tcc-grant-latency-vs-silence-watchdog`
skill):

1. **Re-toggle** the permission: *System Settings → Privacy & Security →
   Screen & System Audio Recording* → toggle the responsible app (VS Code) off
   and on. Relaunch the runner (§5).
2. **If it still hangs/denies**, delete the TCC record and re-grant from scratch:
   ```bash
   tccutil reset All com.microsoft.VSCode   # the responsible bundle id
   ```
   Then re-run the **attended** setup so probe #1 fires a fresh prompt:
   ```bash
   bash scripts/setup-tcc-runner.sh   # re-run detects the configured runner, skips to verification
   ```

The point is **re-grant attended** — the guard cannot self-heal a lost grant,
which is why the workflow's error text sends you here.

---

## 7. Alternative: launchd LaunchAgent (honest tradeoffs)

You *can* install the runner as a launchd service instead:

```bash
cd ~/actions-runner-rsac
./svc.sh install     # LaunchAgent under your user
./svc.sh start
```

- **Upside:** survives logout-ish and auto-restarts, no manual relaunch.
- **The catch:** the service's **responsible process is `launchd`, not VS
  Code** — so whether it inherits your Audio-Capture grant is an **open question
  that must be re-validated ATTENDED on first run**. Watch the first job's
  capture: if it is silent/hangs, the grant did not carry, and you must either
  (a) grant Audio-Capture to the runner's own identity if macOS offers a prompt,
  or (b) fall back to the terminal-child default.
- The kit **defaults to terminal-child** precisely because the VS Code terminal
  is the *only proven* grant path on this machine; launchd is unproven here.

---

## 8. Teardown

```bash
# Stop the terminal-child (if running):
pkill -f "$HOME/actions-runner-rsac/bin/Runner.Listener" || true

# Or, if you installed the launchd service:
cd ~/actions-runner-rsac && ./svc.sh stop && ./svc.sh uninstall

# Remove the runner registration from GitHub (needs a fresh removal token):
TOKEN=$(gh api -X POST \
  repos/Codeseys-Labs/rust-crossplat-audio-capture/actions/runners/remove-token \
  --jq .token)
cd ~/actions-runner-rsac && ./config.sh remove --token "$TOKEN"

# Optionally revoke the TCC grant:
tccutil reset All com.microsoft.VSCode
```

---

## 9. Related

- [`CI_AUDIO_TESTING.md`](CI_AUDIO_TESTING.md) — the truth table + the TCC gate.
- [`.github/workflows/ci-audio-tests.yml`](../.github/workflows/ci-audio-tests.yml)
  — the managed macOS leg this one complements (skip-early Process Tap).
- [`ADR-0016`](designs/0016-macos-process-tap-silent-zeros-guard.md) — the
  silent-zeros diagnostic + the 10 s grace window.
- Skill `macos-tcc-grant-latency-vs-silence-watchdog` — grant latency, the
  three-mode failure surface, and the `tccutil reset` recovery.
