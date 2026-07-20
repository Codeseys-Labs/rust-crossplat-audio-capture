#!/usr/bin/env bash
# =============================================================================
# scripts/start-tcc-runner.sh — post-reboot RELAUNCH helper for the self-hosted
# macOS Audio-Capture (TCC-granted) CI runner.
#
# The runner is deliberately NOT a launchd service: it runs as a terminal-child
# (nohup ./run.sh) so it inherits the VS Code terminal's TCC responsibility
# context and thus the Audio-Capture grant (see scripts/setup-tcc-runner.sh and
# docs/SELF_HOSTED_TCC_RUNNER.md — "the responsible-bundle trap"). The tradeoff:
# the runner DIES on logout/reboot. After a reboot, RE-RUN THIS FROM A VS CODE
# TERMINAL to bring it back with the same grant inheritance.
#
# This helper does NOT (re)configure the runner and needs NO registration token —
# it only relaunches an already-configured runner. If the runner was never
# configured, run scripts/setup-tcc-runner.sh <TOKEN> first.
#
# Usage (from a VS Code integrated terminal):
#   bash scripts/start-tcc-runner.sh
# =============================================================================
set -euo pipefail

RUNNER_DIR="$HOME/actions-runner-rsac"

log()  { printf '\n\033[1m── start-tcc-runner: %s\033[0m\n' "$1"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$1" >&2; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

[ "$(uname -s)" = "Darwin" ] || die "macOS-only (host is $(uname -s))."
[ -f "$RUNNER_DIR/.runner" ] || die "no configured runner at $RUNNER_DIR — run scripts/setup-tcc-runner.sh <TOKEN> first."

# ── Preflight: same responsible-bundle check as setup (relaunching from a
# non-VS-Code terminal would inherit the WRONG TCC context and silently break
# the next capture). ─────────────────────────────────────────────────────────
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

log "Preflight: responsible bundle Audio-Capture capability"
echo "TERM_PROGRAM=${TERM_PROGRAM:-<unset>}"
RESPONSIBLE_BUNDLE="$(find_responsible_bundle || true)"
if [ -n "$RESPONSIBLE_BUNDLE" ] && bundle_has_audio_capture_key "$RESPONSIBLE_BUNDLE"; then
  echo "OK: $RESPONSIBLE_BUNDLE declares NSAudioCaptureUsageDescription."
elif [ "${TERM_PROGRAM:-}" = "vscode" ]; then
  warn "no Audio-Capture-capable .app ancestor resolved, but TERM_PROGRAM=vscode — proceeding on the VS Code heuristic."
else
  die "this terminal cannot host the Audio-Capture grant (need a VS Code terminal). See docs/SELF_HOSTED_TCC_RUNNER.md."
fi

# ── Relaunch (idempotent) ────────────────────────────────────────────────────
if pgrep -f "$RUNNER_DIR/bin/Runner.Listener" >/dev/null 2>&1; then
  log "Runner listener is ALREADY running — nothing to do."
  pgrep -lf "$RUNNER_DIR/bin/Runner.Listener" || true
  exit 0
fi

log "Relaunching the runner as a terminal-child (nohup ./run.sh)"
( cd "$RUNNER_DIR" && nohup ./run.sh >"$RUNNER_DIR/runner.stdout.log" 2>&1 & )
sleep 3

if pgrep -f "$RUNNER_DIR/bin/Runner.Listener" >/dev/null 2>&1; then
  log "Runner is back ONLINE."
  echo "Listener: $(pgrep -f "$RUNNER_DIR/bin/Runner.Listener" | tr '\n' ' ')"
  echo "Logs: $RUNNER_DIR/runner.stdout.log and $RUNNER_DIR/_diag/"
  echo "Trigger the leg: gh workflow run ci-audio-macos-tcc.yml"
else
  die "runner listener did not start — check $RUNNER_DIR/runner.stdout.log."
fi
