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
