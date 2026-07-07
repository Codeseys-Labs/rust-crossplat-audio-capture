#!/usr/bin/env bash
# =============================================================================
# Deterministic PipeWire routing gate for the Linux audio CI jobs
# (seeds rsac-b106 / rsac-6efb).
#
# Called by: .github/workflows/ci-audio-tests.yml (linux-system, linux-device,
#            linux-process), after the ci_test_sink null sink exists.
# Local use: any Linux box with a running PipeWire stack —
#            `bash scripts/ci-linux-audio-route.sh` (no args). Without
#            $GITHUB_ENV it prints the exports instead of persisting them.
#
# What it does, in order:
#
#   1. Pin `ci_test_sink` as the PipeWire default sink (ladder: pactl →
#      pw-metadata → wpctl), then VERIFY. If pinning is unsupported (the
#      dbus-less Firecracker case) but ci_test_sink is the ONLY sink, default
#      routing lands on it anyway — accepted, and recorded as such.
#   2. Prove the route END-TO-END at the OS level, exactly the way the
#      ci_audio tests use it: play a generated 440 Hz tone with `pw-play`
#      (default routing, the tests' primary player), record the sink monitor
#      through the Pulse layer (`parecord -d ci_test_sink.monitor`), and
#      assert non-silence (RMS) + tone dominance (rough frequency) with sox.
#   3. Only on success: export RSAC_CI_AUDIO_DETERMINISTIC=1 (capture tests'
#      content assertions hard-fail on silence instead of soft-warning) and
#      PULSE_SINK=ci_test_sink (pins the tests' `paplay` fallback player to
#      the same sink).
#
# Design: this probe is a HARD GATE. If the virtual route is broken, the job
# fails here at setup with routing diagnostics — never later as a confusing
# "capture saw silence" test failure, and never as a soft-warned false green.
# Do not weaken this to advisory without re-softening the test assertions.
# =============================================================================
set -euo pipefail

SINK="ci_test_sink"
TONE="/tmp/rsac_route_probe_tone.wav"
CAPTURE="/tmp/rsac_route_probe_capture.wav"
PWDUMP="/tmp/rsac_route_probe_pwdump.json"

# Failure evidence, printed on any exit that isn't a success.
diagnostics() {
  echo "── routing diagnostics ─────────────────────────────────────────"
  pactl list short sinks 2>&1 || true
  pactl list short sources 2>&1 || true
  echo "default sink: $(pactl get-default-sink 2>&1 || true)"
  wpctl status 2>&1 || true
  pw-dump --no-colors >"$PWDUMP" 2>/dev/null || true
  echo "pw-dump written to $PWDUMP"
}
trap 'status=$?; if [ "$status" -ne 0 ]; then diagnostics; fi' EXIT

# ── 1. Pin ci_test_sink as the default sink ────────────────────────────────
echo "── pinning $SINK as the PipeWire default sink"
PINNED_VIA="none"

if pactl set-default-sink "$SINK" 2>/dev/null; then
  PINNED_VIA="pactl"
elif pw-metadata 0 default.configured.audio.sink "{\"name\":\"$SINK\"}" >/dev/null 2>&1; then
  PINNED_VIA="pw-metadata"
elif command -v wpctl >/dev/null 2>&1; then
  # Parse the sink's object id out of wpctl's Sinks section (the '*' marker
  # and tree glyphs vary, so match the description/name loosely).
  SINK_ID="$(wpctl status 2>/dev/null \
    | sed -n '/Sinks:/,/Sources:/p' \
    | grep -iE "ci_test" \
    | grep -oE '[0-9]+\.' | head -n1 | tr -d '.')" || true
  if [ -n "${SINK_ID:-}" ] && wpctl set-default "$SINK_ID" 2>/dev/null; then
    PINNED_VIA="wpctl (id $SINK_ID)"
  fi
fi

DEFAULT_SINK="$(pactl get-default-sink 2>/dev/null || echo unknown)"
SINK_COUNT="$(pactl list short sinks | grep -c . || true)"
echo "pin attempt via: $PINNED_VIA; effective default: '$DEFAULT_SINK'; sink count: $SINK_COUNT"

