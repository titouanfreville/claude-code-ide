/**
 * The plan gate as a webview.
 *
 * A webview rather than a quick-pick because the comments are anchored: each note
 * belongs to a section of the plan, and the agent is told which. A flat input box
 * would collect the same words and lose the one thing that makes them actionable.
 *
 * The panel renders the plan; the verdict buttons are live only while a hold is
 * actually outstanding, and the reason is worth repeating: `PlanProposed` also fires
 * from transcript detection for sessions that finished hours ago, so a plan on screen
 * is not evidence that anything is waiting. Only `ApprovalRequested` — surfaced here
 * as core's held-approval state — arms a verdict.
 */
import * as vscode from 'vscode';
import * as crypto from 'crypto';

import type { MoonlightApi } from 'moonlight-core';

import { decidePlan } from './gate';
import { blockTitle, planBlocks, type PlanVerdict } from './feedback';
import { escapeHtml, renderMarkdown } from './markdown';

const VIEW_TYPE = 'moonlight.planReview';

/** Messages the webview sends back. */
type InboundMessage =
  | { type: 'comment'; index: number; body: string }
  | { type: 'editing'; open: boolean }
  | { type: 'decide'; verdict: PlanVerdict };

export class PlanReviewPanel {
  private static current: PlanReviewPanel | undefined;

  private readonly disposables: vscode.Disposable[] = [];
  private comments = new Map<number, string>();
  private sessionId: string | undefined;
  private plan = '';
  /** How many comment editors are open — a refresh while one is would discard it. */
  private editing = 0;
  /** A refresh that arrived while the operator was typing, owed once they stop. */
  private renderPending = false;

  static show(core: MoonlightApi, sessionId: string | undefined): void {
    if (PlanReviewPanel.current) {
      PlanReviewPanel.current.panel.reveal(vscode.ViewColumn.Active);
      PlanReviewPanel.current.bind(sessionId);
      return;
    }
    const panel = vscode.window.createWebviewPanel(
      VIEW_TYPE,
      'Plan review',
      vscode.ViewColumn.Active,
      { enableScripts: true, retainContextWhenHidden: true }
    );
    PlanReviewPanel.current = new PlanReviewPanel(panel, core);
    PlanReviewPanel.current.bind(sessionId);
  }

  private constructor(
    private readonly panel: vscode.WebviewPanel,
    private readonly core: MoonlightApi
  ) {
    this.disposables.push(
      panel.onDidDispose(() => this.dispose()),
      panel.webview.onDidReceiveMessage((message: InboundMessage) => {
        void this.onMessage(message);
      })
    );
    if (core.onDidChangeHeldApprovals) {
      // Not while a comment is open. `render()` reassigns `webview.html`, which rebuilds
      // the DOM — and a comment only reaches the extension on `blur`, so a hold change
      // anywhere in the fleet used to wipe a half-written note without warning. The
      // refresh is replayed once the operator finishes (see `onMessage`).
      this.disposables.push(
        core.onDidChangeHeldApprovals(() => {
          if (this.editing > 0) {
            this.renderPending = true;
            return;
          }
          this.render();
        })
      );
    }
  }

  /**
   * Point the panel at a session.
   *
   * Passing `undefined` means "whichever session is blocked", which is the common
   * case: an operator opening this is answering the thing that stopped, and asking
   * them which session that is would be asking a question the editor can answer.
   */
  private bind(sessionId: string | undefined): void {
    const held = this.core.heldApprovals?.() ?? [];
    this.sessionId = sessionId ?? held[0]?.sessionId ?? this.core.activeSession()?.sessionId;
    this.comments = new Map();
    this.render();
  }

