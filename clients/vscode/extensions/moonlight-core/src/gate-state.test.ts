import assert from 'node:assert/strict';
import { test } from 'node:test';

import { GateState } from './gate-state';

/**
 * The failure this guards: a transcript from a session that finished hours ago
 * republishes its plan, and the operator is shown an approve button with no hook
 * behind it. Clicking it does nothing, and they believe they approved something.
 */
test('a plan proposal supplies text but arms nothing', () => {
  const gate = new GateState();
  gate.apply({ kind: 'PlanProposed', session: 's1', plan: '# Old plan' });

  assert.equal(gate.plan('s1'), '# Old plan');
  assert.equal(gate.approval('s1'), undefined);
  assert.deepEqual(gate.approvals(), []);
});

test('an approval request arms the verdict and picks up the plan already seen', () => {
  const gate = new GateState();
  gate.apply({ kind: 'PlanProposed', session: 's1', plan: '# Do it' });
  gate.apply(
    { kind: 'ApprovalRequested', session: 's1', what: 'approve plan', authorizeTool: undefined },
    1_000
  );

  const held = gate.approval('s1');
  assert.equal(held?.what, 'approve plan');
  assert.equal(held?.plan, '# Do it');
  assert.equal(held?.sinceMs, 1_000);
});

/**
 * The hook can be released by anyone — this window, the cockpit, another editor. The
 * server announces that as a return to a non-waiting status, and a client that kept
 * offering a verdict would be offering one on a hook that is already gone.
 */
test('a session that stops waiting is no longer held', () => {
  const gate = new GateState();
  gate.apply({ kind: 'ApprovalRequested', session: 's1', what: 'rm -rf /', authorizeTool: undefined });

  assert.equal(gate.apply({ kind: 'SessionStateChanged', session: 's1', status: 'WaitingInput' }), false);
  assert.notEqual(gate.approval('s1'), undefined);

  assert.equal(gate.apply({ kind: 'SessionStateChanged', session: 's1', status: 'Running' }), true);
  assert.equal(gate.approval('s1'), undefined);
});

/** Nothing changed means nothing redraws: surfaces fold this on every event. */
test('events that change nothing report no change', () => {
  const gate = new GateState();
  gate.apply({ kind: 'PlanProposed', session: 's1', plan: '# Same' });

  assert.equal(gate.apply({ kind: 'PlanProposed', session: 's1', plan: '# Same' }), false);
  assert.equal(gate.apply({ kind: 'SessionStateChanged', session: 's2', status: 'Running' }), false);
  assert.equal(gate.apply({ kind: 'SessionRemoved', session: 's2' }), false);
});

/**
 * A resync replaces the held set: a stream that dropped may have carried both the
 * arrival and the answering of a hold, and a survivor would be an answerable-looking
 * verdict on a hook nobody holds.
 */
test('a resync drops holds the server no longer reports, and keeps the plans', () => {
  const gate = new GateState();
  gate.apply({ kind: 'PlanProposed', session: 's1', plan: '# Do it' });
  gate.apply({ kind: 'ApprovalRequested', session: 's1', what: 'approve plan', authorizeTool: undefined });

  gate.resync([
    { session_id: 's2', what: 'approve plan', plan: '# Other', mcp_tool: null, since_ms: 5 },
  ]);

  assert.equal(gate.approval('s1'), undefined);
  assert.equal(gate.plan('s1'), '# Do it', 'a plan stays readable after its hold ends');
  assert.equal(gate.approval('s2')?.plan, '# Other');
});

test('holds are listed longest-waiting first', () => {
  const gate = new GateState();
  gate.resync([
    { session_id: 'late', what: 'a', plan: null, mcp_tool: null, since_ms: 900 },
    { session_id: 'early', what: 'b', plan: null, mcp_tool: null, since_ms: 100 },
  ]);
  assert.deepEqual(
    gate.approvals().map((hold) => hold.sessionId),
    ['early', 'late']
  );
});
