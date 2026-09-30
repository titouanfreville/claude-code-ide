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
const ownership_1 = require("./ownership");
(0, node_test_1.test)('a folder contains itself and its descendants', () => {
    assert.equal((0, ownership_1.isInside)('/work/app', '/work/app'), true);
    assert.equal((0, ownership_1.isInside)('/work/app/src', '/work/app'), true);
    assert.equal((0, ownership_1.isInside)('/work', '/work/app'), false);
});
(0, node_test_1.test)('containment is by segment, not by prefix', () => {
    // The bug a `startsWith` check has: a window open on /work/app would claim a session
    // rooted in the unrelated /work/app-old next to it.
    assert.equal((0, ownership_1.isInside)('/work/app-old', '/work/app'), false);
    assert.equal((0, ownership_1.isInside)('/work/application', '/work/app'), false);
});
(0, node_test_1.test)('trailing separators do not change the answer', () => {
    assert.equal((0, ownership_1.isInside)('/work/app/', '/work/app'), true);
    assert.equal((0, ownership_1.isInside)('/work/app/src', '/work/app/'), true);
});
(0, node_test_1.test)('the active session is owned whatever its root says', () => {
    // The claim that matters most: a session the operator adopted in this window is
    // theirs even when it has no root, or its root is open nowhere.
    assert.equal((0, ownership_1.windowOwnsSession)('s1', 's1', undefined, []), true);
    assert.equal((0, ownership_1.windowOwnsSession)('s1', 's1', '/elsewhere', ['/work/app']), true);
});
(0, node_test_1.test)('a session rooted in this workspace is owned', () => {
    assert.equal((0, ownership_1.windowOwnsSession)('s1', undefined, '/work/app/src', ['/work/app']), true);
    assert.equal((0, ownership_1.windowOwnsSession)('s1', 's2', '/work/app/src', ['/work/app']), true);
});
(0, node_test_1.test)('a session belonging to another workspace is not owned', () => {
    // This is the case that made the plan panel open in every window at once.
    assert.equal((0, ownership_1.windowOwnsSession)('s1', 's2', '/other/repo', ['/work/app']), false);
    assert.equal((0, ownership_1.windowOwnsSession)('s1', undefined, undefined, ['/work/app']), false);
    assert.equal((0, ownership_1.windowOwnsSession)('s1', undefined, '/work/app', []), false);
});
(0, node_test_1.test)('any one of several workspace folders is enough', () => {
    assert.equal((0, ownership_1.windowOwnsSession)('s1', undefined, '/b/pkg/src', ['/a', '/b/pkg', '/c']), true);
});
(0, node_test_1.test)('an empty workspace folder claims nothing', () => {
    // `[].every(...)` is vacuously true, so an empty string used to match every root —
    // one bad entry and the window claimed the whole fleet.
    assert.equal((0, ownership_1.isInside)('/work/app', ''), false);
    assert.equal((0, ownership_1.windowOwnsSession)('s1', undefined, '/work/app', ['']), false);
});
(0, node_test_1.test)('path case is ignored off Linux', () => {
    // On Windows the drive letter's case genuinely differs between the APIs a session
    // root and a workspace folder come from; macOS volumes are case-insensitive by
    // default. Comparing exactly meant no window owned the session: the plan panel
    // opened nowhere and auto-adopt never fired.
    assert.equal((0, ownership_1.isInside)('C:\\Work\\App\\src', 'c:\\work\\app', 'win32'), true);
    assert.equal((0, ownership_1.isInside)('/Users/Me/Repo', '/users/me/repo', 'darwin'), true);
    // Linux filesystems are case-sensitive, so two differently-cased paths are two
    // different directories and must stay that way.
    assert.equal((0, ownership_1.isInside)('/Users/Me/Repo', '/users/me/repo', 'linux'), false);
});
//# sourceMappingURL=ownership.test.js.map