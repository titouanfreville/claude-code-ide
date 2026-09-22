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
import * as vscode from 'vscode';
import type { OwnedTerminals } from 'moonlight-core';
export type Delivery = {
    kind: 'terminal';
    detail: string;
} | {
    kind: 'manual';
    reason: string;
};
export type ConfirmedDelivery = Extract<Delivery, {
    kind: 'terminal';
}>;
/**
 * Whether this delivery actually put the text in front of the agent — a type
 * predicate so the compiler enforces that only a confirmed delivery is treated as
 * one. Marking comments delivered is a claim about reality; narrowing it here means
 * the wrong branch can't reach that call by accident.
 */
export declare function isConfirmed(d: Delivery): d is ConfirmedDelivery;
/**
 * Deliver `text` to `sessionId`, returning what actually happened.
 *
 * `text` is expected to be a single line (a pointer to the review file). That is not
 * incidental: `sendText` writes straight to the pty's stdin, so an embedded newline
 * would submit a truncated turn and send the rest as a second one — the hazard the
 * desktop had to defuse with bracketed paste. Keeping the payload one line avoids it
 * rather than working around it.
 */
export declare function deliver(terminals: OwnedTerminals, sessionId: string, text: string, reviewPath: string | undefined): Promise<Delivery>;
export declare function showHandoff(context: vscode.ExtensionContext, pointer: string, reviewPath: string | undefined, delivery: Delivery, commentCount: number): void;
