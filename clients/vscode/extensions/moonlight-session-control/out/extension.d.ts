/**
 * MoonlightCode Session Control — which sessions are governed, and under what phase.
 *
 * Three operator actions, each addressing something the editor otherwise cannot do:
 *
 * - **Adopt.** Nothing in VSCode offers this, and adoption is what first lets the
 *   gate deny a tool call. An unadopted session is always allowed.
 * - **Set phase.** A freshly adopted session sits in `Plan`, where project writes
 *   are denied. The desktop cockpit has a phase stepper; without an equivalent here,
 *   adopting from the editor would freeze a session with no way out.
 * - **Start a governed session.** A session we launch has a pinned id and a terminal
 *   we own, which is what makes review delivery reliable instead of best-effort.
 */
import * as vscode from 'vscode';
export declare function activate(context: vscode.ExtensionContext): void;
export declare function deactivate(): void;
