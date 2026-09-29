import * as assert from 'node:assert/strict';
import { test } from 'node:test';

import { isInside, windowOwnsSession } from './ownership';

test('a folder contains itself and its descendants', () => {
  assert.equal(isInside('/work/app', '/work/app'), true);
  assert.equal(isInside('/work/app/src', '/work/app'), true);
  assert.equal(isInside('/work', '/work/app'), false);
});

test('containment is by segment, not by prefix', () => {
  // The bug a `startsWith` check has: a window open on /work/app would claim a session
  // rooted in the unrelated /work/app-old next to it.
  assert.equal(isInside('/work/app-old', '/work/app'), false);
  assert.equal(isInside('/work/application', '/work/app'), false);
});

test('trailing separators do not change the answer', () => {
  assert.equal(isInside('/work/app/', '/work/app'), true);
  assert.equal(isInside('/work/app/src', '/work/app/'), true);
});

test('the active session is owned whatever its root says', () => {
  // The claim that matters most: a session the operator adopted in this window is
  // theirs even when it has no root, or its root is open nowhere.
  assert.equal(windowOwnsSession('s1', 's1', undefined, []), true);
  assert.equal(windowOwnsSession('s1', 's1', '/elsewhere', ['/work/app']), true);
});

test('a session rooted in this workspace is owned', () => {
  assert.equal(windowOwnsSession('s1', undefined, '/work/app/src', ['/work/app']), true);
  assert.equal(windowOwnsSession('s1', 's2', '/work/app/src', ['/work/app']), true);
});

test('a session belonging to another workspace is not owned', () => {
  // This is the case that made the plan panel open in every window at once.
  assert.equal(windowOwnsSession('s1', 's2', '/other/repo', ['/work/app']), false);
  assert.equal(windowOwnsSession('s1', undefined, undefined, ['/work/app']), false);
  assert.equal(windowOwnsSession('s1', undefined, '/work/app', []), false);
});

test('any one of several workspace folders is enough', () => {
  assert.equal(
    windowOwnsSession('s1', undefined, '/b/pkg/src', ['/a', '/b/pkg', '/c']),
    true
  );
});
