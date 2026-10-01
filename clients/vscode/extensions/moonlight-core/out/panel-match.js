"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.CLAUDE_PANEL_VIEW_TYPE = void 0;
exports.isClaudePanel = isClaudePanel;
exports.panelLabelFor = panelLabelFor;
exports.matchPanelSession = matchPanelSession;
/**
 * Which session an unlinked Claude Code agent panel belongs to, read off its tab.
 *
 * Claude Code exposes no session id to other extensions, but it names each panel's
 * tab after the session title — the same `ai-title` / `custom-title` the daemon reads
 * from the transcript. So the label is a real, if lossy, link: lossy because it is
 * truncated and titles are not unique. Anything short of exactly one match is
 * reported as no match, never picked from.
 */
const ownership_1 = require("./ownership");
/**
 * Claude Code's panel view type. VSCode reports webview tabs with an internal prefix
 * (`mainThreadWebview-…`), so compare with {@link isClaudePanel}, not `===`.
 */
exports.CLAUDE_PANEL_VIEW_TYPE = 'claudeVSCodePanel';
/** What Claude Code labels a panel before the session has a title. */
const UNTITLED_LABEL = 'Claude Code';
function isClaudePanel(viewType) {
    return viewType === exports.CLAUDE_PANEL_VIEW_TYPE || viewType.endsWith(`-${exports.CLAUDE_PANEL_VIEW_TYPE}`);
}
/**
 * The tab label Claude Code gives a session title. Mirrors its own rule — over 25
 * characters becomes the first 24 plus an ellipsis — so matching is equality on the
 * label, not a fuzzy prefix test that two similar titles would both pass.
 */
function panelLabelFor(title) {
    return title.length > 25 ? `${title.substring(0, 24)}…` : title;
}
/**
 * The one session whose title produces this tab label, or `undefined`.
 *
 * Ties are narrowed by what makes a session more likely to be the one on screen:
 * first rooted in this window's workspace, then running. A tie that survives both
 * stays unresolved — the operator can still link the panel by hand.
 */
function matchPanelSession(label, sessions, folders) {
    if (label === UNTITLED_LABEL) {
        return undefined;
    }
    let matches = sessions.filter((s) => s.title && panelLabelFor(s.title) === label);
    for (const narrow of [
        (s) => !!s.root && folders.some((f) => (0, ownership_1.isInside)(s.root, f)),
        (s) => s.status === 'Running',
    ]) {
        if (matches.length <= 1) {
            break;
        }
        const narrowed = matches.filter(narrow);
        if (narrowed.length > 0) {
            matches = narrowed;
        }
    }
    return matches.length === 1 ? matches[0].session_id : undefined;
}
//# sourceMappingURL=panel-match.js.map