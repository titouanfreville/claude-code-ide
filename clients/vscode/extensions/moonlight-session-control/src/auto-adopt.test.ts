import * as assert from 'node:assert/strict';
import { test } from 'node:test';

import type { DiscoverableSession } from 'moonlight-control-client';

import { sessionsToAdopt, staleAttempts } from './auto-adopt';

function session(id: string, adopted: boolean): DiscoverableSession {
  return {
    session_id: id,
    title: id,
    root: `/work/${id}`,
    adopted,
    status: 'Idle' as DiscoverableSession['status'],
    phase: 'Plan' as DiscoverableSession['phase'],
    unreviewed_files: 0,
  };
}

const ownsAll = () => true;

test('only unadopted sessions are taken', () => {
  const sessions = [session('a', false), session('b', true)];
  assert.deepEqual(
    sessionsToAdopt(sessions, ownsAll, new Set()).map((s) => s.session_id),
    ['a']
  );
});

test('sessions this window does not own are left alone', () => {
  // The bug this prevents: every open window sees the whole fleet, so an unscoped
  // rule has all of them racing to adopt sessions from unrelated projects.
  const sessions = [session('mine', false), session('theirs', false)];
  const owns = (id: string) => id === 'mine';
  assert.deepEqual(
    sessionsToAdopt(sessions, owns, new Set()).map((s) => s.session_id),
    ['mine']
  );
});

test('an already attempted session is not retried', () => {
  // A five-second poll retrying a failing adopt is a request storm.
  const sessions = [session('a', false)];
  assert.deepEqual(sessionsToAdopt(sessions, ownsAll, new Set(['a'])), []);
});

test('attempts are forgotten once the session is adopted', () => {
  // Otherwise the set only grows, and a session released later could never be
  // adopted again by this window.
  assert.deepEqual(staleAttempts(new Set(['a']), [session('a', true)]), ['a']);
});

test('attempts are forgotten once the session is gone', () => {
  assert.deepEqual(staleAttempts(new Set(['a']), []), ['a']);
});

test('an attempt for a session still waiting is kept', () => {
  assert.deepEqual(staleAttempts(new Set(['a']), [session('a', false)]), []);
});
