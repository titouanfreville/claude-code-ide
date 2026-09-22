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
import * as vscode from 'vscode';
export declare function activate(context: vscode.ExtensionContext): void;
export declare function deactivate(): void;
