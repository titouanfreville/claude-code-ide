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
const daemon_1 = require("./daemon");
(0, node_test_1.test)('the daemon carries .exe on Windows only', () => {
    assert.equal((0, daemon_1.daemonBinaryName)('win32'), 'moonlightd.exe');
    assert.equal((0, daemon_1.daemonBinaryName)('darwin'), 'moonlightd');
    assert.equal((0, daemon_1.daemonBinaryName)('linux'), 'moonlightd');
});
(0, node_test_1.test)('a Windows PATH scan finds moonlightd.exe', () => {
    // The bug this covers: probing for the extension-less name matched nothing on
    // Windows, so autostart reported `no-binary` forever on a correct install.
    //
    // The directory is deliberately delimiter-neutral. `resolveDaemonBinary` splits on
    // `path.delimiter`, which is `:` wherever these tests run, so a literal `C:\tools`
    // would be torn into two entries and prove nothing about the binary name.
    const dir = '/tools';
    const present = new Set([`${dir}/moonlightd.exe`]);
    const found = (0, daemon_1.resolveDaemonBinary)(undefined, dir, (c) => present.has(c), 'moonlightd.exe');
    assert.equal(found, `${dir}/moonlightd.exe`);
    // And the old behaviour is what failed: the extension-less probe finds nothing.
    assert.equal((0, daemon_1.resolveDaemonBinary)(undefined, dir, (c) => present.has(c), 'moonlightd'), undefined);
});
//# sourceMappingURL=daemon-binary.test.js.map