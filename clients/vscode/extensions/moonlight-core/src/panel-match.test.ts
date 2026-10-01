import * as assert from 'node:assert/strict';
import { test } from 'node:test';

import { isClaudePanel, matchPanelSession, panelLabelFor } from './panel-match';

const session = (session_id: string, title: string | null, root = '/work/app', status = 'Running') => ({
  session_id,
  title,
  root,
  status,
});

test('labels follow Claude Code truncation', () => {
  assert.equal(panelLabelFor('Refactor auth'), 'Refactor auth');
  assert.equal(panelLabelFor('x'.repeat(25)), 'x'.repeat(25));
  assert.equal(panelLabelFor('x'.repeat(26)), `${'x'.repeat(24)}…`);
});

test('the webview prefix VSCode adds does not hide a Claude panel', () => {
  assert.equal(isClaudePanel('claudeVSCodePanel'), true);
  assert.equal(isClaudePanel('mainThreadWebview-claudeVSCodePanel'), true);
  assert.equal(isClaudePanel('mainThreadWebview-claudePlanPreview'), false);
});

test('a focused panel resolves to the session its label names', () => {
  const sessions = [session('a', 'Refactor auth'), session('b', 'Follow the focused session in VSCode')];
  assert.equal(matchPanelSession('Refactor auth', sessions, ['/work/app']), 'a');
  assert.equal(matchPanelSession('Follow the focused sessi…', sessions, ['/work/app']), 'b');
});

test('an untitled panel matches nothing', () => {
  assert.equal(matchPanelSession('Claude Code', [session('a', 'Claude Code')], ['/work/app']), undefined);
});

test('ties narrow to this workspace, then to running sessions', () => {
  const sessions = [
    session('elsewhere', 'Fix tests', '/other'),
    session('idle', 'Fix tests', '/work/app', 'Idle'),
    session('here', 'Fix tests', '/work/app'),
  ];
  assert.equal(matchPanelSession('Fix tests', sessions, ['/work/app']), 'here');
});

test('an unresolvable tie is no match rather than a pick', () => {
  const sessions = [session('a', 'Fix tests'), session('b', 'Fix tests')];
  assert.equal(matchPanelSession('Fix tests', sessions, ['/work/app']), undefined);
});
