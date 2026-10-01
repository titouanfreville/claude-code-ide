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
const panel_match_1 = require("./panel-match");
const session = (session_id, title, root = '/work/app', status = 'Running') => ({
    session_id,
    title,
    root,
    status,
});
(0, node_test_1.test)('labels follow Claude Code truncation', () => {
    assert.equal((0, panel_match_1.panelLabelFor)('Refactor auth'), 'Refactor auth');
    assert.equal((0, panel_match_1.panelLabelFor)('x'.repeat(25)), 'x'.repeat(25));
    assert.equal((0, panel_match_1.panelLabelFor)('x'.repeat(26)), `${'x'.repeat(24)}…`);
});
(0, node_test_1.test)('the webview prefix VSCode adds does not hide a Claude panel', () => {
    assert.equal((0, panel_match_1.isClaudePanel)('claudeVSCodePanel'), true);
    assert.equal((0, panel_match_1.isClaudePanel)('mainThreadWebview-claudeVSCodePanel'), true);
    assert.equal((0, panel_match_1.isClaudePanel)('mainThreadWebview-claudePlanPreview'), false);
});
(0, node_test_1.test)('a focused panel resolves to the session its label names', () => {
    const sessions = [session('a', 'Refactor auth'), session('b', 'Follow the focused session in VSCode')];
    assert.equal((0, panel_match_1.matchPanelSession)('Refactor auth', sessions, ['/work/app']), 'a');
    assert.equal((0, panel_match_1.matchPanelSession)('Follow the focused sessi…', sessions, ['/work/app']), 'b');
});
(0, node_test_1.test)('an untitled panel matches nothing', () => {
    assert.equal((0, panel_match_1.matchPanelSession)('Claude Code', [session('a', 'Claude Code')], ['/work/app']), undefined);
});
(0, node_test_1.test)('ties narrow to this workspace, then to running sessions', () => {
    const sessions = [
        session('elsewhere', 'Fix tests', '/other'),
        session('idle', 'Fix tests', '/work/app', 'Idle'),
        session('here', 'Fix tests', '/work/app'),
    ];
    assert.equal((0, panel_match_1.matchPanelSession)('Fix tests', sessions, ['/work/app']), 'here');
});
(0, node_test_1.test)('an unresolvable tie is no match rather than a pick', () => {
    const sessions = [session('a', 'Fix tests'), session('b', 'Fix tests')];
    assert.equal((0, panel_match_1.matchPanelSession)('Fix tests', sessions, ['/work/app']), undefined);
});
//# sourceMappingURL=panel-match.test.js.map