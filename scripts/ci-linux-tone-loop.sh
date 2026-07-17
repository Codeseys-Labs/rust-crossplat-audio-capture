#!/usr/bin/env bash
# CI-only: generate a 440 Hz fixture and loop-play it to the pinned CI sink so
# binding-level SystemDefault capture smokes (rsac-3635) have continuous audio.
#
# Called by: .github/workflows/ci-audio-tests.yml (linux-audio-bindings job).
#            Each smoke step spawns this ONCE, captures the printed PID, runs
#            the binding, and kills the PID in a trap — a per-step spawn that
#            mirrors the Rust tiers' per-test spawn (helpers::spawn_test_tone_player)
#            rather than relying on one background player surviving across
#            GitHub steps.
#
# Player-preference ladder mirrors tests/ci_audio/helpers.rs::spawn_test_tone_player:
# when PULSE_SINK is set, prefer paplay (the Pulse layer honors PULSE_SINK, so
# routing is deterministic even on the dbus-less Firecracker runners); otherwise
# fall back to pw-play (PipeWire-native default routing). sox generates the
# fixture once; pw-play / paplay are all installed by the pipewire-setup
# composite (daemon mode).
#
# Backgrounds the loop and prints its PID on stdout (nothing else) so the caller
# can `PLAYER_PID="$(bash scripts/ci-linux-tone-loop.sh)"`.
#
# Linux-CI-only. On a box without sox/paplay/pw-play this exits non-zero at the
# sox generation step (set -e) — it is not meant to run on the macOS dev host.
set -euo pipefail

TONE="${RSAC_CI_TONE_WAV:-/tmp/rsac_binding_tone.wav}"

# Generate the 30 s 440 Hz stereo fixture once (reused across steps if present).
[ -s "$TONE" ] || sox -n -r 48000 -c 2 "$TONE" synth 30 sine 440 vol 0.8

# setsid: the loop runs as the leader of its own process GROUP, so the
# caller's cleanup can `kill -- -PID` (negative = whole group) and reap the
# in-flight paplay/pw-play grandchild too — a bare `kill $PID` only kills the
# wrapper shell and orphans the looping player, which then accumulates across
# the job's sequential smoke steps.
export RSAC_TONE_PATH="$TONE"
if [ -n "${PULSE_SINK:-}" ] && command -v paplay >/dev/null 2>&1; then
  # PULSE_SINK is re-exported so the setsid child (and paplay under it) sees
  # the pinned sink even if the caller set it un-exported.
  export PULSE_SINK
  setsid bash -c 'while true; do paplay "$RSAC_TONE_PATH" || sleep 1; done' >/dev/null 2>&1 &
else
  setsid bash -c 'while true; do pw-play "$RSAC_TONE_PATH" || sleep 1; done' >/dev/null 2>&1 &
fi

echo "$!"
