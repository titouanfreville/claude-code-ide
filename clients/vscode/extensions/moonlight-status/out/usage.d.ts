/**
 * The Claude usage readout — how much of your allowance you have spent, how long
 * until it refills, and how full the current session's context is.
 *
 * These are the numbers that decide whether you can keep working, and every one of
 * them is normally invisible: the 5-hour and weekly windows live behind an API call,
 * and context occupancy is buried in Claude Code's own status line. Running out of
 * either mid-task is the failure this exists to prevent, so the bar shows the
 * figures continuously rather than answering after the fact.
 *
 * The one rule this file follows everywhere: an unknown figure renders as `—`, never
 * as 0. A usage meter that reads zero when it simply could not fetch is worse than
 * no meter at all, because zero is read as headroom.
 */
import * as vscode from 'vscode';
import type { UsageResponse } from 'moonlight-control-client';
/**
 * Own the usage status-bar item: render it from each poll, and expose a refresh
 * command so the operator can force a read instead of waiting out the poll interval.
 */
export declare function registerUsageItem(context: vscode.ExtensionContext, read: () => {
    usage: UsageResponse | undefined;
    sessionId: string | undefined;
}, refresh: () => Promise<void>, backendError: () => string | undefined): () => void;
