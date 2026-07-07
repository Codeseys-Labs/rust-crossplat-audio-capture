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
# Each rung is only accepted if `pactl get-default-sink` VERIFIES afterwards
# (first-run lesson: pw-metadata exits 0 writing a key wireplumber never
# applies — an unverified rung poisons the ladder).
echo "── pinning $SINK as the PipeWire default sink"
PINNED_VIA="none"

verify_default() { [ "$(pactl get-default-sink 2>/dev/null || echo unknown)" = "$SINK" ]; }

if pactl set-default-sink "$SINK" 2>/dev/null && verify_default; then
  PINNED_VIA="pactl"
fi
if [ "$PINNED_VIA" = "none" ] && command -v wpctl >/dev/null 2>&1; then
  # Parse the sink's object id out of wpctl's Sinks section (the '*' marker
  # and tree glyphs vary, so match the description/name loosely).
  SINK_ID="$(wpctl status 2>/dev/null \
    | sed -n '/Sinks:/,/Sources:/p' \
    | grep -iE "ci_test" \
    | grep -oE '[0-9]+\.' | head -n1 | tr -d '.')" || true
  if [ -n "${SINK_ID:-}" ] && wpctl set-default "$SINK_ID" 2>/dev/null; then
    sleep 1
    if verify_default; then
      PINNED_VIA="wpctl (id $SINK_ID)"
    fi
  fi
fi
if [ "$PINNED_VIA" = "none" ]; then
  if pw-metadata 0 default.configured.audio.sink "{\"name\":\"$SINK\"}" >/dev/null 2>&1; then
    sleep 1
    if verify_default; then
      PINNED_VIA="pw-metadata"
    fi
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

# ── 2. End-to-end probe: player → sink monitor → sox analysis ──────────────
# Recording starts BEFORE the player: a capture stream on the monitor port
# resumes the (otherwise SUSPENDED) null-sink node, removing both the
# startup race and the suspended-clock failure mode observed on the first
# evidence run (parecord read 0 samples from a SUSPENDED sink).
#
# Player ladder: pw-play (the tests' primary player, native default routing)
# then paplay (the tests' fallback, Pulse-layer routing honoring PULSE_SINK).
# Each attempt is analyzed independently so the log records exactly which
# player paths deliver on this runner.
echo "── probing tone → $SINK → monitor capture, end to end"
sox -n -r 48000 -c 2 "$TONE" synth 4 sine 440 vol 0.8

probe_with_player() {
  # $1 = label, $2... = player command (backgrounded)
  local label="$1"; shift
  rm -f "$CAPTURE"
  echo "── probe attempt: $label"

  timeout --preserve-status 4s \
    parecord --device="${SINK}.monitor" --rate=48000 --channels=2 \
    --file-format=wav "$CAPTURE" &
  local rec_pid=$!
  sleep 0.5

  local player_log="/tmp/rsac_route_probe_player.log"
  "$@" >"$player_log" 2>&1 &
  local player_pid=$!
  sleep 1
  if ! kill -0 "$player_pid" 2>/dev/null; then
    echo "player '$label' exited early; output:"
    cat "$player_log" || true
  else
    # Mid-play graph snapshot: the Streams section proves (or disproves)
    # that the player actually linked into the graph.
    wpctl status 2>/dev/null | sed -n '/Audio/,/Video/p' || true
  fi

  wait "$rec_pid" 2>/dev/null || true
  kill "$player_pid" 2>/dev/null || true
  wait "$player_pid" 2>/dev/null || true
  [ -s "$player_log" ] && { echo "player '$label' output:"; cat "$player_log"; }

  if [ ! -s "$CAPTURE" ]; then
    echo "probe '$label': monitor capture produced no data"
    return 1
  fi
  # Analyze the LAST 2 s, mixed to mono: sox's rough-frequency estimate is a
  # zero-crossing count over the whole stream, so interleaved stereo and the
  # record-before-play leading silence both skew it (observed: 311 Hz for a
  # clean 440 Hz tone on evidence run 28905294002). The mono tail is pure
  # tone, so the estimate is accurate there.
  local stat rms freq
  stat="$(sox "$CAPTURE" -n remix 1 trim -2 stat 2>&1)" || true
  rms="$(awk '/RMS.*amplitude/ {print $3; exit}' <<<"$stat")"
  freq="$(awk '/Rough.*frequency/ {print $3; exit}' <<<"$stat")"
  echo "probe '$label': RMS=${rms:-unparseable} rough_frequency=${freq:-unparseable}"
  if [ -z "${rms:-}" ] || ! awk -v r="$rms" 'BEGIN{exit !(r > 0.05)}'; then
    return 1
  fi
  if [ -z "${freq:-}" ] || ! awk -v f="$freq" 'BEGIN{exit !(f >= 380 && f <= 500)}'; then
    echo "probe '$label': non-silent but not the 440 Hz tone"
    return 1
  fi
  return 0
}

PROVEN_PLAYER="none"
if probe_with_player "pw-play (default routing)" pw-play "$TONE"; then
  PROVEN_PLAYER="pw-play"
elif probe_with_player "paplay (PULSE_SINK routing)" env PULSE_SINK="$SINK" paplay "$TONE"; then
  # pw-play (the tests' first choice) does not deliver here but the Pulse
  # layer does. The tests' spawn helper prefers paplay when PULSE_SINK is
  # set, so this is still a deterministic test route.
  PROVEN_PLAYER="paplay"
else
  # Last diagnostic rung — NOT accepted as proof (the tests never target
  # explicitly), but it separates "streams don't link by default" from
  # "the graph clock never runs at all" in the failure evidence.
  probe_with_player "pw-play --target $SINK (diagnostic only)" pw-play --target "$SINK" "$TONE" || true
  echo "ERROR: no default-routed player path delivers tone to ${SINK}.monitor — routing is not deterministic on this runner"
  exit 1
fi
echo "OK: route proven end-to-end via $PROVEN_PLAYER"

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
