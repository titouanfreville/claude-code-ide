/**
 * MoonlightCode Status — the at-a-glance answer to "is anything actually governed?"
 *
 * This is the fail-open visibility problem the whole client family started from: the
 * hooks can be uninstalled, or the backend can be down, and Claude Code carries on
 * perfectly happily with nothing gating it. Silence looks identical to safety, so
 * this says which one you are in.
 */
import * as vscode from 'vscode';
export declare function activate(context: vscode.ExtensionContext): Promise<void>;
export declare function deactivate(): void;
