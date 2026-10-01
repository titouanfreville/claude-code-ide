package io.github.titouanfreville.moonlight.agentic

import io.github.titouanfreville.moonlight.client.PLAN_HOLD
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test

class HoldRoutingTest {
    /** A session that had ever proposed a plan once sent its Bash approvals to the plan review. */
    @Test
    fun `only the gate's plan marker routes to the plan review`() {
        assertTrue(isPlanHold(PLAN_HOLD))
        assertFalse(isPlanHold("run `npx js-yaml`"))
        assertFalse(isPlanHold("write outside the workspace"))
    }

    @Test
    fun `the owning window announces, and only it`() {
        assertTrue(announcesHere("a", owner = "a", focused = "b", firstOpen = "b"))
        assertFalse(announcesHere("b", owner = "a", focused = "b", firstOpen = "b"))
    }

    /** No owner is not nobody: a held agent nobody is told about stays held. */
    @Test
    fun `with no owner the focused window announces, else the first open one`() {
        assertTrue(announcesHere("b", owner = null, focused = "b", firstOpen = "a"))
        assertFalse(announcesHere("a", owner = null, focused = "b", firstOpen = "a"))
        assertTrue(announcesHere("a", owner = null, focused = null, firstOpen = "a"))
    }
}
