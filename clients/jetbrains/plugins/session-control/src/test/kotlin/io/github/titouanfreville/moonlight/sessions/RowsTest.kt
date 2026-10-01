package io.github.titouanfreville.moonlight.sessions

import io.github.titouanfreville.moonlight.client.DiscoverableSession
import io.github.titouanfreville.moonlight.client.Phase
import io.github.titouanfreville.moonlight.client.SessionStatus
import io.github.titouanfreville.moonlight.core.HeldApproval
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

class RowsTest {
    private fun session(id: String, adopted: Boolean = true, status: SessionStatus = SessionStatus.Idle, phase: Phase = Phase.AutoImplement, unreviewed: Int = 0) =
        DiscoverableSession(id, id, "/w", adopted, status, phase, unreviewed)

    private val hold = HeldApproval("s", "run `rm`", null, null, 0)

    @Test
    fun `lists hold one half of the fleet each, worst first`() {
        val all = listOf(session("idle"), session("waiting", status = SessionStatus.WaitingInput), session("free", adopted = false))
        assertEquals(listOf("waiting", "idle"), sessionsFor(Scope.Governed, all).map { it.sessionId })
        assertEquals(listOf("free"), sessionsFor(Scope.Unadopted, all).map { it.sessionId })
    }

    /** "Plan" does not look like "cannot write files" to anyone who has not read the docs. */
    @Test
    fun `the description says what is true, in words`() {
        assertEquals("not gated · working", describe(session("s", adopted = false, status = SessionStatus.Running), null))
        assertEquals("Plan · writes denied", describe(session("s", phase = Phase.Plan), null))
        assertEquals("AutoImplement · idle", describe(session("s"), null))
        assertEquals("WAITING ON YOU", describe(session("s"), hold))
        assertEquals("PLAN WAITING ON YOU", describe(session("s"), hold.copy(plan = "# p")))
    }

    /** One badge, three contenders: hold > unreviewed count > status. */
    @Test
    fun `the badge goes to what would make you click`() {
        assertEquals(Badge("!", BadgeTone.Warning), badge(session("s", unreviewed = 3), held = true))
        assertEquals(Badge("3", BadgeTone.Info), badge(session("s", unreviewed = 3), held = false))
        assertEquals(Badge("99+", BadgeTone.Info), badge(session("s", unreviewed = 150), held = false))
        assertEquals(Badge("✕", BadgeTone.Error), badge(session("s", status = SessionStatus.Errored), held = false))
        assertEquals(Badge("●", BadgeTone.Muted), badge(session("s", adopted = false, status = SessionStatus.Running), held = false))
    }
}
