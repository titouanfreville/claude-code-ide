"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
const strict_1 = __importDefault(require("node:assert/strict"));
const node_test_1 = require("node:test");
const moonlight_control_client_1 = require("moonlight-control-client");
const gate_state_1 = require("./gate-state");
/**
 * The failure this guards: a transcript from a session that finished hours ago
 * republishes its plan, and the operator is shown an approve button with no hook
 * behind it. Clicking it does nothing, and they believe they approved something.
 */
(0, node_test_1.test)('a plan proposal supplies text but arms nothing', () => {
    const gate = new gate_state_1.GateState();
    gate.apply({ kind: 'PlanProposed', session: 's1', plan: '# Old plan' });
    strict_1.default.equal(gate.plan('s1'), '# Old plan');
    strict_1.default.equal(gate.approval('s1'), undefined);
    strict_1.default.deepEqual(gate.approvals(), []);
});
(0, node_test_1.test)('an approval request arms the verdict and picks up the plan already seen', () => {
    const gate = new gate_state_1.GateState();
    gate.apply({ kind: 'PlanProposed', session: 's1', plan: '# Do it' });
    gate.apply({ kind: 'ApprovalRequested', session: 's1', what: 'approve plan', authorizeTool: undefined }, 1_000);
    const held = gate.approval('s1');
    strict_1.default.equal(held?.what, 'approve plan');
    strict_1.default.equal(held?.plan, '# Do it');
    strict_1.default.equal(held?.sinceMs, 1_000);
});
/**
 * The hook can be released by anyone — this window, the cockpit, another editor. The
 * server announces that as a return to a non-waiting status, and a client that kept
 * offering a verdict would be offering one on a hook that is already gone.
 */
(0, node_test_1.test)('a session that stops waiting is no longer held', () => {
    const gate = new gate_state_1.GateState();
    gate.apply({ kind: 'ApprovalRequested', session: 's1', what: 'rm -rf /', authorizeTool: undefined });
    strict_1.default.equal(gate.apply({ kind: 'SessionStateChanged', session: 's1', status: 'WaitingInput' }), false);
    strict_1.default.notEqual(gate.approval('s1'), undefined);
    strict_1.default.equal(gate.apply({ kind: 'SessionStateChanged', session: 's1', status: 'Running' }), true);
    strict_1.default.equal(gate.approval('s1'), undefined);
});
/** Nothing changed means nothing redraws: surfaces fold this on every event. */
(0, node_test_1.test)('events that change nothing report no change', () => {
    const gate = new gate_state_1.GateState();
    gate.apply({ kind: 'PlanProposed', session: 's1', plan: '# Same' });
    strict_1.default.equal(gate.apply({ kind: 'PlanProposed', session: 's1', plan: '# Same' }), false);
    strict_1.default.equal(gate.apply({ kind: 'SessionStateChanged', session: 's2', status: 'Running' }), false);
    strict_1.default.equal(gate.apply({ kind: 'SessionRemoved', session: 's2' }), false);
});
/**
 * A resync replaces the held set: a stream that dropped may have carried both the
 * arrival and the answering of a hold, and a survivor would be an answerable-looking
 * verdict on a hook nobody holds.
 */
