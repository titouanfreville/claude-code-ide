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
const auto_adopt_1 = require("./auto-adopt");
function session(id, adopted) {
    return {
        session_id: id,
        title: id,
        root: `/work/${id}`,
        adopted,
        status: 'Idle',
        phase: 'Plan',
        unreviewed_files: 0,
    };
}
const ownsAll = () => true;
(0, node_test_1.test)('only unadopted sessions are taken', () => {
    const sessions = [session('a', false), session('b', true)];
    assert.deepEqual((0, auto_adopt_1.sessionsToAdopt)(sessions, ownsAll, new Set()).map((s) => s.session_id), ['a']);
});
(0, node_test_1.test)('sessions this window does not own are left alone', () => {
    // The bug this prevents: every open window sees the whole fleet, so an unscoped
    // rule has all of them racing to adopt sessions from unrelated projects.
    const sessions = [session('mine', false), session('theirs', false)];
    const owns = (id) => id === 'mine';
    assert.deepEqual((0, auto_adopt_1.sessionsToAdopt)(sessions, owns, new Set()).map((s) => s.session_id), ['mine']);
});
(0, node_test_1.test)('an already attempted session is not retried', () => {
    // A five-second poll retrying a failing adopt is a request storm.
    const sessions = [session('a', false)];
    assert.deepEqual((0, auto_adopt_1.sessionsToAdopt)(sessions, ownsAll, new Set(['a'])), []);
});
(0, node_test_1.test)('attempts are forgotten once the session is adopted', () => {
    // Otherwise the set only grows, and a session released later could never be
    // adopted again by this window.
    assert.deepEqual((0, auto_adopt_1.staleAttempts)(new Set(['a']), [session('a', true)]), ['a']);
});
(0, node_test_1.test)('attempts are forgotten once the session is gone', () => {
    assert.deepEqual((0, auto_adopt_1.staleAttempts)(new Set(['a']), []), ['a']);
});
(0, node_test_1.test)('an attempt for a session still waiting is kept', () => {
    assert.deepEqual((0, auto_adopt_1.staleAttempts)(new Set(['a']), [session('a', false)]), []);
});
//# sourceMappingURL=auto-adopt.test.js.map