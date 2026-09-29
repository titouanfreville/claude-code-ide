/**
 * Which window should act on a session unprompted.
 *
 * Holds are fleet-wide: every open VSCode window polls the same daemon and sees the
 * same held approvals. Acting on all of them means a plan panel opening in windows
 * that have nothing to do with the session, and the operator answering in one while
 * the rest sit on a panel about a hold that is already settled.
 *
 * No `vscode` import — the containment rule is the part that is easy to get subtly
 * wrong, so it is a pure function with tests rather than a condition inside a callback.
 */

/**
 * Whether `root` is inside `folder`, by path segments.
 *
 * Segment-wise rather than `startsWith`, which claims `/work/app-old` for a window
 * open on `/work/app`. Equal paths count: a window opened directly on the session's
 * own root is the clearest possible owner.
 */
export function isInside(root: string, folder: string): boolean {
  const norm = (p: string) => p.replace(/[/\\]+$/, '').split(/[/\\]/).filter(Boolean);
  const r = norm(root);
  const f = norm(folder);
  if (f.length > r.length) {
    return false;
  }
  return f.every((segment, i) => segment === r[i]);
}

/**
 * Whether this window should act on the session on its own.
 *
 * `activeSessionId` wins outright: the operator saying "this is the session I am
 * working in" is a stronger claim than any path match, and it is what makes a session
 * with no root — or one outside every open folder — still reachable from the window
 * that adopted it.
 *
 * Best-effort by construction. Two windows open on the same folder both answer true
 * and cannot see each other; anything needing exactly-once has to be arbitrated by the
 * daemon, which is the only party that sees the whole fleet.
 */
export function windowOwnsSession(
  sessionId: string,
  activeSessionId: string | undefined,
  sessionRoot: string | undefined,
  workspaceFolders: readonly string[]
): boolean {
  if (activeSessionId === sessionId) {
    return true;
  }
  if (!sessionRoot) {
    return false;
  }
  return workspaceFolders.some((folder) => isInside(sessionRoot, folder));
}
