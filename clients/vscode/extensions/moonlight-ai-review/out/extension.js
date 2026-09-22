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
exports.activate = activate;
exports.deactivate = deactivate;
/**
 * MoonlightCode AI Review — reviewing what an agent changed.
 *
 * GitHub-style review, with the agent as the author: a real diff editor against the
 * session's own baseline, inline comment threads, and one batched review delivered
 * back to the session.
 *
 * Scope is the **session** diff base only — every file this agent wrote, diffed from
 * the pre-image captured the first time it touched them. Not the git working tree;
 * that answers a different question and is the only base whose hunks can be staged.
 *
 * Everything a review produces lives in the shared tracker (the same SQLite store the
 * desktop cockpit reads), never in this extension's memory — so a review survives
 * closing the editor, and the two surfaces cannot disagree about what was said.
 */
const vscode = __importStar(require("vscode"));
const crypto = __importStar(require("crypto"));
const controlApi = __importStar(require("moonlight-control-client"));
const target_1 = require("./target");
const review_1 = require("./review");
const delivery_1 = require("./delivery");
const core_1 = require("./core");
function activate(context) {
    // Resolved on first use, not during activate: an early return here would leave
    // every command declared but unregistered, so the palette offers them and each
    // fails with "command not found" and no explanation.
    let cached;
    const withCore = async () => (cached ??= await (0, core_1.requireCore)());
    context.subscriptions.push(vscode.workspace.registerTextDocumentContentProvider(review_1.BASELINE_SCHEME, new review_1.BaselineProvider()));
    const reviewComments = new review_1.ReviewComments(() => void refreshPending());
    reviewComments.watchSelections(context);
    context.subscriptions.push(reviewComments);
    // Distinct from the gating indicator that `moonlight-status` owns at priority 100:
    // that one answers "is anything governed", this one answers "what changed". They sit
    // side by side, and each extension can be installed without the other.
    // The command is set per state — the bar is a different button in each.
    const pendingBar = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 99);
    context.subscriptions.push(pendingBar);
    /**
     * Which session the review acts on — the one whose diff was opened last.
     *
     * Deliberately NOT `core.activeSession()`, which answers a different question:
     * where the operator is working. Comments go to the agent whose diff they were
     * written on, so conflating the two would deliver a review about one session's code
     * to another session. Hence the name — this is the review's target, not the
     * editor's focus.
     */
    let reviewTarget;
    /**
     * Two states, in priority order: unsent comments first, then "this session has
     * changes nobody has looked at".
     *
     * Comments win because they are the state with a deadline — a review written and
     * never sent helps nobody, while a file count is not going anywhere. Showing both
     * at once would need a second permanent fixture in the status bar, which is the
     * verbosity the phase indicator was made iconic to escape.
     */
    const refreshPending = async () => {
        const core = await withCore();
        const { pending, resolved } = await reviewCounts();
        if (pending > 0) {
            pendingBar.text = `$(comment-discussion) Send review (${pending})`;
            pendingBar.command = 'moonlight.review.submit';
            pendingBar.tooltip = [
                `${pending} comment(s) will be delivered to the session as one message.`,
                // Settled comments are deliberately not in that number, so say where they went
                // — a count that silently excludes things reads as comments having vanished.
                ...(resolved > 0
                    ? [
                        '',
                        `${resolved} resolved comment(s) are not included — they stay in the record`,
                        'and are shown collapsed in the diff.',
                    ]
                    : []),
            ].join('\n');
            pendingBar.show();
            return;
        }
        // Only the session the operator is working in. Another session's unreviewed files
        // are not a call to action here — they belong to whoever has that session in front
        // of them, and the review queue command still lists the whole fleet.
        const active = core?.activeSession();
        const mine = active && core?.sessions().find((s) => s.session_id === active.sessionId);
        const changed = mine?.unreviewed_files ?? 0;
        if (!mine || changed === 0) {
            pendingBar.hide();
            return;
        }
        pendingBar.text = `$(diff) ${changed} changed`;
        // Carries the session the count is about. Without it the click lands on a picker
        // asking which session to review, having just been told.
        pendingBar.command = {
            command: 'moonlight.review.openSession',
            title: 'Review changes',
            arguments: [mine.session_id],
        };
        pendingBar.tooltip = [
            `"${mine.title ?? mine.session_id}" has written ${changed} file(s) nobody has reviewed.`,
            '',
            'Click to open the diffs.',
        ].join('\n');
        pendingBar.show();
    };
    /**
     * How the open review stands: what is queued to send, and what is already settled.
     *
     * Counted together because the pair is the answer to "where am I in this review" —
     * `3 queued` alone cannot say whether the other eleven comments were dealt with or
     * never existed.
     */
    const reviewCounts = async () => {
        if (!reviewTarget) {
            return { pending: 0, resolved: 0 };
        }
        try {
            const all = await controlApi.comments(reviewTarget);
            return {
                pending: all.filter((c) => !c.sent && !c.resolved).length,
                resolved: all.filter((c) => c.resolved).length,
            };
        }
        catch {
            return { pending: 0, resolved: 0 };
        }
    };
    context.subscriptions.push(vscode.commands.registerCommand('moonlight.review.addComment', (reply) => reviewComments.create(reply)), vscode.commands.registerCommand('moonlight.review.resolveComment', (thread) => reviewComments.setResolved(thread, true)), vscode.commands.registerCommand('moonlight.review.unresolveComment', (thread) => reviewComments.setResolved(thread, false)), vscode.commands.registerCommand('moonlight.review.editComment', (thread) => reviewComments.edit(thread)), vscode.commands.registerCommand('moonlight.review.deleteComment', (thread) => reviewComments.remove(thread)), 
    /**
     * Show or hide settled threads.
     *
     * A resolved comment is the record of why the code looks the way it does, so it
     * is kept and shown by default — but on a long review it is noise between the
     * things still open. The desktop panel has had this since it shipped; the editor
     * showed resolved and unresolved threads with nothing to tell them apart.
     */
    vscode.commands.registerCommand('moonlight.review.toggleResolved', async () => {
        const showing = await reviewComments.toggleResolved();
        void vscode.window.showInformationMessage(showing ? 'Showing resolved threads.' : 'Hiding resolved threads.');
    }), 
    /**
     * Comment on the selected lines.
     *
     * Delegates to VSCode's own `workbench.action.addComment`, which takes the
     * editor selection and hands it straight to the comment controller. The gutter
     * `+` is unreliable for this: a plain click only honours a selection when you hit
     * the glyph itself *and* the clicked line is inside it, and dragging the gutter
     * is undiscoverable. Going through the command means the range is the selection,
     * full stop.
     */
    vscode.commands.registerCommand('moonlight.review.commentOnSelection', () => vscode.commands.executeCommand('workbench.action.addComment')), 
    /**
     * Comment on the file as a whole — `{ fileComment: true }` makes the thread
     * range-less, which is exactly `CommentScope::File`. Not every remark belongs on
     * a line.
     */
    vscode.commands.registerCommand('moonlight.review.commentOnFile', () => vscode.commands.executeCommand('workbench.action.addComment', { fileComment: true })), vscode.commands.registerCommand('moonlight.review.submit', async () => {
        if (!reviewTarget) {
            void vscode.window.showInformationMessage('Open a file from the review queue first.');
            return;
        }
        const core = await withCore();
        if (!core) {
            return;
        }
        await submitReview(context, reviewTarget, core.terminals);
        await refreshPending();
    }));
    /**
     * The primary review entry point: pick a session, then land straight in its diffs.
     *
     * A review belongs to one session, so the session is chosen first — mixing several
     * sessions' changes into one review would produce comments addressed to whichever
     * agent happened to touch that file.
     *
     * `requested` is that choice already made. The status bar says "3 changed" *about a
     * particular session* — it is computed from the active one — so clicking it and
     * being asked which session to review discards an answer the operator just gave and
     * makes them give it again. Passed explicitly rather than read here, because the
     * command is also on the palette, where nothing has been chosen yet.
     *
     * A requested session with nothing in the queue falls through to the normal path
     * instead of opening an empty review: the status bar can be a poll behind, and by
     * the time the click lands those files may already have been reviewed.
     */
    context.subscriptions.push(vscode.commands.registerCommand('moonlight.review.openSession', async (requested) => {
        const core = await withCore();
        if (!core) {
            return;
        }
        let queue;
        try {
            queue = await controlApi.reviewQueue();
        }
        catch (err) {
            void vscode.window.showErrorMessage(err instanceof Error ? err.message : String(err));
            return;
        }
        if (queue.length === 0) {
            void vscode.window.showInformationMessage('Nothing to review — no unreviewed files.');
            return;
        }
        // Group by session, so the picker offers reviews rather than files.
        const bySession = new Map();
        for (const item of queue) {
            const list = bySession.get(item.session_id) ?? [];
            list.push(item);
            bySession.set(item.session_id, list);
        }
        // Asking is the last resort — see `resolveReviewSession` for the order and why.
        let sessionId = (0, target_1.resolveReviewSession)(requested, core.activeSession()?.sessionId, new Set(bySession.keys()));
        if (sessionId === undefined) {
            const picked = await vscode.window.showQuickPick([...bySession.entries()].map(([id, items]) => {
                const known = core.sessions().find((s) => s.session_id === id);
                const working = known && !controlApi.isReviewable(known.status);
                return {
                    // Two sessions can share a title; reviewing the wrong one's diff sends
                    // comments to an agent that never wrote the code.
                    label: known
                        ? controlApi.sessionLabel(known, core.sessions())
                        : (items[0].session_title ?? id),
                    description: `${items.length} file(s)${working ? ` — still working (${known?.status})` : ''}`,
                    detail: id,
                    id,
                };
            }), { placeHolder: 'Which session\u2019s changes do you want to review?' });
            sessionId = picked?.id;
        }
        if (!sessionId) {
            return;
        }
        // Same guard as the per-file path: an agent still writing these files makes the
        // review a moving target.
        const owner = core.sessions().find((s) => s.session_id === sessionId);
        if (owner && !controlApi.isReviewable(owner.status)) {
            const go = 'Review anyway';
            const choice = await vscode.window.showWarningMessage(`"${owner.title ?? owner.session_id}" is still working (${owner.status}). Comments you write may be about code it is replacing.`, go);
            if (choice !== go) {
                return;
            }
        }
        const items = bySession.get(sessionId) ?? [];
        reviewTarget = sessionId;
        await (0, review_1.openSessionReview)(sessionId, items[0].session_title ?? sessionId, items, reviewComments);
        await refreshPending();
    }));
    let panel;
    context.subscriptions.push(vscode.commands.registerCommand('moonlight.review.openQueue', async () => {
        const core = await withCore();
        if (!core) {
            return;
        }
        if (panel) {
            panel.reveal();
            await renderQueue(panel);
            return;
        }
        panel = vscode.window.createWebviewPanel('moonlightReviewQueue', 'MoonlightCode Review Queue', vscode.ViewColumn.Active, { enableScripts: true, retainContextWhenHidden: true });
        const current = panel;
        current.onDidDispose(() => {
            panel = undefined;
        });
        current.webview.onDidReceiveMessage(async (message) => {
            try {
                switch (message.type) {
                    case 'accept':
                        await controlApi.accept(message.sessionId, message.path);
                        break;
                    case 'reject': {
                        // Reject drops the file from the queue and steers the session. Only the
                        // first is guaranteed, so report the second rather than letting the
                        // operator assume their feedback landed.
                        const outcome = await controlApi.reject(message.sessionId, message.path, message.text);
                        if (outcome.feedback.status === 'undeliverable') {
                            void vscode.window.showWarningMessage(`Rejected — but your feedback was NOT delivered to the session: ${outcome.feedback.reason}`);
                        }
                        else if (message.text.trim().length > 0) {
                            void vscode.window.showInformationMessage('Rejected — feedback sent to the session.');
                        }
                        break;
                    }
                    case 'open': {
                        // A session mid-turn is still writing these files: comments written now
                        // would be about code that no longer exists by the time they are read.
                        // Our own detection knows this; Claude Code exposes no idle signal.
                        const owner = core
                            .sessions()
                            .find((x) => x.session_id === message.sessionId);
                        if (owner && !controlApi.isReviewable(owner.status)) {
                            void vscode.window.showWarningMessage(`"${owner.title ?? owner.session_id}" is still working (${owner.status}). Wait until it stops before reviewing — otherwise you are commenting on a moving target.`);
                            return;
                        }
                        reviewTarget = message.sessionId;
                        const item = (await controlApi.reviewQueue()).find((i) => i.session_id === message.sessionId && i.file_path === message.path);
                        if (item) {
                            await (0, review_1.openDiff)(item, reviewComments);
                        }
                        await refreshPending();
                        return;
                    }
                    case 'ignore':
                        await controlApi.setIgnored(message.sessionId, message.path, true);
                        break;
                    case 'refresh':
                        break;
                    default:
                        return;
                }
            }
            catch (err) {
                void vscode.window.showErrorMessage(err instanceof Error ? err.message : String(err));
            }
            await renderQueue(current);
        });
        await renderQueue(current);
    }));
    // The changed-file count rides core's fleet poll, so the bar has to follow it —
    // previously it only moved when the operator did something in this extension.
    // Resolved out of band rather than awaited: an early return here would leave every
    // command above registered but the extension half-activated.
    void (async () => {
        const core = await withCore();
        if (!core) {
            return;
        }
        context.subscriptions.push(core.onDidChange(() => void refreshPending()));
        await refreshPending();
    })();
}
/**
 * Deliver every unsent, unresolved comment as one message.
 *
 * Delivery is reported honestly: the backend delivers when it owns the session's
 * terminal, this extension delivers when *it* does, and otherwise nothing does. A
 * comment is marked delivered only on a path that actually wrote the text — a queued
 * review can be sent again, whereas one wrongly marked delivered is simply lost.
 */