if [ "$DEFAULT_SINK" = "$SINK" ]; then
  echo "OK: $SINK is the verified default sink"
elif [ "$SINK_COUNT" = "1" ]; then
  # No metadata support (dbus-less Firecracker), but with exactly one sink
  # PipeWire's fallback links every playback stream to it — deterministic in
  # practice, and the end-to-end probe below still has to prove it.
  echo "OK-ish: default-sink metadata not settable here, but $SINK is the ONLY sink — default routing falls through to it (probe will verify)"
else
  echo "ERROR: cannot make $SINK the default sink and $SINK_COUNT sinks exist — routing would be nondeterministic"
  exit 1
fi

# ── 2. End-to-end probe: pw-play → sink monitor → sox analysis ─────────────
echo "── probing tone → $SINK → monitor capture, end to end"
rm -f "$TONE" "$CAPTURE"
sox -n -r 48000 -c 2 "$TONE" synth 4 sine 440 vol 0.8

# Play exactly like tests/ci_audio/helpers.rs spawn_test_tone_player(): pw-play,
# no explicit target (default routing — that is the property under test).
pw-play "$TONE" &
PLAYER_PID=$!
cleanup_player() { kill "$PLAYER_PID" 2>/dev/null || true; wait "$PLAYER_PID" 2>/dev/null || true; }
sleep 0.7

# Record ~2.5 s of the monitor through the Pulse layer. `timeout` ends the
# otherwise-endless parecord; --preserve-status + `|| true` keep set -e happy.
timeout --preserve-status 2.5s \
  parecord --device="${SINK}.monitor" --rate=48000 --channels=2 \
  --file-format=wav "$CAPTURE" || true
cleanup_player

if [ ! -s "$CAPTURE" ]; then
  echo "ERROR: monitor capture produced no data ($CAPTURE empty/missing)"
  exit 1
fi

STAT="$(sox "$CAPTURE" -n stat 2>&1)" || true
echo "$STAT"
RMS="$(awk '/RMS.*amplitude/ {print $3; exit}' <<<"$STAT")"
FREQ="$(awk '/Rough.*frequency/ {print $3; exit}' <<<"$STAT")"

if [ -z "${RMS:-}" ] || ! awk -v r="$RMS" 'BEGIN{exit !(r > 0.05)}'; then
  echo "ERROR: monitor capture is silent (RMS='${RMS:-unparseable}', need > 0.05) — the tone did not reach ${SINK}.monitor"
  exit 1
fi
if [ -z "${FREQ:-}" ] || ! awk -v f="$FREQ" 'BEGIN{exit !(f >= 380 && f <= 500)}'; then
  echo "ERROR: captured audio is not the 440 Hz probe tone (rough frequency='${FREQ:-unparseable}', need 380–500)"
  exit 1
fi
echo "OK: route proven — RMS $RMS, rough frequency $FREQ Hz"

# Keep the routing-state snapshot as evidence either way (uploaded by the
# workflow's log artifact).
pw-dump --no-colors >"$PWDUMP" 2>/dev/null || true

# ── 3. Harden the test assertions ───────────────────────────────────────────
# PULSE_SINK pins the tests' paplay fallback; pw-play follows the (now
# proven) default routing. RSAC_CI_AUDIO_DETERMINISTIC flips soft warnings
# into hard failures (docs/CI_AUDIO_TESTING.md §5).
if [ -n "${GITHUB_ENV:-}" ]; then
  {
    echo "RSAC_CI_AUDIO_DETERMINISTIC=1"
    echo "PULSE_SINK=$SINK"
  } >>"$GITHUB_ENV"
  echo "exported RSAC_CI_AUDIO_DETERMINISTIC=1 and PULSE_SINK=$SINK to \$GITHUB_ENV"
else
  echo "no \$GITHUB_ENV — set these yourself:"
  echo "  export RSAC_CI_AUDIO_DETERMINISTIC=1"
  echo "  export PULSE_SINK=$SINK"
fi
