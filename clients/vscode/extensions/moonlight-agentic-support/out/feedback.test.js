"use strict";
var __importDefault = (this && this.__importDefault) || function (mod) {
    return (mod && mod.__esModule) ? mod : { "default": mod };
};
Object.defineProperty(exports, "__esModule", { value: true });
const strict_1 = __importDefault(require("node:assert/strict"));
const node_test_1 = require("node:test");
const feedback_1 = require("./feedback");
const PLAN = `# Migrate the store
Move the rows first.

# Delete the old table
Only after the backfill.`;
(0, node_test_1.test)('a plan splits on headings, not on blank lines', () => {
    const blocks = (0, feedback_1.planBlocks)(PLAN);
    strict_1.default.equal(blocks.length, 2);
    strict_1.default.equal((0, feedback_1.blockTitle)(blocks[0]), 'Migrate the store');
    strict_1.default.equal((0, feedback_1.blockTitle)(blocks[1]), 'Delete the old table');
});
/** A plan with no headings is still one commentable piece, not zero. */
(0, node_test_1.test)('prose with no heading is a single block', () => {
    const blocks = (0, feedback_1.planBlocks)('just do the thing');
    strict_1.default.deepEqual(blocks, ['just do the thing']);
    strict_1.default.equal((0, feedback_1.blockTitle)(blocks[0]), 'just do the thing');
});
(0, node_test_1.test)('an empty plan has no blocks', () => {
    strict_1.default.deepEqual((0, feedback_1.planBlocks)('   \n\n'), []);
});
/**
 * The anchor is the point. Without the heading the agent receives opinions with no
 * referent — it has the plan, but not which part each note is about.
 */
(0, node_test_1.test)('each comment is quoted under the section it lands on', () => {
    const feedback = (0, feedback_1.reviewFeedback)(PLAN, new Map([[1, 'back this up first']]), 'refine');
    strict_1.default.match(feedback, /## Delete the old table/);
    strict_1.default.match(feedback, /  back this up first/);
    strict_1.default.doesNotMatch(feedback, /## Migrate the store/);
});
(0, node_test_1.test)('comments arrive in the order the plan reads', () => {
    const feedback = (0, feedback_1.reviewFeedback)(PLAN, new Map([
        [1, 'second note'],
        [0, 'first note'],
    ]), 'refine');
    strict_1.default.ok(feedback.indexOf('first note') < feedback.indexOf('second note'));
});
/**
 * The same notes mean different things under different verdicts, and the preamble is
 * the only thing carrying that difference.
 */
(0, node_test_1.test)('the verdict changes the instruction the comments arrive under', () => {
    const comments = new Map([[0, 'why this order?']]);
    strict_1.default.match((0, feedback_1.reviewFeedback)(PLAN, comments, 'approve'), /keep these in mind/);
    strict_1.default.match((0, feedback_1.reviewFeedback)(PLAN, comments, 'open-question'), /needs answering/);
    strict_1.default.match((0, feedback_1.reviewFeedback)(PLAN, comments, 'refine'), /approach is right/);
    strict_1.default.match((0, feedback_1.reviewFeedback)(PLAN, comments, 'no-go'), /not the right one/);
});
(0, node_test_1.test)('one comment reads as singular, two as plural', () => {
    const one = (0, feedback_1.reviewFeedback)(PLAN, new Map([[0, 'a']]), 'open-question');
    strict_1.default.match(one, /1 question needs answering/);
    const two = (0, feedback_1.reviewFeedback)(PLAN, new Map([
        [0, 'a'],
        [1, 'b'],
    ]), 'open-question');
    strict_1.default.match(two, /2 questions need answering/);
});
/** A comment on a block that no longer exists is dropped, not rendered headless. */
(0, node_test_1.test)('a comment with no surviving block is left out', () => {
    const feedback = (0, feedback_1.reviewFeedback)('# only one', new Map([[7, 'stale']]), 'refine');
    strict_1.default.doesNotMatch(feedback, /stale/);
});
/**
 * A denial's reason is everything the agent learns from being refused, so an empty
 * one still has to say something it can act on.
 */
(0, node_test_1.test)('a wordless refusal still carries a reason', () => {
    strict_1.default.equal((0, feedback_1.denialReason)(PLAN, new Map(), 'no-go'), `[verdict: no-go] ${feedback_1.DEFAULT_REJECT_REASON}`);
});
(0, node_test_1.test)('only approve approves', () => {
    strict_1.default.equal((0, feedback_1.approves)('approve'), true);
    for (const verdict of ['open-question', 'refine', 'no-go']) {
        strict_1.default.equal((0, feedback_1.approves)(verdict), false);
    }
});
/**
 * The verdict must survive as data, not as a sentence an agent has to recognise.
 * `refine` and `no-go` are the pair that matters: keep this approach versus discard it.
 */
(0, node_test_1.test)('every verdict leads with a machine-readable tag', () => {
    const comments = new Map([[0, 'a note']]);
    for (const verdict of ['approve', 'open-question', 'refine', 'no-go']) {
        strict_1.default.ok((0, feedback_1.reviewFeedback)(PLAN, comments, verdict).startsWith(`[verdict: ${verdict}]`), verdict);
    }
});
(0, node_test_1.test)('a wordless refusal still says which verdict it was', () => {
    // Without the tag these two are byte-identical, and they ask for opposite things.
    const refine = (0, feedback_1.denialReason)(PLAN, new Map(), 'refine');
    const noGo = (0, feedback_1.denialReason)(PLAN, new Map(), 'no-go');
    strict_1.default.notEqual(refine, noGo);
    strict_1.default.ok(refine.startsWith('[verdict: refine]'), refine);
    strict_1.default.ok(noGo.startsWith('[verdict: no-go]'), noGo);
});
//# sourceMappingURL=feedback.test.js.map