async function submitReview(context, sessionId, terminals) {
    let outcome;
    try {
        outcome = await controlApi.submitReview(sessionId);
    }
    catch (err) {
        void vscode.window.showErrorMessage(err instanceof Error ? err.message : String(err));
        return;
    }
    if (!outcome) {
        void vscode.window.showInformationMessage('Nothing to send — no pending comments.');
        return;
    }
    const n = outcome.comment_count;
    // A one-line pointer, because the review itself is a file: nothing to truncate and
    // no length limit to manage.
    const pointer = outcome.review_path
        ? `Please review the code review at ${outcome.review_path} and address each of the ${n} comment(s).`
        : `A code review with ${n} comment(s) is waiting in MoonlightCode.`;
    if (outcome.feedback.status === 'queued') {
        void vscode.window.showInformationMessage(`Review delivered — ${n} comment(s) as one message.`);
        // Shown even here: writing to a pty is not proof the agent was at its prompt to
        // read it, so the message stays one click from the operator either way.
        (0, delivery_1.showHandoff)(context, pointer, outcome.review_path ?? undefined, { kind: 'terminal', detail: 'the session' }, n);
        return;
    }
    const delivery = await (0, delivery_1.deliver)(terminals, sessionId, pointer, outcome.review_path ?? undefined);
    (0, delivery_1.showHandoff)(context, pointer, outcome.review_path ?? undefined, delivery, n);
    if ((0, delivery_1.isConfirmed)(delivery)) {
        try {
            await controlApi.markDelivered(sessionId, outcome.comment_ids);
        }
        catch {
            void vscode.window.showWarningMessage(`Review sent to ${delivery.detail}, but MoonlightCode could not record it as delivered — it may be offered again.`);
            return;
        }
        void vscode.window.showInformationMessage(`Review delivered — ${n} comment(s) sent to ${delivery.detail}.`);
        return;
    }
    void vscode.window.showWarningMessage(`Review of ${n} comment(s) saved but NOT delivered — it stays queued. See the hand-off panel to pass it on.`);
}
async function renderQueue(panel) {
    let items = [];
    let error;
    try {
        items = await controlApi.reviewQueue();
    }
    catch (err) {
        error = err instanceof Error ? err.message : String(err);
    }
    panel.webview.html = renderHtml(items, error);
}
function esc(value) {
    return value
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;');
}
/**
 * A CSP nonce.
 *
 * `crypto.randomBytes`, not `Math.random()`: a nonce the page's own content could
 * predict is not a nonce, and `Math.random()` is not a CSPRNG. Cheap insurance for the
 * one value standing between a missed escape and script execution.
 */
