#!/usr/bin/env bash
# rsac-e6d3 — the on-emulator test payload for ci-android-emu.yml.
#
# Lives in a FILE (not inline in the workflow) because the
# android-emulator-runner action executes its `script:` input via
# `/usr/bin/sh -c`, where a multi-line single-quoted `bash -c '...'` block is
# a syntax error (first-run lesson: "Unterminated quoted string").
#
# Expects: adb on PATH with a booted emulator; emu-payload/ populated by the
# build step; REQUIRE_FRAMES exported (0/1).
set -euo pipefail

adb wait-for-device
adb push emu-payload/rsac_unit_tests /data/local/tmp/
adb push emu-payload/android_emu_smoke /data/local/tmp/
adb shell chmod 755 /data/local/tmp/rsac_unit_tests /data/local/tmp/android_emu_smoke

# 2>&1: adb shell protocol v2 separates remote stderr onto the CLIENT stderr —
# without merging, the tests' eprintln evidence (delivered/negotiated/refusal
# lines) never reaches the log or the step summary.
echo "=== dormant cfg(android) unit tests ==="
adb shell "cd /data/local/tmp && RUST_BACKTRACE=1 ./rsac_unit_tests --test-threads=1" \
  2>&1 | tee unit-tests.log

echo "=== emulator smoke (frames + honest refusal) ==="
adb shell "cd /data/local/tmp && RUST_BACKTRACE=1 \
  RSAC_CI_ANDROID_EMU=1 \
  RSAC_CI_ANDROID_REQUIRE_FRAMES=${REQUIRE_FRAMES:-0} \
  RSAC_TEST_CAPTURE_TIMEOUT_SECS=15 \
  ./android_emu_smoke --test-threads=1 --nocapture" \
  2>&1 | tee smoke.log