(0, node_test_1.test)('a resync drops holds the server no longer reports, and keeps the plans', () => {
    const gate = new gate_state_1.GateState();
    gate.apply({ kind: 'PlanProposed', session: 's1', plan: '# Do it' });
    gate.apply({ kind: 'ApprovalRequested', session: 's1', what: 'approve plan', authorizeTool: undefined });
    gate.resync([
        { session_id: 's2', what: 'approve plan', plan: '# Other', mcp_tool: null, since_ms: 5 },
    ]);
    strict_1.default.equal(gate.approval('s1'), undefined);
    strict_1.default.equal(gate.plan('s1'), '# Do it', 'a plan stays readable after its hold ends');
    strict_1.default.equal(gate.approval('s2')?.plan, '# Other');
});
(0, node_test_1.test)('holds are listed longest-waiting first', () => {
    const gate = new gate_state_1.GateState();
    gate.resync([
        { session_id: 'late', what: 'a', plan: null, mcp_tool: null, since_ms: 900 },
        { session_id: 'early', what: 'b', plan: null, mcp_tool: null, since_ms: 100 },
    ]);
    strict_1.default.deepEqual(gate.approvals().map((hold) => hold.sessionId), ['early', 'late']);
});
(0, node_test_1.test)('a tool approval after a plan does not carry that plan', () => {
    // The bug: `plans` is per session and outlives the hold that produced it, so every
    // later hold was handed the old plan. The plan panel routes on "does this hold carry
    // a plan", so a Bash approval opened a plan review for a plan already approved,
    // instead of asking about the command.
    const gate = new gate_state_1.GateState();
    gate.apply({ kind: 'PlanProposed', session: 's1', plan: '# Do the thing' });
    gate.apply({ kind: 'ApprovalRequested', session: 's1', what: moonlight_control_client_1.PLAN_HOLD });
    strict_1.default.equal(gate.approval('s1')?.plan, '# Do the thing');
    // Same session, a different kind of hold.
    gate.apply({
        kind: 'ApprovalRequested',
        session: 's1',
        what: 'run `npx js-yaml`',
    });
    const held = gate.approval('s1');
    strict_1.default.equal(held?.what, 'run `npx js-yaml`');
    strict_1.default.equal(held?.plan, undefined);
});
(0, node_test_1.test)('the plan stays readable on its own after the hold moves on', () => {
    // The cache is still worth keeping — a plan outlives its verdict, and the panel
    // reads it through `plan()`. Only the per-hold field was wrong.
    const gate = new gate_state_1.GateState();
    gate.apply({ kind: 'PlanProposed', session: 's1', plan: '# Plan' });
    gate.apply({ kind: 'ApprovalRequested', session: 's1', what: 'run `ls`' });
    strict_1.default.equal(gate.plan('s1'), '# Plan');
});
(0, node_test_1.test)('a plan arriving after its hold still reaches the hold', () => {
    // The notifier publishes PlanProposed first, but both travel the same stream and a
    // resync can land between them. Before this, the late plan updated only the cache,
    // so the panel opened on a real hold with nothing in it.
    const gate = new gate_state_1.GateState();
    gate.apply({ kind: 'ApprovalRequested', session: 's1', what: moonlight_control_client_1.PLAN_HOLD });
    strict_1.default.equal(gate.approval('s1')?.plan, undefined);
    gate.apply({ kind: 'PlanProposed', session: 's1', plan: '# Late' });
    strict_1.default.equal(gate.approval('s1')?.plan, '# Late');
});
(0, node_test_1.test)('a repeated plan still fills a hold that is missing it', () => {
    // Transcript detection republishes plans, so the same text can arrive twice with the
    // hold folded in between. Deduping on the text alone skipped the one that mattered.
    const gate = new gate_state_1.GateState();
    gate.apply({ kind: 'PlanProposed', session: 's1', plan: '# Same' });
    gate.apply({ kind: 'ApprovalRequested', session: 's1', what: moonlight_control_client_1.PLAN_HOLD });
    // Simulate the ordering where the hold folded before the plan was cached.
    const fresh = new gate_state_1.GateState();
    fresh.apply({ kind: 'ApprovalRequested', session: 's1', what: moonlight_control_client_1.PLAN_HOLD });
    fresh.apply({ kind: 'PlanProposed', session: 's1', plan: '# Same' });
    fresh.apply({ kind: 'PlanProposed', session: 's1', plan: '# Same' });
    strict_1.default.equal(fresh.approval('s1')?.plan, '# Same');
    strict_1.default.equal(gate.approval('s1')?.plan, '# Same');
});
(0, node_test_1.test)('a late plan does not attach itself to a command approval', () => {
    // The bug this fold was fixed for, from the other direction: patching any waiting
    // hold would put a plan back on a Bash approval and reopen the plan panel for it.
    const gate = new gate_state_1.GateState();
    gate.apply({ kind: 'ApprovalRequested', session: 's1', what: 'run `ls`' });
    gate.apply({ kind: 'PlanProposed', session: 's1', plan: '# Unrelated' });
    strict_1.default.equal(gate.approval('s1')?.plan, undefined);
    strict_1.default.equal(gate.plan('s1'), '# Unrelated');
});
//# sourceMappingURL=gate-state.test.js.map