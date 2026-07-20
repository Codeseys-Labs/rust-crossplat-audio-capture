#!/usr/bin/env bash
# =============================================================================
# scripts/setup-tcc-runner.sh — ONE-SHOT onboarding for the self-hosted macOS
# Audio-Capture (TCC-granted) CI runner that backs
# .github/workflows/ci-audio-macos-tcc.yml.
#
# The owner runs this ONCE, ATTENDED, FROM A VS CODE TERMINAL. It:
#   1. Preflights the terminal's responsible bundle for NSAudioCaptureUsageDescription.
#   2. Installs + configures the GitHub Actions runner (labels: self-hosted,macos,tcc-audio).
#   3. Launches the runner as a TERMINAL-CHILD (nohup ./run.sh) — NOT a launchd
#      service — so it inherits the terminal's TCC responsibility context.
#   4. Fires the one-time interactive Audio-Capture grant via a probe capture,
#      then verifies a second probe captures a non-silent tone.
# Re-running is idempotent: an already-configured runner skips to verification.
#
# ─────────────────────────────────────────────────────────────────────────────
# THE TCC RESPONSIBLE-BUNDLE TRAP (the crux — read docs/SELF_HOSTED_TCC_RUNNER.md)
# ─────────────────────────────────────────────────────────────────────────────
# macOS attaches a kTCCServiceAudioCapture grant to the RESPONSIBLE BUNDLE of a
# process, not to the process itself. A shell in VS Code's integrated terminal
# has VS Code as its responsible bundle, and VS Code ships
# NSAudioCaptureUsageDescription in its Info.plist — the ONLY proven grant path
# on the owner's machine (Terminal.app / Ghostty / cmux categorically refused,
# evidence 2026-07-07). Child processes inherit their parent's responsibility, so
# a `nohup ./run.sh` launched FROM this VS Code terminal inherits VS Code's
# grant. A runner installed as a launchd LaunchAgent instead gets ITS OWN
# responsible process (launchd) — the grant made here would NOT apply, and the
# first CI capture would prompt headlessly (hang or silent deny). That is why the
# DEFAULT here is terminal-child, not `svc.sh install`. Cost: the runner dies on
# logout/reboot; re-launch with scripts/start-tcc-runner.sh from a VS Code
# terminal. The launchd alternative is documented (honestly, with its
# re-validate-attended caveat) in docs/SELF_HOSTED_TCC_RUNNER.md.
#
# NOTE: whether a nohup terminal-child TRULY inherits VS Code's TCC
# responsibility can only be PROVEN by an attended run — the probe in step 4 is
# that proof. If the probe prompt does NOT appear (or capture stays silent),
# the inheritance assumption failed on this machine; see the runbook.
#
# Usage:
#   bash scripts/setup-tcc-runner.sh <REGISTRATION_TOKEN>
#
# Mint a fresh registration token (expires in ~1h; consumed by config.sh and
# NEVER written to disk by this script):
#   gh api -X POST \
#     repos/Codeseys-Labs/rust-crossplat-audio-capture/actions/runners/registration-token \
#     --jq .token
# =============================================================================
set -euo pipefail

RUNNER_DIR="$HOME/actions-runner-rsac"
RUNNER_NAME="$(hostname)-tcc-audio"
RUNNER_LABELS="self-hosted,macos,tcc-audio"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Repo URL: derive from origin, normalize to the https://github.com/owner/repo
# form config.sh wants, with the canonical value as the fallback.
REPO_URL_DEFAULT="https://github.com/Codeseys-Labs/rust-crossplat-audio-capture"
REPO_URL="$(git -C "$REPO_ROOT" remote get-url origin 2>/dev/null || echo "$REPO_URL_DEFAULT")"
REPO_URL="${REPO_URL%.git}"
REPO_URL="${REPO_URL/git@github.com:/https://github.com/}"

