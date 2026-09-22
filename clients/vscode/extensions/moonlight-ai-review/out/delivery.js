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
exports.isConfirmed = isConfirmed;
exports.deliver = deliver;
exports.showHandoff = showHandoff;
/**
 * Getting a submitted review to the session.
 *
 * The desktop app does this by writing into the PTY it owns: paste the text, then a
 * carriage return. There is no single equivalent in an editor, because a session can
 * be hosted in several different ways and only some of them expose anything we can
 * write to. So delivery is a **strategy chosen per session**, not one mechanism.
 *
 * Deliberate constraint: **public VSCode API only.** The Claude Code extension is
 * proprietary ("© Anthropic PBC. All rights reserved."), ships as a minified bundle,
 * exposes no extension API, and explicitly refuses to apply a prompt to a session
 * that is already open. Calling its unexported `claude-vscode.*` commands would work
 * today and break on its next release, and the failure would be silent — the
 * operator would believe a review had been delivered when it had not. So this file
 * uses only `window.createTerminal` / `Terminal.sendText`, both documented and
 * stable, and degrades to telling the truth.
 *
 * The honesty rule: a comment is marked delivered **only** on a path that actually
 * wrote the text and pressed return. Everything else says so plainly and leaves the
 * review queued — a queued review can be sent again; a review wrongly marked
 * delivered is simply lost.
 */
const vscode = __importStar(require("vscode"));
const crypto = __importStar(require("crypto"));
/**
 * Whether this delivery actually put the text in front of the agent — a type
 * predicate so the compiler enforces that only a confirmed delivery is treated as
 * one. Marking comments delivered is a claim about reality; narrowing it here means
 * the wrong branch can't reach that call by accident.
 */
function isConfirmed(d) {
    return d.kind === 'terminal';
}
/**
 * Deliver `text` to `sessionId`, returning what actually happened.
 *
 * `text` is expected to be a single line (a pointer to the review file). That is not
 * incidental: `sendText` writes straight to the pty's stdin, so an embedded newline
 * would submit a truncated turn and send the rest as a second one — the hazard the
 * desktop had to defuse with bracketed paste. Keeping the payload one line avoids it
 * rather than working around it.
 */
async function deliver(terminals, sessionId, text, reviewPath) {
    const oneLine = text.replace(/\s*\n\s*/g, ' ').trim();
    const terminal = terminals.get(sessionId);
    if (terminal) {
        // `shouldExecute: true` is the newline — the documented equivalent of the
        // desktop writing "\r" after the paste.
        terminal.sendText(oneLine, true);
        terminal.show();
        return { kind: 'terminal', detail: terminal.name };
    }
    // Nothing we can write to. Put the pointer where the operator can use it in one
    // gesture, and say plainly that the agent has not been told.
    await vscode.env.clipboard.writeText(oneLine);
    return {
        kind: 'manual',
        reason: reviewPath
            ? 'MoonlightCode does not own a terminal for this session, so it could not tell the agent. The pointer is on your clipboard — paste it into the session.'
            : 'MoonlightCode does not own a terminal for this session, so it could not tell the agent.',
    };
}
/**
 * The hand-off panel: the exact message to give the session, ready to copy.
 *
 * Shown after every submit, including a delivery we believe worked. `sendText`
 * writes to the pty, but nothing guarantees the agent was at its prompt to receive
 * it — and the session may not be in a terminal at all. Rather than make the
 * operator reconstruct the message, this keeps it one click away, and states plainly
 * which of those two worlds they are in.
 */
let fallbackPanel;
/**
 * The message the panel is currently showing.
 *
 * Held here rather than captured by the message handler: the handler is registered
 * once, when the panel is created, but the panel is reused for every later submit.
 * A captured value would leave Copy handing back the *first* review's message while
 * the panel displayed the latest one.
 */
let current = {
    pointer: '',
    reviewPath: undefined,
};
function showHandoff(context, pointer, reviewPath, delivery, commentCount) {
    current = { pointer, reviewPath };
    if (!fallbackPanel) {
        fallbackPanel = vscode.window.createWebviewPanel('moonlightcodeReviewHandoff', 'MoonlightCode — Review ready', { viewColumn: vscode.ViewColumn.Beside, preserveFocus: true }, { enableScripts: true });
        fallbackPanel.onDidDispose(() => {
            fallbackPanel = undefined;
        });
        fallbackPanel.webview.onDidReceiveMessage(async (m) => {
            if (m.type === 'open' && current.reviewPath) {
                await vscode.window.showTextDocument(vscode.Uri.file(current.reviewPath));
            }
            if (m.type === 'copy') {
                await vscode.env.clipboard.writeText(current.pointer);
                void vscode.window.showInformationMessage('Review message copied.');
            }
        });
        context.subscriptions.push(fallbackPanel);
    }
    fallbackPanel.webview.html = handoffHtml(pointer, reviewPath, delivery, commentCount);
    fallbackPanel.reveal(vscode.ViewColumn.Beside, true);
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
function handoffHtml(pointer, reviewPath, delivery, commentCount) {
    const nonce = cspNonce();
    const state = isConfirmed(delivery)
        ? `<p class="ok">Sent to <strong>${esc(delivery.detail)}</strong> — ${commentCount} comment(s).</p>`
        : `<p class="warn">Not delivered. ${esc(delivery.reason)}</p>`;
    const fileLink = reviewPath
        ? `<p class="meta">Full review: <a href="#" id="open">${esc(reviewPath)}</a></p>`
        : '';
    return `<!DOCTYPE html>
<html>
<head>
<meta charset="UTF-8" />
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'nonce-${nonce}';" />
<style>
  body { font-family: var(--vscode-font-family); padding: 0 1rem 1rem; font-size: 0.9rem; }
  h2 { font-size: 1rem; margin-bottom: 0.25rem; }
  .ok { color: var(--vscode-charts-green); }
  .warn { color: var(--vscode-inputValidation-warningForeground);
          background: var(--vscode-inputValidation-warningBackground);
          padding: 0.4rem 0.6rem; border-radius: 3px; }
  .meta { color: var(--vscode-descriptionForeground); word-break: break-all; }
  pre { background: var(--vscode-textCodeBlock-background); padding: 0.6rem;
        white-space: pre-wrap; word-break: break-word; user-select: all; }
  a { color: var(--vscode-textLink-foreground); }
</style>
</head>
<body>
<h2>Review ready</h2>
${state}
<p>If the session didn't receive it, paste this message to it:</p>
<pre id="msg">${esc(pointer)}</pre>
<button id="copy">Copy message</button>
${fileLink}
<script nonce="${nonce}">
  const vscode = acquireVsCodeApi();
  document.getElementById('copy').addEventListener('click', () => {
    vscode.postMessage({ type: 'copy' });
  });
  const open = document.getElementById('open');
  if (open) {
    open.addEventListener('click', (e) => {
      e.preventDefault();
      vscode.postMessage({ type: 'open' });
    });
  }
</script>
</body>
</html>`;
}
//# sourceMappingURL=delivery.js.map