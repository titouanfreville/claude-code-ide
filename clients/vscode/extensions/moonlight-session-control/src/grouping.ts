/**
 * How the sidebar's session lists are grouped.
 *
 * Pure and `vscode`-free so the rules can be tested directly — the same reason
 * `feedback.ts` and `target.ts` are shaped this way. The provider renders what this
 * returns and decides nothing itself.
 *
 * **Input must already be triage-sorted.** Group order is then first-appearance order,
 * which puts a group containing the worst session at the top for free. Sorting groups
 * alphabetically instead would hide a blocked session behind a collapsed row, which is
 * the opposite of what the triage order exists for.
 */
import * as path from 'path';

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
export type Grouping =
  | { kind: 'flat'; sessions: DiscoverableSession[] }
  | { kind: 'grouped'; groups: SessionGroup[] };

/** Sessions detection has found but has no path for yet. Always last. */
const NO_PROJECT = 'no-project';

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
export function effectiveMode(scope: 'governed' | 'unadopted', mode: GroupBy): GroupBy {
  return scope === 'unadopted' && mode === 'custom' ? 'project' : mode;
}

export function groupSessions(
  sessions: readonly DiscoverableSession[],
  mode: GroupBy,
  custom: readonly CustomGroup[] = []
): Grouping {
  if (mode === 'none' || sessions.length === 0) {
    return { kind: 'flat', sessions: [...sessions] };
  }

  const groups: SessionGroup[] = [];
  let remaining = [...sessions];

  if (mode === 'custom') {
    for (const group of custom) {
      // Membership is checked against the live list: a group keeps ids of sessions
      // that have since ended, and an empty group is worth rendering (it is somewhere
      // to drop things) but must not claim sessions that no longer exist.
      const members = remaining.filter((s) => group.sessionIds.includes(s.session_id));
      groups.push({
        key: `custom:${group.id}`,
        label: group.name,
        tooltip: undefined,
        sessions: members,
        custom: true,
      });
      remaining = remaining.filter((s) => !group.sessionIds.includes(s.session_id));
    }
  }

  // Everything not in a custom group still groups by project, so turning custom
  // grouping on does not empty the tree until you have filled it in.
  groups.push(...projectGroups(remaining));

  // One group and nothing custom is the flat case. A custom group is kept even when
  // it is the only one — the operator made it, and hiding it would read as it having
  // been lost.
  if (groups.length <= 1 && !groups.some((g) => g.custom)) {
    return { kind: 'flat', sessions: [...sessions] };
  }
  return { kind: 'grouped', groups };
}

/** Group by `root`, labelled by basename, disambiguated only where it collides. */
function projectGroups(sessions: readonly DiscoverableSession[]): SessionGroup[] {
  const byRoot = new Map<string, DiscoverableSession[]>();
  for (const session of sessions) {
    const root = session.root ?? NO_PROJECT;
    byRoot.set(root, [...(byRoot.get(root) ?? []), session]);
  }

  const roots = [...byRoot.keys()].filter((r) => r !== NO_PROJECT);
  const groups = roots.map((root) => ({
    key: `project:${root}`,
    label: projectLabel(root, roots),
    tooltip: root,
    sessions: byRoot.get(root) ?? [],
    custom: false,
  }));

  const orphans = byRoot.get(NO_PROJECT);
  if (orphans) {
    // Last, always: "no project" is the absence of the thing being grouped on, and
    // floating it up on triage order would put the least identifiable rows first.
    groups.push({
      key: `project:${NO_PROJECT}`,
      label: 'No project',
      tooltip: 'Sessions with no known root',
      sessions: orphans,
      custom: false,
    });
  }
  return groups;
}

/**
 * A readable name for a project root.
 *
 * The basename alone, until two roots share one — `web` and `web` tells you nothing
 * about which is which. Then the parent directory is prepended, the same escalation
 * `sessionLabel` makes for colliding session titles, and for the same reason: a
 * qualifier on every row is noise that trains you to stop reading the row.
 */
export function projectLabel(root: string, among: readonly string[]): string {
  const base = path.basename(root) || root;
  const collides = among.some((other) => other !== root && path.basename(other) === base);
  if (!collides) {
    return base;
  }
  const parent = path.basename(path.dirname(root));
  return parent ? `${parent}/${base}` : root;
}
