import assert from 'node:assert/strict';
import { test } from 'node:test';

import { parseEngineEvent, readFrames, statusIsWaiting } from './events';

/**
 * A chunk boundary falls wherever the network put it. Treating a half-arrived frame as
 * complete would hand `JSON.parse` a truncated event — and the frame that gets split
 * is disproportionately likely to be a big one, which is to say a plan.
 */
test('a frame split across chunks is held until its terminator arrives', () => {
  const first = readFrames('data: {"SessionRemoved":{"ses');
  assert.deepEqual(first.frames, []);

  const second = readFrames(`${first.rest}sion":"s1"}}\n\n`);
  assert.equal(second.frames.length, 1);
  assert.equal(second.rest, '');
  assert.deepEqual(parseEngineEvent(second.frames[0].data), {
    kind: 'SessionRemoved',
    session: 's1',
  });
});

test('keep-alive comments and blank lines carry no frame', () => {
  const { frames } = readFrames(':\n\n: keep-alive\n\n');
  assert.deepEqual(frames, []);
});

/** A lag is the server saying "your view is incomplete" — it must not read as data. */
test('a lagged frame keeps its event name', () => {
  const { frames } = readFrames('event: lagged\ndata: 12\n\n');
  assert.deepEqual(frames, [{ event: 'lagged', data: '12' }]);
});

test('a multi-line data payload is rejoined', () => {
  const { frames } = readFrames('data: {"a":\ndata: 1}\n\n');
  assert.equal(frames[0].data, '{"a":\n1}');
});

test('a plan proposal carries its markdown', () => {
  const event = parseEngineEvent('{"PlanProposed":{"session":"s1","plan":"# Step one"}}');
  assert.deepEqual(event, { kind: 'PlanProposed', session: 's1', plan: '# Step one' });
});

/** Plain plan and danger-zone holds have no tool; only an MCP-authorize hold does. */
test('an approval request reports its tool only when there is one', () => {
  assert.deepEqual(
    parseEngineEvent('{"ApprovalRequested":{"session":"s1","what":"approve plan","authorize_tool":null}}'),
    { kind: 'ApprovalRequested', session: 's1', what: 'approve plan', authorizeTool: undefined }
  );
  assert.deepEqual(
    parseEngineEvent(
      '{"ApprovalRequested":{"session":"s1","what":"run a tool","authorize_tool":"mcp__x__y"}}'
    ),
    { kind: 'ApprovalRequested', session: 's1', what: 'run a tool', authorizeTool: 'mcp__x__y' }
  );
});

/**
 * The engine grows variants we do not model. Falling over on one would take the whole
 * stream down — including the plan gate, which is the part that cannot wait.
 */
test('an unmodelled variant is ignored rather than fatal', () => {
  assert.equal(parseEngineEvent('{"AuditAppended":{"session":"s1","summary":"x"}}'), undefined);
  assert.equal(parseEngineEvent('not json at all'), undefined);
  assert.equal(parseEngineEvent('{"PlanProposed":{"session":"s1"}}'), undefined);
});

test('only WaitingInput counts as waiting on the operator', () => {
  assert.equal(statusIsWaiting('WaitingInput'), true);
  assert.equal(statusIsWaiting('Running'), false);
  assert.equal(statusIsWaiting('Idle'), false);
});
