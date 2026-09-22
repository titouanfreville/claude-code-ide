import assert from 'node:assert/strict';
import { test } from 'node:test';

import { ensureDaemon, resolveDaemonBinary } from './daemon';
import { isSafeSessionId, mcpConfigFlag, safeEndpointUrl } from './index';

/**
 * Resolution order is the whole contract of the setting: an operator who points the
 * editor at their own build expects that build, not the installed one that happens to
 * be first on `PATH`.
 */
test('an explicit path wins over PATH', () => {
  const found = resolveDaemonBinary('/builds/moonlightd', '/usr/bin:/usr/local/bin', (c) => true);
  assert.equal(found, '/builds/moonlightd');
});

test('a named daemon that is not executable starts nothing', () => {
  // Deliberately not a fallback to PATH: silently starting a different binary is how
  // a dev build gets replaced by the installed one without anybody noticing.
  const found = resolveDaemonBinary('/builds/moonlightd', '/usr/bin', (c) => c !== '/builds/moonlightd');
  assert.equal(found, undefined);
});

test('with no override it takes the first moonlightd on PATH', () => {
  const found = resolveDaemonBinary(undefined, '/a:/b', (c) => c === '/b/moonlightd');
  assert.equal(found, '/b/moonlightd');
});

/**
 * The manual-daemon case: someone running their own moonlightd must not have it
 * displaced by a window spawning another one every five seconds.
 */
test('autostart off starts nothing and says so', () => {
  const result = ensureDaemon({ autostart: false });
  assert.deepEqual(result, { kind: 'disabled' });
});

test('autostart off is reported even with a path configured', () => {
  // The path says *which* daemon, the toggle says *whether* — one must not imply the
  // other, or configuring a path would silently re-enable spawning.
  const result = ensureDaemon({ autostart: false, path: '/builds/moonlightd' });
  assert.deepEqual(result, { kind: 'disabled' });
});

test('autostart defaults to on when unspecified', () => {
  // No binary exists at this path, so this stops at `no-binary` rather than spawning
  // anything — but it proves the disabled branch was not taken.
  const result = ensureDaemon({ path: '/definitely/not/a/real/moonlightd' });
  assert.equal(result.kind, 'no-binary');
});

/**
 * The URL is an HTTP response body from whatever holds the port in a world-readable
 * discovery file, and the flag it builds is typed into the operator's terminal followed
 * by Enter. JSON.stringify escapes " and never ', so the old single-quoted argument was
 * a shell injection.
 */
test('a quote in the endpoint URL cannot escape the shell argument', () => {
  // A loopback URL with a quote in its path is a *valid* URL, so the allowlist does not
  // reject it — the quoting is what makes it safe. Both layers matter: the allowlist
  // decides whether we will talk to it at all, the quoting decides whether it can stop
  // being an argument.
  const evil = "http://127.0.0.1:1/'; id; echo '";
  const flag = mcpConfigFlag(evil);
  assert.ok(flag, 'a loopback URL is still accepted');
  // The payload never closes its own quoting: every ' is emitted as the '\'' dance.
  assert.ok(!/[^\\]'; id/.test(flag), flag);
  assert.ok(flag.includes(`'\\''`), flag);
});

test('only loopback http(s) endpoints are accepted', () => {
  assert.ok(safeEndpointUrl('http://127.0.0.1:5050/mcp'));
  assert.ok(safeEndpointUrl('http://localhost:5050/mcp'));
  assert.equal(safeEndpointUrl('http://evil.example/mcp'), undefined);
  assert.equal(safeEndpointUrl('file:///etc/passwd'), undefined);
  assert.equal(safeEndpointUrl('not a url'), undefined);
});

test('a good endpoint still produces a usable, single-quoted flag', () => {
  const flag = mcpConfigFlag('http://127.0.0.1:5050/mcp');
  assert.ok(flag, 'a loopback endpoint should be accepted');
  assert.ok(flag.startsWith(" --mcp-config '"), flag);
  assert.ok(flag.endsWith("'"), flag);
  assert.ok(flag.includes('127.0.0.1:5050'), flag);
});

test('a session id that is not the shape we mint is refused', () => {
  assert.ok(isSafeSessionId('c391cd75-fef3-47a8-9ff9-2544aff34d09'));
  assert.ok(!isSafeSessionId("abc'; id; echo '"));
  assert.ok(!isSafeSessionId('../../etc/passwd'));
  assert.ok(!isSafeSessionId(''));
});
