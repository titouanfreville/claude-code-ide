package io.github.titouanfreville.moonlight.agentic

import io.github.titouanfreville.moonlight.client.CommentScope
import io.github.titouanfreville.moonlight.client.ControlApi
import io.github.titouanfreville.moonlight.client.DiffSide
import io.github.titouanfreville.moonlight.client.FeedbackDelivery

/**
 * Answering a hold — a port of `gate.ts`.
 *
 * Approving a plan is **two** calls: the server swallows the approval that released a held
 * hook, so a phase advance is what moves the session out of `Plan`, and the hook release
 * is what lets it act at all. Send one without the other and the approval did nothing.
 *
 * Blocking — call off the EDT. Failures are returned rather than thrown: a hold that could
 * not be answered is information the operator needs now, because the session is still
 * stopped and it is still their move.
 */
sealed interface GateOutcome {
    data class Ok(val summary: String) : GateOutcome
    data class Failed(val error: String) : GateOutcome
}

fun decidePlan(control: ControlApi, sessionId: String, plan: String, comments: Map<Int, String>, verdict: PlanVerdict): GateOutcome =
    try {
        if (verdict.approves) {
            control.approvePlan(sessionId)
            if (comments.isEmpty()) {
                GateOutcome.Ok("Plan approved — the session may proceed.")
            } else {
                // After the release, not instead of it: a session still blocked on its hook
                // never reads the message.
                deliverNotes(control, sessionId, denialReason(plan, comments, PlanVerdict.Approve))
            }
        } else {
            control.verdict(sessionId, false, denialReason(plan, comments, verdict))
            GateOutcome.Ok("Plan sent back with your feedback (${verdict.label}).")
        }
    } catch (e: Exception) {
        GateOutcome.Failed(e.message ?: e.toString())
    }

/**
 * Notes on an approved plan travel as a review-scope comment plus a submit — the path that
 * actually writes into a session and reports honestly whether it managed to. The plan is
 * approved by now, so a failure costs the notes, not the approval, and the summary says so.
 */
private fun deliverNotes(control: ControlApi, sessionId: String, body: String): GateOutcome {
    // Review-scoped: about the work, not a file — an empty path and no line range.
    control.addComment(sessionId, CommentScope.Review, "", DiffSide.After, 0, 0, body)
    val sent = control.submitReview(sessionId)
    val feedback = sent?.feedback
    return if (feedback is FeedbackDelivery.Undeliverable) {
        GateOutcome.Ok("Plan approved. Your notes could not be delivered (${feedback.reason}) — they are queued in the review tracker.")
    } else {
        GateOutcome.Ok("Plan approved — your notes went to the session.")
    }
}

/** Answer a danger-zone or MCP-authorize hold — no plan, just allow or refuse. */
fun decideAction(control: ControlApi, sessionId: String, allow: Boolean, reason: String): GateOutcome =
    try {
        control.verdict(sessionId, allow, if (allow) null else reason)
        GateOutcome.Ok(if (allow) "Action allowed." else "Action refused — the session was told why.")
    } catch (e: Exception) {
        GateOutcome.Failed(e.message ?: e.toString())
    }