function cspNonce() {
    return crypto.randomBytes(16).toString('hex');
}
function renderHtml(items, error) {
    const nonce = cspNonce();
    const body = error
        ? `<p class="error">${esc(error)}</p>`
        : items.length === 0
            ? '<p>Nothing pending review.</p>'
            : items.map(renderItem).join('\n');
    return `<!DOCTYPE html>
<html>
<head>
<meta charset="UTF-8" />
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'nonce-${nonce}';" />
<style>
  body { font-family: var(--vscode-font-family); padding: 0 1rem 1rem; }
  .item { border: 1px solid var(--vscode-panel-border); border-radius: 4px; margin: 1rem 0; padding: 0.75rem; }
  .item h3 { margin: 0 0 0.25rem 0; font-size: 1rem; word-break: break-all; }
  .item .meta { color: var(--vscode-descriptionForeground); font-size: 0.85rem; margin-bottom: 0.5rem; }
  pre { background: var(--vscode-textCodeBlock-background); padding: 0.5rem; overflow-x: auto; white-space: pre; }
  button { margin-right: 0.5rem; }
  textarea { width: 100%; box-sizing: border-box; margin-top: 0.5rem; font-family: var(--vscode-font-family); }
  .error { color: var(--vscode-errorForeground); }
  .badge { display: inline-block; padding: 0 0.4rem; border-radius: 3px; font-size: 0.75rem;
           background: var(--vscode-badge-background); color: var(--vscode-badge-foreground); }
  .badge.new { background: var(--vscode-charts-green); color: var(--vscode-editor-background); }
  .badge.warn { background: var(--vscode-inputValidation-warningBackground);
                color: var(--vscode-inputValidation-warningForeground); }
</style>
</head>
<body>
<h2>Review Queue <button id="refresh">Refresh</button></h2>
${body}
<script nonce="${nonce}">
  const vscode = acquireVsCodeApi();
  document.getElementById('refresh').addEventListener('click', () => {
    vscode.postMessage({ type: 'refresh' });
  });
  document.querySelectorAll('[data-accept]').forEach((el) => {
    el.addEventListener('click', () => {
      vscode.postMessage({
        type: 'accept',
        sessionId: el.getAttribute('data-session'),
        path: el.getAttribute('data-path'),
      });
    });
  });
  document.querySelectorAll('[data-open]').forEach((el) => {
    el.addEventListener('click', () => {
      vscode.postMessage({
        type: 'open',
        sessionId: el.getAttribute('data-session'),
        path: el.getAttribute('data-path'),
      });
    });
  });
  document.querySelectorAll('[data-ignore]').forEach((el) => {
    el.addEventListener('click', () => {
      vscode.postMessage({
        type: 'ignore',
        sessionId: el.getAttribute('data-session'),
        path: el.getAttribute('data-path'),
      });
    });
  });
  document.querySelectorAll('[data-reject]').forEach((el) => {
    el.addEventListener('click', () => {
      const root = el.closest('.item');
      const text = root.querySelector('textarea').value;
      vscode.postMessage({
        type: 'reject',
        sessionId: el.getAttribute('data-session'),
        path: el.getAttribute('data-path'),
        text,
      });
    });
  });
</script>
</body>
</html>`;
}
/**
 * Badges for the modification state the tracker records. `from HEAD` matters most:
 * that baseline was inferred from VCS, so the "before" side may include edits that
 * were already uncommitted — reading it as an exact pre-image misleads the reviewer.
 */
