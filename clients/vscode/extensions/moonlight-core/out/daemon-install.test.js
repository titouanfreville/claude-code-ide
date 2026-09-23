"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
const assert = __importStar(require("node:assert/strict"));
const node_test_1 = require("node:test");
const daemon_install_1 = require("./daemon-install");
(0, node_test_1.test)('targetTriple covers every target the release matrix builds', () => {
    assert.equal((0, daemon_install_1.targetTriple)('darwin', 'arm64'), 'aarch64-apple-darwin');
    assert.equal((0, daemon_install_1.targetTriple)('darwin', 'x64'), 'x86_64-apple-darwin');
    assert.equal((0, daemon_install_1.targetTriple)('linux', 'x64'), 'x86_64-unknown-linux-gnu');
    assert.equal((0, daemon_install_1.targetTriple)('linux', 'arm64'), 'aarch64-unknown-linux-gnu');
    assert.equal((0, daemon_install_1.targetTriple)('win32', 'x64'), 'x86_64-pc-windows-msvc');
});
(0, node_test_1.test)('targetTriple is undefined where nothing is published', () => {
    // win32 on ARM is the live example: it runs the x64 build under emulation today,
    // so claiming a triple here would download an asset the release does not have.
    assert.equal((0, daemon_install_1.targetTriple)('win32', 'arm64'), undefined);
    assert.equal((0, daemon_install_1.targetTriple)('freebsd', 'x64'), undefined);
});
(0, node_test_1.test)('asset names and URLs match what build.yml uploads', () => {
    assert.equal((0, daemon_install_1.assetName)('0.1.1', 'x86_64-pc-windows-msvc'), 'moonlightd-0.1.1-x86_64-pc-windows-msvc.gz');
    assert.equal((0, daemon_install_1.assetUrl)('v0.1.1', '0.1.1', 'aarch64-apple-darwin'), 'https://github.com/titouanfreville/claude-code-ide/releases/download/v0.1.1/moonlightd-0.1.1-aarch64-apple-darwin.gz');
    // The nightly release keeps one rolling tag whose assets are named per commit.
    assert.equal((0, daemon_install_1.assetUrl)('nightly', 'nightly-abc1234', 'x86_64-pc-windows-msvc'), 'https://github.com/titouanfreville/claude-code-ide/releases/download/nightly/moonlightd-nightly-abc1234-x86_64-pc-windows-msvc.gz');
});
(0, node_test_1.test)('an unpinned build never downloads', () => {
    // The repo ships DAEMON_PINS undefined so a locally packaged .vsix falls back to
    // PATH instead of reaching for a release that may not exist.
    assert.deepEqual((0, daemon_install_1.planInstall)(undefined, 'aarch64-apple-darwin', false), { kind: 'unpinned' });
});
(0, node_test_1.test)('a platform absent from the pins is reported, not guessed at', () => {
    const pins = { version: '0.1.1', tag: 'v0.1.1', assets: { 'aarch64-apple-darwin': 'a'.repeat(64) } };
    const result = (0, daemon_install_1.planInstall)(pins, 'x86_64-pc-windows-msvc', false);
    assert.ok(!('action' in result));
    assert.equal(result.kind, 'unsupported-platform');
});
(0, node_test_1.test)('a pinned, supported target yields the hash to verify against', () => {
    const sha = 'b'.repeat(64);
    const pins = { version: '0.1.1', tag: 'v0.1.1', assets: { 'aarch64-apple-darwin': sha } };
    assert.deepEqual((0, daemon_install_1.planInstall)(pins, 'aarch64-apple-darwin', false), {
        action: 'download',
        tag: 'v0.1.1',
        version: '0.1.1',
        target: 'aarch64-apple-darwin',
        sha256: sha,
    });
});
//# sourceMappingURL=daemon-install.test.js.map