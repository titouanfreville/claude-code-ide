import type { DiscoverableSession } from 'moonlight-control-client';
/** How the operator wants the list arranged. */
export type GroupBy = 'project' | 'custom' | 'none';
/** A group the operator made, stored between sessions. */
export interface CustomGroup {
    id: string;
    name: string;
    sessionIds: string[];
}
/** One rendered group. `key` is stable across refreshes so expansion state survives. */
export interface SessionGroup {
    key: string;
    label: string;
    /** The full path, for a project group whose label is only a basename. */
    tooltip: string | undefined;
    sessions: DiscoverableSession[];
    /** Custom groups get rename/delete actions; project groups do not. */
    custom: boolean;
}
/**
 * What the tree should show.
 *
 * `flat` is a real outcome, not an empty case: one group containing the whole list is
 * a row that costs a line and tells you nothing, so a single group renders as no
 * grouping at all.
 */
export type Grouping = {
    kind: 'flat';
    sessions: DiscoverableSession[];
} | {
    kind: 'grouped';
    groups: SessionGroup[];
};
/**
 * The grouping a view may actually use.
 *
 * Custom groups are for **adopted** sessions only. An unadopted session is one the
 * operator has not decided about yet — it is in the list to be triaged and adopted,
 * and filing it into a hand-made group is arranging work that is not yet yours. The
 * practical version of the same point: those sessions come and go as detection finds
 * them, so a group of them is mostly stale ids, and a session that is later adopted
 * would move lists and silently leave the group anyway.
 *
 * So the not-adopted view falls back to project grouping, which is the question you do
 * ask of an unadopted session — "what is this, and is it mine?"
 */
export declare function effectiveMode(scope: 'governed' | 'unadopted', mode: GroupBy): GroupBy;
export declare function groupSessions(sessions: readonly DiscoverableSession[], mode: GroupBy, custom?: readonly CustomGroup[]): Grouping;
/**
 * A readable name for a project root.
 *
 * The basename alone, until two roots share one — `web` and `web` tells you nothing
 * about which is which. Then the parent directory is prepended, the same escalation
 * `sessionLabel` makes for colliding session titles, and for the same reason: a
 * qualifier on every row is noise that trains you to stop reading the row.
 */
export declare function projectLabel(root: string, among: readonly string[]): string;
