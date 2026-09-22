/**
 * Where the operator's custom session groups live, and the commands that edit them.
 *
 * `workspaceState`, matching `moonlight-core`'s session pins — a group is a way of
 * arranging the work in front of you, and the sessions in it are usually the ones this
 * window is about. Moving to `globalState` later is a one-line change here and nothing
 * elsewhere, which is why the storage sits behind this type rather than being read at
 * the call sites.
 */
import * as vscode from 'vscode';
import type { CustomGroup, GroupBy } from './grouping';
/** Reads the grouping mode from settings and the groups from workspace storage. */
export declare class GroupStore {
    private readonly memento;
    private readonly changed;
    /** Fires when a group is created, edited or removed, so the views can refresh. */
    readonly onDidChange: vscode.Event<void>;
    constructor(memento: vscode.Memento);
    /**
     * Read per call rather than cached, so changing the setting takes effect on the next
     * refresh instead of needing a window reload — the same reasoning as the daemon
     * options in core.
     */
    mode(): GroupBy;
    custom(): readonly CustomGroup[];
    private write;
    create(name: string): Promise<CustomGroup>;
    rename(id: string, name: string): Promise<void>;
    remove(id: string): Promise<void>;
    /**
     * Put a session in a group, taking it out of any other.
     *
     * One group per session, deliberately: a session in two groups renders twice, and a
     * status badge appearing in two places reads as two sessions in trouble.
     */
    assign(sessionId: string, groupId: string): Promise<void>;
    unassign(sessionId: string): Promise<void>;
    dispose(): void;
}
