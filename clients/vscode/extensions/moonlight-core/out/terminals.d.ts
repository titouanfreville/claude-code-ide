import * as vscode from 'vscode';
import type { OwnedTerminals } from './api';
/**
 * Terminals this extension family started, by session id.
 *
 * Only terminals we created are here. We could try to *find* the terminal hosting a
 * session we didn't start — `window.terminals` exposes every terminal and
 * `Terminal.processId` gives its pid — but matching a pid to a session means walking
 * the process tree for a `claude` process carrying that id, and that fails for a
 * session in an external terminal, a multiplexer, or another window. A wrong match
 * would type a review into an unrelated shell, so this only claims what it knows.
 */
export declare class SessionTerminals implements OwnedTerminals {
    private readonly owned;
    constructor(context: vscode.ExtensionContext);
    start(sessionId: string, cwd: string | undefined): vscode.Terminal;
    /**
     * Type the launch command, with this session's `moonlight` MCP server wired in.
     *
     * The endpoint has to be bound *before* launching, because its URL goes into the
     * launch arguments — and it cannot be bound in advance, since it belongs to a
     * session id we mint here. A session launched without it is gated by the hooks but
     * has no `moonlight` verbs: no `present_plan`, no `request_phase`, no `phase_status`,
     * no `report_blocked`. From the operator's side that looks like an agent that cannot
     * get out of `Plan` and will not say why.
     *
     * Best-effort, like the desktop app: a backend too old to bind one (or built without
     * the MCP transport) still gets a session, with a warning that says what is missing.
     * Refusing to start would be a worse trade.
     */
    private launch;
    get(sessionId: string): vscode.Terminal | undefined;
    has(sessionId: string): boolean;
    adopt(sessionId: string, terminal: vscode.Terminal): void;
}
