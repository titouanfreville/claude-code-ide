/**
 * Answering a hold.
 *
 * The one rule worth stating twice: approving a plan is **two** calls. The server
 * swallows the `ApproveAction` that resolved a held hook, so it never reaches the
 * engine — a phase advance is what actually moves the session out of `Plan`, and the
 * hook release is what lets it act at all. Send one without the other and the
 * operator sees an approval that did nothing.
 */
import * as controlApi from 'moonlight-control-client';

import { approves, denialReason, type PlanVerdict } from './feedback';

/** What happened, in words the caller can show. */
export type GateOutcome = { ok: true; summary: string } | { ok: false; error: string };

/**
 * Apply a plan verdict to a held session.
 *
 * Reports failure rather than throwing: a hold that could not be answered is
 * information the operator needs immediately — the session is still stopped, and it
 * is still their move.
 */
export async function decidePlan(
  sessionId: string,
  plan: string,
  comments: ReadonlyMap<number, string>,
  verdict: PlanVerdict
): Promise<GateOutcome> {
  try {
    if (approves(verdict)) {
      await controlApi.approvePlan(sessionId);
      if (comments.size === 0) {
        return { ok: true, summary: 'Plan approved — the session may proceed.' };
      }
      // Notes on an approved plan have to reach the agent somehow, and an approval
      // carries no text of its own. They go through the review tracker — the one
      // path that actually writes into a session and reports honestly whether it
      // managed to — rather than being attached to the release and silently dropped.
      //
      // After the release, not instead of it: a session still blocked on its hook
      // never reads the message.
      return await deliverNotes(sessionId, denialReason(plan, comments, 'approve'));
    }
    await controlApi.verdict(sessionId, false, denialReason(plan, comments, verdict));
    return { ok: true, summary: 'Plan sent back with your feedback.' };
  } catch (err) {
    return { ok: false, error: err instanceof Error ? err.message : String(err) };
  }
}

/**
 * Post the operator's notes as a review-scope comment and send the batch.
 *
 * The plan is approved by the time this runs, so a failure here costs the notes, not
 * the approval — which is exactly why the summary says what happened to them instead
 * of reporting a flat success.
 */
async function deliverNotes(sessionId: string, body: string): Promise<GateOutcome> {
  await controlApi.addComment({
    sessionId,
    scope: 'Review',
    // A review-scope comment is about the work, not a file: the server expects an
    // empty path and no line range (see `CommentScope::Review` in domain/changes).
    path: '',
    side: 'After',
    startLine: 0,
    endLine: 0,
    body,
  });
  const sent = await controlApi.submitReview(sessionId);
  if (sent && sent.feedback.status === 'undeliverable') {
    return {
      ok: true,
      summary: `Plan approved. Your notes could not be delivered (${sent.feedback.reason}) — they are queued in the review tracker.`,
    };
  }
  return { ok: true, summary: 'Plan approved — your notes went to the session.' };
}

/**
 * Answer a danger-zone or MCP-authorize hold — no plan, just allow or refuse.
 */
export async function decideAction(
  sessionId: string,
  allow: boolean,
  reason: string
): Promise<GateOutcome> {
  try {
    await controlApi.verdict(sessionId, allow, allow ? undefined : reason);
    return {
      ok: true,
      summary: allow ? 'Action allowed.' : 'Action refused — the session was told why.',
    };
  } catch (err) {
    return { ok: false, error: err instanceof Error ? err.message : String(err) };
  }
}