  private async onMessage(message: InboundMessage): Promise<void> {
    if (message.type === 'editing') {
      this.editing = Math.max(0, this.editing + (message.open ? 1 : -1));
      if (this.editing === 0 && this.renderPending) {
        this.renderPending = false;
        this.render();
      }
      return;
    }
    if (message.type === 'comment') {
      if (message.body.trim().length === 0) {
        this.comments.delete(message.index);
      } else {
        this.comments.set(message.index, message.body.trim());
      }
      // No render here: this arrives on blur, and the `editing` message right behind it
      // replays any refresh that was held back. Rendering now would race that.
      return;
    }

    const sessionId = this.sessionId;
    if (!sessionId) {
      return;
    }
    const outcome = await decidePlan(sessionId, this.plan, this.comments, message.verdict);
    if (outcome.ok) {
      this.comments = new Map();
      void vscode.window.showInformationMessage(outcome.summary);
    } else {
      // The session is still stopped — say so plainly rather than letting the panel
      // fall quiet and look like the verdict went through.
      void vscode.window.showErrorMessage(
        `The verdict did not reach the session (${outcome.error}). It is still waiting.`
      );
    }
    this.render();
  }

  private render(): void {
    const sessionId = this.sessionId;
    const held = sessionId ? this.core.heldApproval?.(sessionId) : undefined;
    this.plan = (sessionId ? this.core.proposedPlan?.(sessionId) : undefined) ?? held?.plan ?? '';
    this.panel.title = held ? 'Plan review — waiting' : 'Plan review';
    this.panel.webview.html = this.html(sessionId, this.plan, held !== undefined);
  }

