/**
 * Choosing which session a review acts on.
 *
 * Pure and `vscode`-free so the rule can be tested directly, following `feedback.ts`
 * in the agentic-support extension for the same reason: this is the part that has to
 * be right, and everything around it is plumbing.
 *
 * The rule exists because a picker is the most expensive way to answer a question the
 * editor usually already knows. The status bar reads "3 changed" *about a session* —
 * it is computed from the active one — so a click that then asks which session to
 * review has discarded an answer the operator just gave it.
 */

/**
 * The session whose diffs to open, or `undefined` when the operator has to be asked.
 *
 * In order:
 *
 * 1. `requested` — a caller that already knows, such as the status bar click.
 * 2. `active` — the session the operator is working in.
 * 3. The only session with anything to review.
 *
 * A named session with nothing queued is skipped rather than honoured: the status bar
 * can be a poll behind, and opening an empty review because of a stale count is worse
 * than falling through to the picker. That is also why this takes the queue's sessions
 * rather than trusting the caller — every candidate is checked against real work.
 */
export function resolveReviewSession(
  requested: string | undefined,
  active: string | undefined,
  withChanges: ReadonlySet<string>
): string | undefined {
  if (requested !== undefined && withChanges.has(requested)) {
    return requested;
  }
  if (active !== undefined && withChanges.has(active)) {
    return active;
  }
  if (withChanges.size === 1) {
    return [...withChanges][0];
  }
  // Several sessions have changes and none of them is the obvious one — the only case
  // where the operator genuinely has to choose.
  return undefined;
}
