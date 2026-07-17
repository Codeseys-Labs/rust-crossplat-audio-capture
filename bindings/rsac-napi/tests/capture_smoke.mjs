// Deterministic-route SystemDefault capture smoke for @rsac/audio (rsac-3635).
// Linux-CI-only. Invoked EXPLICITLY (node --test tests/capture_smoke.mjs) — the
// filename has NO `.test.` infix, so `npm test` (`node --test`, which auto-globs
// *.test.mjs) in the device-free check-bindings job does NOT pick it up.
// Defense-in-depth: still self-skips when RSAC_CI_AUDIO_DETERMINISTIC is unset.
//
// Mirrors the Rust system-capture contract (tests/ci_audio/system_capture.rs +
// lifecycle_terminal.rs): build -> start -> non-silent chunk -> stop ->
// terminal-observable read -> (implicit clean drop). RMS floor 0.01 copied from
// helpers::verify_rms_energy. Per index.d.ts: the non-blocking read() is NOT
// terminal-observable (throws a recoverable "not running" while stopped), so we
// poll it for the non-silent chunk; readBlocking() IS terminal-observable, so it
// is the surface we assert throws after stop().
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const rsac = require('../index.js');

const DET = process.env.RSAC_CI_AUDIO_DETERMINISTIC === '1';
const timeoutMs = Number(process.env.RSAC_TEST_CAPTURE_TIMEOUT_SECS ?? '15') * 1000;

test('SystemDefault capture delivers non-silence and stops cleanly', { skip: !DET && 'RSAC_CI_AUDIO_DETERMINISTIC != 1' }, async () => {
  const cap = rsac.AudioCapture.create(rsac.CaptureTarget.systemDefault(), 48000, 2);
  cap.start();
  assert.equal(cap.isRunning, true, 'isRunning after start()');

  let gotNonSilence = false;
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    let chunk = null;
    try {
      chunk = cap.read();
    } catch {
      // read() is not terminal-observable: while running it may throw a
      // transient recoverable "not running" hiccup — tolerate and retry.
    }
    if (chunk && chunk.rms > 0.01) {
      gotNonSilence = true;
      break;
    }
    // 10ms backoff between empty polls — matches the Python (sleep 0.01)
    // and Rust (10ms) smoke loops; a tight loop pegs a runner core.
    await sleep(10);
  }
  assert.ok(gotNonSilence, 'deterministic source: at least one non-silent chunk');

  cap.stop();
  assert.equal(cap.isRunning, false, 'isRunning false after stop()');
  // readBlocking() is terminal-observable — throws the terminal error after stop.
  assert.throws(() => cap.readBlocking(), 'readBlocking() after stop() must throw terminal');
});
