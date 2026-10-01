package io.github.titouanfreville.moonlight.sessions

import io.github.titouanfreville.moonlight.client.DiscoverableSession

/**
 * How the sidebar's session lists are grouped — a port of `grouping.ts`.
 *
 * Pure so the rules can be tested directly; the tree renders what this returns and decides
 * nothing itself.
 *
 * **Input must already be triage-sorted.** Group order is then first-appearance order,
 * which puts the group holding the worst session at the top for free. Sorting groups
 * alphabetically instead would hide a blocked session behind a collapsed row.
 */

/** How the operator wants the list arranged. */
enum class GroupBy { Project, Custom, None }

/** Which list a view shows. The two are never mixed. */
enum class Scope { Governed, Unadopted }

/** A group the operator made, stored between sessions. */
data class CustomGroup(val id: String, val name: String, val sessionIds: List<String>)

/** One rendered group. `key` is stable across refreshes so expansion state survives. */
data class SessionGroup(
    val key: String,
    val label: String,
    /** The full path, for a project group whose label is only a basename. */
    val tooltip: String?,
    val sessions: List<DiscoverableSession>,
    /** Custom groups get rename/delete actions; project groups do not. */
    val custom: Boolean,
) {
    /** The stored group's id, for a custom group. */
    val customId: String? get() = if (custom) key.removePrefix(CUSTOM_PREFIX) else null
}

/**
 * What the tree should show. `Flat` is a real outcome, not an empty case: one group holding
 * the whole list is a row that costs a line and tells you nothing.
 */
sealed interface Grouping {
    data class Flat(val sessions: List<DiscoverableSession>) : Grouping
    data class Grouped(val groups: List<SessionGroup>) : Grouping
}

const val CUSTOM_PREFIX = "custom:"
private const val NO_PROJECT = "no-project"

/**
 * The grouping a view may actually use.
 *
 * Custom groups are for **adopted** sessions only. An unadopted one is still being
 * triaged, it comes and goes as detection finds it, and it changes lists the moment it is
 * adopted — so the not-adopted view falls back to project grouping, which is the question
 * you do ask of an unadopted session: "what is this, and is it mine?"
 */
fun effectiveMode(scope: Scope, mode: GroupBy): GroupBy =
    if (scope == Scope.Unadopted && mode == GroupBy.Custom) GroupBy.Project else mode

fun groupSessions(sessions: List<DiscoverableSession>, mode: GroupBy, custom: List<CustomGroup> = emptyList()): Grouping {
    if (mode == GroupBy.None || sessions.isEmpty()) return Grouping.Flat(sessions)

    val groups = mutableListOf<SessionGroup>()
    var remaining = sessions
    if (mode == GroupBy.Custom) {
        for (group in custom) {
            // Checked against the live list: a group keeps ids of sessions that have ended,
            // and an empty group is worth rendering (somewhere to drop things) but must not
            // claim sessions that no longer exist.
            val members = remaining.filter { it.sessionId in group.sessionIds }
            groups += SessionGroup("$CUSTOM_PREFIX${group.id}", group.name, null, members, custom = true)
            remaining = remaining.filter { it.sessionId !in group.sessionIds }
        }
    }
    // Everything not in a custom group still groups by project, so turning custom grouping
    // on does not empty the tree until you have filled it in.
    groups += projectGroups(remaining)

    // One group and nothing custom is the flat case. A custom group is kept even alone —
    // the operator made it, and hiding it would read as it having been lost.
    if (groups.size <= 1 && groups.none { it.custom }) return Grouping.Flat(sessions)
    return Grouping.Grouped(groups)
}

private fun projectGroups(sessions: List<DiscoverableSession>): List<SessionGroup> {
    val byRoot = LinkedHashMap<String, MutableList<DiscoverableSession>>()
    for (session in sessions) byRoot.getOrPut(session.root ?: NO_PROJECT) { mutableListOf() } += session
    val roots = byRoot.keys.filter { it != NO_PROJECT }
    val groups = roots.map { root -> SessionGroup("project:$root", projectLabel(root, roots), root, byRoot.getValue(root), custom = false) }
    // Last, always: "no project" is the absence of the thing grouped on, and floating it up
    // on triage order would put the least identifiable rows first.
    val orphans = byRoot[NO_PROJECT] ?: return groups
    return groups + SessionGroup("project:$NO_PROJECT", "No project", "Sessions with no known root", orphans, custom = false)
}

/**
 * A readable name for a project root: the basename, until two roots share one — then the
 * parent is prepended, the same escalation `sessionLabel` makes for colliding titles.
 */
fun projectLabel(root: String, among: List<String>): String {
    val base = basename(root).ifEmpty { root }
    val collides = among.any { it != root && basename(it) == base }
    if (!collides) return base
    val parent = basename(dirname(root))
    return if (parent.isNotEmpty()) "$parent/$base" else root
}

private fun basename(path: String): String = path.trimEnd('/', '\\').substringAfterLast('/').substringAfterLast('\\')

/** Everything before the last separator, `/` or `\\` — whichever comes last. */
private fun dirname(path: String): String {
    val trimmed = path.trimEnd('/', '\\')
    val cut = maxOf(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\'))
    return if (cut > 0) trimmed.substring(0, cut) else ""
}
