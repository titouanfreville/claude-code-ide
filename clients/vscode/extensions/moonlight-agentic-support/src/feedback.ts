/**
 * Turning an operator's verdict and notes into feedback an agent can act on.
 *
 * A port of `review_feedback` in
 * `apps/zed_based_desktop/crates/moonlight_ui/src/plan_review.rs`. Kept as pure
 * functions with no `vscode` import so the composition can be tested directly — it is
 * the part that has to be right, since this text is the *only* thing the held session
 * receives. Everything else in this extension is a way of collecting it.
 */

/**
 * What the operator decided about a plan.
 *
 * All four carry the comments; they differ in what the agent is being asked to *do*
 * with them. That instruction is the whole point — the same three notes mean "keep
 * these in mind", "answer these first", "fix these", or "start over" depending on the
 * verdict, and an agent given the notes without the instruction has to guess.
 */
export type PlanVerdict = 'approve' | 'open-question' | 'refine' | 'no-go';

/** Default feedback when the operator sends a plan back without typing anything. */
export const DEFAULT_REJECT_REASON =
  'Plan rejected by operator — please revise and re-propose.';

/**
 * The verdict in a form an agent can read without interpreting prose.
 *
 * The sentences below are written for a person, and an agent recovering the verdict
 * from them is doing paraphrase-matching on text we are free to reword. The two that
 * matter most are also the closest in wording: `refine` says keep this approach and fix
 * it, `no-go` says throw it away — and an agent that reads one as the other revises
 * exactly the approach it was told to abandon.
 *
 * So the verdict leads, in a fixed machine-readable form, and the prose follows
 * unchanged for whoever reads the transcript.
 */
export function verdictTag(verdict: PlanVerdict): string {
  return `[verdict: ${verdict}]`;
}

/** Whether the verdict releases the plan or sends it back. */
export function approves(verdict: PlanVerdict): boolean {
  return verdict === 'approve';
}

/** The instruction the comments arrive under. */
function preamble(verdict: PlanVerdict, count: number): string {
  const plural = count === 1 ? '' : 's';
  // The verb has to agree too: "1 question needs answering", not "need".
  const verb = count === 1 ? 'needs' : 'need';
  const tag = verdictTag(verdict);
  switch (verdict) {
    case 'approve':
      return `${tag} Plan approved with ${count} note${plural} — keep these in mind as you work:`;
    case 'open-question':
      return `${tag} Before this plan can be approved, ${count} question${plural} ${verb} answering. Answer them and re-propose:`;
    case 'refine':
      return `${tag} The approach is right but the plan needs revision. Address these ${count} comment${plural} and re-propose:`;
    case 'no-go':
      return `${tag} This approach is not the right one. Discard this plan and propose a different approach, taking account of these ${count} comment${plural}:`;
  }
}

/**
 * Split a plan into addressable sections, one per markdown heading.
 *
 * Headings, not blank lines: a plan is written as titled steps, and that is how an
 * operator thinks about it — "the migration bit", not "the fourth paragraph". Blank
 * lines split in places nobody considers a boundary, which scatters comments across
 * fragments of a single thought.
 *
 * Anything before the first heading becomes its own leading section, so a plan with
 * no headings at all is still commentable as one piece.
 */
export function planBlocks(plan: string): string[] {
  const sections: string[] = [];
  let current = '';

  for (const line of plan.split('\n')) {
    if (line.trimStart().startsWith('#') && current.trim().length > 0) {
      sections.push(current.trim());
      current = '';
    }
    current += `${line}\n`;
  }
  if (current.trim().length > 0) {
    sections.push(current.trim());
  }
  return sections;
}

/** The first heading of a section, for compact display. */
export function blockTitle(block: string): string {
  const first = block.split('\n')[0] ?? '';
  const title = first.replace(/^#+/, '').trim();
  return title.length > 0 ? title : '(untitled)';
}

/**
 * Render the verdict and its comments as feedback the agent can act on.
 *
 * Each comment quotes the section heading it lands on. Without the anchor the agent
 * receives opinions with no referent — it has the plan, but not which part each note
 * is about, and revises by guesswork.
 */
export function reviewFeedback(
  plan: string,
  comments: ReadonlyMap<number, string>,
  verdict: PlanVerdict
): string {
  const blocks = planBlocks(plan);
  let out = `${preamble(verdict, comments.size)}\n`;

  // Ascending block order, so the notes arrive in the order the plan reads.
  for (const index of [...comments.keys()].sort((a, b) => a - b)) {
    const block = blocks[index];
    if (block === undefined) {
      continue;
    }
    out += `\n## ${blockTitle(block)}\n`;
    for (const line of (comments.get(index) ?? '').split('\n')) {
      out += `  ${line}\n`;
    }
  }
  return out;
}

/**
 * The text sent with a non-approving verdict: the composed feedback, or a plain
 * refusal when the operator wrote nothing.
 *
 * A bare denial reaches the agent as "MoonlightCode: <reason>" and is all it has to
 * work from, so an empty one leaves it to re-propose the same plan by guesswork.
 */
export function denialReason(
  plan: string,
  comments: ReadonlyMap<number, string>,
  verdict: PlanVerdict
): string {
  if (comments.size === 0) {
    // Tagged as well: a no-go with nothing typed is a different instruction from a
    // refine with nothing typed, and the shared sentence cannot tell them apart.
    return `${verdictTag(verdict)} ${DEFAULT_REJECT_REASON}`;
  }
  return reviewFeedback(plan, comments, verdict);
}
