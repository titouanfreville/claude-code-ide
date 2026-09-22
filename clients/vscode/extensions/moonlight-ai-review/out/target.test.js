"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
const strict_1 = __importDefault(require("node:assert/strict"));
const node_test_1 = require("node:test");
const target_1 = require("./target");
const queued = (...ids) => new Set(ids);
/** The reported bug: the status bar knew the session, the click asked anyway. */
(0, node_test_1.test)('a named session with changes is used without asking', () => {
    strict_1.default.equal((0, target_1.resolveReviewSession)('a', 'b', queued('a', 'b', 'c')), 'a');
});
(0, node_test_1.test)('with nothing named, the active session wins over a picker', () => {
    strict_1.default.equal((0, target_1.resolveReviewSession)(undefined, 'b', queued('a', 'b', 'c')), 'b');
});
(0, node_test_1.test)('a lone session with changes needs no choosing', () => {
    strict_1.default.equal((0, target_1.resolveReviewSession)(undefined, undefined, queued('a')), 'a');
});
(0, node_test_1.test)('several sessions and no hint is the one case worth asking about', () => {
    strict_1.default.equal((0, target_1.resolveReviewSession)(undefined, undefined, queued('a', 'b')), undefined);
});
/**
 * The count in the status bar comes from a poll, so it can describe work that was
 * reviewed a moment ago. Honouring the stale id would open an empty review.
 */
(0, node_test_1.test)('a named session with nothing queued falls through to the active one', () => {
    strict_1.default.equal((0, target_1.resolveReviewSession)('gone', 'b', queued('b', 'c')), 'b');
});
(0, node_test_1.test)('a named session with nothing queued still falls through to a lone candidate', () => {
    strict_1.default.equal((0, target_1.resolveReviewSession)('gone', undefined, queued('only')), 'only');
});
(0, node_test_1.test)('an active session with nothing queued does not suppress the picker', () => {
    // 'b' is where the operator is working but has nothing to review; two other
    // sessions do. Choosing either for them would be a guess.
    strict_1.default.equal((0, target_1.resolveReviewSession)(undefined, 'b', queued('a', 'c')), undefined);
});
(0, node_test_1.test)('an empty queue never yields a session', () => {
    strict_1.default.equal((0, target_1.resolveReviewSession)('a', 'b', queued()), undefined);
});
//# sourceMappingURL=target.test.js.map