  private html(sessionId: string | undefined, plan: string, armed: boolean): string {
    const blocks = planBlocks(plan);
    // A CSPRNG: a nonce the page's own content could predict is not a nonce.
    const nonce = crypto.randomBytes(16).toString('hex');

    const status = !sessionId
      ? 'No session selected.'
      : armed
        ? `Session ${escapeHtml(sessionId.slice(0, 8))} is waiting on your verdict.`
        : `Session ${escapeHtml(sessionId.slice(0, 8))} is not currently blocked — this plan is for reading.`;

    const body =
      blocks.length === 0
        ? '<p class="empty">No plan proposed yet.</p>'
        : blocks
            .map((block, index) => {
              const comment = this.comments.get(index) ?? '';
              // The editor opens only for a section being commented on. A plan is
              // mostly read, not annotated: an input under every section turns a
              // document into a form, and the reading is the part that matters.
              const open = comment.length > 0;
              return `
                <section class="${open ? 'block commented' : 'block'}">
                  <div class="md">${renderMarkdown(block)}</div>
                  <div class="note" data-index="${index}">
                    <button class="add-comment"${open ? ' hidden' : ''}
                      aria-label="Comment on ${escapeHtml(blockTitle(block))}">
                      <span class="plus">+</span> Comment
                    </button>
                    <textarea data-index="${index}" rows="3"${open ? '' : ' hidden'}
                      placeholder="Comment on this section…">${escapeHtml(comment)}</textarea>
                  </div>
                </section>`;
            })
            .join('');

    // Every verdict is offered whether or not a hold is outstanding, but disabled
    // when there is nothing to answer: a button that silently does nothing is how an
    // operator comes to believe they approved something they did not.
    const verdicts: [PlanVerdict, string][] = [
      ['approve', 'Approve'],
      ['open-question', 'Open question'],
      ['refine', 'Refine'],
      ['no-go', 'No-go'],
    ];
    const buttons = verdicts
      .map(
        ([verdict, label]) =>
          `<button data-verdict="${verdict}"${armed ? '' : ' disabled'}>${label}</button>`
      )
      .join('');

    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta http-equiv="Content-Security-Policy"
    content="default-src 'none'; style-src 'unsafe-inline'; script-src 'nonce-${nonce}';" />
  <style>
    body { font-family: var(--vscode-font-family); color: var(--vscode-foreground);
           padding: 0 1rem 4rem; }
    .status { color: var(--vscode-descriptionForeground); margin: 1rem 0; }
    .block { border-left: 2px solid var(--vscode-panel-border); padding-left: .75rem;
             margin-bottom: 1rem; }
    .block.commented { border-left-color: var(--vscode-focusBorder); }
    .md h3, .md h4, .md h5, .md h6 { margin: .6rem 0 .3rem; line-height: 1.3; }
    .md h3 { font-size: 1.15rem; }
    .md h4 { font-size: 1rem; }
    .md h5, .md h6 { font-size: .92rem; color: var(--vscode-descriptionForeground); }
    .md p { margin: .4rem 0; line-height: 1.5; }
    .md ul, .md ol { margin: .4rem 0; padding-left: 1.4rem; }
    .md li { margin: .2rem 0; line-height: 1.5; }
    .md code { font-family: var(--vscode-editor-font-family); font-size: .9em;
               background: var(--vscode-textCodeBlock-background); padding: .1em .3em;
               border-radius: 3px; }
    .md pre.code { font-family: var(--vscode-editor-font-family); font-size: .9em;
                   background: var(--vscode-textCodeBlock-background); padding: .6rem .8rem;
                   border-radius: 4px; overflow-x: auto; white-space: pre; margin: .5rem 0; }
    .md pre.code code { background: none; padding: 0; }
    .md blockquote { margin: .4rem 0; padding-left: .8rem;
                     border-left: 3px solid var(--vscode-textBlockQuote-border, var(--vscode-panel-border));
                     color: var(--vscode-descriptionForeground); }
    .md a { color: var(--vscode-textLink-foreground); }
    .md hr { border: 0; border-top: 1px solid var(--vscode-panel-border); margin: .8rem 0; }
    .md strong { font-weight: 600; }
    /* A quiet affordance: present on every section, loud on none. */
    .note { margin: .35rem 0 .1rem; }
    .add-comment { background: none; color: var(--vscode-descriptionForeground);
                   border: 1px dashed var(--vscode-panel-border); border-radius: 3px;
                   padding: .15rem .5rem; font-size: .85rem; opacity: .7; }
    .add-comment:hover { opacity: 1; color: var(--vscode-foreground);
                         border-color: var(--vscode-focusBorder); }
    .add-comment .plus { font-weight: 600; }
    [hidden] { display: none !important; }
    textarea { width: 100%; background: var(--vscode-input-background);
               color: var(--vscode-input-foreground); font-family: var(--vscode-font-family);
               border: 1px solid var(--vscode-input-border, transparent); padding: .3rem; }
    .actions { position: sticky; bottom: 0; background: var(--vscode-editor-background);
               padding: .75rem 0; display: flex; gap: .5rem; flex-wrap: wrap; }
    button { background: var(--vscode-button-background); color: var(--vscode-button-foreground);
             border: 0; padding: .4rem .9rem; cursor: pointer; }
    button[disabled] { opacity: .5; cursor: default; }
    .empty { color: var(--vscode-descriptionForeground); }
  </style>
</head>
<body>
  <p class="status">${status}</p>
  ${body}
  <div class="actions">${buttons}</div>
  <script nonce="${nonce}">
    const vscode = acquireVsCodeApi();
    for (const button of document.querySelectorAll('.add-comment')) {
      button.addEventListener('click', () => {
        // Revealed and focused locally, with no round-trip: asking the extension to
        // re-render would rebuild the panel and hand focus back to nothing, so the
        // first thing the operator typed would go nowhere.
        const note = button.closest('.note');
        const area = note.querySelector('textarea');
        button.hidden = true;
        area.hidden = false;
        area.focus();
        vscode.postMessage({ type: 'editing', open: true });
      });
    }
    for (const area of document.querySelectorAll('textarea')) {
      // On blur, not on every keystroke: a re-render on each character would move the
      // caret out from under the operator mid-sentence.
      // A textarea rendered already-open (it has a saved comment) counts as editing
      // from the moment it takes focus, not only from the reveal button.
      area.addEventListener('focus', () => {
        vscode.postMessage({ type: 'editing', open: true });
      });
      area.addEventListener('blur', () => {
        vscode.postMessage({
          type: 'comment',
          index: Number(area.dataset.index),
          body: area.value,
        });
        vscode.postMessage({ type: 'editing', open: false });
      });
    }
    for (const button of document.querySelectorAll('button[data-verdict]')) {
      // mousedown, not click: clicking a verdict blurs an open textarea first, which
      // posts the comment and could rebuild the DOM before the click fired — swallowing
      // the verdict while the operator watched nothing happen.
      button.addEventListener('mousedown', () => {
        vscode.postMessage({ type: 'decide', verdict: button.dataset.verdict });
      });
    }
  </script>
</body>
</html>`;
  }

  private dispose(): void {
    PlanReviewPanel.current = undefined;
    for (const item of this.disposables) {
      item.dispose();
    }
  }
}
