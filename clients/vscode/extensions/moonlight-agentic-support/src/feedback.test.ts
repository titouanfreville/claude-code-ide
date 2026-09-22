import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  DEFAULT_REJECT_REASON,
  approves,
  blockTitle,
  denialReason,
  planBlocks,
  reviewFeedback,
} from './feedback';

const PLAN = `# Migrate the store
Move the rows first.

# Delete the old table
Only after the backfill.`;

test('a plan splits on headings, not on blank lines', () => {
  const blocks = planBlocks(PLAN);
  assert.equal(blocks.length, 2);
  assert.equal(blockTitle(blocks[0]), 'Migrate the store');
  assert.equal(blockTitle(blocks[1]), 'Delete the old table');
});

/** A plan with no headings is still one commentable piece, not zero. */
test('prose with no heading is a single block', () => {
  const blocks = planBlocks('just do the thing');
  assert.deepEqual(blocks, ['just do the thing']);
  assert.equal(blockTitle(blocks[0]), 'just do the thing');
});

test('an empty plan has no blocks', () => {
  assert.deepEqual(planBlocks('   \n\n'), []);
});

/**
 * The anchor is the point. Without the heading the agent receives opinions with no
 * referent — it has the plan, but not which part each note is about.
 */
test('each comment is quoted under the section it lands on', () => {
  const feedback = reviewFeedback(PLAN, new Map([[1, 'back this up first']]), 'refine');
  assert.match(feedback, /## Delete the old table/);
  assert.match(feedback, /  back this up first/);
  assert.doesNotMatch(feedback, /## Migrate the store/);
});

test('comments arrive in the order the plan reads', () => {
  const feedback = reviewFeedback(
    PLAN,
    new Map([
      [1, 'second note'],
      [0, 'first note'],
    ]),
    'refine'
  );
  assert.ok(feedback.indexOf('first note') < feedback.indexOf('second note'));
});

/**
 * The same notes mean different things under different verdicts, and the preamble is
 * the only thing carrying that difference.
 */
test('the verdict changes the instruction the comments arrive under', () => {
  const comments = new Map([[0, 'why this order?']]);
  assert.match(reviewFeedback(PLAN, comments, 'approve'), /keep these in mind/);
  assert.match(reviewFeedback(PLAN, comments, 'open-question'), /needs answering/);
  assert.match(reviewFeedback(PLAN, comments, 'refine'), /approach is right/);
  assert.match(reviewFeedback(PLAN, comments, 'no-go'), /not the right one/);
});

test('one comment reads as singular, two as plural', () => {
  const one = reviewFeedback(PLAN, new Map([[0, 'a']]), 'open-question');
  assert.match(one, /1 question needs answering/);
  const two = reviewFeedback(
    PLAN,
    new Map([
      [0, 'a'],
      [1, 'b'],
    ]),
    'open-question'
  );
  assert.match(two, /2 questions need answering/);
});

/** A comment on a block that no longer exists is dropped, not rendered headless. */
test('a comment with no surviving block is left out', () => {
  const feedback = reviewFeedback('# only one', new Map([[7, 'stale']]), 'refine');
  assert.doesNotMatch(feedback, /stale/);
});

/**
 * A denial's reason is everything the agent learns from being refused, so an empty
 * one still has to say something it can act on.
 */
test('a wordless refusal still carries a reason', () => {
  assert.equal(denialReason(PLAN, new Map(), 'no-go'), `[verdict: no-go] ${DEFAULT_REJECT_REASON}`);
});

test('only approve approves', () => {
  assert.equal(approves('approve'), true);
  for (const verdict of ['open-question', 'refine', 'no-go'] as const) {
    assert.equal(approves(verdict), false);
  }
});

/**
 * The verdict must survive as data, not as a sentence an agent has to recognise.
 * `refine` and `no-go` are the pair that matters: keep this approach versus discard it.
 */
test('every verdict leads with a machine-readable tag', () => {
  const comments = new Map([[0, 'a note']]);
  for (const verdict of ['approve', 'open-question', 'refine', 'no-go'] as const) {
    assert.ok(
      reviewFeedback(PLAN, comments, verdict).startsWith(`[verdict: ${verdict}]`),
      verdict
    );
  }
});

test('a wordless refusal still says which verdict it was', () => {
  // Without the tag these two are byte-identical, and they ask for opposite things.
  const refine = denialReason(PLAN, new Map(), 'refine');
  const noGo = denialReason(PLAN, new Map(), 'no-go');
  assert.notEqual(refine, noGo);
  assert.ok(refine.startsWith('[verdict: refine]'), refine);
  assert.ok(noGo.startsWith('[verdict: no-go]'), noGo);
});
