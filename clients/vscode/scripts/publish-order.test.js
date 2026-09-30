/**
 * Tests for the publish order.
 *
 * This script encodes the defect that broke the v0.1.1 publish — the pack published
 * before the extensions it packs, because a shell glob is alphabetical — and a
 * regression in it is discovered against a live marketplace, mid-release, in the half
 * -published state that is the most expensive one to be in.
 *
 * Fixtures are real .vsix files, because the script reads the manifest out of the zip:
 * a stub that returned parsed manifests would test a function this script does not have.
 */
const assert = require('node:assert/strict');
const { execFileSync } = require('node:child_process');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { test } = require('node:test');
const zlib = require('node:zlib');

const SCRIPT = path.join(__dirname, 'publish-order.js');

/** A minimal but genuine zip holding `extension/package.json`. */
function writeVsix(dir, file, manifest) {
  const name = 'extension/package.json';
  const body = Buffer.from(JSON.stringify(manifest));
  const deflated = zlib.deflateRawSync(body);
  const crc = zlib.crc32 ? zlib.crc32(body) : crc32(body);
  const nameBuf = Buffer.from(name);

  const local = Buffer.alloc(30);
  local.writeUInt32LE(0x04034b50, 0);
  local.writeUInt16LE(20, 4);
  local.writeUInt16LE(0, 6);
  local.writeUInt16LE(8, 8);
  local.writeUInt32LE(crc >>> 0, 14);
  local.writeUInt32LE(deflated.length, 18);
  local.writeUInt32LE(body.length, 22);
  local.writeUInt16LE(nameBuf.length, 26);

  const central = Buffer.alloc(46);
  central.writeUInt32LE(0x02014b50, 0);
  central.writeUInt16LE(20, 4);
  central.writeUInt16LE(20, 6);
  central.writeUInt16LE(8, 10);
  central.writeUInt32LE(crc >>> 0, 16);
  central.writeUInt32LE(deflated.length, 20);
  central.writeUInt32LE(body.length, 24);
  central.writeUInt16LE(nameBuf.length, 28);

  const offset = local.length + nameBuf.length + deflated.length;
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(1, 8);
  end.writeUInt16LE(1, 10);
  end.writeUInt32LE(central.length + nameBuf.length, 12);
  end.writeUInt32LE(offset, 16);

  fs.writeFileSync(
    path.join(dir, file),
    Buffer.concat([local, nameBuf, deflated, central, nameBuf, end])
  );
}

/** CRC-32, for Node versions without `zlib.crc32`. */
function crc32(buf) {
  let c = ~0;
  for (const byte of buf) {
    c ^= byte;
    for (let i = 0; i < 8; i += 1) {
      c = (c >>> 1) ^ (0xedb88320 & -(c & 1));
    }
  }
  return ~c;
}

function run(dir, ...args) {
  return execFileSync('node', [SCRIPT, dir, ...args], { encoding: 'utf8' })
    .trim()
    .split('\n')
    .filter(Boolean)
    .map((p) => path.basename(p));
}

function fixture() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'publish-order-'));
  writeVsix(dir, 'moonlight-core-1.0.0.vsix', { name: 'moonlight-core' });
  writeVsix(dir, 'moonlight-status-1.0.0.vsix', { name: 'moonlight-status' });
  // Sorts before the feature extensions alphabetically — which is exactly how it came
  // to be published second, ahead of the extensions it lists.
  writeVsix(dir, 'moonlight-ai-1.0.0.vsix', {
    name: 'moonlight-ai',
    extensionPack: ['titouanfreville.moonlight-core', 'titouanfreville.moonlight-status'],
  });
  return dir;
}

test('core first, the pack last, whatever the filenames sort to', () => {
  assert.deepEqual(run(fixture()), [
    'moonlight-core-1.0.0.vsix',
    'moonlight-status-1.0.0.vsix',
    'moonlight-ai-1.0.0.vsix',
  ]);
});

test('the pack is found by extensionPack, not by its name', () => {
  // The name was the thing that had to change when `moonlight` turned out to be taken,
  // so matching on it is what made the ordering fragile in the first place.
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'publish-order-'));
  writeVsix(dir, 'aaa-core.vsix', { name: 'moonlight-core' });
  writeVsix(dir, 'aab-bundle.vsix', { name: 'something-entirely-different', extensionPack: ['x'] });
  writeVsix(dir, 'zzz-status.vsix', { name: 'moonlight-status' });
  assert.deepEqual(run(dir), ['aaa-core.vsix', 'zzz-status.vsix', 'aab-bundle.vsix']);
});

test('--skip drops what already went out, keeping the rest in order', () => {
  assert.deepEqual(run(fixture(), '--skip', 'moonlight-core,moonlight-ai'), [
    'moonlight-status-1.0.0.vsix',
  ]);
});

test('a missing core is refused rather than published around', () => {
  // The dependents would each fail against a dependency the marketplace cannot
  // resolve — one confusing failure per extension instead of one clear one.
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'publish-order-'));
  writeVsix(dir, 'moonlight-status-1.0.0.vsix', { name: 'moonlight-status' });
  assert.throws(() => run(dir), /moonlight-core/);
});

test('skipping everything is an error, not an empty publish', () => {
  assert.throws(
    () => run(fixture(), '--skip', 'moonlight-core,moonlight-status,moonlight-ai'),
    /nothing to publish/
  );
});

test('an empty directory is refused', () => {
  assert.throws(() => run(fs.mkdtempSync(path.join(os.tmpdir(), 'publish-order-'))), /no \.vsix/);
});
