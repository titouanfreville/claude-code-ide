package io.github.titouanfreville.moonlight.sessions

import io.github.titouanfreville.moonlight.client.DiscoverableSession
import io.github.titouanfreville.moonlight.client.Phase
import io.github.titouanfreville.moonlight.client.SessionStatus
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** Ported from `grouping.test.ts`. */
class GroupingTest {
    private fun session(id: String, root: String?) = DiscoverableSession(id, id, root, true, SessionStatus.Idle, Phase.Plan, 0)

    private fun labels(g: Grouping): List<String> = (g as? Grouping.Grouped)?.groups?.map { it.label } ?: emptyList()

    @Test
    fun `sessions in different repos group by project`() {
        val g = groupSessions(listOf(session("a", "/work/alpha"), session("b", "/work/beta")), GroupBy.Project)
        assertTrue(g is Grouping.Grouped)
        assertEquals(listOf("alpha", "beta"), labels(g))
    }

    /** One header over the whole list costs a row and says nothing. */
    @Test
    fun `a single group renders flat instead`() {
        assertTrue(groupSessions(listOf(session("a", "/work/alpha"), session("b", "/work/alpha")), GroupBy.Project) is Grouping.Flat)
    }

    @Test
    fun `grouping off is always flat`() {
        assertTrue(groupSessions(listOf(session("a", "/x"), session("b", "/y")), GroupBy.None) is Grouping.Flat)
    }

    /** The caller sorts worst-first, so the group holding the session that needs you comes first. */
    @Test
    fun `group order follows the triage order of the input`() {
        assertEquals(listOf("beta", "alpha"), labels(groupSessions(listOf(session("needs-you", "/work/beta"), session("idle", "/work/alpha")), GroupBy.Project)))
    }

    @Test
    fun `sessions with no root land in a bucket that sorts last`() {
        assertEquals(listOf("alpha", "No project"), labels(groupSessions(listOf(session("orphan", null), session("a", "/work/alpha")), GroupBy.Project)))
    }

    @Test
    fun `colliding basenames are disambiguated by parent, others are not`() {
        val roots = listOf("/one/web", "/two/web", "/solo/api")
        assertEquals("one/web", projectLabel("/one/web", roots))
        assertEquals("two/web", projectLabel("/two/web", roots))
        assertEquals("api", projectLabel("/solo/api", roots))
    }

    @Test
    fun `a custom group takes its members, the rest still group by project`() {
        val custom = listOf(CustomGroup("g1", "Release work", listOf("a")))
        assertEquals(listOf("Release work", "beta"), labels(groupSessions(listOf(session("a", "/work/alpha"), session("b", "/work/beta")), GroupBy.Custom, custom)))
    }

    @Test
    fun `a custom group survives being the only group`() {
        assertEquals(listOf("Mine"), labels(groupSessions(listOf(session("a", "/work/alpha")), GroupBy.Custom, listOf(CustomGroup("g1", "Mine", listOf("a"))))))
    }

    @Test
    fun `a custom group keeps ids of sessions that have ended without claiming them`() {
        val g = groupSessions(listOf(session("a", "/work/alpha")), GroupBy.Custom, listOf(CustomGroup("g1", "Mine", listOf("gone", "a")))) as Grouping.Grouped
        assertEquals(listOf("a"), g.groups[0].sessions.map { it.sessionId })
    }

    /** It is somewhere to drop things; hiding it reads as the group having been lost. */
    @Test
    fun `an empty custom group is still rendered`() {
        assertEquals(listOf("Empty", "alpha"), labels(groupSessions(listOf(session("a", "/work/alpha")), GroupBy.Custom, listOf(CustomGroup("g1", "Empty", emptyList())))))
    }

    @Test
    fun `no sessions is flat, not an empty group`() {
        assertEquals(Grouping.Flat(emptyList()), groupSessions(emptyList(), GroupBy.Project))
    }

    @Test
    fun `group keys are stable across calls so expansion state survives a refresh`() {
        val input = listOf(session("a", "/work/alpha"), session("b", "/work/beta"))
        val first = groupSessions(input, GroupBy.Project) as Grouping.Grouped
        val second = groupSessions(input, GroupBy.Project) as Grouping.Grouped
        assertEquals(first.groups.map { it.key }, second.groups.map { it.key })
    }

    @Test
    fun `a custom group exposes its stored id, a project group has none`() {
        val g = groupSessions(listOf(session("a", "/work/alpha"), session("b", "/work/beta")), GroupBy.Custom, listOf(CustomGroup("g1", "Mine", listOf("a")))) as Grouping.Grouped
        assertEquals(listOf("g1", null), g.groups.map { it.customId })
    }

    @Test
    fun `the not-adopted view falls back to project grouping`() {
        assertEquals(GroupBy.Project, effectiveMode(Scope.Unadopted, GroupBy.Custom))
    }

    @Test
    fun `every other mode is left alone in both views`() {
        for (scope in Scope.values()) {
            assertEquals(GroupBy.Project, effectiveMode(scope, GroupBy.Project))
            assertEquals(GroupBy.None, effectiveMode(scope, GroupBy.None))
        }
        assertEquals(GroupBy.Custom, effectiveMode(Scope.Governed, GroupBy.Custom))
    }
}
