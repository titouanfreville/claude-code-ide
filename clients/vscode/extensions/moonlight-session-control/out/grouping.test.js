"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
const strict_1 = __importDefault(require("node:assert/strict"));
const node_test_1 = require("node:test");
const grouping_1 = require("./grouping");
function session(id, root) {
    return {
        session_id: id,
        title: id,
        root,
        adopted: true,
        status: 'Idle',
        phase: 'Plan',
        unreviewed_files: 0,
    };
}
const keys = (g) => g.kind === 'grouped' ? g.groups.map((x) => x.label) : [];
(0, node_test_1.test)('sessions in different repos group by project', () => {
    const grouping = (0, grouping_1.groupSessions)([session('a', '/work/alpha'), session('b', '/work/beta')], 'project');
    strict_1.default.equal(grouping.kind, 'grouped');
    strict_1.default.deepEqual(keys(grouping), ['alpha', 'beta']);
});
/** One header over the whole list costs a row and says nothing. */
(0, node_test_1.test)('a single group renders flat instead', () => {
    const grouping = (0, grouping_1.groupSessions)([session('a', '/work/alpha'), session('b', '/work/alpha')], 'project');
    strict_1.default.equal(grouping.kind, 'flat');
});
(0, node_test_1.test)('grouping off is always flat', () => {
    const grouping = (0, grouping_1.groupSessions)([session('a', '/x'), session('b', '/y')], 'none');
    strict_1.default.equal(grouping.kind, 'flat');
});
/**
 * Group order is first-appearance order, and the caller sorts worst-first — so the
 * group holding the session that needs you comes first without sorting groups at all.
 */
(0, node_test_1.test)('group order follows the triage order of the input', () => {
    const grouping = (0, grouping_1.groupSessions)([session('needs-you', '/work/beta'), session('idle', '/work/alpha')], 'project');
    strict_1.default.deepEqual(keys(grouping), ['beta', 'alpha']);
});
(0, node_test_1.test)('sessions with no root land in a bucket that sorts last', () => {
    const grouping = (0, grouping_1.groupSessions)([session('orphan', null), session('a', '/work/alpha')], 'project');
    // Sorts last despite appearing first in the triage order.
    strict_1.default.deepEqual(keys(grouping), ['alpha', 'No project']);
});
(0, node_test_1.test)('colliding basenames are disambiguated by parent, others are not', () => {
    const roots = ['/one/web', '/two/web', '/solo/api'];
    strict_1.default.equal((0, grouping_1.projectLabel)('/one/web', roots), 'one/web');
    strict_1.default.equal((0, grouping_1.projectLabel)('/two/web', roots), 'two/web');
    strict_1.default.equal((0, grouping_1.projectLabel)('/solo/api', roots), 'api');
});
(0, node_test_1.test)('a custom group takes its members, the rest still group by project', () => {
    const custom = [{ id: 'g1', name: 'Release work', sessionIds: ['a'] }];
    const grouping = (0, grouping_1.groupSessions)([session('a', '/work/alpha'), session('b', '/work/beta')], 'custom', custom);
    strict_1.default.deepEqual(keys(grouping), ['Release work', 'beta']);
});
(0, node_test_1.test)('a custom group survives being the only group', () => {
    // The flat-when-single rule must not swallow a group the operator made by hand.
    const custom = [{ id: 'g1', name: 'Mine', sessionIds: ['a'] }];
    const grouping = (0, grouping_1.groupSessions)([session('a', '/work/alpha')], 'custom', custom);
    strict_1.default.deepEqual(keys(grouping), ['Mine']);
});
(0, node_test_1.test)('a custom group keeps ids of sessions that have ended without claiming them', () => {
    const custom = [{ id: 'g1', name: 'Mine', sessionIds: ['gone', 'a'] }];
    const grouping = (0, grouping_1.groupSessions)([session('a', '/work/alpha')], 'custom', custom);
    strict_1.default.equal(grouping.kind, 'grouped');
    if (grouping.kind === 'grouped') {
        strict_1.default.deepEqual(grouping.groups[0].sessions.map((s) => s.session_id), ['a']);
    }
});
(0, node_test_1.test)('an empty custom group is still rendered', () => {
    // It is somewhere to drop things; hiding it reads as the group having been lost.
    const custom = [{ id: 'g1', name: 'Empty', sessionIds: [] }];
    const grouping = (0, grouping_1.groupSessions)([session('a', '/work/alpha')], 'custom', custom);
    strict_1.default.deepEqual(keys(grouping), ['Empty', 'alpha']);
});
(0, node_test_1.test)('no sessions is flat, not an empty group', () => {
    strict_1.default.deepEqual((0, grouping_1.groupSessions)([], 'project'), { kind: 'flat', sessions: [] });
});
(0, node_test_1.test)('group keys are stable across calls so expansion state survives a refresh', () => {
    const input = [session('a', '/work/alpha'), session('b', '/work/beta')];
    const first = (0, grouping_1.groupSessions)(input, 'project');
    const second = (0, grouping_1.groupSessions)(input, 'project');
    strict_1.default.equal(first.kind, 'grouped');
    if (first.kind === 'grouped' && second.kind === 'grouped') {
        strict_1.default.deepEqual(first.groups.map((g) => g.key), second.groups.map((g) => g.key));
    }
});
/**
 * Custom groups are for sessions the operator has taken on. An unadopted one is still
 * being triaged, and it changes lists the moment it is adopted.
 */
(0, node_test_1.test)('the not-adopted view falls back to project grouping', () => {
    strict_1.default.equal((0, grouping_1.effectiveMode)('unadopted', 'custom'), 'project');
});
(0, node_test_1.test)('every other mode is left alone in both views', () => {
    for (const scope of ['governed', 'unadopted']) {
        strict_1.default.equal((0, grouping_1.effectiveMode)(scope, 'project'), 'project');
        strict_1.default.equal((0, grouping_1.effectiveMode)(scope, 'none'), 'none');
    }
    strict_1.default.equal((0, grouping_1.effectiveMode)('governed', 'custom'), 'custom');
});
//# sourceMappingURL=grouping.test.js.map