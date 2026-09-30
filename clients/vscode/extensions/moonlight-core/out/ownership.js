"use strict";
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
Object.defineProperty(exports, "__esModule", { value: true });
exports.isInside = isInside;
exports.windowOwnsSession = windowOwnsSession;
/**
 * Whether `root` is inside `folder`, by path segments.
 *
 * Segment-wise rather than `startsWith`, which claims `/work/app-old` for a window
 * open on `/work/app`. Equal paths count: a window opened directly on the session's
 * own root is the clearest possible owner.
 */
function isInside(root, folder, platform = process.platform) {
    // Case-folded off Linux. macOS volumes are case-insensitive by default and Windows
    // always is — and on Windows the drive letter's case genuinely differs between the
    // APIs a root and a workspace folder come from. Comparing exactly there meant a
    // window disowning its own session: no plan panel anywhere, auto-adopt silently
    // adopting nothing.
    const fold = platform === 'linux' ? (s) => s : (s) => s.toLowerCase();
    const norm = (p) => p.replace(/[/\\]+$/, '').split(/[/\\]/).filter(Boolean).map(fold);
    const r = norm(root);
    const f = norm(folder);
    // An empty folder is not the filesystem root, it is a missing value. `every` on an
    // empty list is vacuously true, so without this an empty string claimed every
    // session in the fleet.
    if (f.length === 0 || f.length > r.length) {
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
function windowOwnsSession(sessionId, activeSessionId, sessionRoot, workspaceFolders, platform = process.platform) {
    if (activeSessionId === sessionId) {
        return true;
    }
    if (!sessionRoot) {
        return false;
    }
    return workspaceFolders.some((folder) => isInside(sessionRoot, folder, platform));
}
//# sourceMappingURL=ownership.js.map