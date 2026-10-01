/**
 * Claude Code's panel view type. VSCode reports webview tabs with an internal prefix
 * (`mainThreadWebview-…`), so compare with {@link isClaudePanel}, not `===`.
 */
export declare const CLAUDE_PANEL_VIEW_TYPE = "claudeVSCodePanel";
export declare function isClaudePanel(viewType: string): boolean;
/**
 * The tab label Claude Code gives a session title. Mirrors its own rule — over 25
 * characters becomes the first 24 plus an ellipsis — so matching is equality on the
 * label, not a fuzzy prefix test that two similar titles would both pass.
 */
export declare function panelLabelFor(title: string): string;
export interface PanelCandidate {
    session_id: string;
    title: string | null;
    root: string | null;
    status: string;
}
/**
 * The one session whose title produces this tab label, or `undefined`.
 *
 * Ties are narrowed by what makes a session more likely to be the one on screen:
 * first rooted in this window's workspace, then running. A tie that survives both
 * stays unresolved — the operator can still link the panel by hand.
 */
export declare function matchPanelSession(label: string, sessions: readonly PanelCandidate[], folders: readonly string[]): string | undefined;
