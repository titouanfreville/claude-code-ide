package io.github.titouanfreville.moonlight.client

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.assertThrows

class ModelTest {
    @Test
    fun `a discoverable session decodes from the server's snake_case`() {
        val raw = Json.parse(
            """{"session_id":"s1","title":null,"root":"/w","adopted":true,"status":"WaitingInput","phase":"Plan","unreviewed_files":3}"""
        )
        assertEquals(
            DiscoverableSession("s1", null, "/w", true, SessionStatus.WaitingInput, Phase.Plan, 3),
            DiscoverableSession.decode(raw),
        )
    }

    @Test
    fun `a session names the launch it continues, and an old daemon names none`() {
        val resumed = DiscoverableSession.decode(
            Json.parse("""{"session_id":"213d1944","title":null,"root":"/w","adopted":true,"status":"Running","phase":"Plan","unreviewed_files":0,"launch_id":"a05a347e"}""")
        )
        assertEquals("a05a347e", resumed.launchId)
        assertTrue(resumed.isOrContinues("a05a347e"))
        assertTrue(resumed.isOrContinues("213d1944"))
        assertFalse(resumed.isOrContinues("other"))

        val old = DiscoverableSession.decode(
            Json.parse("""{"session_id":"s1","title":null,"root":null,"adopted":false,"status":"Idle","phase":"Plan","unreviewed_files":0}""")
        )
        assertNull(old.launchId)
    }

    @Test
    fun `usage keeps unknown figures unknown`() {
        val raw = Json.parse("""{"quota":{"five_hour_pct":null,"weekly_pct":12,"sonnet_pct":null},"sessions":[]}""")
        val quota = UsageResponse.decode(raw).quota!!
        assertNull(quota.fiveHourPct)
        assertEquals(12.0, quota.weeklyPct)
        assertNull(UsageResponse.decode(Json.parse("""{"quota":null,"sessions":[]}""")).quota)
    }

    @Test
    fun `tagged unions decode by their tag`() {
        assertEquals(BaselineView.Created, BaselineView.decode(Json.parse("""{"kind":"created"}""")))
        assertEquals(BaselineView.FromHead("x"), BaselineView.decode(Json.parse("""{"kind":"from_head","text":"x"}""")))
        assertEquals(
            FeedbackDelivery.Undeliverable("headless"),
            FeedbackDelivery.decode(Json.parse("""{"status":"undeliverable","reason":"headless"}""")),
        )
    }

    @Test
    fun `a missing required field is an error, not a default`() {
        assertThrows<DecodeException> { DiscoverableSession.decode(Json.parse("""{"session_id":"s1"}""")) }
        assertThrows<DecodeException> {
            DiscoverableSession.decode(
                Json.parse("""{"session_id":"s1","adopted":true,"status":"Bogus","phase":"Plan","unreviewed_files":0}""")
            )
        }
    }

    @Test
    fun `frozen phases are Plan and Commit`() {
        assertEquals(listOf(Phase.Plan, Phase.Commit), Phase.ORDERED.filter { it.frozen })
    }
}
