import assert from 'node:assert/strict';
import { test } from 'node:test';

import { resolveReviewSession } from './target';

const queued = (...ids: string[]): ReadonlySet<string> => new Set(ids);

/** The reported bug: the status bar knew the session, the click asked anyway. */
test('a named session with changes is used without asking', () => {
  assert.equal(resolveReviewSession('a', 'b', queued('a', 'b', 'c')), 'a');
});

test('with nothing named, the active session wins over a picker', () => {
  assert.equal(resolveReviewSession(undefined, 'b', queued('a', 'b', 'c')), 'b');
});

test('a lone session with changes needs no choosing', () => {
  assert.equal(resolveReviewSession(undefined, undefined, queued('a')), 'a');
});

test('several sessions and no hint is the one case worth asking about', () => {
  assert.equal(resolveReviewSession(undefined, undefined, queued('a', 'b')), undefined);
});

/**
 * The count in the status bar comes from a poll, so it can describe work that was
 * reviewed a moment ago. Honouring the stale id would open an empty review.
 */
test('a named session with nothing queued falls through to the active one', () => {
  assert.equal(resolveReviewSession('gone', 'b', queued('b', 'c')), 'b');
});

test('a named session with nothing queued still falls through to a lone candidate', () => {
  assert.equal(resolveReviewSession('gone', undefined, queued('only')), 'only');
});

test('an active session with nothing queued does not suppress the picker', () => {
  // 'b' is where the operator is working but has nothing to review; two other
  // sessions do. Choosing either for them would be a guess.
  assert.equal(resolveReviewSession(undefined, 'b', queued('a', 'c')), undefined);
});

test('an empty queue never yields a session', () => {
  assert.equal(resolveReviewSession('a', 'b', queued()), undefined);
});
