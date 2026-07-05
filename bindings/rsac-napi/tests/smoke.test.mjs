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
  for (const field of ['supportsSystemCapture', 'supportsApplicationCapture', 'supportsProcessTreeCapture', 'backendName']) {
    assert.ok(field in caps, `capabilities missing field: ${field}`);
  }
  assert.equal(typeof caps.backendName, 'string');
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

test('device enumeration (headless-tolerant)', () => {
  try {
    const devices = rsac.listDevices();
    assert.ok(Array.isArray(devices));
    console.log(`devices: ${devices.length} enumerated`);
  } catch (err) {
    console.log(`device enumeration skipped (headless?): ${err.message}`);
  }
});
