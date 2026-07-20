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

# ── Instrumented tier (rsac-255b) ────────────────────────────────────────
# The shell-uid smoke above CANNOT produce mic frames-delivered evidence
# (AAudioStream_requestStart -> AAUDIO_ERROR_INTERNAL for shell). The
# self-instrumenting androidTest APK installs as a real package holding
# RECORD_AUDIO (GrantPermissionRule) and drives Device("default") through
# the SHIPPED C ABI (librsac_ffi.so + the test-only shim, both host-built
# into the git-ignored src/androidTest/jniLibs/x86_64/ by the workflow).
# `gradle` (setup-gradle-provided — the repo has no committed gradlew)
# talks to THIS action's already-booted emulator over adb.
#
# REQUIRE_FRAMES crosses into the instrumentation as the
# `rsac_require_frames` runner arg — same skip-with-loud-summary vs
# hard-fail discipline as RSAC_CI_ANDROID_REQUIRE_FRAMES above.
#
# Runbook fallback (NOT default — GrantPermissionRule should suffice): if
# self-instrumentation permission-granting surprises on API 30, split the
# task into `gradle -p mobile/android installDebugAndroidTest`, then
#   adb shell pm grant ai.codeseys.rsac.test android.permission.RECORD_AUDIO
# then run connectedDebugAndroidTest.
echo "=== instrumented androidTest (real app uid + RECORD_AUDIO, shipped C ABI) ==="
adb logcat -c || true
# Capture gradle's exit instead of letting set -e abort here: a failing test
# run (including the intended rsac_require_frames=1 hard-fail path) is EXACTLY
# when the logcat evidence below matters most — dump it first, then propagate
# the failure (CodeRabbit PR #66).
GRADLE_STATUS=0
gradle -p mobile/android connectedDebugAndroidTest --no-daemon --stacktrace \
  "-Pandroid.testInstrumentationRunnerArguments.rsac_require_frames=${REQUIRE_FRAMES:-0}" \
  2>&1 | tee instrumented.log || GRADLE_STATUS=$?
# -d: dump-and-exit (never stream/hang). RsacFramesTest (mic tier) and
# RsacPlaybackTest (playback tier, rsac-e6d3) carry the frames/negotiated-format
# evidence and any SKIP-WITH-SUMMARY line; TestRunner carries the JUnit result.
adb logcat -d -s RsacFramesTest RsacPlaybackTest TestRunner 2>&1 | tee instrumented-logcat.log || true
if [ "$GRADLE_STATUS" -ne 0 ]; then
  echo "::error::connectedDebugAndroidTest failed (exit $GRADLE_STATUS) — see instrumented.log + instrumented-logcat.log"
  exit "$GRADLE_STATUS"
fi
