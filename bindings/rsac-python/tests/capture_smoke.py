"""Deterministic-route SystemDefault capture smoke for the rsac Python binding
(rsac-3635). Linux-CI-only: run under the deterministic PipeWire route with
RSAC_CI_AUDIO_DETERMINISTIC=1. Skips cleanly (exit 0) otherwise.

Invoked BY EXPLICIT PATH only (like the device-free smoke.py), so it never runs
in the headless check-bindings job:

    python bindings/rsac-python/tests/capture_smoke.py

Mirrors smoke.py's failures[] + sys.exit(1) discipline, and mirrors the Rust
system-capture contract in tests/ci_audio/system_capture.rs +
lifecycle_terminal.rs (build -> start -> non-silent buffer -> stop ->
terminal-observable read -> clean drop), with the RMS floor copied from
helpers::verify_rms_energy (0.01)."""

import os
import sys
import time

import rsac

DET = os.environ.get("RSAC_CI_AUDIO_DETERMINISTIC") == "1"
if not DET:
    print("capture smoke SKIPPED: RSAC_CI_AUDIO_DETERMINISTIC != 1")
    sys.exit(0)

timeout = float(os.environ.get("RSAC_TEST_CAPTURE_TIMEOUT_SECS", "15"))
failures = []

# 1. build a SystemDefault capture (48000 Hz, 2ch). Under the deterministic
#    route a build/start failure is a hard fail.
cap = rsac.AudioCapture(rsac.CaptureTarget.system_default(), sample_rate=48000, channels=2)

# 2. start; is_running must be True.
cap.start()
assert cap.is_running, "capture must be running after start()"

# 3. read loop until a non-silent buffer OR the bounded deadline. Under the
#    deterministic route at least one non-silent buffer is mandatory.
got_non_silence = False
deadline = time.monotonic() + timeout
while time.monotonic() < deadline:
    buf = cap.try_read()
    if buf is None:
        time.sleep(0.01)
        continue
    if buf.rms() > 0.01:  # mirrors helpers::verify_rms_energy floor
        got_non_silence = True
        print(f"first non-silent buffer: {buf.num_frames} frames RMS={buf.rms():.6f}")
        break

if not got_non_silence:
    failures.append("deterministic source: no non-silent buffer within timeout")

# 4. stop; is_running must be False.
cap.stop()
if cap.is_running:
    failures.append("is_running must be False after stop()")

# 5. terminal-observable: blocking read() after stop() raises the stream's true
#    FATAL terminal (StreamEnded → StreamError with "Stream ended: ..."), not a
#    recoverable "not running" downgrade (the rsac-477d regression class).
try:
    cap.read()
    failures.append("read() after stop() should raise the terminal error")
except rsac.StreamError as e:
    if "stream ended" in str(e).lower():
        print("post-stop read raised the fatal StreamEnded terminal: ok")
    else:
        failures.append(
            f"read() after stop() raised StreamError but not the StreamEnded "
            f"terminal (got: {e}) — recoverable downgrade regression?"
        )
except rsac.RsacError as e:
    failures.append(
        f"read() after stop() raised a non-Stream RsacError: {type(e).__name__}: {e}"
    )

# 6. close/drop cleanly — close() is idempotent.
cap.close()
cap.close()

if failures:
    for f in failures:
        print(f"FAIL: {f}", file=sys.stderr)
    sys.exit(1)
print("python capture smoke: OK")
