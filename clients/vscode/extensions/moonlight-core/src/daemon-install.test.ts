import * as assert from 'node:assert/strict';
import { test } from 'node:test';

import { assetName, assetUrl, planInstall, targetTriple } from './daemon-install';

test('targetTriple covers every target the release matrix builds', () => {
  assert.equal(targetTriple('darwin', 'arm64'), 'aarch64-apple-darwin');
  assert.equal(targetTriple('darwin', 'x64'), 'x86_64-apple-darwin');
  assert.equal(targetTriple('linux', 'x64'), 'x86_64-unknown-linux-gnu');
  assert.equal(targetTriple('linux', 'arm64'), 'aarch64-unknown-linux-gnu');
  assert.equal(targetTriple('win32', 'x64'), 'x86_64-pc-windows-msvc');
});

test('targetTriple is undefined where nothing is published', () => {
  // win32 on ARM is the live example: it runs the x64 build under emulation today,
  // so claiming a triple here would download an asset the release does not have.
  assert.equal(targetTriple('win32', 'arm64'), undefined);
  assert.equal(targetTriple('freebsd', 'x64'), undefined);
});

test('asset names and URLs match what build.yml uploads', () => {
  assert.equal(assetName('0.1.1', 'x86_64-pc-windows-msvc'), 'moonlightd-0.1.1-x86_64-pc-windows-msvc.gz');
  assert.equal(
    assetUrl('v0.1.1', '0.1.1', 'aarch64-apple-darwin'),
    'https://github.com/titouanfreville/claude-code-ide/releases/download/v0.1.1/moonlightd-0.1.1-aarch64-apple-darwin.gz'
  );
  // The nightly release keeps one rolling tag whose assets are named per commit.
  assert.equal(
    assetUrl('nightly', 'nightly-abc1234', 'x86_64-pc-windows-msvc'),
    'https://github.com/titouanfreville/claude-code-ide/releases/download/nightly/moonlightd-nightly-abc1234-x86_64-pc-windows-msvc.gz'
  );
});

test('an unpinned build never downloads', () => {
  // The repo ships DAEMON_PINS undefined so a locally packaged .vsix falls back to
  // PATH instead of reaching for a release that may not exist.
  assert.deepEqual(planInstall(undefined, 'aarch64-apple-darwin', false), { kind: 'unpinned' });
});

test('a platform absent from the pins is reported, not guessed at', () => {
  const pins = { version: '0.1.1', tag: 'v0.1.1', assets: { 'aarch64-apple-darwin': 'a'.repeat(64) } };
  const result = planInstall(pins, 'x86_64-pc-windows-msvc', false);
  assert.ok(!('action' in result));
  assert.equal(result.kind, 'unsupported-platform');
});

test('a pinned, supported target yields the hash to verify against', () => {
  const sha = 'b'.repeat(64);
  const pins = { version: '0.1.1', tag: 'v0.1.1', assets: { 'aarch64-apple-darwin': sha } };
  assert.deepEqual(planInstall(pins, 'aarch64-apple-darwin', false), {
    action: 'download',
    tag: 'v0.1.1',
    version: '0.1.1',
    target: 'aarch64-apple-darwin',
    sha256: sha,
  });
});
