package io.github.titouanfreville.moonlight.sessions

import io.github.titouanfreville.moonlight.client.DiscoverableSession
import io.github.titouanfreville.moonlight.client.Phase
import io.github.titouanfreville.moonlight.client.SessionStatus
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Test

/** Ported from `auto-adopt.test.ts`. */
class AutoAdoptTest {
    private fun session(id: String, adopted: Boolean) = DiscoverableSession(id, id, "/work/$id", adopted, SessionStatus.Idle, Phase.Plan, 0)
    private val ownsAll: (String) -> Boolean = { true }

    @Test
    fun `only unadopted sessions are taken`() {
        assertEquals(listOf("a"), sessionsToAdopt(listOf(session("a", false), session("b", true)), ownsAll, emptySet()).map { it.sessionId })
    }

    /** Every window sees the whole fleet; unscoped, all of them race to adopt everything. */
    @Test
    fun `sessions this window does not own are left alone`() {
        assertEquals(listOf("mine"), sessionsToAdopt(listOf(session("mine", false), session("theirs", false)), { it == "mine" }, emptySet()).map { it.sessionId })
    }

    /** A five-second poll retrying a failing adopt is a request storm. */
    @Test
    fun `an already attempted session is not retried`() {
        assertEquals(emptyList<DiscoverableSession>(), sessionsToAdopt(listOf(session("a", false)), ownsAll, setOf("a")))
    }

    @Test
    fun `attempts are forgotten once the session is adopted`() {
        assertEquals(listOf("a"), staleAttempts(setOf("a"), listOf(session("a", true))))
    }

    @Test
    fun `attempts are forgotten once the session is gone`() {
        assertEquals(listOf("a"), staleAttempts(setOf("a"), emptyList()))
    }

    @Test
    fun `an attempt for a session still waiting is kept`() {
        assertEquals(emptyList<String>(), staleAttempts(setOf("a"), listOf(session("a", false))))
    }
}
