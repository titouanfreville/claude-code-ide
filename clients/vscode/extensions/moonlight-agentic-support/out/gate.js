"use strict";
var __createBinding = (this && this.__createBinding) || (Object.create ? (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    var desc = Object.getOwnPropertyDescriptor(m, k);
    if (!desc || ("get" in desc ? !m.__esModule : desc.writable || desc.configurable)) {
      desc = { enumerable: true, get: function() { return m[k]; } };
    }
    Object.defineProperty(o, k2, desc);
}) : (function(o, m, k, k2) {
    if (k2 === undefined) k2 = k;
    o[k2] = m[k];
}));
var __setModuleDefault = (this && this.__setModuleDefault) || (Object.create ? (function(o, v) {
    Object.defineProperty(o, "default", { enumerable: true, value: v });
}) : function(o, v) {
    o["default"] = v;
});
var __importStar = (this && this.__importStar) || (function () {
    var ownKeys = function(o) {
        ownKeys = Object.getOwnPropertyNames || function (o) {
            var ar = [];
            for (var k in o) if (Object.prototype.hasOwnProperty.call(o, k)) ar[ar.length] = k;
            return ar;
        };
        return ownKeys(o);
    };
    return function (mod) {
        if (mod && mod.__esModule) return mod;
        var result = {};
        if (mod != null) for (var k = ownKeys(mod), i = 0; i < k.length; i++) if (k[i] !== "default") __createBinding(result, mod, k[i]);
        __setModuleDefault(result, mod);
        return result;
    };
})();
Object.defineProperty(exports, "__esModule", { value: true });
exports.decidePlan = decidePlan;
exports.decideAction = decideAction;
/**
 * Answering a hold.
 *
 * The one rule worth stating twice: approving a plan is **two** calls. The server
 * swallows the `ApproveAction` that resolved a held hook, so it never reaches the
 * engine — a phase advance is what actually moves the session out of `Plan`, and the
 * hook release is what lets it act at all. Send one without the other and the
 * operator sees an approval that did nothing.
 */
const controlApi = __importStar(require("moonlight-control-client"));
const feedback_1 = require("./feedback");
/**
 * Apply a plan verdict to a held session.
 *
 * Reports failure rather than throwing: a hold that could not be answered is
 * information the operator needs immediately — the session is still stopped, and it
 * is still their move.
 */
async function decidePlan(sessionId, plan, comments, verdict) {
    try {
        if ((0, feedback_1.approves)(verdict)) {
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
            return await deliverNotes(sessionId, (0, feedback_1.denialReason)(plan, comments, 'approve'));
        }
        await controlApi.verdict(sessionId, false, (0, feedback_1.denialReason)(plan, comments, verdict));
        return { ok: true, summary: 'Plan sent back with your feedback.' };
    }
    catch (err) {
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
async function deliverNotes(sessionId, body) {
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
async function decideAction(sessionId, allow, reason) {
    try {
        await controlApi.verdict(sessionId, allow, allow ? undefined : reason);
        return {
            ok: true,
            summary: allow ? 'Action allowed.' : 'Action refused — the session was told why.',
        };
    }
    catch (err) {
        return { ok: false, error: err instanceof Error ? err.message : String(err) };
    }
}
//# sourceMappingURL=gate.js.map