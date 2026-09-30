import * as assert from 'node:assert/strict';
import { test } from 'node:test';

import { PLAN_HOLD } from 'moonlight-control-client';

import { isPlanHold, shouldActUnprompted } from './hold-routing';

test('only the gate\u2019s plan marker routes to the plan panel', () => {
  assert.equal(isPlanHold({ what: PLAN_HOLD }), true);
  // The bug this replaced: a session that had ever proposed a plan had one cached
  // against it, so every later hold looked like a plan and a command approval opened
  // the plan review panel.
  assert.equal(isPlanHold({ what: 'run `npx js-yaml`' }), false);
  assert.equal(isPlanHold({ what: 'write outside the workspace' }), false);
});

test('a core too old to answer keeps the old behaviour', () => {
  // `undefined` is not `false`. Written as a function because a refactor turning
  // `=== false` into `!== true` would stop the plan panel opening anywhere at all
  // against an older core — the worse of the two failures.
  assert.equal(shouldActUnprompted(undefined), true);
  assert.equal(shouldActUnprompted(true), true);
  assert.equal(shouldActUnprompted(false), false);
});