log()  { printf '\n\033[1m── setup-tcc-runner: %s\033[0m\n' "$1"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$1" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

# ── Step 0: host sanity ──────────────────────────────────────────────────────
[ "$(uname -s)" = "Darwin" ] || die "this runner is macOS-only (host is $(uname -s))."

# ─────────────────────────────────────────────────────────────────────────────
# Step 1: preflight — the invoking terminal's responsible bundle MUST ship
# NSAudioCaptureUsageDescription, or no attended grant is possible.
# ─────────────────────────────────────────────────────────────────────────────
# Walk the process-parent chain from this shell up to the first .app bundle —
# that is (heuristically) the responsible GUI bundle TCC will attribute the
# grant to. `ps -o comm=` prints the full executable path on macOS.
find_responsible_bundle() {
  local pid=$$ exe ppid
  while [ "${pid:-0}" -gt 1 ]; do
    exe="$(ps -o comm= -p "$pid" 2>/dev/null || true)"
    case "$exe" in
      *.app/Contents/MacOS/*)
        printf '%s.app\n' "${exe%%.app/Contents/MacOS/*}"
        return 0
        ;;
    esac
    ppid="$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ' || true)"
    [ -n "$ppid" ] && [ "$ppid" != "$pid" ] || break
    pid="$ppid"
  done
  return 1
}

bundle_has_audio_capture_key() {
  local plist="$1/Contents/Info.plist"
  [ -f "$plist" ] || return 1
  /usr/libexec/PlistBuddy -c "Print :NSAudioCaptureUsageDescription" "$plist" >/dev/null 2>&1
}

print_vscode_guidance() {
  cat >&2 <<'EOF'

  This terminal's responsible app bundle does NOT declare
  NSAudioCaptureUsageDescription, so macOS will NEVER show the Audio-Capture
  "Allow" prompt from here — every capture would silently return zeros.

  Proven-working path on this machine (evidence 2026-07-07): run this script
  from the INTEGRATED TERMINAL of Visual Studio Code (TERM_PROGRAM=vscode).
  VS Code ships NSAudioCaptureUsageDescription; Terminal.app, Ghostty, and cmux
  do NOT and were categorically refused.

  Open VS Code → Terminal → New Terminal, cd into this repo, and re-run:
    bash scripts/setup-tcc-runner.sh <REGISTRATION_TOKEN>

  See docs/SELF_HOSTED_TCC_RUNNER.md for the full responsible-bundle rationale.
EOF
}

log "Preflight: responsible bundle Audio-Capture capability"
echo "TERM_PROGRAM=${TERM_PROGRAM:-<unset>}"
RESPONSIBLE_BUNDLE="$(find_responsible_bundle || true)"
# NOTE: in a VS Code integrated terminal the nearest .app ancestor is a
# KEYLESS "Code Helper*.app" (the pty-host) — the key lives only on the main
# Visual Studio Code.app, which is the bundle TCC actually holds responsible
# (responsibility delegates up from the helper; that is why the grant works).
# So a keyless nearest-.app must NOT abort when TERM_PROGRAM=vscode — the
# probe in step 4 is the real proof either way. (Verified live: all four
# Code Helper bundles lack NSAudioCaptureUsageDescription; only the main
# bundle ships it.)
if [ -n "$RESPONSIBLE_BUNDLE" ] && bundle_has_audio_capture_key "$RESPONSIBLE_BUNDLE"; then
  echo "Responsible bundle (nearest .app ancestor): $RESPONSIBLE_BUNDLE"
  echo "OK: $RESPONSIBLE_BUNDLE declares NSAudioCaptureUsageDescription."
elif [ "${TERM_PROGRAM:-}" = "vscode" ]; then
  echo "Responsible bundle (nearest .app ancestor): ${RESPONSIBLE_BUNDLE:-<none>}"
  warn "nearest .app is keyless or unresolved, but TERM_PROGRAM=vscode — proceeding on the VS Code heuristic (TCC delegates responsibility to the main VS Code bundle; the probe in step 4 is the real proof)."
else
  if [ -n "$RESPONSIBLE_BUNDLE" ]; then
    warn "$RESPONSIBLE_BUNDLE does NOT declare NSAudioCaptureUsageDescription."
  else
    warn "could not resolve a responsible .app bundle and TERM_PROGRAM is not 'vscode'."
  fi
  print_vscode_guidance
  die "no proven Audio-Capture grant path from this terminal — aborting."
fi

# ─────────────────────────────────────────────────────────────────────────────
# Step 2: install + configure the GitHub Actions runner (idempotent).
# ─────────────────────────────────────────────────────────────────────────────
TOKEN="${1:-}"

runner_is_configured() { [ -f "$RUNNER_DIR/.runner" ]; }

if runner_is_configured; then
  log "Runner already configured at $RUNNER_DIR — skipping install/config (idempotent)."
else
  log "Installing the GitHub Actions runner into $RUNNER_DIR"
  [ -n "$TOKEN" ] || {
    cat >&2 <<EOF
error: a registration token is required for first-time setup.

Mint one (expires ~1h) and re-run:
  TOKEN=\$(gh api -X POST \\
    repos/Codeseys-Labs/rust-crossplat-audio-capture/actions/runners/registration-token \\
    --jq .token)
  bash scripts/setup-tcc-runner.sh "\$TOKEN"
EOF
    exit 1
  }

  case "$(uname -m)" in
    arm64)  RUNNER_ARCH="osx-arm64" ;;
    x86_64) RUNNER_ARCH="osx-x64" ;;
    *)      die "unsupported macOS arch '$(uname -m)'." ;;
  esac

  mkdir -p "$RUNNER_DIR"
  if [ ! -x "$RUNNER_DIR/config.sh" ]; then
    log "Downloading the latest runner release ($RUNNER_ARCH)"
    RUNNER_TAG="$(curl -fsSL https://api.github.com/repos/actions/runner/releases/latest \
      | awk -F'"' '/"tag_name"/ {print $4; exit}')"
    [ -n "$RUNNER_TAG" ] || die "could not resolve the latest actions/runner release tag."
    RUNNER_VERSION="${RUNNER_TAG#v}"
    TARBALL="actions-runner-${RUNNER_ARCH}-${RUNNER_VERSION}.tar.gz"
    curl -fsSL -o "$RUNNER_DIR/$TARBALL" \
      "https://github.com/actions/runner/releases/download/${RUNNER_TAG}/${TARBALL}"
    tar -xzf "$RUNNER_DIR/$TARBALL" -C "$RUNNER_DIR"
    rm -f "$RUNNER_DIR/$TARBALL"
    echo "Extracted runner $RUNNER_VERSION."
  else
    echo "Runner binaries already present; (re)configuring."
  fi

  log "Configuring runner '$RUNNER_NAME' with labels '$RUNNER_LABELS'"
  # The token is passed straight to config.sh and NEVER written to a file by
  # this script. config.sh exchanges it for the runner's own .credentials
  # (GitHub's mechanism — not the registration token). --replace lets a re-run
  # rebind a same-named stale registration.
  ( cd "$RUNNER_DIR" && ./config.sh \
      --url "$REPO_URL" \
      --token "$TOKEN" \
      --labels "$RUNNER_LABELS" \
      --name "$RUNNER_NAME" \
      --unattended --replace )
  echo "Runner configured."
fi

# ─────────────────────────────────────────────────────────────────────────────
# Step 3: launch the runner as a TERMINAL-CHILD (inherits TCC responsibility).
# ─────────────────────────────────────────────────────────────────────────────
log "Launching the runner as a terminal-child (nohup ./run.sh)"
if pgrep -f "$RUNNER_DIR/bin/Runner.Listener" >/dev/null 2>&1; then
  echo "Runner listener already running — leaving it (idempotent)."
else
  ( cd "$RUNNER_DIR" && nohup ./run.sh >"$RUNNER_DIR/runner.stdout.log" 2>&1 & )
  sleep 3
  if pgrep -f "$RUNNER_DIR/bin/Runner.Listener" >/dev/null 2>&1; then
    echo "Runner listener started (logs: $RUNNER_DIR/runner.stdout.log and _diag/)."
  else
    warn "runner listener did not appear after 3s — check $RUNNER_DIR/runner.stdout.log."
  fi
fi

# ─────────────────────────────────────────────────────────────────────────────
# Step 4: fire the one-time interactive grant + verify a second probe is non-silent.
# ─────────────────────────────────────────────────────────────────────────────
# Both probes run as direct children of THIS terminal, sharing the same
# responsible bundle the terminal-child runner inherited — so the grant approved
# here is the grant the runner will use.
command -v sox >/dev/null 2>&1 || { log "Installing sox (tone generator)"; brew install sox; }

log "Building probe examples (record_to_file + verify_audio)"
( cd "$REPO_ROOT" && cargo build --example record_to_file --example verify_audio \
    --no-default-features --features "feat_macos,cli,sink-wav" )

TONE=/tmp/rsac_tcc_setup_tone.wav
sox -n -r 48000 -c 2 -b 32 -e floating-point "$TONE" synth 25 sine 440

# probe_capture <capture-wav> <seconds> → 0 if a 440 Hz tone was captured.
probe_capture() {
  local out="$1" secs="$2" tone_pid
  # A stale WAV from an earlier successful run must not stand in for a fresh
  # probe: if record_to_file dies before opening the sink, verify_audio would
  # otherwise "verify" old audio and report a live grant that isn't
  # (CodeRabbit PR #69).
  rm -f "$out"
  afplay "$TONE" & tone_pid=$!
  sleep 1
  ( cd "$REPO_ROOT" && cargo run --quiet --example record_to_file \
      --no-default-features --features "feat_macos,cli,sink-wav" \
      -- --output "$out" --duration "$secs" ) || true
  kill "$tone_pid" 2>/dev/null || true
  wait "$tone_pid" 2>/dev/null || true
  ( cd "$REPO_ROOT" && cargo run --quiet --example verify_audio \
      --no-default-features --features "feat_macos,cli" \
      -- --input "$out" --frequency 440 --amplitude-threshold 0.001 --verbose )
}

log "Probe #1 — this should raise the macOS Audio-Capture 'Allow' prompt. CLICK ALLOW."
echo "(If no prompt appears and capture is silent, the responsible-bundle"
echo " inheritance failed — see docs/SELF_HOSTED_TCC_RUNNER.md.)"
probe_capture /tmp/rsac_tcc_setup_probe1.wav 14 || \
  warn "probe #1 did not detect a tone yet — a fresh grant needs ~6.7s to propagate; re-probing."

# ~7s > the observed ~6.7s fresh-grant propagation latency (ADR-0016).
log "Waiting 8s for the fresh grant to propagate, then re-verifying (probe #2)"
sleep 8

if probe_capture /tmp/rsac_tcc_setup_probe2.wav 14; then
  log "SUCCESS: Audio-Capture grant is LIVE and the terminal-child runner shares it."
  cat <<EOF

  The runner '$RUNNER_NAME' is online with labels [$RUNNER_LABELS].
  Trigger the leg: gh workflow run ci-audio-macos-tcc.yml   (or wait for the weekly cron).

  REMEMBER: this runner is a terminal-child — it DIES on logout/reboot.
  After a reboot, re-launch it from a VS Code terminal:
    bash scripts/start-tcc-runner.sh
EOF
else
  die "probe #2 still captured only silence — the grant did not take on this bundle. See docs/SELF_HOSTED_TCC_RUNNER.md (responsible-bundle trap + tccutil reset recovery)."
fi
