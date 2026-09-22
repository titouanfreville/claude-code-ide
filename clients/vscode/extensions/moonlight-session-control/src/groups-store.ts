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

const GROUPS_KEY = 'moonlight.sessionGroups';

/** Reads the grouping mode from settings and the groups from workspace storage. */
export class GroupStore {
  private readonly changed = new vscode.EventEmitter<void>();
  /** Fires when a group is created, edited or removed, so the views can refresh. */
  readonly onDidChange = this.changed.event;

  constructor(private readonly memento: vscode.Memento) {}

  /**
   * Read per call rather than cached, so changing the setting takes effect on the next
   * refresh instead of needing a window reload — the same reasoning as the daemon
   * options in core.
   */
  mode(): GroupBy {
    const value = vscode.workspace
      .getConfiguration('moonlight')
      .get<string>('sessions.groupBy');
    return value === 'custom' || value === 'none' ? value : 'project';
  }

  custom(): readonly CustomGroup[] {
    return this.memento.get<CustomGroup[]>(GROUPS_KEY) ?? [];
  }

  private async write(groups: CustomGroup[]): Promise<void> {
    await this.memento.update(GROUPS_KEY, groups);
    this.changed.fire();
  }

  async create(name: string): Promise<CustomGroup> {
    // Time-based rather than a name slug: two groups may share a name, and a key that
    // changed when a group was renamed would drop the tree's expansion state. The
    // random suffix is what makes it an id — `Date.now()` alone collides for two groups
    // made in the same millisecond, after which rename and delete hit both.
    const id = `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    const group: CustomGroup = { id, name, sessionIds: [] };
    await this.write([...this.custom(), group]);
    return group;
  }

  async rename(id: string, name: string): Promise<void> {
    await this.write(this.custom().map((g) => (g.id === id ? { ...g, name } : g)));
  }

  async remove(id: string): Promise<void> {
    await this.write(this.custom().filter((g) => g.id !== id));
  }

  /**
   * Put a session in a group, taking it out of any other.
   *
   * One group per session, deliberately: a session in two groups renders twice, and a
   * status badge appearing in two places reads as two sessions in trouble.
   */
  async assign(sessionId: string, groupId: string): Promise<void> {
    await this.write(
      this.custom().map((g) => ({
        ...g,
        sessionIds:
          g.id === groupId
            ? [...g.sessionIds.filter((id) => id !== sessionId), sessionId]
            : g.sessionIds.filter((id) => id !== sessionId),
      }))
    );
  }

  async unassign(sessionId: string): Promise<void> {
    await this.write(
      this.custom().map((g) => ({
        ...g,
        sessionIds: g.sessionIds.filter((id) => id !== sessionId),
      }))
    );
  }

  dispose(): void {
    this.changed.dispose();
  }
}
