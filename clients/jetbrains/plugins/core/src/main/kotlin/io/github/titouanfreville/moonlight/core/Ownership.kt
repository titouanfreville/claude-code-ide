package io.github.titouanfreville.moonlight.core

import io.github.titouanfreville.moonlight.client.Os

/**
 * Which project window should act on a session unprompted — a port of `ownership.ts`.
 *
 * Holds are fleet-wide: every open window sees the same held approvals. Acting in all of
 * them means a plan panel opening in windows that have nothing to do with the session.
 * No IntelliJ imports: the containment rule is the part that is easy to get subtly wrong,
 * so it is pure and tested.
 */

/**
 * Whether `root` is inside `folder`, by path segments.
 *
 * Segment-wise rather than `startsWith`, which claims `/work/app-old` for a window open on
 * `/work/app`. Case-folded off Linux: macOS volumes are case-insensitive by default and
 * Windows always is.
 */
fun isInside(root: String, folder: String, os: Os = Os.current()): Boolean {
    val fold: (String) -> String = if (os == Os.Linux) { s -> s } else { s -> s.lowercase() }
    fun norm(p: String) = p.trimEnd('/', '\\').split('/', '\\').filter { it.isNotEmpty() }.map(fold)
    val r = norm(root)
    val f = norm(folder)
    // An empty folder is a missing value, not the filesystem root — `all` on an empty list
    // is vacuously true, and one bad entry would claim the whole fleet.
    if (f.isEmpty() || f.size > r.size) return false
    return f.indices.all { f[it] == r[it] }
}

/**
 * Whether a project window with these folders owns the session.
 *
 * `activeSessionId` wins outright: the operator saying "this is the session I am working
 * in" is a stronger claim than any path match, and it is what makes a session with no
 * root still reachable from the window that adopted it.
 */
fun ownsSession(
    sessionId: String,
    activeSessionId: String?,
    sessionRoot: String?,
    folders: List<String>,
    os: Os = Os.current(),
): Boolean {
    if (activeSessionId == sessionId) return true
    if (sessionRoot == null) return false
    return folders.any { isInside(sessionRoot, it, os) }
}

/**
 * The one window that acts, among those that own the session.
 *
 * This is where JetBrains does better than VS Code: every project window lives in one
 * process, so instead of each window answering "is it mine?" alone — two windows on the
 * same folder both said yes — the IDE picks exactly one. The focused window when it is an
 * owner, since that is where the operator is looking; otherwise the first.
 */
fun <P> pickOwner(owners: List<P>, focused: P?): P? =
    if (focused != null && focused in owners) focused else owners.firstOrNull()