function badges(item) {
    const out = [];
    if (item.created) {
        out.push('<span class="badge new">new file</span>');
    }
    if (item.from_head) {
        out.push('<span class="badge warn" title="Baseline inferred from VCS, not an observed pre-image">from HEAD</span>');
    }
    if (item.touches > 0) {
        out.push(`<span class="badge">${item.touches} touch${item.touches === 1 ? '' : 'es'}</span>`);
    }
    if (item.tool) {
        out.push(`<span class="badge">${esc(item.tool)}</span>`);
    }
    return out.join(' ');
}
function renderItem(item) {
    const session = esc(item.session_id);
    const filePath = esc(item.file_path);
    const name = esc(item.file_path.split('/').pop() ?? item.file_path);
    return `<div class="item">
  <h3>${name}</h3>
  <div class="meta">${esc(item.file_path)}</div>
  <div class="meta">${esc(item.session_title ?? item.session_id)} ${badges(item)}</div>
  <div>
    <button data-open data-session="${session}" data-path="${filePath}">Open diff &amp; comment</button>
    <button data-accept data-session="${session}" data-path="${filePath}">Mark reviewed</button>
    <button data-ignore data-session="${session}" data-path="${filePath}">Ignore</button>
  </div>
  <textarea rows="2" placeholder="Reject this file with feedback (sent to the session now)"></textarea>
  <div>
    <button data-reject data-session="${session}" data-path="${filePath}">Reject with feedback</button>
  </div>
</div>`;
}
function deactivate() { }
//# sourceMappingURL=extension.js.map