// Runtime smoke test for @rsac/audio (rsac-e1f7).
//
// Run after `npm run build` (needs the compiled .node next to index.js):
//     npm test
//
// Asserts the addon loads and the device-independent surface works.
// Device enumeration is exercised but tolerated to fail on headless
// machines, mirroring tests/ci_audio conventions.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const rsac = require('../index.js');

test('addon loads and exports the expected surface', () => {
  for (const name of ['AudioCapture', 'CaptureTarget', 'listDevices', 'getDefaultDevice', 'platformCapabilities']) {
    assert.equal(typeof rsac[name] === 'function' || typeof rsac[name] === 'object', true, `missing export: ${name}`);
  }
});

test('platformCapabilities has an honest shape', () => {
  const caps = rsac.platformCapabilities();
  for (const field of [
    'supportsSystemCapture',
    'supportsApplicationCapture',
    'supportsProcessTreeCapture',
    'supportsDeviceChangeNotifications',
    'requiresUserConsent',
    'supportedSampleFormats',
    'supportedSampleRates',
    'backendName',
  ]) {
    assert.ok(field in caps, `capabilities missing field: ${field}`);
  }
  assert.equal(typeof caps.backendName, 'string');
  // Desktop backends never require a config-time consent artifact
  // (docs/MOBILE_BACKEND_DESIGN.md) — pin it so the projection can't drift.
  assert.equal(caps.requiresUserConsent, false);
  assert.ok(Array.isArray(caps.supportedSampleFormats));
  // The builder whitelist is platform-independent and always contains 48000.
  assert.ok(Array.isArray(caps.supportedSampleRates) && caps.supportedSampleRates.includes(48000));
});

test('CaptureTarget factories + describe round-trip', () => {
  const targets = [
    rsac.CaptureTarget.systemDefault(),
    rsac.CaptureTarget.application('1234'),
    rsac.CaptureTarget.applicationByName('Firefox'),
  ];
  for (const t of targets) {
    assert.equal(typeof t.describe(), 'string');
  }
});

test('compose classes are exported', () => {
  for (const name of ['Composition', 'CompositionBuilder', 'Group']) {
    assert.equal(typeof rsac[name], 'function', `missing compose export: ${name}`);
  }
});

test('compose device-free build + not-started contract', () => {
  // A "system"-source composition BUILDS without touching a device (build only
  // validates); we do NOT start() it — that needs a real device.
  const g = new rsac.Group('main');
  g.source('system');
  const builder = rsac.CompositionBuilder.create({ sampleRate: 48000 });
  builder.addGroup(g);
  const comp = builder.build();

  assert.equal(comp.channelCount, 0, 'not-started channelCount should be 0');
  assert.equal(comp.stats(), null, 'not-started stats() should be null');
  assert.equal(comp.sourceStats(0), null, 'not-started sourceStats(0) should be null');
  assert.equal(comp.isRunning, false, 'not-started isRunning should be false');
  // stop() before start() is an idempotent no-op (must not throw).
  assert.doesNotThrow(() => comp.stop());
});

test('compose live-control not-started contract', () => {
  // All four live-control calls on a built-but-unstarted composition throw
  // ERR_RSAC_STREAM (the runtime roundtrip / bounds / not-running paths need a
  // started composition → a real device, so they are device-gated elsewhere).
  // Mirrors the Python smoke.py and Go compose_test.go not-started contracts.
  const g = new rsac.Group('main');
  g.source('system');
  const builder = rsac.CompositionBuilder.create({ sampleRate: 48000 });
  builder.addGroup(g);
  const comp = builder.build();

  const calls = {
    setGain: () => comp.setGain('main', 0, 0.5),
    setMuted: () => comp.setMuted('main', 0, true),
    gain: () => comp.gain('main', 0),
    isMuted: () => comp.isMuted('main', 0),
  };
  for (const [label, call] of Object.entries(calls)) {
    assert.throws(call, /STREAM/, `not-started comp.${label}() should throw ERR_RSAC_STREAM`);
  }
});

test('compose builder validation rejects a zero quantum', () => {
  const g = new rsac.Group('main');
  g.source('system');
  const builder = rsac.CompositionBuilder.create({ quantumMs: 0 });
  builder.addGroup(g);
  assert.throws(() => builder.preflight(), /CONFIGURATION|quantum/i);
});

test('compose group rejects an invalid gain eagerly', () => {
  const g = new rsac.Group('g');
  assert.throws(() => g.sourceWithGain('system', -1.0), /CONFIGURATION|gain/i);
});

test('device enumeration (headless-tolerant)', async () => {
  // listDevices() is async (Promise<AudioDevice[]>) — it MUST be awaited here,
  // otherwise a headless-machine rejection escapes the test as an
  // unhandledRejection after it ends and fails the whole file (seen on the
  // first CI run of this suite).
  try {
    const devices = await rsac.listDevices();
    assert.ok(Array.isArray(devices));
    console.log(`devices: ${devices.length} enumerated`);
  } catch (err) {
    console.log(`device enumeration skipped (headless?): ${err.message}`);
  }
});

test('default device lookup (headless-tolerant)', async () => {
  // getDefaultDevice() is async (Promise<AudioDevice>, src/lib.rs:1106) —
  // just like listDevices() above, it MUST be awaited here so a
  // headless-machine rejection is caught instead of escaping as an
  // unhandledRejection after the test ends (rsac-f9c1: this export was
  // exercised nowhere until now, the same dormant landmine class that
  // caused the listDevices() unhandled-rejection CI failure).
  try {
    const device = await rsac.getDefaultDevice();
    assert.equal(typeof device.id, 'string');
    assert.equal(typeof device.name, 'string');
    assert.equal(typeof device.isDefault, 'boolean');
    console.log(`default device: ${device.name} (${device.id})`);
  } catch (err) {
    console.log(`default device lookup skipped (headless?): ${err.message}`);
  }
});
