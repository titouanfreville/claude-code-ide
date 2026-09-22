/**
 * MoonlightCode Agentic Support — the plan gate and held-approval surface.
 *
 * A session proposing a plan, or attempting a danger-zone action, *holds* at the gate
 * waiting for a human answer. The desktop cockpit has surfaces for both; until this
 * extension existed an editor had neither, so from VSCode the only way to answer a
 * hold was to open a different application.
 *
 * Why that matters more than a missing feature usually does: the session is stopped
 * for as long as the hold is open. A bounded hold with no answer resolves to a
 * **deny**, by design — the server decides cleanly instead of failing open — and an
 * unbounded one (what the daemon uses when a client is expected) waits forever. Both
 * failure modes look to the operator like an agent that mysteriously stopped working.
 *
 * The state comes from core, which owns the single event-stream connection, so this
 * extension never has to reconcile its own view of what is held.
 */
import * as vscode from 'vscode';
export declare function activate(context: vscode.ExtensionContext): Promise<void>;
export declare function deactivate(): void;
