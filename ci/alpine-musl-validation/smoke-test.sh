#!/usr/bin/env bash
# Alpine musl + PipeWire smoke test for rsac.
#
# Runs INSIDE the ci/alpine-musl-validation Docker image. Mirrors the
# daemon-launch pattern used by .github/workflows/ci-audio-tests.yml
# (lines ~65-88) so the smoke environment matches production CI as
# closely as possible.
#
# Phases:
#   1. Build rsac's smoke_alpine binary against x86_64-unknown-linux-musl
#      via cargo-zigbuild, with --no-default-features --features feat_linux.
#   2. Launch the PipeWire daemon stack manually (no systemd --user in a
#      container, same constraint as Blacksmith microVMs in CI).
#   3. Create a null-sink via pactl so enumerate_devices() has *something*
#      to return.
#   4. Run the smoke_alpine binary. It calls rsac::get_device_enumerator()
#      and enumerate_devices(). Any dlopen failure for libpipewire-0.3.so.0
#      surfaces here as a non-zero exit.
#
# Exit 0 means: "musl wheels are safe to promote from experimental."
# Exit non-zero means: "investigate before promoting."
#
# NOTE: This script is not idempotent in-container — each `docker run`
# gets a fresh filesystem, which is exactly what we want.

set -euo pipefail

# Pretty banners — makes it obvious in CI logs which phase failed.
banner() { printf '\n===== %s =====\n' "$*"; }

cd /workspace

TARGET="${RSAC_SMOKE_TARGET:-x86_64-unknown-linux-musl}"
BIN_NAME="smoke_alpine"

banner "Phase 1: cargo zigbuild (--target ${TARGET})"
# Same flags we'd use for the shipped musl napi artifact. We intentionally
# disable default features and opt back into feat_linux only — this is the
# narrowest valid build on Alpine and also matches what the experimental
# musl napi rows produce.
cargo zigbuild \
    --release \
    --target "${TARGET}" \
    --no-default-features \
    --features feat_linux \
    --bin "${BIN_NAME}"

BIN_PATH="target/${TARGET}/release/${BIN_NAME}"
if [[ ! -x "${BIN_PATH}" ]]; then
    echo "ERROR: expected binary at ${BIN_PATH} but it was not produced." >&2
    exit 2
fi

banner "Phase 1b: ldd / file sanity check on ${BIN_PATH}"
file "${BIN_PATH}" || true
# `ldd` on a static musl binary will say "not a dynamic executable" and
# exit non-zero on Alpine — that's fine, we just want the output logged.
ldd "${BIN_PATH}" 2>&1 || true

banner "Phase 2: start PipeWire stack (no systemd --user available)"
# Mirror of .github/workflows/ci-audio-tests.yml lines 65-88. The container
# has no D-Bus user session, so we launch each daemon by hand and sleep
# between them to let sockets settle.
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
mkdir -p "${XDG_RUNTIME_DIR}"
chmod 700 "${XDG_RUNTIME_DIR}"

# Kill any stale pulseaudio that might race with pipewire-pulse. Alpine
# images rarely have one running, but be defensive.
pulseaudio --kill 2>/dev/null || true

pipewire &
PW_PID=$!
sleep 1

wireplumber &
WP_PID=$!
sleep 1

pipewire-pulse &
PWP_PID=$!
sleep 2

# Log who we started, so a stuck daemon is easy to spot in the output.
echo "pipewire pid=${PW_PID} wireplumber pid=${WP_PID} pipewire-pulse pid=${PWP_PID}"

banner "Phase 2b: verify daemon accessibility"
if ! pw-cli info 0; then
    echo "WARNING: pw-cli info 0 failed — daemon may not be fully up." >&2
    # Don't abort yet: the smoke binary itself is the real test.
fi

banner "Phase 3: create null-sink (so enumeration has a target)"
# Same rate/channels/format as the CI null-sink for consistency.
pactl load-module module-null-sink \
    sink_name=alpine_smoke_sink \
    sink_properties=device.description="Alpine_Smoke_Sink" \
    rate=48000 channels=2 format=float32le || \
    echo "WARNING: pactl load-module failed (not fatal for dlopen test)"

pactl set-default-sink alpine_smoke_sink 2>/dev/null || true

banner "Phase 4: run rsac smoke binary (the actual dlopen test)"
# This is the payload. If libpipewire-0.3.so.0 can't be dlopened on this
# musl target, this line exits non-zero and the whole run fails.
set +e
"${BIN_PATH}"
RC=$?
set -e

banner "Phase 4 result: exit code ${RC}"
if [[ "${RC}" -ne 0 ]]; then
    echo "FAILURE: rsac smoke binary exited ${RC}. Do NOT promote musl" >&2
    echo "         napi-rs rows from experimental until investigated." >&2
    exit "${RC}"
fi

echo "SUCCESS: rsac enumerated devices on Alpine musl. Safe to promote."
exit 0
