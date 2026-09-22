import assert from 'node:assert/strict';
import { test } from 'node:test';

import type { DiscoverableSession } from 'moonlight-control-client';

import { effectiveMode, groupSessions, projectLabel, type CustomGroup } from './grouping';

function session(id: string, root: string | null): DiscoverableSession {
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

const keys = (g: ReturnType<typeof groupSessions>): string[] =>
  g.kind === 'grouped' ? g.groups.map((x) => x.label) : [];

test('sessions in different repos group by project', () => {
  const grouping = groupSessions(
    [session('a', '/work/alpha'), session('b', '/work/beta')],
    'project'
  );
  assert.equal(grouping.kind, 'grouped');
  assert.deepEqual(keys(grouping), ['alpha', 'beta']);
});

/** One header over the whole list costs a row and says nothing. */
test('a single group renders flat instead', () => {
  const grouping = groupSessions([session('a', '/work/alpha'), session('b', '/work/alpha')], 'project');
  assert.equal(grouping.kind, 'flat');
});

test('grouping off is always flat', () => {
  const grouping = groupSessions([session('a', '/x'), session('b', '/y')], 'none');
  assert.equal(grouping.kind, 'flat');
});

/**
 * Group order is first-appearance order, and the caller sorts worst-first — so the
 * group holding the session that needs you comes first without sorting groups at all.
 */
test('group order follows the triage order of the input', () => {
  const grouping = groupSessions(
    [session('needs-you', '/work/beta'), session('idle', '/work/alpha')],
    'project'
  );
  assert.deepEqual(keys(grouping), ['beta', 'alpha']);
});

test('sessions with no root land in a bucket that sorts last', () => {
  const grouping = groupSessions(
    [session('orphan', null), session('a', '/work/alpha')],
    'project'
  );
  // Sorts last despite appearing first in the triage order.
  assert.deepEqual(keys(grouping), ['alpha', 'No project']);
});

test('colliding basenames are disambiguated by parent, others are not', () => {
  const roots = ['/one/web', '/two/web', '/solo/api'];
  assert.equal(projectLabel('/one/web', roots), 'one/web');
  assert.equal(projectLabel('/two/web', roots), 'two/web');
  assert.equal(projectLabel('/solo/api', roots), 'api');
});

test('a custom group takes its members, the rest still group by project', () => {
  const custom: CustomGroup[] = [{ id: 'g1', name: 'Release work', sessionIds: ['a'] }];
  const grouping = groupSessions(
    [session('a', '/work/alpha'), session('b', '/work/beta')],
    'custom',
    custom
  );
  assert.deepEqual(keys(grouping), ['Release work', 'beta']);
});

test('a custom group survives being the only group', () => {
  // The flat-when-single rule must not swallow a group the operator made by hand.
  const custom: CustomGroup[] = [{ id: 'g1', name: 'Mine', sessionIds: ['a'] }];
  const grouping = groupSessions([session('a', '/work/alpha')], 'custom', custom);
  assert.deepEqual(keys(grouping), ['Mine']);
});

test('a custom group keeps ids of sessions that have ended without claiming them', () => {
  const custom: CustomGroup[] = [{ id: 'g1', name: 'Mine', sessionIds: ['gone', 'a'] }];
  const grouping = groupSessions([session('a', '/work/alpha')], 'custom', custom);
  assert.equal(grouping.kind, 'grouped');
  if (grouping.kind === 'grouped') {
    assert.deepEqual(grouping.groups[0].sessions.map((s) => s.session_id), ['a']);
  }
});

test('an empty custom group is still rendered', () => {
  // It is somewhere to drop things; hiding it reads as the group having been lost.
  const custom: CustomGroup[] = [{ id: 'g1', name: 'Empty', sessionIds: [] }];
  const grouping = groupSessions([session('a', '/work/alpha')], 'custom', custom);
  assert.deepEqual(keys(grouping), ['Empty', 'alpha']);
});

test('no sessions is flat, not an empty group', () => {
  assert.deepEqual(groupSessions([], 'project'), { kind: 'flat', sessions: [] });
});

test('group keys are stable across calls so expansion state survives a refresh', () => {
  const input = [session('a', '/work/alpha'), session('b', '/work/beta')];
  const first = groupSessions(input, 'project');
  const second = groupSessions(input, 'project');
  assert.equal(first.kind, 'grouped');
  if (first.kind === 'grouped' && second.kind === 'grouped') {
    assert.deepEqual(
      first.groups.map((g) => g.key),
      second.groups.map((g) => g.key)
    );
  }
});

/**
 * Custom groups are for sessions the operator has taken on. An unadopted one is still
 * being triaged, and it changes lists the moment it is adopted.
 */
test('the not-adopted view falls back to project grouping', () => {
  assert.equal(effectiveMode('unadopted', 'custom'), 'project');
});

test('every other mode is left alone in both views', () => {
  for (const scope of ['governed', 'unadopted'] as const) {
    assert.equal(effectiveMode(scope, 'project'), 'project');
    assert.equal(effectiveMode(scope, 'none'), 'none');
  }
  assert.equal(effectiveMode('governed', 'custom'), 'custom');
});
