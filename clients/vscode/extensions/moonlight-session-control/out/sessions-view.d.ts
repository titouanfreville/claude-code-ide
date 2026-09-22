/**
 * The MoonlightCode sidebar: every session, what it is doing, and what you can do
 * about it.
 *
 * ## Governed and ungoverned are two lists, not two groups
 *
 * The first cut put both in one tree under collapsible headers. That buries the only
 * distinction that really matters — a governed session can be *stopped*, an
 * ungoverned one cannot, and the gate allows it everything — under a header you can
 * collapse, in rows that otherwise look identical.
 *
 * So they are two separate views in the container. A view header is the strongest
 * separator a sidebar has: it is always visible, carries its own count, its own
 * actions and its own empty state, and no amount of scrolling merges the two lists.
 * The rows reinforce it — the icon vocabularies are deliberately disjoint, so which
 * list a row belongs to is legible from the icon column alone.
 *
 * ## What the tree API gives you
 *
 * A row is one line: `[icon] label   description   [badge]`. No second line, no
 * custom layout, no colour on the label. Four slots, more facts than slots, so each
 * slot carries exactly one thing and everything else goes to the tooltip.
 */
import * as vscode from 'vscode';
import * as controlApi from 'moonlight-control-client';
import type { DiscoverableSession } from 'moonlight-control-client';
import type { HeldApproval, MoonlightApi } from 'moonlight-core';
import { type CustomGroup, type GroupBy, type SessionGroup } from './grouping';
/** Phases in workflow order — the order the stepper walks, not alphabetical. */
export declare const PHASES: readonly controlApi.Phase[];
/** Which list a view shows. The two are never mixed. */
export type Scope = 'governed' | 'unadopted';
export declare function sessionUri(sessionId: string): vscode.Uri;
interface SessionNode {
    kind: 'session';
    session: DiscoverableSession;
    held: HeldApproval | undefined;
    following: boolean;
    among: readonly DiscoverableSession[];
    /** The custom group this row sits in, so "Remove from group" knows which. */
    groupId: string | undefined;
}
interface GroupNode {
    kind: 'group';
    group: SessionGroup;
}
export type Node = GroupNode | SessionNode;
/**
 * Where the provider reads the operator's grouping choices from.
 *
 * Injected rather than read here so the provider keeps knowing nothing about settings
 * or storage — and so both views share one source of truth instead of each reading
 * the config on its own tick and briefly disagreeing.
 */
export interface GroupingSource {
    mode(): GroupBy;
    custom(): readonly CustomGroup[];
}
/**
 * One list. Two instances exist, one per scope — same rows, same rules, different
 * halves of the fleet.
 */
export declare class SessionsProvider implements vscode.TreeDataProvider<Node> {
    private readonly core;
    private readonly scope;
    private readonly grouping;
    private readonly changed;
    readonly onDidChangeTreeData: vscode.Event<Node | undefined>;
    constructor(core: MoonlightApi, scope: Scope, grouping: GroupingSource);
    refresh(): void;
    /** The sessions this view is responsible for, worst-first. */
    sessions(): DiscoverableSession[];
    getTreeItem(node: Node): vscode.TreeItem;
    /**
     * A group header: name, how many sessions, and nothing else.
     *
     * Expanded by default — a collapsed group hides exactly the status badges the list
     * exists to surface — and `id` is the group's stable key, because the view refreshes
     * on the backend poll and an id that changed per render would re-collapse the tree
     * under the operator every few seconds.
     */
    private groupItem;
    getChildren(node?: Node): Node[];
    /** Session rows. `among` stays the whole scope so labels disambiguate across groups. */
    private rows;
}
/**
 * The badge at the end of a row — the only slot in a tree that carries colour.
 *
 * One glyph, and a strict priority, because there is one badge and three things that
 * might want it: a hold beats an unreviewed count beats the resting status. That
 * order is "what would make you click this row", which is the only ranking a
 * glanceable badge can usefully encode.
 */
export declare class SessionDecorations implements vscode.FileDecorationProvider {
    private readonly core;
    private readonly changed;
    readonly onDidChangeFileDecorations: vscode.Event<vscode.Uri[] | undefined>;
    constructor(core: MoonlightApi);
    refresh(): void;
    provideFileDecoration(uri: vscode.Uri): vscode.FileDecoration | undefined;
}
/** The session a tree command was invoked on, or `undefined` when it came from the palette. */
export declare function nodeSession(node: unknown): DiscoverableSession | undefined;
export {};
