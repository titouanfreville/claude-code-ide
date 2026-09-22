"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
const strict_1 = __importDefault(require("node:assert/strict"));
const node_test_1 = require("node:test");
const daemon_1 = require("./daemon");
const index_1 = require("./index");
/**
 * Resolution order is the whole contract of the setting: an operator who points the
 * editor at their own build expects that build, not the installed one that happens to
 * be first on `PATH`.
 */
(0, node_test_1.test)('an explicit path wins over PATH', () => {
    const found = (0, daemon_1.resolveDaemonBinary)('/builds/moonlightd', '/usr/bin:/usr/local/bin', (c) => true);
    strict_1.default.equal(found, '/builds/moonlightd');
});
(0, node_test_1.test)('a named daemon that is not executable starts nothing', () => {
    // Deliberately not a fallback to PATH: silently starting a different binary is how
    // a dev build gets replaced by the installed one without anybody noticing.
    const found = (0, daemon_1.resolveDaemonBinary)('/builds/moonlightd', '/usr/bin', (c) => c !== '/builds/moonlightd');
    strict_1.default.equal(found, undefined);
});
(0, node_test_1.test)('with no override it takes the first moonlightd on PATH', () => {
    const found = (0, daemon_1.resolveDaemonBinary)(undefined, '/a:/b', (c) => c === '/b/moonlightd');
    strict_1.default.equal(found, '/b/moonlightd');
});
/**
 * The manual-daemon case: someone running their own moonlightd must not have it
 * displaced by a window spawning another one every five seconds.
 */
(0, node_test_1.test)('autostart off starts nothing and says so', () => {
    const result = (0, daemon_1.ensureDaemon)({ autostart: false });
    strict_1.default.deepEqual(result, { kind: 'disabled' });
});
(0, node_test_1.test)('autostart off is reported even with a path configured', () => {
    // The path says *which* daemon, the toggle says *whether* — one must not imply the
    // other, or configuring a path would silently re-enable spawning.
    const result = (0, daemon_1.ensureDaemon)({ autostart: false, path: '/builds/moonlightd' });
    strict_1.default.deepEqual(result, { kind: 'disabled' });
});
(0, node_test_1.test)('autostart defaults to on when unspecified', () => {
    // No binary exists at this path, so this stops at `no-binary` rather than spawning
    // anything — but it proves the disabled branch was not taken.
    const result = (0, daemon_1.ensureDaemon)({ path: '/definitely/not/a/real/moonlightd' });
    strict_1.default.equal(result.kind, 'no-binary');
});
/**
 * The URL is an HTTP response body from whatever holds the port in a world-readable
 * discovery file, and the flag it builds is typed into the operator's terminal followed
 * by Enter. JSON.stringify escapes " and never ', so the old single-quoted argument was
 * a shell injection.
 */
(0, node_test_1.test)('a quote in the endpoint URL cannot escape the shell argument', () => {
    // A loopback URL with a quote in its path is a *valid* URL, so the allowlist does not
    // reject it — the quoting is what makes it safe. Both layers matter: the allowlist
    // decides whether we will talk to it at all, the quoting decides whether it can stop
    // being an argument.
    const evil = "http://127.0.0.1:1/'; id; echo '";
    const flag = (0, index_1.mcpConfigFlag)(evil);
    strict_1.default.ok(flag, 'a loopback URL is still accepted');
    // The payload never closes its own quoting: every ' is emitted as the '\'' dance.
    strict_1.default.ok(!/[^\\]'; id/.test(flag), flag);
    strict_1.default.ok(flag.includes(`'\\''`), flag);
});
(0, node_test_1.test)('only loopback http(s) endpoints are accepted', () => {
    strict_1.default.ok((0, index_1.safeEndpointUrl)('http://127.0.0.1:5050/mcp'));
    strict_1.default.ok((0, index_1.safeEndpointUrl)('http://localhost:5050/mcp'));
    strict_1.default.equal((0, index_1.safeEndpointUrl)('http://evil.example/mcp'), undefined);
    strict_1.default.equal((0, index_1.safeEndpointUrl)('file:///etc/passwd'), undefined);
    strict_1.default.equal((0, index_1.safeEndpointUrl)('not a url'), undefined);
});
(0, node_test_1.test)('a good endpoint still produces a usable, single-quoted flag', () => {
    const flag = (0, index_1.mcpConfigFlag)('http://127.0.0.1:5050/mcp');
    strict_1.default.ok(flag, 'a loopback endpoint should be accepted');
    strict_1.default.ok(flag.startsWith(" --mcp-config '"), flag);
    strict_1.default.ok(flag.endsWith("'"), flag);
    strict_1.default.ok(flag.includes('127.0.0.1:5050'), flag);
});
(0, node_test_1.test)('a session id that is not the shape we mint is refused', () => {
    strict_1.default.ok((0, index_1.isSafeSessionId)('c391cd75-fef3-47a8-9ff9-2544aff34d09'));
    strict_1.default.ok(!(0, index_1.isSafeSessionId)("abc'; id; echo '"));
    strict_1.default.ok(!(0, index_1.isSafeSessionId)('../../etc/passwd'));
    strict_1.default.ok(!(0, index_1.isSafeSessionId)(''));
});
//# sourceMappingURL=daemon.test.js.map