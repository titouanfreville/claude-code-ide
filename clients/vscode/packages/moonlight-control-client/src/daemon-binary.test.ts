import * as assert from 'node:assert/strict';
import { test } from 'node:test';

import { daemonBinaryName, resolveDaemonBinary } from './daemon';

test('the daemon carries .exe on Windows only', () => {
  assert.equal(daemonBinaryName('win32'), 'moonlightd.exe');
  assert.equal(daemonBinaryName('darwin'), 'moonlightd');
  assert.equal(daemonBinaryName('linux'), 'moonlightd');
});

test('a Windows PATH scan finds moonlightd.exe', () => {
  // The bug this covers: probing for the extension-less name matched nothing on
  // Windows, so autostart reported `no-binary` forever on a correct install.
  //
  // The directory is deliberately delimiter-neutral. `resolveDaemonBinary` splits on
  // `path.delimiter`, which is `:` wherever these tests run, so a literal `C:\tools`
  // would be torn into two entries and prove nothing about the binary name.
  const dir = '/tools';
  const present = new Set([`${dir}/moonlightd.exe`]);
  const found = resolveDaemonBinary(undefined, dir, (c) => present.has(c), 'moonlightd.exe');
  assert.equal(found, `${dir}/moonlightd.exe`);

  // And the old behaviour is what failed: the extension-less probe finds nothing.
  assert.equal(resolveDaemonBinary(undefined, dir, (c) => present.has(c), 'moonlightd'), undefined);
});
