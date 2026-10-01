package io.github.titouanfreville.moonlight.client

import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNull
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

/** Ported from `events.test.ts`. */
class EventsTest {
    /**
     * A chunk boundary falls wherever the network put it, and the frame that gets split is
     * disproportionately likely to be a big one — which is to say a plan.
     */
    @Test
    fun `a frame split across chunks is held until its terminator arrives`() {
        val (first, rest) = readFrames("data: {\"SessionRemoved\":{\"ses")
        assertEquals(emptyList<SseFrame>(), first)

        val (second, rest2) = readFrames("${rest}sion\":\"s1\"}}\n\n")
        assertEquals(1, second.size)
        assertEquals("", rest2)
        assertEquals(EngineEvent.SessionRemoved("s1"), parseEngineEvent(second[0].data))
    }

    @Test
    fun `keep-alive comments and blank lines carry no frame`() {
        assertEquals(emptyList<SseFrame>(), readFrames(":\n\n: keep-alive\n\n").first)
    }

    @Test
    fun `a lagged frame keeps its event name`() {
        assertEquals(listOf(SseFrame("lagged", "12")), readFrames("event: lagged\ndata: 12\n\n").first)
    }

    @Test
    fun `a multi-line data payload is rejoined`() {
        assertEquals("{\"a\":\n1}", readFrames("data: {\"a\":\ndata: 1}\n\n").first[0].data)
    }

    @Test
    fun `CRLF line endings are tolerated`() {
        assertEquals(listOf(SseFrame("message", "x")), readFrames("data: x\r\n\n").first)
    }

    @Test
    fun `a plan proposal carries its markdown`() {
        assertEquals(
            EngineEvent.PlanProposed("s1", "# Step one"),
            parseEngineEvent("""{"PlanProposed":{"session":"s1","plan":"# Step one"}}"""),
        )
    }

    @Test
    fun `an approval request reports its tool only when there is one`() {
        assertEquals(
            EngineEvent.ApprovalRequested("s1", "approve plan", null),
            parseEngineEvent("""{"ApprovalRequested":{"session":"s1","what":"approve plan","authorize_tool":null}}"""),
        )
        assertEquals(
            EngineEvent.ApprovalRequested("s1", "run a tool", "mcp__x__y"),
            parseEngineEvent("""{"ApprovalRequested":{"session":"s1","what":"run a tool","authorize_tool":"mcp__x__y"}}"""),
        )
    }

    /** Falling over on an unmodelled variant would take the plan gate down with it. */
    @Test
    fun `an unmodelled variant is ignored rather than fatal`() {
        assertNull(parseEngineEvent("""{"AuditAppended":{"session":"s1","summary":"x"}}"""))
        assertNull(parseEngineEvent("not json at all"))
        assertNull(parseEngineEvent("""{"PlanProposed":{"session":"s1"}}"""))
    }

    @Test
    fun `only WaitingInput counts as waiting on the operator`() {
        assertTrue(statusIsWaiting("WaitingInput"))
        assertFalse(statusIsWaiting("Running"))
        assertFalse(statusIsWaiting("Idle"))
    }

    @Test
    fun `a session alert is parsed, and a cleared one carries no kind`() {
        assertEquals(
            EngineEvent.SessionAlert("s1", "Incomplete"),
            parseEngineEvent("""{"SessionAlert":{"session":"s1","alert":"Incomplete"}}"""),
        )
        assertEquals(EngineEvent.SessionAlert("s1", null), parseEngineEvent("""{"SessionAlert":{"session":"s1","alert":null}}"""))
    }
}
